//! Tests for `fx_channel`: the build channel and what follows from it.
//!
//! Contract exercised here:
//!
//! - `Channel::{Release, Rc, Beta}`; `Channel::CURRENT` is fixed at compile
//!   time from `NE_CHANNEL` (unset, empty or `release`, `rc`, `beta`) and
//!   `CURRENT_NUMBER` from `NE_CHANNEL_NUMBER` (0 for a release build).
//!   `from_env` is the rule behind them; a bad value is an error, which the
//!   compile-time constants turn into a build failure.
//! - Release builds are the daily app: Not-Everything,
//!   `io.3645.not-everything`, data folder `Not-Everything`, command
//!   `not-everything`, hotkey option+space. Release candidates and betas are
//!   the separate Preview app: Not-Everything Preview,
//!   `io.3645.not-everything.preview`, data folder `Not-Everything Preview`,
//!   command `not-everything-preview`, hotkey shift+option+space.
//! - `version_label` is the bare version for a release, `X.Y.Z-RC-N` and
//!   `X.Y.Z-beta-N` otherwise.
//! - `build_number("X.Y.Z")` is `X*10000 + Y*100 + Z`, and `None` for
//!   anything that is not three plain numbers or has a minor or patch part
//!   of 100 or more.

use fx_channel::{CURRENT_NUMBER, Channel, build_number, from_env, version_label};

const PREVIEWS: [Channel; 2] = [Channel::Rc, Channel::Beta];

#[test]
fn all_lists_every_channel_once() {
    assert_eq!(Channel::ALL, [Channel::Release, Channel::Rc, Channel::Beta]);
}

#[test]
fn names_parse_back() {
    for c in Channel::ALL {
        assert_eq!(Channel::parse(c.as_str()), Some(c), "{c:?}");
    }
    assert_eq!(Channel::Release.as_str(), "release");
    assert_eq!(Channel::Rc.as_str(), "rc");
    assert_eq!(Channel::Beta.as_str(), "beta");
    for bad in ["", "RC", "Release", "beta ", "preview", "debug"] {
        assert_eq!(Channel::parse(bad), None, "{bad:?}");
    }
}

#[test]
fn release_is_the_daily_app() {
    let c = Channel::Release;
    assert!(!c.is_preview());
    assert_eq!(c.app_name(), "Not-Everything");
    assert_eq!(c.bundle_id(), "io.3645.not-everything");
    assert_eq!(c.data_folder(), "Not-Everything");
    assert_eq!(c.command_name(), "not-everything");
    assert_eq!(c.default_hotkey(), "option+space");
}

#[test]
fn release_candidates_and_betas_are_the_preview_app() {
    for c in PREVIEWS {
        assert!(c.is_preview(), "{c:?}");
        assert_eq!(c.app_name(), "Not-Everything Preview", "{c:?}");
        assert_eq!(c.bundle_id(), "io.3645.not-everything.preview", "{c:?}");
        assert_eq!(c.data_folder(), "Not-Everything Preview", "{c:?}");
        assert_eq!(c.command_name(), "not-everything-preview", "{c:?}");
        assert_eq!(c.default_hotkey(), "shift+option+space", "{c:?}");
    }
}

/// The Preview app must never share anything with the daily app: macOS
/// keys Full Disk Access, Launch at Login and the hotkey registration by
/// bundle, and the data folder holds the settings and indexes.
#[test]
fn preview_shares_nothing_with_the_release_app() {
    let r = Channel::Release;
    for c in PREVIEWS {
        assert_ne!(c.app_name(), r.app_name());
        assert_ne!(c.bundle_id(), r.bundle_id());
        assert_ne!(c.data_folder(), r.data_folder());
        assert_ne!(c.command_name(), r.command_name());
        assert_ne!(c.default_hotkey(), r.default_hotkey());
    }
}

#[test]
fn version_labels() {
    assert_eq!(version_label("0.2.0", Channel::Release, 0), "0.2.0");
    assert_eq!(version_label("0.2.0", Channel::Release, 3), "0.2.0");
    assert_eq!(version_label("0.2.0", Channel::Rc, 1), "0.2.0-RC-1");
    assert_eq!(version_label("1.10.3", Channel::Rc, 12), "1.10.3-RC-12");
    assert_eq!(version_label("0.2.0", Channel::Beta, 4), "0.2.0-beta-4");
}

#[test]
fn build_numbers() {
    assert_eq!(build_number("0.2.0"), Some(200));
    assert_eq!(build_number("0.1.3"), Some(103));
    assert_eq!(build_number("0.0.0"), Some(0));
    assert_eq!(build_number("1.0.0"), Some(10_000));
    assert_eq!(build_number("1.2.3"), Some(10_203));
    assert_eq!(build_number("2.99.99"), Some(29_999));
    assert_eq!(build_number("12.34.56"), Some(123_456));
}

#[test]
fn build_numbers_never_collide() {
    // Every minor and patch part below 100 gets its own number, in version
    // order.
    let mut last = None;
    for major in 0..3 {
        for minor in 0..100 {
            for patch in 0..100 {
                let n = build_number(&format!("{major}.{minor}.{patch}")).unwrap();
                if let Some(prev) = last {
                    assert!(n > prev, "{major}.{minor}.{patch}");
                }
                last = Some(n);
            }
        }
    }
}

#[test]
fn build_number_rejects_anything_else() {
    for bad in [
        "",
        "0",
        "0.2",
        "0.2.0.1",
        "0.100.0",
        "0.2.100",
        "0.2.0-RC-1",
        "0.2.0-beta.1",
        "v0.2.0",
        " 0.2.0",
        "0.2.0 ",
        "0..0",
        ".2.0",
        "0.2.",
        "a.b.c",
        "0.-2.0",
        "+0.2.0",
        "00.2.0",
        "0.02.0",
        "0.2.00",
        "999999999.0.0",
    ] {
        assert_eq!(build_number(bad), None, "{bad:?}");
    }
}

#[test]
fn from_env_defaults_to_release() {
    assert_eq!(from_env(None, None), Ok((Channel::Release, 0)));
    assert_eq!(from_env(Some(""), None), Ok((Channel::Release, 0)));
    assert_eq!(from_env(Some("release"), None), Ok((Channel::Release, 0)));
    assert_eq!(
        from_env(Some("release"), Some("")),
        Ok((Channel::Release, 0))
    );
}

#[test]
fn from_env_reads_the_preview_number() {
    assert_eq!(from_env(Some("rc"), Some("1")), Ok((Channel::Rc, 1)));
    assert_eq!(from_env(Some("rc"), Some("12")), Ok((Channel::Rc, 12)));
    assert_eq!(from_env(Some("beta"), Some("3")), Ok((Channel::Beta, 3)));
}

#[test]
fn from_env_rejects_bad_values() {
    for (channel, number) in [
        (Some("RC"), Some("1")),
        (Some("preview"), Some("1")),
        (Some("debug"), None),
        // A preview build needs its number.
        (Some("rc"), None),
        (Some("beta"), Some("")),
        // Numbers are whole numbers from 1, without leading zeros or signs.
        (Some("rc"), Some("0")),
        (Some("rc"), Some("01")),
        (Some("rc"), Some("-1")),
        (Some("rc"), Some("+1")),
        (Some("rc"), Some("1a")),
        (Some("beta"), Some(" 2")),
        (Some("beta"), Some("99999999999")),
        // A release has no number.
        (None, Some("1")),
        (Some("release"), Some("2")),
    ] {
        let got = from_env(channel, number);
        let message = got.err().unwrap_or_else(|| {
            panic!("NE_CHANNEL={channel:?} NE_CHANNEL_NUMBER={number:?} must be refused")
        });
        assert!(
            message.contains("NE_CHANNEL"),
            "the message names the variable: {message:?}"
        );
    }
}

/// The constants this build was compiled with follow the same rule.
#[test]
fn the_current_build_is_consistent() {
    assert_eq!(
        from_env(option_env!("NE_CHANNEL"), option_env!("NE_CHANNEL_NUMBER")),
        Ok((Channel::CURRENT, CURRENT_NUMBER))
    );
    assert_eq!(CURRENT_NUMBER == 0, !Channel::CURRENT.is_preview());
}
