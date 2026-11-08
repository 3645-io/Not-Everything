//! Tests for `fx_ui_macos::app_icon`: the six approved app icon
//! designs the Settings window offers, and their embedded artwork.
//!
//! Contract exercised here:
//!
//! - `AppIcon::ALL` holds the approved set in its canvas order: 19 Soft
//!   Lens, 20 Pressed Lens, 21 Match Row, 22 Glow Row, 10G Front Stack,
//!   10H Lit Slot. Match Row is the default (the icon in the `.app`).
//! - `dark_born()`: whether the design is drawn dark in its
//!   Default artwork: Pressed Lens, Glow Row and Lit Slot; Soft Lens, Match
//!   Row and Front Stack are light.
//! - `title()`, `design_number()`; `as_str()` is the settings-file value
//!   (kebab case) and `parse` reads it back; `Display` writes `as_str()`.
//! - `png(dark)` is the icon rendered by `actool` (masked, with the system's
//!   edge and shadow), `ICON_PIXELS` square: the design's Default artwork,
//!   or its Dark artwork when `dark`. Every image is distinct.
//! - `dock_icon(icon, mode, dark)` is what the Dock shows while the app
//!   runs: the bundle icon (drawn by macOS in the system's appearance and
//!   icon style) for the default icon with the System appearance, otherwise
//!   the chosen design's image for the app's light or dark appearance.

use fx_ui_macos::app_icon::{AppIcon, DockIcon, ICON_PIXELS, dock_icon};
use fx_ui_macos::appearance::AppearanceMode;

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

/// Width and height from a PNG's IHDR chunk.
fn png_size(bytes: &[u8]) -> (u32, u32) {
    assert!(bytes.len() > 33, "too short for a PNG");
    assert_eq!(bytes[..8], PNG_SIGNATURE, "not a PNG");
    assert_eq!(&bytes[12..16], b"IHDR", "first chunk is not IHDR");
    let be = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
    (be(16), be(20))
}

#[test]
fn all_is_the_approved_set_in_canvas_order() {
    let got: Vec<(&str, &str)> = AppIcon::ALL
        .iter()
        .map(|i| (i.design_number(), i.title()))
        .collect();
    assert_eq!(
        got,
        [
            ("19", "Soft Lens"),
            ("20", "Pressed Lens"),
            ("21", "Match Row"),
            ("22", "Glow Row"),
            ("10G", "Front Stack"),
            ("10H", "Lit Slot"),
        ]
    );
}

#[test]
fn match_row_is_the_default() {
    assert_eq!(AppIcon::default(), AppIcon::MatchRow);
    for icon in AppIcon::ALL {
        assert_eq!(icon.is_default(), icon == AppIcon::MatchRow, "{icon:?}");
    }
}

#[test]
fn light_and_dark_born_designs() {
    let dark: Vec<AppIcon> = AppIcon::ALL.into_iter().filter(|i| i.dark_born()).collect();
    assert_eq!(
        dark,
        [AppIcon::PressedLens, AppIcon::GlowRow, AppIcon::LitSlot]
    );
}

#[test]
fn settings_values_round_trip() {
    for icon in AppIcon::ALL {
        assert_eq!(AppIcon::parse(icon.as_str()), Some(icon));
        assert_eq!(icon.to_string(), icon.as_str());
    }
    assert_eq!(AppIcon::MatchRow.as_str(), "match-row");
    assert_eq!(AppIcon::LitSlot.as_str(), "lit-slot");
    for bad in ["", "Match Row", "match_row", "MATCH-ROW", "21", "glow"] {
        assert_eq!(AppIcon::parse(bad), None, "{bad:?}");
    }
}

#[test]
fn every_icon_has_light_and_dark_artwork_of_the_right_size() {
    assert_eq!(ICON_PIXELS, 256);
    for icon in AppIcon::ALL {
        for dark in [false, true] {
            let size = png_size(icon.png(dark));
            assert_eq!(size, (ICON_PIXELS, ICON_PIXELS), "{icon:?} dark={dark}");
        }
    }
}

#[test]
fn every_image_is_distinct() {
    let mut images: Vec<&[u8]> = Vec::new();
    for icon in AppIcon::ALL {
        for dark in [false, true] {
            let png = icon.png(dark);
            assert!(
                !images.contains(&png),
                "{icon:?} dark={dark} repeats another image"
            );
            images.push(png);
        }
    }
    assert_eq!(images.len(), 12);
}

#[test]
fn default_icon_following_the_system_leaves_the_dock_to_macos() {
    for dark in [false, true] {
        assert_eq!(
            dock_icon(AppIcon::MatchRow, AppearanceMode::System, dark),
            DockIcon::Bundle
        );
    }
}

#[test]
fn a_forced_appearance_shows_the_matching_artwork() {
    for icon in AppIcon::ALL {
        assert_eq!(
            dock_icon(icon, AppearanceMode::Light, false),
            DockIcon::Image { icon, dark: false },
            "{icon:?}"
        );
        assert_eq!(
            dock_icon(icon, AppearanceMode::Dark, true),
            DockIcon::Image { icon, dark: true },
            "{icon:?}"
        );
    }
}

#[test]
fn another_icon_follows_the_app_appearance() {
    for icon in AppIcon::ALL.into_iter().filter(|i| !i.is_default()) {
        for dark in [false, true] {
            assert_eq!(
                dock_icon(icon, AppearanceMode::System, dark),
                DockIcon::Image { icon, dark },
                "{icon:?}"
            );
        }
    }
}
