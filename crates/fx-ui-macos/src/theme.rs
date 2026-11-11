//! The 3645 design system's values for the custom-drawn interface: colours
//! for Light and Dark, the four shadows, type styles,
//! spacing, radii and the component metrics of the app mockups canvas.
//!
//! Transcribed from the design system's `tokens.json` (version 1) and the
//! canvas's component stylesheet (`ne-` classes). The AppKit layer reads
//! every value from here; `tests/theme.rs` pins each one.
//!
//! # Colours
//!
//! [`Theme::color`] gives each [`Token`] in sRGB. Aliases are resolved:
//! `text` is `ink` in Light and `glass` in Dark, `vol` is `ink-strong`, and
//! `on-vol` is `panel` in Light and `well` in Dark. The design's usage rules
//! (from its README) in short: the page on `ground`; subjects on `panel`
//! with [`ShadowToken::Raised`]; fields, wells and selected choices on
//! `well` or `green-wash` with [`ShadowToken::Pressed`]; `green` only as a
//! fill (with `on-green` text), `green-ink` wherever green must be read
//! (focus rings, the rim of a selected control, matched text); text in
//! `text`, headings and values in `ink-strong`, hints in `muted`; `line`
//! only for dividers; `danger-ink` only for errors.
//!
//! # Shadows: from CSS to AppKit
//!
//! The design writes shadows as CSS `box-shadow` lists ([`Shadow`]). A CSS
//! shadow is its shape (the border box offset by `x`, `y` and grown by
//! `spread`) blurred by a Gaussian whose standard deviation is half the blur
//! radius ([`Shadow::sigma`]); an outer shadow is painted only outside the
//! border box, an inset one only inside the padding box, and the first
//! shadow of a list is on top. `crate::surface` renders them exactly that
//! way into shared images.
//!
//! A single outer shadow without spread can also be a `CALayer` shadow
//! ([`Shadow::ca_layer`]): `shadowRadius` = blur ÷ 2 (the Gaussian's
//! standard deviation), `shadowOffset` = (x, −y) because AppKit's y runs up,
//! `shadowColor` = the RGB and `shadowOpacity` = the alpha. A layer has one
//! shadow and no inset shadows, so the raised (two shadows), pressed and key
//! surfaces always use the rendered images.
//!
//! # Type
//!
//! [`TypeStyle`] holds the CSS values. The family is the system font
//! (`system-ui`), or the system monospaced font for `mono` styles. CSS
//! weights map onto AppKit's named weights by [`weight_mix`]; 650 falls
//! between Semibold and Bold and is drawn Semibold, as the board's browser
//! draws it, so the app matches the approved mockups.
//! `tabular` asks for tabular figures (the monospaced-digit system font).
//! `tracking_em` is CSS `letter-spacing` in ems ([`TypeStyle::kern`] gives
//! points); `caps` means the design sets the text in capitals
//! (`text-transform: uppercase`), so the view uppercases it.

#![forbid(unsafe_code)]

use crate::FixedList;

/// An sRGB colour as `0xRRGGBB`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgb(pub u32);

impl Rgb {
    /// Red, green and blue from 0 to 1.
    pub fn components(self) -> [f64; 3] {
        self.bytes().map(|c| f64::from(c) / 255.0)
    }

    /// Red, green and blue from 0 to 255.
    pub const fn bytes(self) -> [u8; 3] {
        [(self.0 >> 16) as u8, (self.0 >> 8) as u8, self.0 as u8]
    }

    /// `#rrggbb`.
    pub fn hex(self) -> String {
        format!("#{:06x}", self.0 & 0x00ff_ffff)
    }
}

/// An sRGB colour with an alpha from 0 to 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    /// The colour.
    pub rgb: Rgb,
    /// Its opacity, from 0 (clear) to 1.
    pub alpha: f64,
}

impl Rgba {
    /// `0xRRGGBB` at `alpha`.
    pub const fn new(rgb: u32, alpha: f64) -> Rgba {
        Rgba {
            rgb: Rgb(rgb),
            alpha,
        }
    }

    /// `rgb`, opaque.
    pub const fn opaque(rgb: Rgb) -> Rgba {
        Rgba { rgb, alpha: 1.0 }
    }
}

/// Light or Dark: the design's two themes, following the app's appearance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Theme {
    /// The light theme (the design's default).
    Light,
    /// The dark theme.
    Dark,
}

impl Theme {
    /// Both themes.
    pub const ALL: [Theme; 2] = [Theme::Light, Theme::Dark];

    /// The theme for a dark (`true`) or light effective appearance.
    pub const fn for_dark(dark: bool) -> Theme {
        if dark { Theme::Dark } else { Theme::Light }
    }

    /// Whether this is the dark theme.
    pub const fn is_dark(self) -> bool {
        matches!(self, Theme::Dark)
    }

    /// The theme's id in the design: `light` or `dark`.
    pub const fn name(self) -> &'static str {
        match self {
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }

    /// `token`'s colour in this theme, aliases resolved.
    pub const fn color(self, token: Token) -> Rgb {
        let (light, dark) = token.values();
        Rgb(if self.is_dark() { dark } else { light })
    }

    /// The shadows of `token` in this theme, top first.
    pub fn shadow(self, token: ShadowToken) -> &'static [Shadow] {
        match (token, self) {
            (ShadowToken::Raised, Theme::Light) => &RAISED_LIGHT,
            (ShadowToken::Raised, Theme::Dark) => &RAISED_DARK,
            (ShadowToken::Pressed, Theme::Light) => &PRESSED_LIGHT,
            (ShadowToken::Pressed, Theme::Dark) => &PRESSED_DARK,
            (ShadowToken::Float, Theme::Light) => &FLOAT_LIGHT,
            (ShadowToken::Float, Theme::Dark) => &FLOAT_DARK,
            (ShadowToken::Key, Theme::Light) => &KEY_LIGHT,
            (ShadowToken::Key, Theme::Dark) => &KEY_DARK,
        }
    }
}

/// A colour token of the design system.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Token {
    /// The page ground; the app icon's fill.
    Ground,
    /// The ground of the dark-born icons; a darker panel.
    GroundDarkBorn,
    /// The query bar of Match Row; text on `ground` in Light.
    Ink,
    /// White: the raised parts of the Glass icons.
    Glass,
    /// Running text on `ground` (`ink` in Light, `glass` in Dark).
    Text,
    /// The Green Rim, the one accent: fills and large marks.
    Green,
    /// The top-left stop of the lit gradients.
    GreenLight,
    /// The bottom-right stop of the lit gradients.
    GreenDark,
    /// The green of the dark-born designs in Dark.
    GreenDeep,
    /// Raised surfaces: the top bar, panels, chips, buttons.
    Panel,
    /// Pressed surfaces: fields, passive chips, the progress track.
    Well,
    /// Headings, names, typed values, labels that lead a line.
    InkStrong,
    /// Hints, captions, meta lines and placeholders.
    Muted,
    /// Dividers only.
    Line,
    /// The 1-point border of fields.
    Edge,
    /// Text and marks on `green`.
    OnGreen,
    /// The green that must be read: labels, focus rings, selected rims.
    GreenInk,
    /// A tint of green behind the selected choice.
    GreenWash,
    /// The secondary pill's fill (`ink-strong`).
    Vol,
    /// Text on `vol` (`panel` in Light, `well` in Dark).
    OnVol,
    /// The ground of caution callouts.
    CautionWash,
    /// Text in caution callouts.
    CautionInk,
    /// The border of a caution callout.
    CautionEdge,
    /// Error messages only.
    DangerInk,
}

impl Token {
    /// Every token, in `tokens.json`'s order.
    pub const ALL: FixedList<Token, 24> = FixedList([
        Token::Ground,
        Token::GroundDarkBorn,
        Token::Ink,
        Token::Glass,
        Token::Text,
        Token::Green,
        Token::GreenLight,
        Token::GreenDark,
        Token::GreenDeep,
        Token::Panel,
        Token::Well,
        Token::InkStrong,
        Token::Muted,
        Token::Line,
        Token::Edge,
        Token::OnGreen,
        Token::GreenInk,
        Token::GreenWash,
        Token::Vol,
        Token::OnVol,
        Token::CautionWash,
        Token::CautionInk,
        Token::CautionEdge,
        Token::DangerInk,
    ]);

    /// The token's name in the design system (`ground`, `green-ink`, ...).
    pub const fn name(self) -> &'static str {
        match self {
            Token::Ground => "ground",
            Token::GroundDarkBorn => "ground-dark-born",
            Token::Ink => "ink",
            Token::Glass => "glass",
            Token::Text => "text",
            Token::Green => "green",
            Token::GreenLight => "green-light",
            Token::GreenDark => "green-dark",
            Token::GreenDeep => "green-deep",
            Token::Panel => "panel",
            Token::Well => "well",
            Token::InkStrong => "ink-strong",
            Token::Muted => "muted",
            Token::Line => "line",
            Token::Edge => "edge",
            Token::OnGreen => "on-green",
            Token::GreenInk => "green-ink",
            Token::GreenWash => "green-wash",
            Token::Vol => "vol",
            Token::OnVol => "on-vol",
            Token::CautionWash => "caution-wash",
            Token::CautionInk => "caution-ink",
            Token::CautionEdge => "caution-edge",
            Token::DangerInk => "danger-ink",
        }
    }

    /// The token with this design-system name, if any.
    pub fn from_name(name: &str) -> Option<Token> {
        Token::ALL.iter().copied().find(|t| t.name() == name)
    }

    /// The Light and Dark values, aliases resolved.
    const fn values(self) -> (u32, u32) {
        match self {
            Token::Ground => (0xd9e1e4, 0x393939),
            Token::GroundDarkBorn => (0x393939, 0x161718),
            Token::Ink => (0x555555, 0x8d8f93),
            Token::Glass => (0xffffff, 0xffffff),
            // {ink} in Light, {glass} in Dark.
            Token::Text => (0x555555, 0xffffff),
            Token::Green => (0x18b05a, 0x18b05a),
            Token::GreenLight => (0x3fdc84, 0x3fdc84),
            Token::GreenDark => (0x0f8a45, 0x0f8a45),
            Token::GreenDeep => (0x15a052, 0x15a052),
            Token::Panel => (0xf2f5f6, 0x454545),
            Token::Well => (0xe3e9eb, 0x2c2c2c),
            Token::InkStrong => (0x232527, 0xffffff),
            Token::Muted => (0x5d6164, 0xb8bbbf),
            Token::Line => (0xcfd8dc, 0x555555),
            Token::Edge => (0x7b858a, 0x9b9ea2),
            Token::OnGreen => (0x161718, 0x161718),
            Token::GreenInk => (0x0b6e36, 0x3fdc84),
            Token::GreenWash => (0xd3f0df, 0x1d4a30),
            // {ink-strong}.
            Token::Vol => (0x232527, 0xffffff),
            // {panel} in Light, {well} in Dark.
            Token::OnVol => (0xf2f5f6, 0x2c2c2c),
            Token::CautionWash => (0xfbefd5, 0x3d3218),
            Token::CautionInk => (0x6e4600, 0xf3c776),
            Token::CautionEdge => (0xe2c27f, 0x6b5626),
            Token::DangerInk => (0xb42318, 0xff9b91),
        }
    }
}

/// One CSS `box-shadow`, in points. See the module documentation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shadow {
    /// An inset shadow (inside the padding box) rather than an outer one.
    pub inset: bool,
    /// Horizontal offset; positive is right.
    pub x: f64,
    /// Vertical offset; positive is down, as in CSS.
    pub y: f64,
    /// The CSS blur radius: twice the Gaussian's standard deviation.
    pub blur: f64,
    /// How far the shape grows (or, negative, shrinks) before blurring.
    pub spread: f64,
    /// The shadow's colour.
    pub color: Rgba,
}

/// A `CALayer` shadow equivalent to one outer CSS shadow, from
/// [`Shadow::ca_layer`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CaLayerShadow {
    /// `shadowOffset.width`.
    pub offset_x: f64,
    /// `shadowOffset.height`: CSS `y` negated (AppKit's y runs up).
    pub offset_y: f64,
    /// `shadowRadius`: the CSS blur ÷ 2.
    pub radius: f64,
    /// `shadowColor`, opaque.
    pub color: Rgb,
    /// `shadowOpacity`: the CSS colour's alpha.
    pub opacity: f64,
}

impl Shadow {
    /// An outer shadow without spread.
    pub const fn outer(x: f64, y: f64, blur: f64, color: Rgba) -> Shadow {
        Shadow {
            inset: false,
            x,
            y,
            blur,
            spread: 0.0,
            color,
        }
    }

    /// An inset shadow without spread.
    pub const fn inset(x: f64, y: f64, blur: f64, color: Rgba) -> Shadow {
        Shadow {
            inset: true,
            x,
            y,
            blur,
            spread: 0.0,
            color,
        }
    }

    /// A hard ring `width` points wide just outside the border box
    /// (`0 0 0 <width> <color>`), such as the search field's focus ring.
    pub const fn ring(width: f64, color: Rgba) -> Shadow {
        Shadow {
            inset: false,
            x: 0.0,
            y: 0.0,
            blur: 0.0,
            spread: width,
            color,
        }
    }

    /// The Gaussian's standard deviation in points: half the blur radius.
    pub fn sigma(&self) -> f64 {
        self.blur / 2.0
    }

    /// The `CALayer` shadow drawing the same, or `None` for an inset shadow
    /// or one with spread, which a layer can't draw.
    pub fn ca_layer(&self) -> Option<CaLayerShadow> {
        if self.inset || self.spread != 0.0 {
            return None;
        }
        Some(CaLayerShadow {
            offset_x: self.x,
            offset_y: -self.y,
            radius: self.sigma(),
            color: self.color.rgb,
            opacity: self.color.alpha,
        })
    }

    /// The shadow as CSS writes it, such as
    /// `inset 2px 2px 5px rgba(54, 73, 80, 0.16)`.
    pub fn css(&self) -> String {
        let [r, g, b] = self.color.rgb.bytes();
        let spread = if self.spread != 0.0 {
            format!(" {}px", self.spread)
        } else {
            String::new()
        };
        format!(
            "{}{}px {}px {}px{spread} rgba({r}, {g}, {b}, {})",
            if self.inset { "inset " } else { "" },
            self.x,
            self.y,
            self.blur,
            self.color.alpha
        )
    }
}

/// A shadow token of the design system.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShadowToken {
    /// Panels, sections, raised chips and buttons at rest.
    Raised,
    /// Fields, wells and a selected choice: pressed in.
    Pressed,
    /// Above the page: the top bar.
    Float,
    /// Small raised parts about 20 points across: key caps, the switch's
    /// knob.
    Key,
}

impl ShadowToken {
    /// Every shadow token.
    pub const ALL: FixedList<ShadowToken, 4> = FixedList([
        ShadowToken::Raised,
        ShadowToken::Pressed,
        ShadowToken::Float,
        ShadowToken::Key,
    ]);

    /// The token's name in the design system.
    pub const fn name(self) -> &'static str {
        match self {
            ShadowToken::Raised => "shadow-raised",
            ShadowToken::Pressed => "shadow-pressed",
            ShadowToken::Float => "shadow-float",
            ShadowToken::Key => "shadow-key",
        }
    }
}

const RAISED_LIGHT: [Shadow; 2] = [
    Shadow::outer(-3.0, -3.0, 8.0, Rgba::new(0xffffff, 0.85)),
    Shadow::outer(3.0, 3.0, 10.0, Rgba::new(0x364950, 0.18)),
];
const RAISED_DARK: [Shadow; 2] = [
    Shadow::outer(-2.0, -2.0, 6.0, Rgba::new(0xffffff, 0.05)),
    Shadow::outer(3.0, 3.0, 10.0, Rgba::new(0x000000, 0.5)),
];
const PRESSED_LIGHT: [Shadow; 2] = [
    Shadow::inset(2.0, 2.0, 5.0, Rgba::new(0x364950, 0.16)),
    Shadow::inset(-2.0, -2.0, 5.0, Rgba::new(0xffffff, 0.9)),
];
const PRESSED_DARK: [Shadow; 2] = [
    Shadow::inset(2.0, 2.0, 5.0, Rgba::new(0x000000, 0.5)),
    Shadow::inset(-1.0, -1.0, 3.0, Rgba::new(0xffffff, 0.06)),
];
const FLOAT_LIGHT: [Shadow; 1] = [Shadow::outer(0.0, 12.0, 32.0, Rgba::new(0x161718, 0.22))];
const FLOAT_DARK: [Shadow; 1] = [Shadow::outer(0.0, 12.0, 32.0, Rgba::new(0x000000, 0.55))];
const KEY_LIGHT: [Shadow; 2] = [
    Shadow::outer(-1.0, -1.0, 2.0, Rgba::new(0xffffff, 0.9)),
    Shadow::outer(1.0, 1.0, 3.0, Rgba::new(0x364950, 0.28)),
];
const KEY_DARK: [Shadow; 2] = [
    Shadow::outer(-1.0, -1.0, 2.0, Rgba::new(0xffffff, 0.06)),
    Shadow::outer(1.0, 1.0, 3.0, Rgba::new(0x000000, 0.55)),
];

/// `space-1`: gaps inside small controls.
pub const SPACE_1: f64 = 4.0;
/// `space-2`: between chips and buttons; inside fields.
pub const SPACE_2: f64 = 8.0;
/// `space-3`: between related rows.
pub const SPACE_3: f64 = 12.0;
/// `space-4`: panel padding and the page gutter.
pub const SPACE_4: f64 = 16.0;
/// `space-5`: between sections.
pub const SPACE_5: f64 = 24.0;
/// `space-6`: large gaps.
pub const SPACE_6: f64 = 32.0;
/// The six spacing steps, `space-1` to `space-6`.
pub const SPACE: [f64; 6] = [SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_5, SPACE_6];

/// `radius-sm`: marks, tags, segment buttons, key caps.
pub const RADIUS_SM: f64 = 6.0;
/// `radius-md`: fields, chips, rows, callouts.
pub const RADIUS_MD: f64 = 10.0;
/// `radius-lg`: panels, cards, the top bar's lower corners.
pub const RADIUS_LG: f64 = 16.0;
/// `radius-pill` as CSS writes it; a pill's real radius is half its
/// height ([`pill_radius`]).
pub const RADIUS_PILL: f64 = 999.0;

/// The corner radius of a pill `height` points tall.
pub fn pill_radius(height: f64) -> f64 {
    height / 2.0
}

/// One type style, in CSS terms. See the module documentation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TypeStyle {
    /// Font size in points.
    pub size: f64,
    /// Line height in points.
    pub line_height: f64,
    /// CSS font weight (400 regular, 700 bold).
    pub weight: u16,
    /// Letter spacing in ems.
    pub tracking_em: f64,
    /// Set in capitals.
    pub caps: bool,
    /// Tabular (monospaced) figures.
    pub tabular: bool,
    /// The system monospaced font instead of the system font.
    pub mono: bool,
}

impl TypeStyle {
    const fn new(size: f64, line_height: f64, weight: u16) -> TypeStyle {
        TypeStyle {
            size,
            line_height,
            weight,
            tracking_em: 0.0,
            caps: false,
            tabular: false,
            mono: false,
        }
    }

    const fn tracked(self, em: f64) -> TypeStyle {
        TypeStyle {
            tracking_em: em,
            ..self
        }
    }

    const fn caps(self, em: f64) -> TypeStyle {
        TypeStyle {
            tracking_em: em,
            caps: true,
            ..self
        }
    }

    const fn tabular(self) -> TypeStyle {
        TypeStyle {
            tabular: true,
            ..self
        }
    }

    const fn mono(self) -> TypeStyle {
        TypeStyle { mono: true, ..self }
    }

    /// The letter spacing in points (`NSKernAttributeName`).
    pub fn kern(&self) -> f64 {
        self.size * self.tracking_em
    }

    /// The system weights this style's weight lies between.
    pub fn weight_mix(&self) -> WeightMix {
        weight_mix(self.weight)
    }
}

/// AppKit's named font weights (`NSFontWeightUltraLight` ... `Black`), with
/// the CSS weight each stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NamedWeight {
    /// 100.
    UltraLight = 100,
    /// 200.
    Thin = 200,
    /// 300.
    Light = 300,
    /// 400.
    Regular = 400,
    /// 500.
    Medium = 500,
    /// 600.
    Semibold = 600,
    /// 700.
    Bold = 700,
    /// 800.
    Heavy = 800,
    /// 900.
    Black = 900,
}

impl NamedWeight {
    const ALL: [NamedWeight; 9] = [
        NamedWeight::UltraLight,
        NamedWeight::Thin,
        NamedWeight::Light,
        NamedWeight::Regular,
        NamedWeight::Medium,
        NamedWeight::Semibold,
        NamedWeight::Bold,
        NamedWeight::Heavy,
        NamedWeight::Black,
    ];
}

/// A CSS weight as a point between two named system weights: the AppKit
/// weight is `lower + t × (upper − lower)` of their `NSFontWeight` values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WeightMix {
    /// The named weight at or below.
    pub lower: NamedWeight,
    /// The named weight above (equal to `lower` for a named weight).
    pub upper: NamedWeight,
    /// How far from `lower` to `upper`, from 0 up to 1.
    pub t: f64,
}

/// Where the CSS weight `css` lies among the named system weights; weights
/// outside 100–900 are clamped.
pub fn weight_mix(css: u16) -> WeightMix {
    let css = css.clamp(100, 900);
    let lower = NamedWeight::ALL[usize::from(css / 100 - 1)];
    if css % 100 == 0 {
        return WeightMix {
            lower,
            upper: lower,
            t: 0.0,
        };
    }
    WeightMix {
        lower,
        upper: NamedWeight::ALL[usize::from(css / 100)],
        t: f64::from(css % 100) / 100.0,
    }
}

/// The design system's type styles (`tokens.json`) and the canvas
/// components' text styles (`ne-` classes).
pub mod styles {
    use super::TypeStyle;

    /// `title-app`: the app name in the top bar.
    pub const TITLE_APP: TypeStyle = TypeStyle::new(15.0, 20.0, 700);
    /// `title-display`: the subject of a screen.
    pub const TITLE_DISPLAY: TypeStyle = TypeStyle::new(24.0, 30.0, 700).tracked(-0.01);
    /// `title-section`: empty states and section headings.
    pub const TITLE_SECTION: TypeStyle = TypeStyle::new(20.0, 26.0, 700);
    /// `text-body`: running text.
    pub const TEXT_BODY: TypeStyle = TypeStyle::new(16.0, 24.0, 400);
    /// `text-body-strong`: a sub-heading inside a panel.
    pub const TEXT_BODY_STRONG: TypeStyle = TypeStyle::new(16.0, 24.0, 650);
    /// `text-input`: values typed into fields.
    pub const TEXT_INPUT: TypeStyle = TypeStyle::new(17.0, 24.0, 650).tabular();
    /// `text-small`: meta lines, callouts, guidance.
    pub const TEXT_SMALL: TypeStyle = TypeStyle::new(14.0, 20.0, 400);
    /// `text-caption`: hints and field labels.
    pub const TEXT_CAPTION: TypeStyle = TypeStyle::new(13.0, 18.0, 400);
    /// `label-eyebrow`: class names and panel titles, in capitals.
    pub const LABEL_EYEBROW: TypeStyle = TypeStyle::new(12.0, 16.0, 700).caps(0.06);
    /// `label-group`: the heading of a group in a list, in capitals.
    pub const LABEL_GROUP: TypeStyle = TypeStyle::new(12.0, 16.0, 800).caps(0.08);
    /// `label-tag`: a tag beside a name, in capitals.
    pub const LABEL_TAG: TypeStyle = TypeStyle::new(11.0, 14.0, 700).caps(0.04);
    /// `num-result`: figures in result pills.
    pub const NUM_RESULT: TypeStyle = TypeStyle::new(14.0, 22.0, 800).tabular();

    /// The windows' base text (`.ne-window`).
    pub const WINDOW_TEXT: TypeStyle = TypeStyle::new(14.0, 20.0, 400);
    /// The title in the top bar (`.ne-title`).
    pub const WINDOW_TITLE: TypeStyle = TypeStyle::new(15.0, 20.0, 700).tracked(-0.01);
    /// The RC or beta tag after the title (`.ne-channel`).
    pub const CHANNEL_TAG: TypeStyle = TypeStyle::new(11.0, 14.0, 700).caps(0.04);
    /// The search field's text and placeholder (`.search input`).
    pub const SEARCH_TEXT: TypeStyle = TypeStyle::new(17.0, 20.0, 400);
    /// A search option chip's label (`.ne-toggle`).
    pub const TOGGLE: TypeStyle = TypeStyle::new(14.0, 20.0, 650);
    /// The `.*` chip's label (`.ne-toggle .ne-mono`).
    pub const TOGGLE_MONO: TypeStyle = TypeStyle::new(13.0, 20.0, 650).mono();
    /// A column header (`.ne-th`).
    pub const COLUMN_HEADER: TypeStyle = TypeStyle::new(12.0, 16.0, 700).caps(0.06);
    /// A row's name (`.ne-name`).
    pub const ROW_NAME: TypeStyle = TypeStyle::new(14.0, 20.0, 500);
    /// Matched text in a name or path (`.ne-hit`), in `green-ink`.
    pub const ROW_HIT: TypeStyle = TypeStyle::new(14.0, 20.0, 800);
    /// A row's path (`.ne-path`, in `muted`).
    pub const ROW_TEXT: TypeStyle = TypeStyle::new(14.0, 20.0, 400);
    /// A row's size and date (`.ne-num`, `.ne-date`).
    pub const ROW_FIGURES: TypeStyle = TypeStyle::new(14.0, 20.0, 400).tabular();
    /// The empty state's title (`.ne-empty-title`).
    pub const EMPTY_TITLE: TypeStyle = TypeStyle::new(20.0, 26.0, 700);
    /// The empty state's text (`.ne-empty-text`, in `muted`).
    pub const EMPTY_TEXT: TypeStyle = TypeStyle::new(16.0, 24.0, 400);
    /// "SEARCH SYNTAX" (`.sp-label`, written in capitals, in `green-ink`).
    pub const SYNTAX_LABEL: TypeStyle = TypeStyle::new(12.0, 16.0, 800).tracked(0.08);
    /// A syntax example (`.ne-code`).
    pub const SYNTAX_CODE: TypeStyle = TypeStyle::new(13.0, 20.0, 400).mono();
    /// A syntax example's description (`.ne-syntax dd`).
    pub const SYNTAX_TEXT: TypeStyle = TypeStyle::new(14.0, 20.0, 400);
    /// The invalid regular expression message (`.ne-error`, `danger-ink`).
    pub const REGEX_ERROR: TypeStyle = TypeStyle::new(14.0, 20.0, 650);
    /// The status bar's text (`.ne-status`, in `muted`).
    pub const STATUS_TEXT: TypeStyle = TypeStyle::new(13.0, 18.0, 400);
    /// The status bar's leading figure (`.ne-status b`).
    pub const STATUS_FIGURE: TypeStyle = TypeStyle::new(13.0, 18.0, 700).tabular();
    /// The index state on the right (`.ne-status-index`).
    pub const STATUS_INDEX: TypeStyle = TypeStyle::new(13.0, 18.0, 400).tabular();
    /// A key cap's label (`.ne-key`).
    pub const KEY_CAP: TypeStyle = TypeStyle::new(12.0, 16.0, 700);
    /// The access callout's text (`.callout`).
    pub const BANNER_TEXT: TypeStyle = TypeStyle::new(14.0, 20.0, 400);
    /// The access callout's lead-in (`.ne-banner-text strong`).
    pub const BANNER_LEAD: TypeStyle = TypeStyle::new(14.0, 20.0, 700);
    /// A button's label (`.btn`).
    pub const BUTTON: TypeStyle = TypeStyle::new(14.0, 20.0, 650);
    /// A Settings card's title (`.sec-title`, in `muted`).
    pub const SECTION_TITLE: TypeStyle = TypeStyle::new(12.0, 16.0, 700).caps(0.06);
    /// A setting's name (`.ne-setting-label`).
    pub const SETTING_LABEL: TypeStyle = TypeStyle::new(15.0, 22.0, 650);
    /// A setting's caption (`.ne-setting-caption`, in `muted`).
    pub const SETTING_CAPTION: TypeStyle = TypeStyle::new(13.0, 18.0, 400);
    /// A note under a card's control (`.ne-note`, in `muted`).
    pub const SETTING_NOTE: TypeStyle = TypeStyle::new(13.0, 18.0, 400);
    /// The version footnote (`.ne-footnote`, in `muted`).
    pub const FOOTNOTE: TypeStyle = TypeStyle::new(12.0, 16.0, 400);
    /// A segmented control's label (`.seg button`).
    pub const SEGMENT: TypeStyle = TypeStyle::new(14.0, 20.0, 700);
    /// An app icon's name (`.ne-icon-name`).
    pub const ICON_NAME: TypeStyle = TypeStyle::new(13.0, 18.0, 650);
    /// A tag such as "Default" (`.tag`).
    pub const TAG: TypeStyle = TypeStyle::new(11.0, 14.0, 700).caps(0.04);
    /// The shortcut recorder's "Type shortcut" before a key is held
    /// (`.rec-hint`).
    pub const RECORDER_HINT: TypeStyle = TypeStyle::new(13.0, 18.0, 400);
    /// A link-style button: the recorder's "Use Default"
    /// (`.linkbtn.rec-reset`).
    pub const LINK_BUTTON: TypeStyle = TypeStyle::new(13.0, 18.0, 700);
}

/// Sizes and gaps of the canvas's components, in points (the `ne-` classes
/// and the 3645 components they combine). Four-sided paddings are
/// `[top, right, bottom, left]`, two-sided `[vertical, horizontal]`.
pub mod metrics {
    /// The search window's first size (the owner's choice).
    pub const WINDOW_WIDTH: f64 = 1280.0;
    /// See [`WINDOW_WIDTH`].
    pub const WINDOW_HEIGHT: f64 = 800.0;
    /// The search window's smallest size (the owner's choice; `.ne-grid`).
    pub const WINDOW_MIN_WIDTH: f64 = 760.0;
    /// See [`WINDOW_MIN_WIDTH`].
    pub const WINDOW_MIN_HEIGHT: f64 = 480.0;

    /// The top bar's padding (`.ne-topbar`).
    pub const TOPBAR_PADDING: [f64; 4] = [10.0, 16.0, 12.0, 16.0];
    /// The title row's height, beside the traffic lights.
    pub const TITLE_ROW_HEIGHT: f64 = 24.0;
    /// Below the title row.
    pub const TITLE_ROW_GAP: f64 = 10.0;
    /// Between the search field and the chips, and between wrapped rows.
    pub const TOOLBAR_GAP: f64 = 12.0;
    /// Between the title and the channel tag.
    pub const TITLE_GAP: f64 = 8.0;

    /// The search field's height.
    pub const SEARCH_HEIGHT: f64 = 46.0;
    /// Where the typed text starts.
    pub const SEARCH_TEXT_X: f64 = 42.0;
    /// Where the 18-point search glyph starts.
    pub const SEARCH_GLYPH_X: f64 = 14.0;
    /// The clear button's square side...
    pub const CLEAR_SIZE: f64 = 38.0;
    /// ...and its inset from the field's top and right.
    pub const CLEAR_INSET: f64 = 4.0;
    /// The focus ring's width, outside the control.
    pub const FOCUS_RING_WIDTH: f64 = 2.0;

    /// A search option chip's height...
    pub const TOGGLE_HEIGHT: f64 = 40.0;
    /// ...smallest width...
    pub const TOGGLE_MIN_WIDTH: f64 = 48.0;
    /// ...and side padding.
    pub const TOGGLE_PADDING_X: f64 = 14.0;
    /// Between chips.
    pub const TOGGLE_GAP: f64 = 8.0;
    /// The rim of a selected chip, row or icon pick.
    pub const SELECTED_BORDER: f64 = 2.0;

    /// The results card's margin from the window's sides and the top bar.
    pub const CARD_MARGIN: f64 = 16.0;
    /// The header row's height...
    pub const HEADER_HEIGHT: f64 = 44.0;
    /// ...and side padding.
    pub const HEADER_PADDING_X: f64 = 16.0;
    /// A result row's height.
    pub const ROW_HEIGHT: f64 = 40.0;
    /// Between rows.
    pub const ROW_GAP: f64 = 2.0;
    /// A row's side padding.
    pub const ROW_PADDING_X: f64 = 6.0;
    /// Around the rows inside the card.
    pub const ROWS_INSET: f64 = 8.0;
    /// Between columns.
    pub const COLUMN_GAP: f64 = 16.0;
    /// The Name column: smallest width and share of the free width.
    pub const NAME_COLUMN: (f64, f64) = (220.0, 1.25);
    /// The Path column: smallest width and share of the free width.
    pub const PATH_COLUMN: (f64, f64) = (200.0, 1.5);
    /// The Size column's width.
    pub const SIZE_COLUMN: f64 = 88.0;
    /// The Date Modified column's width.
    pub const DATE_COLUMN: f64 = 156.0;
    /// The table's smallest width; narrower, it scrolls sideways.
    pub const GRID_MIN_WIDTH: f64 = 760.0;
    /// A file glyph's side...
    pub const GLYPH_SIZE: f64 = 18.0;
    /// ...and the gap before the name.
    pub const GLYPH_GAP: f64 = 10.0;
    /// The sort chevron's side...
    pub const SORT_GLYPH_SIZE: f64 = 14.0;
    /// ...and its stroke, in glyph-grid units.
    pub const SORT_GLYPH_STROKE: f64 = 2.0;

    /// The empty state's widest text.
    pub const EMPTY_MAX_WIDTH: f64 = 600.0;
    /// Above the empty state's title.
    pub const EMPTY_PADDING_TOP: f64 = 56.0;
    /// A syntax token's horizontal padding...
    pub const CODE_PADDING_X: f64 = 8.0;
    /// ...and vertical padding.
    pub const CODE_PADDING_Y: f64 = 2.0;

    /// The status bar's padding.
    pub const STATUS_PADDING: [f64; 4] = [10.0, 24.0, 12.0, 24.0];
    /// Between the status bar's slots.
    pub const STATUS_GAP_X: f64 = 24.0;
    /// Between key hints.
    pub const HINT_GAP: f64 = 16.0;
    /// Between a hint's keys and its label.
    pub const HINT_KEY_GAP: f64 = 6.0;
    /// A key cap's height...
    pub const KEY_HEIGHT: f64 = 22.0;
    /// ...smallest width...
    pub const KEY_MIN_WIDTH: f64 = 22.0;
    /// ...side padding...
    pub const KEY_PADDING_X: f64 = 6.0;
    /// ...and the gap between the keys of a combination.
    pub const KEY_GAP: f64 = 3.0;
    /// The "up to date" dot's diameter...
    pub const DOT_SIZE: f64 = 8.0;
    /// ...and its `green-wash` ring.
    pub const DOT_RING: f64 = 3.0;
    /// The progress track's width...
    pub const PROGRESS_WIDTH: f64 = 180.0;
    /// ...height...
    pub const PROGRESS_HEIGHT: f64 = 10.0;
    /// ...and the gap before its text.
    pub const PROGRESS_GAP: f64 = 10.0;

    /// The channel tag's height...
    pub const CHANNEL_TAG_HEIGHT: f64 = 18.0;
    /// ...and side padding.
    pub const CHANNEL_TAG_PADDING_X: f64 = 7.0;

    /// The access callout's padding.
    pub const BANNER_PADDING: [f64; 2] = [12.0, 16.0];
    /// A button's padding.
    pub const BUTTON_PADDING: [f64; 2] = [6.0, 12.0];
    /// Between buttons.
    pub const BUTTON_GAP: f64 = 8.0;

    /// The Settings content's widest width.
    pub const SETTINGS_WIDTH: f64 = 640.0;
    /// The Settings content's padding: top, sides, bottom.
    pub const SETTINGS_PADDING: [f64; 3] = [16.0, 16.0, 32.0];
    /// Between Settings cards.
    pub const SETTINGS_GAP: f64 = 16.0;
    /// A card's padding.
    pub const CARD_PADDING: f64 = 16.0;
    /// A setting row's vertical padding.
    pub const SETTING_ROW_PADDING: f64 = 12.0;
    /// The switch's width...
    pub const SWITCH_WIDTH: f64 = 46.0;
    /// ...and height.
    pub const SWITCH_HEIGHT: f64 = 28.0;
    /// The knob's diameter...
    pub const KNOB_SIZE: f64 = 20.0;
    /// ...its inset from the track's top and left when off...
    pub const KNOB_INSET: f64 = 3.0;
    /// ...and how far it moves when on.
    pub const KNOB_TRAVEL: f64 = 18.0;
    /// The segmented control's padding (and gap between segments)...
    pub const SEGMENT_PADDING: f64 = 2.0;
    /// ...a segment's corner radius...
    pub const SEGMENT_RADIUS: f64 = 5.0;
    /// ...and the chosen segment's rim.
    pub const SEGMENT_BORDER: f64 = 1.5;
    /// An app icon's side in its pick.
    pub const ICON_SIZE: f64 = 72.0;
    /// An icon pick's smallest height.
    pub const ICON_PICK_MIN_HEIGHT: f64 = 140.0;
    /// Icon picks per row.
    pub const ICON_COLUMNS: usize = 3;
}
