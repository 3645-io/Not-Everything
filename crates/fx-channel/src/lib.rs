//! The build channel: whether a build is the release app or a Preview build
//! (a release candidate or a beta), and the names, version label and build
//! number that follow from it.
//!
//! The channel is fixed at compile time from two environment variables,
//! which `scripts/release-macos.sh` sets:
//!
//! - `NE_CHANNEL`: unset, empty or `release` for the release app; `rc` for a
//!   release candidate; `beta` for a beta.
//! - `NE_CHANNEL_NUMBER`: the candidate's or beta's number N, from 1; required
//!   for `rc` and `beta`, refused for a release.
//!
//! Cargo rebuilds the crates that read them when they change. A value
//! [`from_env`] refuses fails the build, not the app at run time.
//!
//! Release candidates and betas are a separate app, Not-Everything Preview,
//! with its own bundle ID, data folder, command and default hotkey, so
//! testing one never touches the daily app's settings, indexes, Full Disk
//! Access grant or login item. The names below are the only names allowed
//! to contain the product name; the owner approved each of them.
//!
//! Each name has a function per channel, so tests cover every channel
//! without rebuilding; [`Channel::CURRENT`] and [`CURRENT_NUMBER`] are this
//! build's.

#![forbid(unsafe_code)]

/// The kind of build.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Channel {
    /// The release app, built from `main` after the release pull request:
    /// the app people use every day.
    Release,
    /// A release candidate, built from an `rc/X.Y.Z` branch: the Preview app.
    Rc,
    /// A beta, built from `dev`: the Preview app, signed but not notarized.
    Beta,
}

/// `NE_CHANNEL` and `NE_CHANNEL_NUMBER` as this crate was compiled with
/// them. A free constant, so it is evaluated, and a bad value fails the
/// build, even when nothing reads it.
const BUILD: (Channel, u32) =
    match from_env(option_env!("NE_CHANNEL"), option_env!("NE_CHANNEL_NUMBER")) {
        Ok(build) => build,
        Err(message) => panic!("{}", message),
    };

/// This build's release candidate or beta number N (from 1), or 0 for a
/// release build.
pub const CURRENT_NUMBER: u32 = BUILD.1;

impl Channel {
    /// Every channel: release, release candidate, beta.
    pub const ALL: [Channel; 3] = [Channel::Release, Channel::Rc, Channel::Beta];

    /// This build's channel, from `NE_CHANNEL` at compile time.
    pub const CURRENT: Channel = BUILD.0;

    /// The channel `NE_CHANNEL` names: `release`, `rc` or `beta`, exactly.
    pub const fn parse(name: &str) -> Option<Channel> {
        let mut i = 0;
        while i < Channel::ALL.len() {
            if str_eq(name, Channel::ALL[i].as_str()) {
                return Some(Channel::ALL[i]);
            }
            i += 1;
        }
        None
    }

    /// The channel's `NE_CHANNEL` value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Channel::Release => "release",
            Channel::Rc => "rc",
            Channel::Beta => "beta",
        }
    }

    /// Whether the build is the Preview app (a release candidate or a beta).
    pub const fn is_preview(self) -> bool {
        !matches!(self, Channel::Release)
    }

    /// The app's name: its bundle and `.app` name, its executable's, and the
    /// name in its menus and windows.
    pub const fn app_name(self) -> &'static str {
        if self.is_preview() {
            "Not-Everything Preview"
        } else {
            "Not-Everything"
        }
    }

    /// The app's bundle identifier.
    pub const fn bundle_id(self) -> &'static str {
        if self.is_preview() {
            "io.3645.not-everything.preview"
        } else {
            "io.3645.not-everything"
        }
    }

    /// The app's folder under `~/Library/Application Support/`, holding the
    /// settings file and the index stores: the app's name.
    pub const fn data_folder(self) -> &'static str {
        self.app_name()
    }

    /// The command-line tool's name. The Cargo binary is always
    /// `not-everything`; the release script renames a Preview build's copy.
    pub const fn command_name(self) -> &'static str {
        if self.is_preview() {
            "not-everything-preview"
        } else {
            "not-everything"
        }
    }

    /// The default global summon hotkey, in the canonical form the app's
    /// hotkey parser reads and writes: option+space for the release app,
    /// shift+option+space for the Preview app, so both can run at once.
    pub const fn default_hotkey(self) -> &'static str {
        if self.is_preview() {
            "shift+option+space"
        } else {
            "option+space"
        }
    }
}

/// The version as the build shows it: `version` for a release,
/// `version-RC-n` for a release candidate and `version-beta-n` for a beta
/// (`0.2.0`, `0.2.0-RC-1`, `0.2.0-beta-1`). The bundle's version strings
/// stay the bare version.
pub fn version_label(version: &str, channel: Channel, n: u32) -> String {
    match channel {
        Channel::Release => version.to_owned(),
        Channel::Rc => format!("{version}-RC-{n}"),
        Channel::Beta => format!("{version}-beta-{n}"),
    }
}

/// The build number of `version` (`X.Y.Z`): `X*10000 + Y*100 + Z`, so 0.2.0
/// is 200 and 1.2.3 is 10203. `None` unless `version` is three plain
/// numbers without leading zeros, with the minor and patch parts below 100,
/// and the result fits a `u32`.
pub const fn build_number(version: &str) -> Option<u32> {
    let bytes = version.as_bytes();
    let mut parts = [0u32; 3];
    let mut part = 0;
    let mut start = 0;
    let mut i = 0;
    while i <= bytes.len() {
        if i == bytes.len() || bytes[i] == b'.' {
            if part >= 3 {
                return None;
            }
            match parse_part(bytes, start, i) {
                Some(n) => parts[part] = n,
                None => return None,
            }
            part += 1;
            start = i + 1;
        }
        i += 1;
    }
    if part != 3 || parts[1] >= 100 || parts[2] >= 100 {
        return None;
    }
    match parts[0].checked_mul(10_000) {
        Some(major) => major.checked_add(parts[1] * 100 + parts[2]),
        None => None,
    }
}

/// The channel and number that `NE_CHANNEL` and `NE_CHANNEL_NUMBER` select
/// (`None` when unset), or why they are refused. An empty value counts as
/// unset. A release has number 0.
///
/// # Errors
///
/// A message naming the variable: an unknown channel, a release candidate
/// or beta without a number, a release with one, or a number that is not a
/// whole number from 1 without leading zeros.
pub const fn from_env(
    channel: Option<&str>,
    number: Option<&str>,
) -> Result<(Channel, u32), &'static str> {
    let channel = match channel {
        None => Channel::Release,
        Some(name) if name.is_empty() => Channel::Release,
        Some(name) => match Channel::parse(name) {
            Some(c) => c,
            None => return Err("NE_CHANNEL must be unset, release, rc or beta"),
        },
    };
    let number = match number {
        None => None,
        Some(n) if n.is_empty() => None,
        Some(n) => {
            let bytes = n.as_bytes();
            match parse_part(bytes, 0, bytes.len()) {
                Some(0) | None => {
                    return Err("NE_CHANNEL_NUMBER must be a whole number from 1, \
                                without leading zeros");
                }
                Some(n) => Some(n),
            }
        }
    };
    match (channel, number) {
        (Channel::Release, None) => Ok((Channel::Release, 0)),
        (Channel::Release, Some(_)) => Err("NE_CHANNEL_NUMBER is only for rc and beta builds \
                                            (NE_CHANNEL=rc or beta)"),
        (_, None) => Err("an rc or beta build (NE_CHANNEL) needs NE_CHANNEL_NUMBER"),
        (c, Some(n)) => Ok((c, n)),
    }
}

/// `bytes[start..end]` as a decimal number: digits only, not empty, no
/// leading zero (but `0` itself), and fitting a `u32`.
const fn parse_part(bytes: &[u8], start: usize, end: usize) -> Option<u32> {
    if start >= end || (bytes[start] == b'0' && end - start > 1) {
        return None;
    }
    let mut value: u32 = 0;
    let mut i = start;
    while i < end {
        let b = bytes[i];
        if !b.is_ascii_digit() {
            return None;
        }
        value = match value.checked_mul(10) {
            Some(v) => match v.checked_add((b - b'0') as u32) {
                Some(v) => v,
                None => return None,
            },
            None => return None,
        };
        i += 1;
    }
    Some(value)
}

/// `a == b`, usable in constants.
const fn str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}
