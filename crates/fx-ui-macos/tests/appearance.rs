//! Tests for `fx_ui_macos::appearance`: the app's light or dark
//! appearance choice in Settings.
//!
//! Contract exercised here:
//!
//! - `AppearanceMode` is `System` (the default), `Light` or `Dark`;
//!   `AppearanceMode::ALL` lists them in the order the Settings window shows
//!   them.
//! - `title()` is the segment label; `as_str()` is the settings-file value
//!   and `parse` reads it back (exact, lower case); `Display` writes
//!   `as_str()`.

use fx_ui_macos::appearance::AppearanceMode;

#[test]
fn default_follows_the_system() {
    assert_eq!(AppearanceMode::default(), AppearanceMode::System);
}

#[test]
fn all_is_system_light_dark_in_order() {
    assert_eq!(
        AppearanceMode::ALL.as_slice(),
        &[
            AppearanceMode::System,
            AppearanceMode::Light,
            AppearanceMode::Dark
        ]
    );
}

#[test]
fn titles_match_the_settings_window_segments() {
    let titles: Vec<&str> = AppearanceMode::ALL.iter().map(|m| m.title()).collect();
    assert_eq!(titles, ["System", "Light", "Dark"]);
}

#[test]
fn settings_values_round_trip() {
    for mode in AppearanceMode::ALL {
        assert_eq!(AppearanceMode::parse(mode.as_str()), Some(mode));
        assert_eq!(mode.to_string(), mode.as_str());
    }
    assert_eq!(AppearanceMode::System.as_str(), "system");
    assert_eq!(AppearanceMode::Light.as_str(), "light");
    assert_eq!(AppearanceMode::Dark.as_str(), "dark");
}

#[test]
fn unknown_values_are_rejected() {
    for bad in ["", "auto", "Dark", " dark", "darkk", "1"] {
        assert_eq!(AppearanceMode::parse(bad), None, "{bad:?}");
    }
}
