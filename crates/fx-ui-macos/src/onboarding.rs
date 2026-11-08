//! Full Disk Access onboarding: a synchronous state machine over
//! [`fx_fs::AccessState`], independent of AppKit.
//!
//! The AppKit layer feeds it [`OnboardingEvent`]s and a
//! [`PermissionProbe`] (the FDA probe in `fx-fs-macos`, a fake in tests) and
//! applies the [`OnboardingOutput`]: show or hide the banner, scan in limited
//! (non-FDA) mode, switch to the full index once access is granted, and
//! schedule the next recheck.
//!
//! - [`OnboardingEvent::Launch`] and [`OnboardingEvent::BecameActive`]
//!   always re-probe, so a grant made in System Settings is seen as soon as
//!   the user switches back.
//! - While the banner shows, every output asks for a
//!   [`OnboardingEvent::RecheckTick`] after [`RECHECK_INTERVAL`] (≤ 1 s),
//!   and each tick re-probes, so a grant made while the app is active is
//!   seen within 2 s. The AppKit layer runs the timer only while the app is
//!   active; activation re-probes anyway.
//! - Access that is not [`AccessState::Granted`] (including `Unknown`) means
//!   limited mode: the scan skips the protected folders so no consent
//!   prompt appears, and the status bar shows [`LIMITED_MODE_SUFFIX`].
//! - When access is first seen `Granted` after a launch in limited mode, the
//!   output asks the app to index the protected folders
//!   ([`OnboardingOutput::rescan_excluded`]) exactly once per session. The
//!   app then opens the full index beside the limited one and switches to
//!   it without a relaunch; [`scope_status`] says so meanwhile.
//!   A launch with access granted never asks: its index is already full.
//! - Dismissing hides the banner (and stops the rechecks) but keeps limited
//!   mode; persisting the dismissal is the caller's job
//!   (`Settings::fda_onboarding_dismissed`).

#![forbid(unsafe_code)]

use std::time::Duration;

use fx_fs::{AccessState, PermissionProbe};

/// How often access is re-probed while the banner shows.
pub const RECHECK_INTERVAL: Duration = Duration::from_millis(750);

/// Status-bar suffix shown in limited (non-FDA) mode.
pub const LIMITED_MODE_SUFFIX: &str = "Limited mode: protected folders not indexed";

/// Status-bar suffix shown while the full index is built (or loaded)
/// beside the limited one after access was granted (see [`scope_status`]).
pub const INDEXING_PROTECTED_SUFFIX: &str = "Indexing protected folders\u{2026}";

/// The scope of the index the window searches, for the status bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IndexScope {
    /// Built in limited (non-FDA) mode: the protected folders are missing.
    Limited,
    /// Still the limited index, while the full one is built (or loaded)
    /// beside it; the window switches to it once it has its first snapshot.
    Upgrading,
    /// Built with Full Disk Access.
    Full,
}

/// Something that happened, as far as onboarding cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OnboardingEvent {
    /// The app launched (before the first scan).
    Launch,
    /// The app became active (for example the user switched back from
    /// System Settings).
    BecameActive,
    /// The user clicked "Grant Full Disk Access…".
    GrantClicked,
    /// The user dismissed the banner.
    Dismissed,
    /// The recheck timer scheduled by [`OnboardingOutput::recheck_after`]
    /// fired.
    RecheckTick,
}

/// What the UI should do after an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OnboardingOutput {
    /// The access state as last probed.
    pub access: AccessState,
    /// Whether the onboarding banner is shown.
    pub show_banner: bool,
    /// Whether the scan runs in limited (non-FDA) mode.
    pub limited_mode: bool,
    /// Whether the app must now index the protected folders its launch
    /// skipped: set once per session, on the first output that sees access
    /// granted after a launch in limited mode. The app opens the full index
    /// and switches to it.
    pub rescan_excluded: bool,
    /// When to send the next [`OnboardingEvent::RecheckTick`]; `None` when
    /// no recheck is needed.
    pub recheck_after: Option<Duration>,
}

/// The onboarding state machine.
#[derive(Clone, Debug)]
pub struct Onboarding {
    access: AccessState,
    probed: bool,
    dismissed: bool,
    /// The launch built a limited index and the switch to the full one has
    /// not been asked for yet.
    index_limited: bool,
}

impl Onboarding {
    /// A new state machine; `dismissed` is the persisted
    /// `Settings::fda_onboarding_dismissed`. Access counts as `Unknown`
    /// (limited) until the first probe.
    pub fn new(dismissed: bool) -> Onboarding {
        Onboarding {
            access: AccessState::Unknown,
            probed: false,
            dismissed,
            index_limited: false,
        }
    }

    /// Handles one event, probing or opening settings through `probe` as
    /// needed, and returns what the UI should do.
    pub fn handle<P: PermissionProbe + ?Sized>(
        &mut self,
        event: OnboardingEvent,
        probe: &P,
    ) -> OnboardingOutput {
        let mut rescan = false;
        match event {
            OnboardingEvent::Launch => {
                let first = !self.probed;
                rescan = self.reprobe(probe);
                if first {
                    // The app builds its index in the scope this probe gives.
                    self.index_limited = self.limited_mode();
                }
            }
            OnboardingEvent::BecameActive => {
                rescan = self.reprobe(probe);
            }
            OnboardingEvent::RecheckTick => {
                if self.show_banner() {
                    rescan = self.reprobe(probe);
                }
            }
            OnboardingEvent::GrantClicked => probe.open_settings(),
            OnboardingEvent::Dismissed => self.dismissed = true,
        }
        self.output(rescan)
    }

    /// Probes access; returns whether the app must now switch to the full
    /// index: access is granted, the launch built a limited index, and the
    /// switch was not asked for before.
    fn reprobe<P: PermissionProbe + ?Sized>(&mut self, probe: &P) -> bool {
        self.access = probe.full_access();
        self.probed = true;
        let rescan = self.index_limited && self.access == AccessState::Granted;
        if rescan {
            self.index_limited = false;
        }
        rescan
    }

    fn output(&self, rescan_excluded: bool) -> OnboardingOutput {
        OnboardingOutput {
            access: self.access,
            show_banner: self.show_banner(),
            limited_mode: self.limited_mode(),
            rescan_excluded,
            recheck_after: self.show_banner().then_some(RECHECK_INTERVAL),
        }
    }

    /// The access state as last probed (`Unknown` before the first probe).
    pub fn access(&self) -> AccessState {
        self.access
    }

    /// Whether the banner is shown: access is not granted and the user has
    /// not dismissed it.
    pub fn show_banner(&self) -> bool {
        self.limited_mode() && !self.dismissed
    }

    /// Whether the scan runs in limited (non-FDA) mode.
    pub fn limited_mode(&self) -> bool {
        self.access != AccessState::Granted
    }

    /// Whether the user dismissed the banner (now or in an earlier session).
    pub fn dismissed(&self) -> bool {
        self.dismissed
    }

    /// The status-bar suffix in limited mode ([`LIMITED_MODE_SUFFIX`]);
    /// `None` when access is granted.
    pub fn status_suffix(&self) -> Option<&str> {
        self.limited_mode().then_some(LIMITED_MODE_SUFFIX)
    }
}

/// The status-bar text about the scope of the index the window searches
/// (`index`), given the onboarding state now.
///
/// - [`IndexScope::Upgrading`]: [`INDEXING_PROTECTED_SUFFIX`] while access
///   stays granted.
/// - Without access, whatever the index: [`LIMITED_MODE_SUFFIX`] (access may
///   have been revoked after a full index was built).
/// - A limited index with access granted: [`LIMITED_MODE_SUFFIX`] too; it
///   lasts only until the app starts the full index, or after that failed.
/// - [`IndexScope::Full`] with access: nothing.
///
/// The app never asks the user to quit and reopen: it switches to the full
/// index by itself.
pub fn scope_status(index: IndexScope, onboarding: &Onboarding) -> Option<&'static str> {
    ScopeNote::of(index, onboarding.limited_mode()).map(ScopeNote::text)
}

/// What the status bar says about the scope of the index (a note in its
/// middle slot), by the rule of [`scope_status`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScopeNote {
    /// [`LIMITED_MODE_SUFFIX`].
    LimitedMode,
    /// [`INDEXING_PROTECTED_SUFFIX`].
    IndexingProtected,
}

impl ScopeNote {
    /// The note for the index the window searches (`index`) while the
    /// onboarding is, or isn't, in `limited_mode`.
    pub fn of(index: IndexScope, limited_mode: bool) -> Option<ScopeNote> {
        match (index, limited_mode) {
            (_, true) | (IndexScope::Limited, false) => Some(ScopeNote::LimitedMode),
            (IndexScope::Upgrading, false) => Some(ScopeNote::IndexingProtected),
            (IndexScope::Full, false) => None,
        }
    }

    /// The note's text.
    pub const fn text(self) -> &'static str {
        match self {
            ScopeNote::LimitedMode => LIMITED_MODE_SUFFIX,
            ScopeNote::IndexingProtected => INDEXING_PROTECTED_SUFFIX,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    struct Fake(Cell<AccessState>);

    impl PermissionProbe for Fake {
        fn full_access(&self) -> AccessState {
            self.0.get()
        }
        fn open_settings(&self) {}
    }

    #[test]
    fn a_grant_after_a_limited_launch_shows_the_full_index_being_built() {
        let probe = Fake(Cell::new(AccessState::Denied));
        let mut ob = Onboarding::new(false);
        let launch = ob.handle(OnboardingEvent::Launch, &probe);
        assert!(launch.limited_mode);
        assert_eq!(
            scope_status(IndexScope::Limited, &ob),
            Some(LIMITED_MODE_SUFFIX)
        );

        probe.0.set(AccessState::Granted);
        let out = ob.handle(OnboardingEvent::BecameActive, &probe);
        assert!(out.rescan_excluded, "the grant asks for the full index");
        assert_eq!(
            scope_status(IndexScope::Upgrading, &ob),
            Some(INDEXING_PROTECTED_SUFFIX)
        );
        assert_eq!(scope_status(IndexScope::Full, &ob), None);

        // Later events never ask again.
        for event in [
            OnboardingEvent::BecameActive,
            OnboardingEvent::RecheckTick,
            OnboardingEvent::Dismissed,
        ] {
            assert!(!ob.handle(event, &probe).rescan_excluded, "{event:?}");
        }
    }

    #[test]
    fn full_scan_has_no_scope_status_while_granted() {
        let probe = Fake(Cell::new(AccessState::Granted));
        let mut ob = Onboarding::new(false);
        let launch = ob.handle(OnboardingEvent::Launch, &probe);
        assert!(!launch.limited_mode);
        assert_eq!(scope_status(IndexScope::Full, &ob), None);
        probe.0.set(AccessState::Denied);
        ob.handle(OnboardingEvent::BecameActive, &probe);
        assert_eq!(
            scope_status(IndexScope::Full, &ob),
            Some(LIMITED_MODE_SUFFIX)
        );
        probe.0.set(AccessState::Granted);
        let out = ob.handle(OnboardingEvent::BecameActive, &probe);
        assert!(!out.rescan_excluded, "the index is already full");
    }
}
