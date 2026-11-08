//! Draws a release candidate's or beta's corner ribbon over the Dock icon.
//! What it says, its colours and where it goes come from
//! [`crate::dock_ribbon`]; this file only draws them into a new image.

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSAffineTransformNSAppKitAdditions, NSBezierPath, NSBitmapImageRep, NSColor, NSColorSpace,
    NSDeviceRGBColorSpace, NSFont, NSFontAttributeName, NSFontWeightHeavy,
    NSForegroundColorAttributeName, NSGraphicsContext, NSImage, NSImageInterpolation,
    NSKernAttributeName, NSStringDrawing, NSStringDrawingOptions, NSStringNSExtendedStringDrawing,
};
use objc2_foundation::{
    NSAffineTransform, NSDictionary, NSNumber, NSPoint, NSRect, NSSize, NSString,
};

use crate::dock_ribbon::{Rgb, Ribbon, RibbonLayout};

/// Pixels per point of the drawn image: 2, so the Dock's largest tile
/// (128 points) gets the embedded images' 256 pixels.
const SCALE: f64 = 2.0;

/// A `size`-point square image of `icon` with `ribbon` across its top-right
/// corner, clipped to the icon's body. `None` if AppKit can't make the
/// bitmap.
pub(super) fn with_ribbon(icon: &NSImage, ribbon: &Ribbon, size: f64) -> Option<Retained<NSImage>> {
    let rep = draw(icon, ribbon, size)?;
    let image = NSImage::initWithSize(NSImage::alloc(), NSSize::new(size, size));
    image.addRepresentation(&rep);
    Some(image)
}

/// The bitmap of [`with_ribbon`]: `size` points square at [`SCALE`].
fn draw(icon: &NSImage, ribbon: &Ribbon, size: f64) -> Option<Retained<NSBitmapImageRep>> {
    let pixels = (size * SCALE).round();
    let side = pixels as isize;
    // SAFETY: null planes make AppKit allocate the pixel buffer; the rest
    // describes 8-bit RGBA with the row and pixel sizes computed by AppKit.
    // The colour space name is an immutable framework constant.
    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            side,
            side,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            0,
            0,
        )
    }?;
    // Draw in sRGB, the colours' space.
    let rep = rep.bitmapImageRepByRetaggingWithColorSpace(&NSColorSpace::sRGBColorSpace())?;
    let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep)?;
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&context));
    context.setImageInterpolation(NSImageInterpolation::High);
    // Until its size is set below, the bitmap is as many points as pixels,
    // so this draws in pixels.
    icon.drawInRect(rect(0.0, 0.0, pixels, pixels));
    draw_band(ribbon, &RibbonLayout::for_image(pixels));
    context.flushGraphics();
    NSGraphicsContext::restoreGraphicsState_class();

    rep.setSize(NSSize::new(size, size));
    Some(rep)
}

/// Draws the band and its text in the current context, clipped to the
/// icon's body.
fn draw_band(ribbon: &Ribbon, layout: &RibbonLayout) {
    let body = rect(
        layout.body_x,
        layout.body_y,
        layout.body_side,
        layout.body_side,
    );
    NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
        body,
        layout.corner_radius,
        layout.corner_radius,
    )
    .addClip();
    // From here on the origin is the band's centre and x runs along it.
    let transform = NSAffineTransform::transform();
    transform.translateXBy_yBy(layout.band_center_x, layout.band_center_y);
    transform.rotateByDegrees(layout.rotation_degrees);
    transform.concat();
    color(ribbon.fill).setFill();
    NSBezierPath::fillRect(rect(
        -layout.band_length / 2.0,
        -layout.band_thickness / 2.0,
        layout.band_length,
        layout.band_thickness,
    ));

    // SAFETY: the weight is an immutable framework constant.
    let font = NSFont::systemFontOfSize_weight(layout.font_size, unsafe { NSFontWeightHeavy });
    let text_color = color(ribbon.text_color);
    let kern = NSNumber::numberWithDouble(layout.kern);
    let values: [&AnyObject; 3] = [font.as_ref(), text_color.as_ref(), kern.as_ref()];
    // SAFETY: the attribute names are immutable framework constants.
    let keys = unsafe {
        [
            NSFontAttributeName,
            NSForegroundColorAttributeName,
            NSKernAttributeName,
        ]
    };
    let attributes = NSDictionary::<NSString, AnyObject>::from_slices(&keys, &values);
    let text = NSString::from_str(ribbon.text);
    // SAFETY: each attribute holds the class AppKit expects for it (NSFont,
    // NSColor, NSNumber).
    let size = unsafe { text.sizeWithAttributes(Some(&attributes)) };
    // Centred: across by its width, which, as in the design, ends with one
    // letter spacing; along the band's centre line by the capitals' middle.
    // Without UsesLineFragmentOrigin the rectangle's origin is the baseline.
    let at = rect(
        -size.width / 2.0,
        -font.capHeight() / 2.0,
        size.width,
        size.height,
    );
    // SAFETY: as for `sizeWithAttributes`.
    unsafe {
        text.drawWithRect_options_attributes_context(
            at,
            NSStringDrawingOptions::empty(),
            Some(&attributes),
            None,
        );
    }
}

/// An sRGB colour, opaque.
fn color(rgb: Rgb) -> Retained<NSColor> {
    let [red, green, blue] = rgb.components();
    NSColor::colorWithSRGBRed_green_blue_alpha(red, green, blue, 1.0)
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

#[cfg(test)]
mod tests {
    use fx_channel::Channel;
    use objc2_foundation::NSData;

    use super::*;
    use crate::app_icon::AppIcon;
    use crate::dock_ribbon::ribbon;

    /// The Dock image's size in points, as the app draws it.
    const POINTS: f64 = 128.0;

    fn icon_image(icon: AppIcon, dark: bool) -> Retained<NSImage> {
        let data = NSData::with_bytes(icon.png(dark));
        NSImage::initWithData(NSImage::alloc(), &data).expect("the embedded PNG decodes")
    }

    /// The pixel at (`x`, `y`) in AppKit's coordinates (y up), as 0-255
    /// red, green, blue and alpha.
    fn pixel(rep: &NSBitmapImageRep, x: f64, y: f64) -> [u8; 4] {
        let height = rep.pixelsHigh();
        let color = rep
            .colorAtX_y(x.round() as isize, height - 1 - y.round() as isize)
            .expect("a pixel inside the bitmap");
        [
            color.redComponent(),
            color.greenComponent(),
            color.blueComponent(),
            color.alphaComponent(),
        ]
        .map(|c| (c * 255.0).round() as u8)
    }

    fn near(got: [u8; 4], rgb: Rgb, tolerance: u8) -> bool {
        let [r, g, b] = rgb.components().map(|c| (c * 255.0).round() as u8);
        got[3] == 255
            && [(got[0], r), (got[1], g), (got[2], b)]
                .iter()
                .all(|&(a, b)| a.abs_diff(b) <= tolerance)
    }

    /// Draws the RC and both BETA ribbons off screen and checks the band
    /// where the layout puts it: its colour either side of the text, the
    /// text's colour in the middle, and nothing outside the icon's body.
    #[test]
    fn draws_the_band_and_text_across_the_corner_clipped_to_the_body() {
        let cases = [
            (Channel::Rc, AppIcon::MatchRow, false),
            (Channel::Beta, AppIcon::SoftLens, false),
            (Channel::Beta, AppIcon::LitSlot, true),
        ];
        for (channel, icon, dark) in cases {
            let ribbon = ribbon(channel, icon, dark).expect("a Preview build draws a ribbon");
            let rep = draw(&icon_image(icon, dark), &ribbon, POINTS).expect("a bitmap");
            assert_eq!((rep.pixelsWide(), rep.pixelsHigh()), (256, 256));
            assert_eq!(rep.size(), NSSize::new(POINTS, POINTS));

            let layout = RibbonLayout::for_image(256.0);
            let angle = layout.rotation_degrees.to_radians();
            // A point `along` the band's centre line from its centre and
            // `across` it.
            let at = |along: f64, across: f64| {
                (
                    layout.band_center_x + along * angle.cos() - across * angle.sin(),
                    layout.band_center_y + along * angle.sin() + across * angle.cos(),
                )
            };
            for along in [-32.0, 32.0] {
                let (x, y) = at(along, 0.0);
                let got = pixel(&rep, x, y);
                assert!(
                    near(got, ribbon.fill, 2),
                    "{channel:?} fill at {along}: {got:?}"
                );
            }
            let text = (-12..=12)
                .flat_map(|a| (-4..=4).map(move |c| (f64::from(a), f64::from(c))))
                .filter(|&(a, c)| {
                    let (x, y) = at(a, c);
                    near(pixel(&rep, x, y), ribbon.text_color, 24)
                })
                .count();
            assert!(text > 10, "{channel:?}: {text} text pixels");
            // Past the body's right edge the band is clipped away.
            let (x, y) = at(95.0, 0.0);
            assert!(x > layout.body_x + layout.body_side + 4.0, "{x}");
            let got = pixel(&rep, x, y);
            assert!(!near(got, ribbon.fill, 24), "{channel:?} outside: {got:?}");
        }
    }
}
