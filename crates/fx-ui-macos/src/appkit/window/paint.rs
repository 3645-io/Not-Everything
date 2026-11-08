//! The theme's colours and type styles as AppKit objects. Values come
//! from [`crate::theme`]; this file only converts them.

use objc2::rc::Retained;
use objc2_app_kit::{
    NSColor, NSFont, NSFontWeight, NSFontWeightBlack, NSFontWeightBold, NSFontWeightHeavy,
    NSFontWeightLight, NSFontWeightMedium, NSFontWeightRegular, NSFontWeightSemibold,
    NSFontWeightThin, NSFontWeightUltraLight,
};

use crate::theme::{NamedWeight, Rgb, Rgba, Theme, Token, TypeStyle};

/// An sRGB colour, opaque.
pub fn rgb(color: Rgb) -> Retained<NSColor> {
    let [r, g, b] = color.components();
    NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0)
}

/// An sRGB colour with its alpha.
pub fn rgba(color: Rgba) -> Retained<NSColor> {
    let [r, g, b] = color.rgb.components();
    NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, color.alpha)
}

/// `token`'s colour in `theme`.
pub fn token(theme: Theme, token: Token) -> Retained<NSColor> {
    rgb(theme.color(token))
}

/// AppKit's value for a named weight.
fn named(weight: NamedWeight) -> NSFontWeight {
    // SAFETY: the weights are immutable framework constants.
    unsafe {
        match weight {
            NamedWeight::UltraLight => NSFontWeightUltraLight,
            NamedWeight::Thin => NSFontWeightThin,
            NamedWeight::Light => NSFontWeightLight,
            NamedWeight::Regular => NSFontWeightRegular,
            NamedWeight::Medium => NSFontWeightMedium,
            NamedWeight::Semibold => NSFontWeightSemibold,
            NamedWeight::Bold => NSFontWeightBold,
            NamedWeight::Heavy => NSFontWeightHeavy,
            NamedWeight::Black => NSFontWeightBlack,
        }
    }
}

/// The `NSFontWeight` for `style`'s CSS weight: its named weight, or for a
/// weight between two named ones (650) the lower one. The board's browser
/// draws 650 as Semibold, while macOS would round the midpoint between
/// Semibold and Bold up to Bold, about 2% wider and heavier.
pub fn weight(style: &TypeStyle) -> NSFontWeight {
    named(style.weight_mix().lower)
}

/// The system font for `style`: monospaced for `mono` styles, with
/// tabular figures for `tabular` ones. Letter spacing (`style.kern()`) and
/// capitals are the text's attributes, not the font's.
pub fn font(style: &TypeStyle) -> Retained<NSFont> {
    let w = weight(style);
    if style.mono {
        NSFont::monospacedSystemFontOfSize_weight(style.size, w)
    } else if style.tabular {
        NSFont::monospacedDigitSystemFontOfSize_weight(style.size, w)
    } else {
        NSFont::systemFontOfSize_weight(style.size, w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::styles;

    #[test]
    fn colours_are_srgb() {
        let c = token(Theme::Dark, Token::GreenInk);
        let [r, g, b] = Rgb(0x3fdc84).components();
        assert!((c.redComponent() - r).abs() < 1e-9);
        assert!((c.greenComponent() - g).abs() < 1e-9);
        assert!((c.blueComponent() - b).abs() < 1e-9);
        assert_eq!(c.alphaComponent(), 1.0);
        assert_eq!(rgba(Rgba::new(0x364950, 0.18)).alphaComponent(), 0.18);
    }

    #[test]
    /// Named weights map to themselves; 650 (between Semibold and Bold)
    /// is drawn Semibold, as the board's browser draws it (macOS would
    /// round the midpoint up to Bold).
    fn weights_sit_between_the_named_ones() {
        let (semibold, bold) = (named(NamedWeight::Semibold), named(NamedWeight::Bold));
        assert_eq!(weight(&styles::TITLE_APP), bold);
        assert_eq!(weight(&styles::TEXT_BODY), named(NamedWeight::Regular));
        assert_eq!(weight(&styles::SETTING_LABEL), semibold);
        assert_eq!(weight(&styles::TOGGLE), semibold);
        assert_eq!(weight(&styles::BUTTON), semibold);
        assert_eq!(weight(&styles::ROW_HIT), named(NamedWeight::Heavy));
    }

    #[test]
    fn fonts_have_the_styles_size_and_family() {
        let f = font(&styles::SEARCH_TEXT);
        assert_eq!(f.pointSize(), 17.0);
        assert!(!f.isFixedPitch());
        assert!(font(&styles::SYNTAX_CODE).isFixedPitch());
        assert_eq!(font(&styles::ROW_FIGURES).pointSize(), 14.0);
    }
}
