//! The corner ribbon a release candidate or beta draws on its Dock icon
//! (the owner chose "Option B, corner ribbon"), so a Preview build
//! is told apart from the release app at a glance. A release draws none.
//!
//! A diagonal band across the top-right corner of whichever icon the Dock
//! shows, clipped to the icon's rounded body, reading "RC" or "BETA". It is
//! drawn only while the app runs; the Finder icon is unchanged. This module
//! holds the choice of text, colours and geometry; the AppKit layer only
//! draws it, again whenever the appearance or the chosen icon changes.
//!
//! The geometry is the approved design's ([`RibbonLayout`]), scaled from its
//! 112-point icon box to the icon's body.

#![forbid(unsafe_code)]

use fx_channel::Channel;

use crate::app_icon::{AppIcon, DockIcon, dock_icon};
use crate::appearance::AppearanceMode;
/// The sRGB colour type, shared with the interface's theme.
pub use crate::theme::Rgb;

/// What the ribbon says and its colours.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ribbon {
    /// "RC" or "BETA".
    pub text: &'static str,
    /// The band's colour.
    pub fill: Rgb,
    /// The text's colour.
    pub text_color: Rgb,
}

/// A release candidate's ribbon: green, on every icon.
const RC: Ribbon = Ribbon {
    text: "RC",
    fill: Rgb(0x18b05a),
    text_color: Rgb(0x161718),
};

/// A beta's ribbon on a light-born icon in the light appearance.
const BETA_CHARCOAL: Ribbon = Ribbon {
    text: "BETA",
    fill: Rgb(0x232527),
    text_color: Rgb(0xf2f5f6),
};

/// A beta's ribbon on a dark-born icon in the light appearance, and on
/// every icon in the dark appearance.
const BETA_WHITE: Ribbon = Ribbon {
    text: "BETA",
    fill: Rgb(0xffffff),
    text_color: Rgb(0x2c2c2c),
};

/// The ribbon a `channel` build draws on `icon` shown in its light or `dark`
/// artwork; `None` for a release.
pub fn ribbon(channel: Channel, icon: AppIcon, dark: bool) -> Option<Ribbon> {
    match channel {
        Channel::Release => None,
        Channel::Rc => Some(RC),
        Channel::Beta if dark || icon.dark_born() => Some(BETA_WHITE),
        Channel::Beta => Some(BETA_CHARCOAL),
    }
}

/// What the Dock shows while the app runs: the icon, and the ribbon drawn
/// over it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DockTile {
    /// The icon ([`dock_icon`]).
    pub icon: DockIcon,
    /// The ribbon over it, for a release candidate or beta.
    pub ribbon: Option<Ribbon>,
}

/// The Dock tile of a `channel` build for the chosen `icon` and appearance
/// `mode`, when the app is `dark`. A release shows [`dock_icon`]. A Preview
/// build always shows the design's embedded image, even where a release
/// leaves the Dock to the bundle's icon, so the ribbon can be drawn on it.
pub fn dock_tile(channel: Channel, icon: AppIcon, mode: AppearanceMode, dark: bool) -> DockTile {
    DockTile {
        icon: if channel.is_preview() {
            DockIcon::Image { icon, dark }
        } else {
            dock_icon(icon, mode, dark)
        },
        ribbon: ribbon(channel, icon, dark),
    }
}

/// The approved design, in points on its 112-point icon box.
mod design {
    /// The icon body's side: the rounded square from 10 to 102.
    pub const BODY: f64 = 92.0;
    /// The body's corner radius.
    pub const CORNER_RADIUS: f64 = 20.0;
    /// The band's box before rotation: its length...
    pub const BAND_LENGTH: f64 = 110.0;
    /// ...and thickness...
    pub const BAND_THICKNESS: f64 = 18.0;
    /// ...placed this far below the body's top...
    pub const BAND_TOP: f64 = 14.0;
    /// ...and this far in from its right edge (negative: past it).
    pub const BAND_RIGHT: f64 = -30.0;
    /// The band turns this many degrees clockwise about its centre.
    pub const ROTATION: f64 = 45.0;
    /// The text's size.
    pub const FONT_SIZE: f64 = 10.0;
    /// The text's letter spacing, in ems.
    pub const LETTER_SPACING: f64 = 0.08;
}

/// The text's weight on the CSS scale: 800, heavy (AppKit's
/// `NSFontWeightHeavy`).
pub const FONT_WEIGHT: u16 = 800;

/// The embedded icon images keep the icon body in the middle 824 of 1024
/// units (the macOS icon grid): this far in from each edge...
const GRID_INSET: f64 = 100.0 / 1024.0;
/// ...and this wide, as fractions of the image.
const GRID_BODY: f64 = 824.0 / 1024.0;

/// Where the ribbon goes, in AppKit's coordinates (origin bottom left, y
/// up) and the icon's units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RibbonLayout {
    /// The icon body's left edge.
    pub body_x: f64,
    /// The icon body's bottom edge.
    pub body_y: f64,
    /// The icon body's width and height; the band is clipped to it.
    pub body_side: f64,
    /// The body's corner radius.
    pub corner_radius: f64,
    /// The band's centre, across...
    pub band_center_x: f64,
    /// ...and up.
    pub band_center_y: f64,
    /// The band's length before rotation.
    pub band_length: f64,
    /// The band's thickness.
    pub band_thickness: f64,
    /// The band's rotation about its centre, in degrees counter-clockwise
    /// (AppKit's sense): −45, so it runs down to the right.
    pub rotation_degrees: f64,
    /// The text's size, centred in the band at [`FONT_WEIGHT`].
    pub font_size: f64,
    /// The space added after each letter.
    pub kern: f64,
}

impl RibbonLayout {
    /// The ribbon for an icon body with its bottom-left corner at (`x`,
    /// `y`), `side` units square.
    pub fn for_body(x: f64, y: f64, side: f64) -> RibbonLayout {
        let scale = side / design::BODY;
        // The band's box, measured from the body's top-left corner as the
        // design places it (y down).
        let box_left = design::BODY - design::BAND_RIGHT - design::BAND_LENGTH;
        let center_from_left = box_left + design::BAND_LENGTH / 2.0;
        let center_from_top = design::BAND_TOP + design::BAND_THICKNESS / 2.0;
        let font_size = design::FONT_SIZE * scale;
        RibbonLayout {
            body_x: x,
            body_y: y,
            body_side: side,
            corner_radius: design::CORNER_RADIUS * scale,
            band_center_x: x + center_from_left * scale,
            band_center_y: y + side - center_from_top * scale,
            band_length: design::BAND_LENGTH * scale,
            band_thickness: design::BAND_THICKNESS * scale,
            rotation_degrees: -design::ROTATION,
            font_size,
            kern: design::LETTER_SPACING * font_size,
        }
    }

    /// The ribbon for an icon image `side` units square whose body sits on
    /// the macOS icon grid, as in the embedded images.
    pub fn for_image(side: f64) -> RibbonLayout {
        let inset = side * GRID_INSET;
        RibbonLayout::for_body(inset, inset, side * GRID_BODY)
    }
}
