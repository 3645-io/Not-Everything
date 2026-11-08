//! Tests for `fx_ui_macos::build_info`: what the app shows of its
//! build channel and version.
//!
//! Contract exercised here:
//!
//! - `BuildInfo { channel, number, version }` (public fields, `Copy`);
//!   `BuildInfo::CURRENT` is this build's: `fx_channel::Channel::CURRENT`,
//!   `fx_channel::CURRENT_NUMBER` and the package version.
//! - `app_name()` is the channel's app name; `label()` is
//!   `fx_channel::version_label` (`0.2.0`, `0.2.0-RC-1`, `0.3.0-beta-2`);
//!   `name_and_label()` is "<app name> <label>", the status menu's first
//!   line.
//! - `window_title(build, Showing, measuring)`: the release app indexing `/`
//!   reads just "Not-Everything"; a `--root` folder, synthetic rows and the
//!   measurement modes add " — <what it shows>". A release candidate or beta
//!   reads "<app name> <label>" in place of the app name.
//! - `about()` gives the standard About panel's ApplicationVersion (the
//!   label) and Version (the build number), shown as "Version <label>
//!   (<build>)".
//!
//! Every test names its channel, so the suite passes in a release, RC or
//! beta build alike.

use std::path::Path;

use fx_channel::Channel;
use fx_ui_macos::build_info::{AboutVersion, BuildInfo, Showing, window_title};

const RELEASE: BuildInfo = BuildInfo {
    channel: Channel::Release,
    number: 0,
    version: "0.2.0",
};
const RC1: BuildInfo = BuildInfo {
    channel: Channel::Rc,
    number: 1,
    version: "0.2.0",
};
const BETA2: BuildInfo = BuildInfo {
    channel: Channel::Beta,
    number: 2,
    version: "0.3.0",
};

fn whole_disk() -> Showing<'static> {
    Showing::Root(Path::new("/"))
}

#[test]
fn current_is_this_build() {
    let current = BuildInfo::CURRENT;
    assert_eq!(current.channel, Channel::CURRENT);
    assert_eq!(current.number, fx_channel::CURRENT_NUMBER);
    assert_eq!(current.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(current.app_name(), fx_ui_macos::APP_NAME);
}

#[test]
fn labels() {
    assert_eq!(RELEASE.label(), "0.2.0");
    assert_eq!(RC1.label(), "0.2.0-RC-1");
    assert_eq!(BETA2.label(), "0.3.0-beta-2");
}

#[test]
fn name_and_label() {
    assert_eq!(RELEASE.name_and_label(), "Not-Everything 0.2.0");
    assert_eq!(RC1.name_and_label(), "Not-Everything Preview 0.2.0-RC-1");
    assert_eq!(
        BETA2.name_and_label(),
        "Not-Everything Preview 0.3.0-beta-2"
    );
}

#[test]
fn release_indexing_the_whole_disk_shows_just_the_app_name() {
    assert_eq!(window_title(RELEASE, whole_disk(), false), "Not-Everything");
}

#[test]
fn release_with_a_root_folder_shows_the_folder() {
    assert_eq!(
        window_title(
            RELEASE,
            Showing::Root(Path::new("/Users/tester/Projects")),
            false
        ),
        "Not-Everything \u{2014} /Users/tester/Projects"
    );
}

#[test]
fn synthetic_rows_show_their_count() {
    for measuring in [false, true] {
        assert_eq!(
            window_title(RELEASE, Showing::Synthetic(1_000_000), measuring),
            "Not-Everything \u{2014} 1000000 synthetic rows"
        );
    }
}

#[test]
fn measurement_runs_keep_the_root() {
    assert_eq!(
        window_title(RELEASE, whole_disk(), true),
        "Not-Everything \u{2014} /"
    );
    assert_eq!(
        window_title(RELEASE, Showing::Root(Path::new("/Applications")), true),
        "Not-Everything \u{2014} /Applications"
    );
}

#[test]
fn release_candidate_shows_the_preview_name_and_label() {
    assert_eq!(
        window_title(RC1, whole_disk(), false),
        "Not-Everything Preview 0.2.0-RC-1"
    );
    assert_eq!(
        window_title(RC1, Showing::Root(Path::new("/Users/tester")), false),
        "Not-Everything Preview 0.2.0-RC-1 \u{2014} /Users/tester"
    );
    assert_eq!(
        window_title(RC1, Showing::Synthetic(10), true),
        "Not-Everything Preview 0.2.0-RC-1 \u{2014} 10 synthetic rows"
    );
}

#[test]
fn beta_shows_the_preview_name_and_label() {
    assert_eq!(
        window_title(BETA2, whole_disk(), false),
        "Not-Everything Preview 0.3.0-beta-2"
    );
    assert_eq!(
        window_title(BETA2, Showing::Root(Path::new("/Volumes/Data")), false),
        "Not-Everything Preview 0.3.0-beta-2 \u{2014} /Volumes/Data"
    );
}

#[test]
fn about_panel_shows_the_label_and_build_number() {
    let cases = [
        (RELEASE, "0.2.0", "200", "Version 0.2.0 (200)"),
        (RC1, "0.2.0-RC-1", "200", "Version 0.2.0-RC-1 (200)"),
        (BETA2, "0.3.0-beta-2", "300", "Version 0.3.0-beta-2 (300)"),
    ];
    for (build, application_version, number, shown) in cases {
        let about = build.about();
        assert_eq!(
            about,
            AboutVersion {
                application_version: application_version.to_string(),
                build: number.to_string(),
            },
            "{build:?}"
        );
        assert_eq!(about.display(), shown, "{build:?}");
    }
}

#[test]
fn about_build_number_is_the_bundles() {
    assert_eq!(
        BuildInfo::CURRENT.about().build,
        fx_ui_macos::bundle::BUILD_NUMBER.to_string()
    );
}

#[test]
fn about_leaves_the_build_blank_when_the_version_cannot_be_numbered() {
    let odd = BuildInfo {
        channel: Channel::Release,
        number: 0,
        version: "1.0",
    };
    let about = odd.about();
    assert_eq!(about.application_version, "1.0");
    assert_eq!(about.build, "");
    assert_eq!(about.display(), "Version 1.0");
}
