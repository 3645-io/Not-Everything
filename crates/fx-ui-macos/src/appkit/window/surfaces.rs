//! Shared images of the interface's surfaces and glyphs, so the custom-drawn
//! window stays as fast as standard controls: each surface is rendered
//! once, no view draws a shadow of its own, and scrolling draws no surface.
//!
//! [`SurfaceCache`] renders each [`Surface`] once per theme and size class
//! ([`crate::surface`]) into an `NSImage` holding a 1x and a 2x
//! representation (or, for a cache made [`SurfaceCache::for_scale`], only
//! the screen's: the search window keeps no 2x pixels on a 1x screen),
//! with cap insets for nine-part stretching, and hands the
//! same image to every view that asks: all rows share one selected image,
//! all chips theirs. Glyphs are rendered once per size into template
//! images, which the view tints (`NSImageView.contentTintColor`).
//!
//! A view shows a surface either by drawing it ([`SurfaceImage::draw`],
//! which respects the cap insets), or, better for scrolling, as a layer's
//! contents: `layer.contents = image.layerContentsForContentsScale(scale)`,
//! `contentsScale = image.recommendedLayerContentsScale(scale)`,
//! `contentsCenter =` [`SurfaceImage::contents_center`], with the layer's
//! frame [`SurfaceImage::frame_for`] the view's bounds (the image reaches
//! past the bounds by the outer shadows). Surfaces without shadows
//! (`SurfaceStyle::needs_image` false) are layer properties instead.
//!
//! Main thread only.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSBitmapImageRep, NSColorSpace, NSCompositingOperation, NSDeviceRGBColorSpace, NSImage,
    NSImageResizingMode,
};
use objc2_foundation::{NSEdgeInsets, NSPoint, NSRect, NSSize};

use crate::file_glyph::Glyph;
use crate::surface::{Pixmap, SizeClass, Surface, TemplateLayout, render, render_glyph};
use crate::theme::Theme;

/// `NSImageResizingModeStretch`: `NSImage.h` gives it 0 on Intel Macs and,
/// where the ABI uses iOS values (`TargetConditionals.h`: every CPU but
/// x86-64), 1.
const RESIZING_STRETCH: NSImageResizingMode =
    NSImageResizingMode(if cfg!(target_arch = "x86_64") { 0 } else { 1 });

/// The scales an image is rendered at when the cache serves any screen:
/// AppKit picks the representation for the screen it draws on.
const SCALES: [f64; 2] = [1.0, 2.0];

/// A rendered surface: the shared image and where the element sits in it.
pub struct SurfaceImage {
    image: Retained<NSImage>,
    layout: TemplateLayout,
}

impl SurfaceImage {
    /// The image, with 1x and 2x representations and cap insets.
    pub fn image(&self) -> &NSImage {
        &self.image
    }

    /// The template layout it was rendered from.
    pub fn layout(&self) -> &TemplateLayout {
        &self.layout
    }

    /// Where to draw the image (or put its layer) for an element at
    /// `bounds`: the bounds grown by the outer shadows' reach. `flipped`
    /// when the coordinates run down (a flipped view).
    pub fn frame_for(&self, bounds: NSRect, flipped: bool) -> NSRect {
        let o = &self.layout.outsets;
        let (below, above) = if flipped {
            (o.top, o.bottom)
        } else {
            (o.bottom, o.top)
        };
        NSRect::new(
            NSPoint::new(bounds.origin.x - o.left, bounds.origin.y - below),
            NSSize::new(
                bounds.size.width + o.left + o.right,
                bounds.size.height + below + above,
            ),
        )
    }

    /// The layer's `contentsCenter` for this image (see the module
    /// documentation), for a layer whose geometry is `flipped` or not.
    pub fn contents_center(&self, flipped: bool) -> NSRect {
        let [x, y, w, h] = self.layout.contents_center(flipped);
        NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    }

    /// Draws the surface for an element at `bounds` in the current
    /// graphics context, upright in a `flipped` context too.
    pub fn draw(&self, bounds: NSRect, flipped: bool) {
        // SAFETY: no hints are passed; the rest are plain values.
        unsafe {
            self.image
                .drawInRect_fromRect_operation_fraction_respectFlipped_hints(
                    self.frame_for(bounds, flipped),
                    NSRect::ZERO,
                    NSCompositingOperation::SourceOver,
                    1.0,
                    true,
                    None,
                );
        }
    }
}

/// The shared surface and glyph images. See the module documentation.
#[derive(Default)]
pub struct SurfaceCache {
    surfaces: RefCell<HashMap<(Surface, Theme, SizeClass), Rc<SurfaceImage>>>,
    glyphs: RefCell<HashMap<(Glyph, u32), Retained<NSImage>>>,
    /// The one screen scale images are rendered at, or `None` for 1x and
    /// 2x ([`SCALES`]).
    scale: Cell<Option<f64>>,
}

impl SurfaceCache {
    /// An empty cache whose images hold a 1x and a 2x representation.
    pub fn new() -> SurfaceCache {
        SurfaceCache::default()
    }

    /// An empty cache whose images hold one representation, for a screen
    /// of `scale` (the search window's, so a 1x screen keeps no 2x pixels;
    /// see [`set_scale`](Self::set_scale)).
    pub fn for_scale(scale: f64) -> SurfaceCache {
        let cache = SurfaceCache::default();
        cache.scale.set(Some(scale));
        cache
    }

    /// Renders for a screen of `scale` from now on (the window moved to
    /// another screen). Every image of another scale is dropped; the views
    /// showing them ask again. Returns whether the scale changed.
    pub fn set_scale(&self, scale: f64) -> bool {
        if self.scale.get() == Some(scale) {
            return false;
        }
        self.scale.set(Some(scale));
        self.surfaces.borrow_mut().clear();
        self.glyphs.borrow_mut().clear();
        true
    }

    /// The scales images are rendered at.
    fn scales(&self) -> Vec<f64> {
        match self.scale.get() {
            Some(scale) => vec![scale],
            None => SCALES.to_vec(),
        }
    }

    /// `surface` in `theme` for an element of `size` points (rounded to
    /// whole points), rendered on first use. Every size of the same class
    /// gets the same image.
    pub fn surface(&self, surface: Surface, theme: Theme, size: NSSize) -> Rc<SurfaceImage> {
        let style = surface.style(theme);
        let layout = TemplateLayout::new(&style, size.width, size.height);
        let key = (surface, theme, layout.size_class());
        if let Some(found) = self.surfaces.borrow().get(&key) {
            return Rc::clone(found);
        }
        let pixmaps: Vec<Pixmap> = self
            .scales()
            .into_iter()
            .map(|s| render(&style, &layout, s))
            .collect();
        let points = NSSize::new(layout.image_width(), layout.image_height());
        let image = image_from_pixmaps(points, &pixmaps);
        let c = &layout.cap_insets;
        image.setCapInsets(NSEdgeInsets {
            top: c.top,
            left: c.left,
            bottom: c.bottom,
            right: c.right,
        });
        image.setResizingMode(RESIZING_STRETCH);
        let made = Rc::new(SurfaceImage { image, layout });
        self.surfaces.borrow_mut().insert(key, Rc::clone(&made));
        made
    }

    /// `glyph` as a template image `size` points square (rounded to a
    /// quarter point), rendered on first use.
    pub fn glyph(&self, glyph: Glyph, size: f64) -> Retained<NSImage> {
        let quarters = (size * 4.0).round().max(1.0) as u32;
        let key = (glyph, quarters);
        if let Some(found) = self.glyphs.borrow().get(&key) {
            return found.clone();
        }
        let side = f64::from(quarters) / 4.0;
        let pixmaps: Vec<Pixmap> = self
            .scales()
            .into_iter()
            .map(|s| render_glyph(glyph, side, s))
            .collect();
        let image = image_from_pixmaps(NSSize::new(side, side), &pixmaps);
        image.setTemplate(true);
        self.glyphs.borrow_mut().insert(key, image.clone());
        image
    }

    /// How many surface and glyph images the cache holds.
    pub fn len(&self) -> usize {
        self.surfaces.borrow().len() + self.glyphs.borrow().len()
    }

    /// Whether the cache holds no image.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drops every image of `theme`'s surfaces (after the appearance
    /// changed, the other theme's images are no longer shown).
    pub fn forget_theme(&self, theme: Theme) {
        self.surfaces
            .borrow_mut()
            .retain(|(_, t, _), _| *t != theme);
    }
}

/// An image `points` large with one sRGB bitmap representation per pixmap
/// (each `points` × its scale pixels).
///
/// # Panics
///
/// Panics if AppKit can't make a bitmap (out of memory).
pub fn image_from_pixmaps(points: NSSize, pixmaps: &[Pixmap]) -> Retained<NSImage> {
    let image = NSImage::initWithSize(NSImage::alloc(), points);
    for pixmap in pixmaps {
        let rep = bitmap(pixmap).expect("AppKit makes an RGBA bitmap");
        rep.setSize(points);
        image.addRepresentation(&rep);
    }
    image
}

/// A bitmap holding `pixmap`'s premultiplied RGBA pixels, tagged sRGB.
fn bitmap(pixmap: &Pixmap) -> Option<Retained<NSBitmapImageRep>> {
    let (w, h) = (pixmap.width as isize, pixmap.height as isize);
    // SAFETY: null planes make AppKit allocate the pixel buffer; the rest
    // describes 8-bit RGBA (premultiplied, alpha last: the default format)
    // with the row and pixel sizes computed by AppKit. The colour space
    // name is an immutable framework constant.
    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            w,
            h,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            0,
            0,
        )
    }?;
    let rep = rep.bitmapImageRepByRetaggingWithColorSpace(&NSColorSpace::sRGBColorSpace())?;
    let stride = usize::try_from(rep.bytesPerRow()).ok()?;
    let row = pixmap.width * 4;
    if stride < row || rep.pixelsWide() != w || rep.pixelsHigh() != h {
        return None;
    }
    let data = rep.bitmapData();
    if data.is_null() {
        return None;
    }
    for y in 0..pixmap.height {
        let src = &pixmap.data[y * row..(y + 1) * row];
        // SAFETY: the bitmap owns `stride × height` bytes from `data`
        // (checked above: `stride` covers a row of `row` bytes, and `y` is
        // below the height); `src` is a separate Rust buffer.
        unsafe {
            std::ptr::copy_nonoverlapping(src.as_ptr(), data.add(y * stride), row);
        }
    }
    Some(rep)
}

#[cfg(test)]
pub(crate) mod tests {
    use objc2_app_kit::{NSGraphicsContext, NSImageRep};

    use super::*;
    use crate::surface::Pixmap;

    /// A transparent `points`-point bitmap at `scale`, made the current
    /// context while `draw` runs; returns its pixels (premultiplied RGBA,
    /// rows from the top).
    pub(crate) fn draw_offscreen(points: NSSize, scale: f64, draw: impl FnOnce()) -> Pixmap {
        let (w, h) = (
            (points.width * scale).round() as usize,
            (points.height * scale).round() as usize,
        );
        let blank = Pixmap {
            width: w,
            height: h,
            data: vec![0; w * h * 4],
        };
        let rep = bitmap(&blank).expect("a bitmap");
        rep.setSize(points);
        let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep).expect("context");
        NSGraphicsContext::saveGraphicsState_class();
        NSGraphicsContext::setCurrentContext(Some(&context));
        draw();
        context.flushGraphics();
        NSGraphicsContext::restoreGraphicsState_class();
        let stride = rep.bytesPerRow() as usize;
        let data = rep.bitmapData();
        let mut out = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            // SAFETY: the bitmap holds `stride × h` bytes and `y < h`; each
            // row has at least `w × 4` bytes.
            let row = unsafe { std::slice::from_raw_parts(data.add(y * stride), w * 4) };
            out.extend_from_slice(row);
        }
        Pixmap {
            width: w,
            height: h,
            data: out,
        }
    }

    fn max_difference(a: &Pixmap, b: &Pixmap) -> u8 {
        assert_eq!((a.width, a.height), (b.width, b.height));
        a.data
            .iter()
            .zip(&b.data)
            .map(|(x, y)| x.abs_diff(*y))
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn surfaces_are_shared_per_theme_and_size_class() {
        let cache = SurfaceCache::new();
        assert!(cache.is_empty());
        let a = cache.surface(Surface::RowSelected, Theme::Light, NSSize::new(900.0, 40.0));
        let b = cache.surface(Surface::RowSelected, Theme::Light, NSSize::new(640.0, 40.0));
        assert!(Rc::ptr_eq(&a, &b), "every row width shares one image");
        let c = cache.surface(Surface::RowSelected, Theme::Dark, NSSize::new(900.0, 40.0));
        assert!(!Rc::ptr_eq(&a, &c));
        let d = cache.surface(Surface::RowSelected, Theme::Light, NSSize::new(900.0, 36.0));
        assert!(!Rc::ptr_eq(&a, &d), "another height is another class");
        assert_eq!(cache.len(), 3);
        cache.forget_theme(Theme::Dark);
        assert_eq!(cache.len(), 2);

        let image = a.image();
        assert_eq!(image.size(), NSSize::new(42.0, 40.0));
        assert_eq!(image.resizingMode(), RESIZING_STRETCH);
        assert_eq!(
            image.capInsets(),
            NSEdgeInsets {
                top: 20.0,
                left: 20.0,
                bottom: 20.0,
                right: 20.0,
            }
        );
        let reps = image.representations();
        let pixels: Vec<(isize, isize)> = reps
            .iter()
            .map(|r: Retained<NSImageRep>| (r.pixelsWide(), r.pixelsHigh()))
            .collect();
        assert_eq!(pixels, [(42, 40), (84, 80)]);
    }

    /// A cache for one screen scale renders only that representation (a
    /// 1x screen keeps no 2x pixels), and a new scale drops what it had.
    #[test]
    fn a_cache_for_one_scale_renders_only_it() {
        let cache = SurfaceCache::for_scale(1.0);
        let row = cache.surface(Surface::RowSelected, Theme::Light, NSSize::new(900.0, 40.0));
        let pixels: Vec<isize> = row
            .image()
            .representations()
            .iter()
            .map(|r| r.pixelsWide())
            .collect();
        assert_eq!(pixels, [42]);
        let glyph = cache.glyph(Glyph::Folder, 18.0);
        assert_eq!(glyph.representations().len(), 1);
        assert_eq!(cache.len(), 2);

        cache.set_scale(1.0);
        assert_eq!(cache.len(), 2, "the same scale keeps the images");
        cache.set_scale(2.0);
        assert!(cache.is_empty(), "another scale drops them");
        let row = cache.surface(Surface::RowSelected, Theme::Light, NSSize::new(900.0, 40.0));
        let pixels: Vec<isize> = row
            .image()
            .representations()
            .iter()
            .map(|r| r.pixelsWide())
            .collect();
        assert_eq!(pixels, [84]);
        assert_eq!(row.image().size(), NSSize::new(42.0, 40.0));
    }

    #[test]
    fn frames_grow_by_the_outer_shadows() {
        let cache = SurfaceCache::new();
        let panel = cache.surface(Surface::Panel, Theme::Light, NSSize::new(400.0, 300.0));
        let bounds = NSRect::new(NSPoint::new(16.0, 20.0), NSSize::new(400.0, 300.0));
        // Light raised: 15 up and left, 18 down and right.
        assert_eq!(
            panel.frame_for(bounds, false),
            NSRect::new(NSPoint::new(1.0, 2.0), NSSize::new(433.0, 333.0))
        );
        assert_eq!(
            panel.frame_for(bounds, true),
            NSRect::new(NSPoint::new(1.0, 5.0), NSSize::new(433.0, 333.0))
        );
        let center = panel.contents_center(false);
        assert_eq!(center.size, NSSize::new(2.0 / 103.0, 2.0 / 103.0));
    }

    /// Off screen, the stretched nine-part image drawn at any size gives
    /// the pixels of the surface rendered at exactly that size, at 1x and
    /// 2x (AppKit picks the matching representation).
    #[test]
    fn a_stretched_image_draws_the_exact_surface() {
        let cache = SurfaceCache::new();
        for (surface, theme, w, h) in [
            (Surface::Panel, Theme::Light, 260.0, 180.0),
            (Surface::Panel, Theme::Dark, 300.0, 120.0),
            (Surface::RowSelected, Theme::Light, 320.0, 40.0),
            (Surface::ChipOn, Theme::Dark, 90.0, 40.0),
            (Surface::Field, Theme::Light, 500.0, 46.0),
        ] {
            let shared = cache.surface(surface, theme, NSSize::new(w, h));
            let style = surface.style(theme);
            let exact = TemplateLayout::exact(&style, w, h);
            let o = exact.outsets;
            let bounds = NSRect::new(NSPoint::new(o.left, o.bottom), NSSize::new(w, h));
            let points = NSSize::new(exact.image_width(), exact.image_height());
            for scale in [1.0, 2.0] {
                let drawn = draw_offscreen(points, scale, || shared.draw(bounds, false));
                let want = render(&style, &exact, scale);
                let diff = max_difference(&drawn, &want);
                assert!(
                    diff <= 2,
                    "{surface:?} {theme:?} at {scale}x: off by {diff}"
                );
            }
        }
    }

    #[test]
    fn glyphs_are_shared_template_images() {
        let cache = SurfaceCache::new();
        let a = cache.glyph(Glyph::Folder, 18.0);
        let b = cache.glyph(Glyph::Folder, 18.0);
        assert!(std::ptr::eq(&*a, &*b), "one image per glyph and size");
        assert!(a.isTemplate());
        assert_eq!(a.size(), NSSize::new(18.0, 18.0));
        let reps: Vec<isize> = a.representations().iter().map(|r| r.pixelsWide()).collect();
        assert_eq!(reps, [18, 36]);
        // Drawn at 2x, it is the rendered mask.
        let drawn = draw_offscreen(NSSize::new(18.0, 18.0), 2.0, || {
            a.drawInRect(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(18.0, 18.0)));
        });
        let want = render_glyph(Glyph::Folder, 18.0, 2.0);
        assert!(max_difference(&drawn, &want) <= 1);
        assert_eq!(cache.len(), 1);
    }
}
