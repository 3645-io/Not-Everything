//! Tests for `fx_ui_macos::dock_ribbon`: the corner ribbon a
//! release candidate or beta draws on its Dock icon (the owner's "Option B,
//! corner ribbon").
//!
//! Contract exercised here:
//!
//! - `ribbon(channel, icon, dark) -> Option<Ribbon>`: none for a release.
//!   "RC" is fill #18b05a, text #161718 on every icon. "BETA" on a
//!   light-born icon (Soft Lens, Match Row, Front Stack) in the light
//!   appearance is fill #232527, text #f2f5f6; on a dark-born icon (Pressed
//!   Lens, Glow Row, Lit Slot) in the light appearance, and on every icon in
//!   the dark appearance, fill #ffffff, text #2c2c2c.
//! - `dock_tile(channel, icon, mode, dark) -> DockTile { icon, ribbon }`: a
//!   release shows `app_icon::dock_icon` and no ribbon; a Preview build
//!   always shows the design's image (so the ribbon can be drawn on it) and
//!   its ribbon.
//! - `RibbonLayout::for_body(x, y, side)` scales the approved design (a
//!   112-point icon box whose body is the rounded square from 10 to 102,
//!   radius 20; a 110 × 18 band rotated 45°, its box at top 14 and right −30
//!   inside the body; 10-point heavy text with 0.08 em letter spacing) to a
//!   body at (x, y) with `side`, in AppKit's coordinates (y up).
//!   `RibbonLayout::for_image(side)` places the body as the embedded icon
//!   images do: the middle 824 of 1024 units.

use fx_channel::Channel;
use fx_ui_macos::app_icon::{AppIcon, DockIcon, dock_icon};
use fx_ui_macos::appearance::AppearanceMode;
use fx_ui_macos::dock_ribbon::{
    DockTile, FONT_WEIGHT, Rgb, Ribbon, RibbonLayout, dock_tile, ribbon,
};

const RC: Ribbon = Ribbon {
    text: "RC",
    fill: Rgb(0x18b05a),
    text_color: Rgb(0x161718),
};
const BETA_CHARCOAL: Ribbon = Ribbon {
    text: "BETA",
    fill: Rgb(0x232527),
    text_color: Rgb(0xf2f5f6),
};
const BETA_WHITE: Ribbon = Ribbon {
    text: "BETA",
    fill: Rgb(0xffffff),
    text_color: Rgb(0x2c2c2c),
};

/// Every icon × appearance × channel, and the ribbon it draws.
#[test]
fn every_icon_appearance_and_channel() {
    use AppIcon::*;
    let light = false;
    let dark = true;
    #[rustfmt::skip]
    let table: [(AppIcon, bool, Channel, Option<Ribbon>); 36] = [
        (SoftLens,    light, Channel::Release, None),
        (SoftLens,    light, Channel::Rc,      Some(RC)),
        (SoftLens,    light, Channel::Beta,    Some(BETA_CHARCOAL)),
        (SoftLens,    dark,  Channel::Release, None),
        (SoftLens,    dark,  Channel::Rc,      Some(RC)),
        (SoftLens,    dark,  Channel::Beta,    Some(BETA_WHITE)),
        (PressedLens, light, Channel::Release, None),
        (PressedLens, light, Channel::Rc,      Some(RC)),
        (PressedLens, light, Channel::Beta,    Some(BETA_WHITE)),
        (PressedLens, dark,  Channel::Release, None),
        (PressedLens, dark,  Channel::Rc,      Some(RC)),
        (PressedLens, dark,  Channel::Beta,    Some(BETA_WHITE)),
        (MatchRow,    light, Channel::Release, None),
        (MatchRow,    light, Channel::Rc,      Some(RC)),
        (MatchRow,    light, Channel::Beta,    Some(BETA_CHARCOAL)),
        (MatchRow,    dark,  Channel::Release, None),
        (MatchRow,    dark,  Channel::Rc,      Some(RC)),
        (MatchRow,    dark,  Channel::Beta,    Some(BETA_WHITE)),
        (GlowRow,     light, Channel::Release, None),
        (GlowRow,     light, Channel::Rc,      Some(RC)),
        (GlowRow,     light, Channel::Beta,    Some(BETA_WHITE)),
        (GlowRow,     dark,  Channel::Release, None),
        (GlowRow,     dark,  Channel::Rc,      Some(RC)),
        (GlowRow,     dark,  Channel::Beta,    Some(BETA_WHITE)),
        (FrontStack,  light, Channel::Release, None),
        (FrontStack,  light, Channel::Rc,      Some(RC)),
        (FrontStack,  light, Channel::Beta,    Some(BETA_CHARCOAL)),
        (FrontStack,  dark,  Channel::Release, None),
        (FrontStack,  dark,  Channel::Rc,      Some(RC)),
        (FrontStack,  dark,  Channel::Beta,    Some(BETA_WHITE)),
        (LitSlot,     light, Channel::Release, None),
        (LitSlot,     light, Channel::Rc,      Some(RC)),
        (LitSlot,     light, Channel::Beta,    Some(BETA_WHITE)),
        (LitSlot,     dark,  Channel::Release, None),
        (LitSlot,     dark,  Channel::Rc,      Some(RC)),
        (LitSlot,     dark,  Channel::Beta,    Some(BETA_WHITE)),
    ];
    for (icon, dark, channel, expected) in table {
        assert_eq!(
            ribbon(channel, icon, dark),
            expected,
            "{icon:?} dark={dark} {channel:?}"
        );
    }
    // The table covers every combination once.
    for icon in AppIcon::ALL {
        for d in [false, true] {
            for channel in Channel::ALL {
                let rows = table
                    .iter()
                    .filter(|r| r.0 == icon && r.1 == d && r.2 == channel)
                    .count();
                assert_eq!(rows, 1, "{icon:?} dark={d} {channel:?}");
            }
        }
    }
}

#[test]
fn a_release_draws_no_ribbon() {
    for icon in AppIcon::ALL {
        for dark in [false, true] {
            assert_eq!(ribbon(Channel::Release, icon, dark), None, "{icon:?}");
        }
    }
}

#[test]
fn colour_components() {
    assert_eq!(Rgb(0xffffff).components(), [1.0, 1.0, 1.0]);
    assert_eq!(Rgb(0x000000).components(), [0.0, 0.0, 0.0]);
    let [r, g, b] = Rgb(0x18b05a).components();
    assert_eq!(
        [r * 255.0, g * 255.0, b * 255.0].map(f64::round),
        [24.0, 176.0, 90.0]
    );
}

#[test]
fn release_dock_tile_is_the_dock_icon_without_a_ribbon() {
    for icon in AppIcon::ALL {
        for mode in AppearanceMode::ALL {
            for dark in [false, true] {
                assert_eq!(
                    dock_tile(Channel::Release, icon, mode, dark),
                    DockTile {
                        icon: dock_icon(icon, mode, dark),
                        ribbon: None,
                    },
                    "{icon:?} {mode:?} dark={dark}"
                );
            }
        }
    }
}

#[test]
fn preview_dock_tile_always_shows_the_image_and_its_ribbon() {
    for channel in [Channel::Rc, Channel::Beta] {
        for icon in AppIcon::ALL {
            for mode in AppearanceMode::ALL {
                for dark in [false, true] {
                    let tile = dock_tile(channel, icon, mode, dark);
                    assert_eq!(
                        tile,
                        DockTile {
                            icon: DockIcon::Image { icon, dark },
                            ribbon: ribbon(channel, icon, dark),
                        },
                        "{channel:?} {icon:?} {mode:?} dark={dark}"
                    );
                    assert!(tile.ribbon.is_some());
                }
            }
        }
    }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

macro_rules! assert_close {
    ($a:expr, $b:expr) => {
        assert!(
            close($a, $b),
            "{} = {}, expected {}",
            stringify!($a),
            $a,
            $b
        )
    };
}

#[test]
fn layout_at_the_design_size() {
    // The design's 112-point box: the body from 10 to 102 on both axes.
    let l = RibbonLayout::for_body(10.0, 10.0, 92.0);
    assert_close!(l.body_x, 10.0);
    assert_close!(l.body_y, 10.0);
    assert_close!(l.body_side, 92.0);
    assert_close!(l.corner_radius, 20.0);
    assert_close!(l.band_length, 110.0);
    assert_close!(l.band_thickness, 18.0);
    // The band's box: right −30 and 110 wide puts its left edge at 12, its
    // top at 14, so its centre is 67 from the body's left and 23 below its
    // top (y up: 102 − 23 = 79).
    assert_close!(l.band_center_x, 77.0);
    assert_close!(l.band_center_y, 79.0);
    // 45° clockwise on screen, as AppKit (y up) counts it.
    assert_close!(l.rotation_degrees, -45.0);
    assert_close!(l.font_size, 10.0);
    assert_close!(l.kern, 0.8);
    assert_eq!(FONT_WEIGHT, 800);
}

#[test]
fn layout_scales_with_the_icon() {
    let unit = RibbonLayout::for_body(0.0, 0.0, 92.0);
    let double = RibbonLayout::for_body(0.0, 0.0, 184.0);
    assert_close!(double.body_side, 2.0 * unit.body_side);
    assert_close!(double.corner_radius, 2.0 * unit.corner_radius);
    assert_close!(double.band_length, 2.0 * unit.band_length);
    assert_close!(double.band_thickness, 2.0 * unit.band_thickness);
    assert_close!(double.band_center_x, 2.0 * unit.band_center_x);
    assert_close!(double.band_center_y, 2.0 * unit.band_center_y);
    assert_close!(double.font_size, 2.0 * unit.font_size);
    assert_close!(double.kern, 2.0 * unit.kern);
    assert_close!(double.rotation_degrees, unit.rotation_degrees);
}

#[test]
fn layout_for_an_embedded_image_uses_the_icon_grid() {
    let l = RibbonLayout::for_image(1024.0);
    assert_close!(l.body_x, 100.0);
    assert_close!(l.body_y, 100.0);
    assert_close!(l.body_side, 824.0);
    assert_eq!(
        RibbonLayout::for_image(256.0),
        RibbonLayout::for_body(25.0, 25.0, 206.0)
    );
}

#[test]
fn the_band_crosses_the_top_right_corner_end_to_end() {
    let l = RibbonLayout::for_body(0.0, 0.0, 92.0);
    let angle = l.rotation_degrees.to_radians();
    let (dx, dy) = (angle.cos(), angle.sin());
    let half = l.band_length / 2.0;
    // Both ends of the band's centre line lie outside the body, one past
    // its top edge and one past its right edge, so clipping to the body
    // leaves no square end showing.
    let start = (l.band_center_x - half * dx, l.band_center_y - half * dy);
    let end = (l.band_center_x + half * dx, l.band_center_y + half * dy);
    assert!(start.1 > l.body_y + l.body_side, "{start:?}");
    assert!(end.0 > l.body_x + l.body_side, "{end:?}");
    // The centre line cuts the top edge at 44 and the right edge 48 below
    // the top, 48 / √2 from the corner.
    let corner = (l.body_x + l.body_side, l.body_y + l.body_side);
    let distance = ((corner.0 - l.band_center_x) * dy - (corner.1 - l.band_center_y) * dx).abs();
    assert_close!(distance, 48.0 / 2f64.sqrt());
}
