//! The app's light or dark appearance, chosen in the Settings window:
//! follow the system, or always light, or always dark.
//!
//! The AppKit layer maps it to `NSApp.appearance` (none, Aqua or Dark
//! Aqua); the choice is saved as the `appearance` line of the settings file
//! ([`crate::settings`]).

#![forbid(unsafe_code)]

use std::fmt;

use crate::FixedList;

/// Which appearance the app uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AppearanceMode {
    /// Follow the system's light or dark setting.
    #[default]
    System,
    /// Always light.
    Light,
    /// Always dark.
    Dark,
}

impl AppearanceMode {
    /// Every mode, in the order the Settings window shows them.
    pub const ALL: FixedList<AppearanceMode, 3> = FixedList([
        AppearanceMode::System,
        AppearanceMode::Light,
        AppearanceMode::Dark,
    ]);

    /// The label of this mode's segment in the Settings window.
    pub const fn title(self) -> &'static str {
        match self {
            AppearanceMode::System => "System",
            AppearanceMode::Light => "Light",
            AppearanceMode::Dark => "Dark",
        }
    }

    /// The settings-file value.
    pub const fn as_str(self) -> &'static str {
        match self {
            AppearanceMode::System => "system",
            AppearanceMode::Light => "light",
            AppearanceMode::Dark => "dark",
        }
    }

    /// Reads a settings-file value ([`as_str`](Self::as_str)); `None` for
    /// anything else.
    pub fn parse(s: &str) -> Option<AppearanceMode> {
        AppearanceMode::ALL.into_iter().find(|m| m.as_str() == s)
    }
}

impl fmt::Display for AppearanceMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
