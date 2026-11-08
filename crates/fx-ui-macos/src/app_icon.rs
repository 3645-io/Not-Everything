//! The app icons the Settings window offers: the six approved
//! designs from the icon canvas ("Search App Icon Concepts", page Green Rim,
//! "Approved set"), all in their Glass version with the Green Rim border.
//!
//! [`AppIcon::MatchRow`] is the icon inside the `.app` (Finder, Launchpad
//! and the Dock), compiled from `assets/AppIcon.icon`. Choosing another
//! design sets the Dock icon while the app runs ([`dock_icon`]); macOS has
//! no public API for alternate Mac app icons, so Finder and Launchpad keep
//! Match Row.
//!
//! Each design's artwork is embedded as the image macOS renders for it
//! (masked, with its edge and shadow), [`ICON_PIXELS`] square, in a light
//! and a dark version. `scripts/build-icons.sh` writes the images to
//! `assets/icons/`.

#![forbid(unsafe_code)]

use std::fmt;

use crate::FixedList;
use crate::appearance::AppearanceMode;

/// Width and height of the embedded icon images, in pixels: the Dock's
/// largest tile (128 points) at 2x.
pub const ICON_PIXELS: u32 = 256;

/// One of the approved app icon designs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AppIcon {
    /// 19 Soft Lens: a raised magnifier with a green glass well, light.
    SoftLens,
    /// 20 Pressed Lens: a pressed magnifier with a green rim, dark.
    PressedLens,
    /// 21 Match Row: a search slot over three rows, the middle one green,
    /// light. The default.
    #[default]
    MatchRow,
    /// 22 Glow Row: a search slot over three rows, one lit green, dark.
    GlowRow,
    /// 10G Front Stack: three raised rows, the top one green, and a
    /// magnifier, light.
    FrontStack,
    /// 10H Lit Slot: pressed charcoal slots, one lit green, and a
    /// magnifier, dark.
    LitSlot,
}

impl AppIcon {
    /// The approved designs, in the canvas's order.
    pub const ALL: FixedList<AppIcon, 6> = FixedList([
        AppIcon::SoftLens,
        AppIcon::PressedLens,
        AppIcon::MatchRow,
        AppIcon::GlowRow,
        AppIcon::FrontStack,
        AppIcon::LitSlot,
    ]);

    /// The design's name.
    pub const fn title(self) -> &'static str {
        match self {
            AppIcon::SoftLens => "Soft Lens",
            AppIcon::PressedLens => "Pressed Lens",
            AppIcon::MatchRow => "Match Row",
            AppIcon::GlowRow => "Glow Row",
            AppIcon::FrontStack => "Front Stack",
            AppIcon::LitSlot => "Lit Slot",
        }
    }

    /// The design's number on the icon canvas.
    pub const fn design_number(self) -> &'static str {
        match self {
            AppIcon::SoftLens => "19",
            AppIcon::PressedLens => "20",
            AppIcon::MatchRow => "21",
            AppIcon::GlowRow => "22",
            AppIcon::FrontStack => "10G",
            AppIcon::LitSlot => "10H",
        }
    }

    /// The settings-file value, also the stem of its image files.
    pub const fn as_str(self) -> &'static str {
        match self {
            AppIcon::SoftLens => "soft-lens",
            AppIcon::PressedLens => "pressed-lens",
            AppIcon::MatchRow => "match-row",
            AppIcon::GlowRow => "glow-row",
            AppIcon::FrontStack => "front-stack",
            AppIcon::LitSlot => "lit-slot",
        }
    }

    /// Reads a settings-file value ([`as_str`](Self::as_str)); `None` for
    /// anything else.
    pub fn parse(s: &str) -> Option<AppIcon> {
        AppIcon::ALL.into_iter().find(|i| i.as_str() == s)
    }

    /// Whether the design is dark in its Default artwork (Pressed Lens, Glow
    /// Row, Lit Slot); the others are light. A beta's Dock ribbon is chosen
    /// by it ([`crate::dock_ribbon`]).
    pub const fn dark_born(self) -> bool {
        matches!(
            self,
            AppIcon::PressedLens | AppIcon::GlowRow | AppIcon::LitSlot
        )
    }

    /// Whether this is the icon inside the `.app`.
    pub const fn is_default(self) -> bool {
        matches!(self, AppIcon::MatchRow)
    }

    /// The icon as a PNG: the design's Default artwork, or its Dark artwork
    /// when `dark`.
    pub const fn png(self, dark: bool) -> &'static [u8] {
        macro_rules! png {
            ($stem:literal) => {
                if dark {
                    include_bytes!(concat!("../assets/icons/", $stem, "-dark.png"))
                } else {
                    include_bytes!(concat!("../assets/icons/", $stem, "-light.png"))
                }
            };
        }
        match self {
            AppIcon::SoftLens => png!("soft-lens"),
            AppIcon::PressedLens => png!("pressed-lens"),
            AppIcon::MatchRow => png!("match-row"),
            AppIcon::GlowRow => png!("glow-row"),
            AppIcon::FrontStack => png!("front-stack"),
            AppIcon::LitSlot => png!("lit-slot"),
        }
    }
}

impl fmt::Display for AppIcon {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What the Dock shows while the app runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockIcon {
    /// The icon inside the `.app`, which macOS draws in the system's
    /// appearance and icon style (including clear and tinted).
    Bundle,
    /// The embedded image [`AppIcon::png`]`(dark)` of `icon`.
    Image {
        /// The chosen design.
        icon: AppIcon,
        /// Whether to show its Dark artwork.
        dark: bool,
    },
}

/// The Dock icon for the chosen `icon` and appearance `mode`, when the app
/// is `dark`. The default icon with the System appearance is left to macOS;
/// any other choice shows the design's artwork for the app's appearance.
pub fn dock_icon(icon: AppIcon, mode: AppearanceMode, dark: bool) -> DockIcon {
    if icon.is_default() && mode == AppearanceMode::System {
        DockIcon::Bundle
    } else {
        DockIcon::Image { icon, dark }
    }
}
