//! What the app shows of its build: the window title, the first line of
//! the menu-bar menu and the About panel's version. Release candidates and
//! betas are a separate app, Not-Everything Preview, so each build says
//! which it is.
//!
//! - The window title is the app's name, "Not-Everything", when the release
//!   app indexes the whole disk; a `--root` folder, synthetic rows and the
//!   measurement modes add " — `<what it shows>`". A release candidate or
//!   beta puts its label after the name: "Not-Everything Preview
//!   0.2.0-RC-1".
//! - The menu-bar menu starts with "`<app name> <version or label>`"
//!   ([`BuildInfo::name_and_label`]), greyed out, in every channel.
//! - The standard About panel shows "Version `<label> (<build number>)`"
//!   ([`BuildInfo::about`]).

#![forbid(unsafe_code)]

use std::path::Path;

use fx_channel::Channel;

use crate::app_args::DEFAULT_ROOT;
use crate::theme::Token;

/// A build's channel, release candidate or beta number and version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BuildInfo {
    /// Release, release candidate or beta.
    pub channel: Channel,
    /// The release candidate's or beta's number N (from 1); 0 for a release.
    pub number: u32,
    /// The version, `X.Y.Z`.
    pub version: &'static str,
}

impl BuildInfo {
    /// This build: [`Channel::CURRENT`], [`fx_channel::CURRENT_NUMBER`] and
    /// the package version.
    pub const CURRENT: BuildInfo = BuildInfo {
        channel: Channel::CURRENT,
        number: fx_channel::CURRENT_NUMBER,
        version: env!("CARGO_PKG_VERSION"),
    };

    /// The app's name: Not-Everything, or Not-Everything Preview for a
    /// release candidate or beta.
    pub const fn app_name(self) -> &'static str {
        self.channel.app_name()
    }

    /// The version as the build shows it: `0.2.0`, `0.2.0-RC-1` or
    /// `0.2.0-beta-1` ([`fx_channel::version_label`]).
    pub fn label(self) -> String {
        fx_channel::version_label(self.version, self.channel, self.number)
    }

    /// The app's name and label, "Not-Everything 0.2.0" or "Not-Everything
    /// Preview 0.2.0-RC-1": the first line of the menu-bar menu.
    pub fn name_and_label(self) -> String {
        format!("{} {}", self.app_name(), self.label())
    }

    /// The tag after the app's name in the top bar (the canvas's
    /// ChannelTag): the label of a release candidate (green) or beta (the
    /// dark neutral, `vol`), as on the Dock ribbon; `None` for a release.
    pub fn channel_tag(self) -> Option<ChannelTag> {
        let (fill, text) = match self.channel {
            Channel::Release => return None,
            Channel::Rc => (Token::Green, Token::OnGreen),
            Channel::Beta => (Token::Vol, Token::OnVol),
        };
        Some(ChannelTag {
            label: self.label(),
            fill,
            text,
        })
    }

    /// The standard About panel's version strings for this build.
    pub fn about(self) -> AboutVersion {
        AboutVersion {
            application_version: self.label(),
            build: fx_channel::build_number(self.version)
                .map(|n| n.to_string())
                .unwrap_or_default(),
        }
    }
}

/// A Preview build's tag in the top bar, from [`BuildInfo::channel_tag`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelTag {
    /// The build's label, `0.2.0-RC-1` or `0.2.0-beta-2`; the tag sets it
    /// in capitals (`theme::styles::CHANNEL_TAG`).
    pub label: String,
    /// The tag's fill.
    pub fill: Token,
    /// The label's colour.
    pub text: Token,
}

/// The version strings passed to the standard About panel, which shows
/// them as "Version `<application version> (<build>)`".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AboutVersion {
    /// `NSAboutPanelOptionApplicationVersion`: the label, `0.2.0-RC-1`.
    pub application_version: String,
    /// `NSAboutPanelOptionVersion`: the build number, `200`; empty (the
    /// panel leaves it out) when the version can't be numbered.
    pub build: String,
}

impl AboutVersion {
    /// The line as the About panel shows it: "Version 0.2.0-RC-1 (200)".
    pub fn display(&self) -> String {
        if self.build.is_empty() {
            format!("Version {}", self.application_version)
        } else {
            format!("Version {} ({})", self.application_version, self.build)
        }
    }
}

/// What the search window shows results from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Showing<'a> {
    /// The index over this (resolved) root.
    Root(&'a Path),
    /// This many synthetic rows (`--synthetic`).
    Synthetic(usize),
}

/// The search window's title. The app's name ([`BuildInfo::name_and_label`]
/// for a release candidate or beta), then " — " and what the window shows,
/// except for the release app's everyday window over the whole disk
/// ([`DEFAULT_ROOT`], not `measuring`), which reads just the name.
pub fn window_title(build: BuildInfo, showing: Showing<'_>, measuring: bool) -> String {
    let name = if build.channel.is_preview() {
        build.name_and_label()
    } else {
        build.app_name().to_owned()
    };
    let detail = match showing {
        Showing::Synthetic(rows) => Some(format!("{rows} synthetic rows")),
        Showing::Root(root) if root == Path::new(DEFAULT_ROOT) && !measuring => None,
        Showing::Root(root) => Some(root.display().to_string()),
    };
    match detail {
        Some(detail) => format!("{name} \u{2014} {detail}"),
        None => name,
    }
}
