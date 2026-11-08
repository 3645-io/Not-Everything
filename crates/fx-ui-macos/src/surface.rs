//! The interface's surfaces and glyphs as pixels: every raised, pressed,
//! key and float surface of the canvas, rendered once per theme and size
//! class into a stretchable nine-part template, and every glyph rendered
//! once per size. The AppKit layer (`appkit::window::surfaces`) wraps the
//! pixels in cached `NSImage`s that every row and control shares, so no
//! view draws a shadow of its own.
//!
//! # Surfaces
//!
//! [`Surface`] names each surface the canvas draws; [`Surface::style`] gives
//! its [`SurfaceStyle`] in a theme, from `crate::theme`'s tokens: a fill, an
//! optional border, corner radii and a CSS `box-shadow` list. Styles without
//! shadows ([`SurfaceStyle::needs_image`] is false: a row's hover, the
//! callout, the channel tag) are cheaper as layer properties
//! (`backgroundColor`, `cornerRadius`, `borderWidth`, `borderColor`) and
//! need no image.
//!
//! [`render`] paints a style the way CSS does: outer shadows (last listed
//! at the bottom), the fill over the border box, inset shadows clipped to
//! the padding box, then the border. Each shadow is its shape (the border
//! box, or for an inset shadow the inverse of the padding box) offset, grown
//! or shrunk by its spread, and blurred by a Gaussian of standard deviation
//! blur ÷ 2; an outer shadow shows only outside the border box. The blur is
//! a true Gaussian (separable, cut at 3σ), not the three-box approximation
//! of the filter specifications, whose σ runs about 10% small at these
//! sizes. Colours are sRGB, composited in sRGB like a browser, with
//! antialiased edges.
//!
//! # Nine-part templates
//!
//! [`TemplateLayout::new`] fits a style to a size class. On each axis the
//! template stretches when the asked length leaves room for both corners'
//! reach (radius plus the farthest shadow: offset, spread and three
//! standard deviations): the template is then two corners plus a
//! [`STRETCH`]-point middle that is uniform along that axis, so one template
//! serves every length. Shorter lengths (a 40-point row's height, a key cap)
//! are rendered at their own length. The image is larger than the element
//! by the outer shadows' reach ([`TemplateLayout::outsets`]); the view draws
//! it over its bounds grown by those outsets, with the cap insets
//! ([`TemplateLayout::cap_insets`]) keeping the corners and edges unscaled.
//!
//! # Glyphs
//!
//! [`render_glyph`] strokes a [`Glyph`]'s path (round caps and joins) into
//! an alpha mask, a template image the view tints.

#![forbid(unsafe_code)]

use crate::FixedList;
use crate::file_glyph::{GRID, Glyph, flatten};
use crate::theme::{
    RADIUS_LG, RADIUS_MD, RADIUS_PILL, RADIUS_SM, Rgba, Shadow, ShadowToken, Theme, Token, metrics,
};

/// The length of a template's stretched middle, in points.
pub const STRETCH: f64 = 2.0;

/// Corner radii in points, as CSS writes them (before clamping).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Corners {
    /// Top left.
    pub top_left: f64,
    /// Top right.
    pub top_right: f64,
    /// Bottom right.
    pub bottom_right: f64,
    /// Bottom left.
    pub bottom_left: f64,
}

impl Corners {
    /// The same radius at every corner.
    pub const fn all(r: f64) -> Corners {
        Corners {
            top_left: r,
            top_right: r,
            bottom_right: r,
            bottom_left: r,
        }
    }

    /// Square top corners and radius `r` at the bottom (the top bar).
    pub const fn bottom(r: f64) -> Corners {
        Corners {
            top_left: 0.0,
            top_right: 0.0,
            bottom_right: r,
            bottom_left: r,
        }
    }

    /// The largest radius.
    pub fn max(&self) -> f64 {
        self.top_left
            .max(self.top_right)
            .max(self.bottom_right)
            .max(self.bottom_left)
    }

    fn map(&self, f: impl Fn(f64) -> f64) -> Corners {
        Corners {
            top_left: f(self.top_left),
            top_right: f(self.top_right),
            bottom_right: f(self.bottom_right),
            bottom_left: f(self.bottom_left),
        }
    }

    /// The radii a `width` × `height` box really gets: CSS scales every
    /// radius down by the same factor until no side's two radii overlap,
    /// so `radius-pill` (999) becomes half the shorter side.
    pub fn clamped(&self, width: f64, height: f64) -> Corners {
        let sides = [
            (width, self.top_left + self.top_right),
            (width, self.bottom_left + self.bottom_right),
            (height, self.top_left + self.bottom_left),
            (height, self.top_right + self.bottom_right),
        ];
        let f = sides
            .iter()
            .filter(|(_, sum)| *sum > 0.0)
            .map(|(len, sum)| len / sum)
            .fold(1.0_f64, f64::min);
        self.map(|r| r * f)
    }
}

/// A border inside the border box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Border {
    /// Width in points.
    pub width: f64,
    /// Colour.
    pub color: Rgba,
}

/// How a surface looks: what [`render`] paints.
#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceStyle {
    /// The background over the border box, if any.
    pub fill: Option<Rgba>,
    /// The border, if any.
    pub border: Option<Border>,
    /// Corner radii before clamping.
    pub corners: Corners,
    /// The `box-shadow` list, top first.
    pub shadows: Vec<Shadow>,
}

/// Distances from each side, in points.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Insets {
    /// From the top.
    pub top: f64,
    /// From the left.
    pub left: f64,
    /// From the bottom.
    pub bottom: f64,
    /// From the right.
    pub right: f64,
}

impl SurfaceStyle {
    /// Whether the style has shadows, and so needs a rendered image; one
    /// without is drawn with layer properties.
    pub fn needs_image(&self) -> bool {
        !self.shadows.is_empty()
    }

    /// How far any shadow's blur reaches from its shape's edge, plus its
    /// offset and spread: the distance past which nothing changes.
    pub fn reach(&self) -> f64 {
        self.shadows
            .iter()
            .map(|s| s.x.abs().max(s.y.abs()) + s.spread.max(0.0) + 3.0 * s.sigma())
            .fold(0.0, f64::max)
    }

    /// How far the outer shadows reach outside the border box on each side,
    /// rounded up to whole points.
    pub fn outsets(&self) -> Insets {
        let mut o = Insets::default();
        for s in self.shadows.iter().filter(|s| !s.inset) {
            let blur = s.spread + 3.0 * s.sigma();
            o.left = o.left.max(blur - s.x);
            o.right = o.right.max(blur + s.x);
            o.top = o.top.max(blur - s.y);
            o.bottom = o.bottom.max(blur + s.y);
        }
        Insets {
            top: o.top.max(0.0).ceil(),
            left: o.left.max(0.0).ceil(),
            bottom: o.bottom.max(0.0).ceil(),
            right: o.right.max(0.0).ceil(),
        }
    }
}

/// A surface of the canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Surface {
    /// The results card and the Settings cards (`.ne-results`, `.card`).
    Panel,
    /// The top bar, floating over the page (`.ne-topbar`).
    TopBar,
    /// The search field at rest (`.search input`).
    Field,
    /// The search field with the focus: a `green-ink` rim and ring.
    FieldFocused,
    /// An option chip that is off (`.ne-toggle`).
    ChipOff,
    /// An option chip that is on.
    ChipOn,
    /// A result row under the pointer (`.ne-row:hover`).
    RowHover,
    /// A selected result row.
    RowSelected,
    /// A key cap (`.ne-key`).
    Key,
    /// A switch's track when off (`.ne-switch`).
    SwitchOff,
    /// A switch's track when on.
    SwitchOn,
    /// A switch's knob when off (`.ne-knob`).
    KnobOff,
    /// A switch's knob when on.
    KnobOn,
    /// A syntax example token (`.ne-code`).
    Code,
    /// A button at rest (`.btn`).
    Button,
    /// A button being pressed.
    ButtonDown,
    /// The primary button at rest (`.btn.primary`).
    PrimaryButton,
    /// The primary button being pressed.
    PrimaryButtonDown,
    /// The segmented control's container (`.seg`).
    Segmented,
    /// The chosen segment (`.seg button[aria-pressed=true]`).
    SegmentOn,
    /// An app icon pick under the pointer (`.ne-icon-pick:hover`).
    IconPickHover,
    /// The chosen app icon pick.
    IconPickSelected,
    /// The progress bar's track (`.progress .track`).
    ProgressTrack,
    /// The access callout (`.callout`).
    Callout,
    /// The "up to date" dot with its ring (`.ne-dot`).
    Dot,
    /// A release candidate's channel tag (`.ne-channel.rc`).
    ChannelRc,
    /// A beta's channel tag (`.ne-channel.beta`).
    ChannelBeta,
}

impl Surface {
    /// Every surface.
    pub const ALL: FixedList<Surface, 27> = FixedList([
        Surface::Panel,
        Surface::TopBar,
        Surface::Field,
        Surface::FieldFocused,
        Surface::ChipOff,
        Surface::ChipOn,
        Surface::RowHover,
        Surface::RowSelected,
        Surface::Key,
        Surface::SwitchOff,
        Surface::SwitchOn,
        Surface::KnobOff,
        Surface::KnobOn,
        Surface::Code,
        Surface::Button,
        Surface::ButtonDown,
        Surface::PrimaryButton,
        Surface::PrimaryButtonDown,
        Surface::Segmented,
        Surface::SegmentOn,
        Surface::IconPickHover,
        Surface::IconPickSelected,
        Surface::ProgressTrack,
        Surface::Callout,
        Surface::Dot,
        Surface::ChannelRc,
        Surface::ChannelBeta,
    ]);

    /// The surface's style in `theme`.
    pub fn style(self, theme: Theme) -> SurfaceStyle {
        let c = |t: Token| Rgba::opaque(theme.color(t));
        let fill = |t: Token| Some(c(t));
        let border = |width: f64, t: Token| Some(Border { width, color: c(t) });
        let shadow = |t: ShadowToken| theme.shadow(t).to_vec();
        let rim = metrics::SELECTED_BORDER;
        let (fill, border, corners, shadows) = match self {
            Surface::Panel => (
                fill(Token::Panel),
                None,
                Corners::all(RADIUS_LG),
                shadow(ShadowToken::Raised),
            ),
            Surface::TopBar => (
                fill(Token::Panel),
                None,
                Corners::bottom(RADIUS_LG),
                shadow(ShadowToken::Float),
            ),
            Surface::Field => (
                fill(Token::Well),
                border(1.0, Token::Edge),
                Corners::all(RADIUS_MD),
                shadow(ShadowToken::Pressed),
            ),
            Surface::FieldFocused => {
                let mut shadows = shadow(ShadowToken::Pressed);
                shadows.push(Shadow::ring(metrics::FOCUS_RING_WIDTH, c(Token::GreenInk)));
                (
                    fill(Token::Well),
                    border(1.0, Token::GreenInk),
                    Corners::all(RADIUS_MD),
                    shadows,
                )
            }
            Surface::ChipOff => (
                fill(Token::Panel),
                None,
                Corners::all(RADIUS_MD),
                shadow(ShadowToken::Raised),
            ),
            Surface::ChipOn | Surface::RowSelected => (
                fill(Token::GreenWash),
                border(rim, Token::GreenInk),
                Corners::all(RADIUS_MD),
                shadow(ShadowToken::Pressed),
            ),
            Surface::RowHover => (fill(Token::Well), None, Corners::all(RADIUS_MD), vec![]),
            Surface::Key => (
                fill(Token::Panel),
                None,
                Corners::all(RADIUS_SM),
                shadow(ShadowToken::Key),
            ),
            Surface::SwitchOff => (
                fill(Token::Well),
                border(1.0, Token::Edge),
                Corners::all(RADIUS_PILL),
                shadow(ShadowToken::Pressed),
            ),
            Surface::SwitchOn => (
                fill(Token::Green),
                border(1.0, Token::GreenInk),
                Corners::all(RADIUS_PILL),
                shadow(ShadowToken::Pressed),
            ),
            Surface::KnobOff => (
                fill(Token::Panel),
                None,
                Corners::all(RADIUS_PILL),
                shadow(ShadowToken::Key),
            ),
            Surface::KnobOn => (
                fill(Token::Glass),
                None,
                Corners::all(RADIUS_PILL),
                shadow(ShadowToken::Key),
            ),
            Surface::Code => (
                fill(Token::Well),
                None,
                Corners::all(RADIUS_SM),
                shadow(ShadowToken::Pressed),
            ),
            Surface::Button => (
                fill(Token::Panel),
                None,
                Corners::all(RADIUS_PILL),
                shadow(ShadowToken::Raised),
            ),
            Surface::ButtonDown => (
                fill(Token::Panel),
                None,
                Corners::all(RADIUS_PILL),
                shadow(ShadowToken::Pressed),
            ),
            Surface::PrimaryButton => (
                fill(Token::Green),
                border(rim, Token::GreenInk),
                Corners::all(RADIUS_PILL),
                shadow(ShadowToken::Raised),
            ),
            Surface::PrimaryButtonDown => (
                fill(Token::Green),
                border(rim, Token::GreenInk),
                Corners::all(RADIUS_PILL),
                shadow(ShadowToken::Pressed),
            ),
            Surface::Segmented => (
                fill(Token::Panel),
                None,
                Corners::all(RADIUS_SM),
                shadow(ShadowToken::Raised),
            ),
            Surface::SegmentOn => (
                fill(Token::Green),
                border(metrics::SEGMENT_BORDER, Token::GreenInk),
                Corners::all(metrics::SEGMENT_RADIUS),
                vec![],
            ),
            Surface::IconPickHover => (fill(Token::Well), None, Corners::all(RADIUS_LG), vec![]),
            Surface::IconPickSelected => (
                fill(Token::GreenWash),
                border(rim, Token::GreenInk),
                Corners::all(RADIUS_LG),
                shadow(ShadowToken::Pressed),
            ),
            Surface::ProgressTrack => (
                fill(Token::Well),
                None,
                Corners::all(RADIUS_PILL),
                shadow(ShadowToken::Pressed),
            ),
            Surface::Callout => (
                fill(Token::CautionWash),
                border(1.0, Token::CautionEdge),
                Corners::all(RADIUS_MD),
                vec![],
            ),
            Surface::Dot => (
                fill(Token::Green),
                None,
                Corners::all(RADIUS_PILL),
                vec![Shadow::ring(metrics::DOT_RING, c(Token::GreenWash))],
            ),
            Surface::ChannelRc => (fill(Token::Green), None, Corners::all(RADIUS_SM), vec![]),
            Surface::ChannelBeta => (fill(Token::Vol), None, Corners::all(RADIUS_SM), vec![]),
        };
        SurfaceStyle {
            fill,
            border,
            corners,
            shadows,
        }
    }
}

/// A template's size class: the length of each axis that is rendered at
/// its own length, in 1/8 points; `None` for an axis that stretches. Two
/// sizes with the same class share one image.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SizeClass {
    /// The fixed width, or `None` when the width stretches.
    pub width: Option<u32>,
    /// The fixed height, or `None` when the height stretches.
    pub height: Option<u32>,
}

/// Where a style sits in its template image. See the module documentation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TemplateLayout {
    /// The element's width in the template, in points.
    pub width: f64,
    /// The element's height in the template, in points.
    pub height: f64,
    /// Whether the width stretches.
    pub stretch_x: bool,
    /// Whether the height stretches.
    pub stretch_y: bool,
    /// The image's margins around the element (the outer shadows' reach).
    pub outsets: Insets,
    /// The image's unscaled borders, from its edges; on an axis that does
    /// not stretch they meet in the middle.
    pub cap_insets: Insets,
    /// The radii at the asked size, clamped.
    pub corners: Corners,
}

impl TemplateLayout {
    /// The template for `style` at `width` × `height` points (rounded to
    /// whole points, at least one).
    pub fn new(style: &SurfaceStyle, width: f64, height: f64) -> TemplateLayout {
        Self::build(style, width, height, true)
    }

    /// A layout that stretches neither axis: the image of exactly
    /// `width` × `height` points.
    pub fn exact(style: &SurfaceStyle, width: f64, height: f64) -> TemplateLayout {
        Self::build(style, width, height, false)
    }

    fn build(style: &SurfaceStyle, width: f64, height: f64, stretch: bool) -> TemplateLayout {
        let (width, height) = (width.round().max(1.0), height.round().max(1.0));
        let corners = style.corners.clamped(width, height);
        // A corner's reach: its radius and the farthest shadow.
        let cap = (corners.max() + style.reach()).ceil();
        let min = 2.0 * cap + STRETCH;
        let stretch_x = stretch && width >= min;
        let stretch_y = stretch && height >= min;
        let tw = if stretch_x { min } else { width };
        let th = if stretch_y { min } else { height };
        let outsets = style.outsets();
        let iw = tw + outsets.left + outsets.right;
        let ih = th + outsets.top + outsets.bottom;
        let (left, right) = if stretch_x {
            (outsets.left + cap, outsets.right + cap)
        } else {
            let half = (iw / 2.0).floor();
            (half, iw - half)
        };
        let (top, bottom) = if stretch_y {
            (outsets.top + cap, outsets.bottom + cap)
        } else {
            let half = (ih / 2.0).floor();
            (half, ih - half)
        };
        TemplateLayout {
            width: tw,
            height: th,
            stretch_x,
            stretch_y,
            outsets,
            cap_insets: Insets {
                top,
                left,
                bottom,
                right,
            },
            corners,
        }
    }

    /// The image's width in points.
    pub fn image_width(&self) -> f64 {
        self.width + self.outsets.left + self.outsets.right
    }

    /// The image's height in points.
    pub fn image_height(&self) -> f64 {
        self.height + self.outsets.top + self.outsets.bottom
    }

    /// The template's size class.
    pub fn size_class(&self) -> SizeClass {
        let eighths = |len: f64| (len * 8.0).round() as u32;
        SizeClass {
            width: (!self.stretch_x).then(|| eighths(self.width)),
            height: (!self.stretch_y).then(|| eighths(self.height)),
        }
    }

    /// The stretched middle as a unit rectangle of the image (x, y, width,
    /// height), for `CALayer.contentsCenter`, with y measured from the
    /// image's bottom (a layer whose geometry is not flipped) or, when
    /// `flipped`, from its top. An axis that doesn't stretch gets the whole
    /// unit length; the layer is then exactly the image's length on it.
    pub fn contents_center(&self, flipped: bool) -> [f64; 4] {
        let (iw, ih) = (self.image_width(), self.image_height());
        let (x, w) = if self.stretch_x {
            (self.cap_insets.left / iw, STRETCH / iw)
        } else {
            (0.0, 1.0)
        };
        let (y, h) = if self.stretch_y {
            let from = if flipped {
                self.cap_insets.top
            } else {
                self.cap_insets.bottom
            };
            (from / ih, STRETCH / ih)
        } else {
            (0.0, 1.0)
        };
        [x, y, w, h]
    }
}

/// Pixels: premultiplied RGBA, 8 bits per channel, rows from the top.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pixmap {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// `width × height × 4` bytes.
    pub data: Vec<u8>,
}

impl Pixmap {
    /// The premultiplied RGBA of pixel (`x`, `y`), from the top left.
    pub fn pixel(&self, x: usize, y: usize) -> [u8; 4] {
        let i = (y * self.width + x) * 4;
        [
            self.data[i],
            self.data[i + 1],
            self.data[i + 2],
            self.data[i + 3],
        ]
    }

    /// The colour of pixel (`x`, `y`) with its alpha divided out, and the
    /// alpha: `([r, g, b], a)`; black when the pixel is clear.
    pub fn color(&self, x: usize, y: usize) -> ([u8; 3], u8) {
        let [r, g, b, a] = self.pixel(x, y);
        if a == 0 {
            return ([0, 0, 0], 0);
        }
        let un = |c: u8| ((f64::from(c) * 255.0 / f64::from(a)).round()).min(255.0) as u8;
        ([un(r), un(g), un(b)], a)
    }
}

/// Renders `style` laid out by `layout` at `scale` pixels per point (1 or
/// 2). See the module documentation.
pub fn render(style: &SurfaceStyle, layout: &TemplateLayout, scale: f64) -> Pixmap {
    // A pad around the image lets each blur see the shape's whole edge.
    let pad = style.reach().ceil();
    let px = |pt: f64| (pt * scale).round() as usize;
    let (iw, ih) = (px(layout.image_width()), px(layout.image_height()));
    let (cw, ch) = (iw + 2 * px(pad), ih + 2 * px(pad));
    let canvas = Canvas { w: cw, h: ch };

    let x0 = (pad + layout.outsets.left) * scale;
    let y0 = (pad + layout.outsets.top) * scale;
    let element = Shape {
        x0,
        y0,
        x1: x0 + layout.width * scale,
        y1: y0 + layout.height * scale,
        corners: layout.corners.map(|r| r * scale),
    };
    let bw = style.border.map_or(0.0, |b| b.width * scale);
    let padding = element.grown(-bw);
    let element_cov = canvas.coverage(&element);
    let padding_cov = canvas.coverage(&padding);

    let mut out = vec![[0.0_f32; 4]; cw * ch];
    for s in style.shadows.iter().rev().filter(|s| !s.inset) {
        let shape = element
            .offset(s.x * scale, s.y * scale)
            .grown(s.spread * scale);
        let mut mask = canvas.coverage(&shape);
        blur(&mut mask, cw, ch, s.sigma() * scale);
        for (m, e) in mask.iter_mut().zip(&element_cov) {
            *m *= 1.0 - e;
        }
        paint(&mut out, &mask, s.color);
    }
    if let Some(fill) = style.fill {
        paint(&mut out, &element_cov, fill);
    }
    for s in style.shadows.iter().rev().filter(|s| s.inset) {
        let shape = padding
            .offset(s.x * scale, s.y * scale)
            .grown(-s.spread * scale);
        let mut mask: Vec<f32> = canvas.coverage(&shape).iter().map(|c| 1.0 - c).collect();
        blur(&mut mask, cw, ch, s.sigma() * scale);
        for (m, p) in mask.iter_mut().zip(&padding_cov) {
            *m *= p;
        }
        paint(&mut out, &mask, s.color);
    }
    if let Some(border) = style.border {
        let ring: Vec<f32> = element_cov
            .iter()
            .zip(&padding_cov)
            .map(|(e, p)| (e - p).max(0.0))
            .collect();
        paint(&mut out, &ring, border.color);
    }

    // Crop the pad.
    let off = px(pad);
    let mut data = Vec::with_capacity(iw * ih * 4);
    for y in 0..ih {
        for x in 0..iw {
            for c in out[(y + off) * cw + x + off] {
                data.push((c.clamp(0.0, 1.0) * 255.0).round() as u8);
            }
        }
    }
    Pixmap {
        width: iw,
        height: ih,
        data,
    }
}

/// Paints `color` at `mask`'s coverage over `out` (premultiplied, source
/// over).
fn paint(out: &mut [[f32; 4]], mask: &[f32], color: Rgba) {
    let [r, g, b] = color.rgb.components().map(|c| c as f32);
    let alpha = color.alpha as f32;
    for (o, m) in out.iter_mut().zip(mask) {
        let a = alpha * m;
        if a <= 0.0 {
            continue;
        }
        let keep = 1.0 - a;
        o[0] = r * a + o[0] * keep;
        o[1] = g * a + o[1] * keep;
        o[2] = b * a + o[2] * keep;
        o[3] = a + o[3] * keep;
    }
}

/// A rounded rectangle in canvas pixels, y down.
#[derive(Clone, Copy, Debug)]
struct Shape {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    corners: Corners,
}

impl Shape {
    fn offset(&self, dx: f64, dy: f64) -> Shape {
        Shape {
            x0: self.x0 + dx,
            y0: self.y0 + dy,
            x1: self.x1 + dx,
            y1: self.y1 + dy,
            ..*self
        }
    }

    /// Grown by `d` on every side (shrunk when negative); the radii follow,
    /// a square corner staying square.
    fn grown(&self, d: f64) -> Shape {
        Shape {
            x0: self.x0 - d,
            y0: self.y0 - d,
            x1: self.x1 + d,
            y1: self.y1 + d,
            corners: self
                .corners
                .map(|r| if r > 0.0 { (r + d).max(0.0) } else { 0.0 }),
        }
    }

    /// The signed distance from (`x`, `y`) to the edge: negative inside.
    fn distance(&self, x: f64, y: f64) -> f64 {
        let (hx, hy) = ((self.x1 - self.x0) / 2.0, (self.y1 - self.y0) / 2.0);
        if hx <= 0.0 || hy <= 0.0 {
            return f64::INFINITY;
        }
        let (dx, dy) = (x - (self.x0 + hx), y - (self.y0 + hy));
        let c = &self.corners;
        let r = match (dx < 0.0, dy < 0.0) {
            (true, true) => c.top_left,
            (false, true) => c.top_right,
            (false, false) => c.bottom_right,
            (true, false) => c.bottom_left,
        }
        .min(hx)
        .min(hy);
        let qx = dx.abs() - hx + r;
        let qy = dy.abs() - hy + r;
        let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
        outside + qx.max(qy).min(0.0) - r
    }
}

/// The canvas being painted.
#[derive(Clone, Copy)]
struct Canvas {
    w: usize,
    h: usize,
}

impl Canvas {
    /// Each pixel's coverage by `shape`, antialiased over one pixel.
    fn coverage(&self, shape: &Shape) -> Vec<f32> {
        let mut out = Vec::with_capacity(self.w * self.h);
        for y in 0..self.h {
            for x in 0..self.w {
                let d = shape.distance(x as f64 + 0.5, y as f64 + 0.5);
                out.push((0.5 - d).clamp(0.0, 1.0) as f32);
            }
        }
        out
    }
}

/// Blurs `buf` (`w` × `h`) by a Gaussian of standard deviation `sigma`
/// pixels, extending its edges outwards: one horizontal and one vertical
/// convolution with the kernel cut at 3σ. A shape's straight stretches give
/// identical rows (then columns), so each distinct line is convolved once.
fn blur(buf: &mut [f32], w: usize, h: usize, sigma: f64) {
    if sigma <= 0.0 {
        return;
    }
    let kernel = gaussian_kernel(sigma);
    let mut last: Option<(Vec<f32>, Vec<f32>)> = None;
    let mut blur_cached = |line: &[f32]| -> Vec<f32> {
        match &last {
            Some((input, output)) if input.as_slice() == line => output.clone(),
            _ => {
                let output = convolve(line, &kernel);
                last = Some((line.to_vec(), output.clone()));
                output
            }
        }
    };
    for y in 0..h {
        let blurred = blur_cached(&buf[y * w..(y + 1) * w]);
        buf[y * w..(y + 1) * w].copy_from_slice(&blurred);
    }
    let mut column = Vec::with_capacity(h);
    for x in 0..w {
        column.clear();
        column.extend((0..h).map(|y| buf[y * w + x]));
        let blurred = blur_cached(&column);
        for (y, v) in blurred.into_iter().enumerate() {
            buf[y * w + x] = v;
        }
    }
}

/// The weights of a Gaussian of standard deviation `sigma`, from −⌈3σ⌉ to
/// ⌈3σ⌉, summing to one.
fn gaussian_kernel(sigma: f64) -> Vec<f32> {
    let radius = (3.0 * sigma).ceil() as i64;
    let weights: Vec<f64> = (-radius..=radius)
        .map(|k| (-((k * k) as f64) / (2.0 * sigma * sigma)).exp())
        .collect();
    let sum: f64 = weights.iter().sum();
    weights.into_iter().map(|w| (w / sum) as f32).collect()
}

/// `line` convolved with the centred `kernel`, its ends extended. Where
/// the kernel covers only one value (most of a shadow's mask is 0 or 1),
/// that value is the result, without the sum.
fn convolve(line: &[f32], kernel: &[f32]) -> Vec<f32> {
    let n = line.len();
    let radius = kernel.len() / 2;
    let mut padded = Vec::with_capacity(n + 2 * radius);
    padded.extend(std::iter::repeat_n(line[0], radius));
    padded.extend_from_slice(line);
    padded.extend(std::iter::repeat_n(line[n - 1], radius));
    // changes[j]: how many neighbours differ among padded[..=j].
    let mut changes = Vec::with_capacity(padded.len());
    let mut count = 0_u32;
    changes.push(0);
    for pair in padded.windows(2) {
        count += u32::from(pair[0] != pair[1]);
        changes.push(count);
    }
    (0..n)
        .map(|i| {
            let window = &padded[i..i + kernel.len()];
            if changes[i] == changes[i + kernel.len() - 1] {
                return window[0];
            }
            dot(window, kernel)
        })
        .collect()
}

/// The dot product of two equally long slices, in eight lanes so the
/// compiler can vectorise it.
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut lanes = [0.0_f32; 8];
    let (a8, b8) = (a.chunks_exact(8), b.chunks_exact(8));
    let tail: f32 = a8
        .remainder()
        .iter()
        .zip(b8.remainder())
        .map(|(x, y)| x * y)
        .sum();
    for (x, y) in a8.zip(b8) {
        for ((lane, xv), yv) in lanes.iter_mut().zip(x).zip(y) {
            *lane += xv * yv;
        }
    }
    lanes.iter().sum::<f32>() + tail
}

/// Renders `glyph` into a `size`-point square at `scale` pixels per point:
/// black, with the stroke's coverage as alpha (a template image).
pub fn render_glyph(glyph: Glyph, size: f64, scale: f64) -> Pixmap {
    let side = (size * scale).round().max(1.0) as usize;
    let k = side as f64 / GRID;
    let half = glyph.stroke_width() * k / 2.0;
    let segments: Vec<[(f64, f64); 2]> = flatten(&glyph.path(), 0.02)
        .iter()
        .flat_map(|line| {
            line.windows(2)
                .map(|w| [(w[0].x * k, w[0].y * k), (w[1].x * k, w[1].y * k)])
                .collect::<Vec<_>>()
        })
        .collect();
    let mut data = Vec::with_capacity(side * side * 4);
    for y in 0..side {
        for x in 0..side {
            let p = (x as f64 + 0.5, y as f64 + 0.5);
            let d = segments
                .iter()
                .map(|s| segment_distance(p, s[0], s[1]))
                .fold(f64::INFINITY, f64::min);
            let a = ((half + 0.5 - d).clamp(0.0, 1.0) * 255.0).round() as u8;
            data.extend_from_slice(&[0, 0, 0, a]);
        }
    }
    Pixmap {
        width: side,
        height: side,
        data,
    }
}

/// The distance from `p` to the segment `a`–`b`.
fn segment_distance(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (cx, cy) = (a.0 + t * dx, a.1 + t * dy);
    ((p.0 - cx).powi(2) + (p.1 - cy).powi(2)).sqrt()
}
