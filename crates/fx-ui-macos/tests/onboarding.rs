//! Tests for `fx_ui_macos::onboarding`: the Full Disk Access
//! onboarding state machine over `fx_fs::AccessState`.
//!
//! Contract exercised here:
//!
//! - `Onboarding::new(dismissed: bool)` where `dismissed` is the persisted
//!   `Settings::fda_onboarding_dismissed`.
//! - `Onboarding::handle(&mut self, event: OnboardingEvent, probe: &P) ->
//!   OnboardingOutput` for any `fx_fs::PermissionProbe`. The events are
//!   `Launch`, `BecameActive`, `GrantClicked`, `Dismissed` and
//!   `RecheckTick`. `Launch`, `BecameActive` and `RecheckTick` (while the
//!   banner shows) re-probe `full_access()`; `GrantClicked` calls
//!   `open_settings()`.
//! - `OnboardingOutput { access, show_banner, limited_mode, rescan_excluded,
//!   recheck_after: Option<Duration> }` (public fields). `limited_mode` is
//!   true whenever access is not `Granted` (the non-FDA scan that skips
//!   protected folders, so no consent prompt appears). `rescan_excluded`
//!   asks the app to switch to the full index: it is true once per
//!   session, on the first output that sees access granted after a launch
//!   in limited mode, and never after a launch with access granted.
//!   `recheck_after` is `Some(d)` with `0 < d <= 1 s` while the banner is
//!   shown, and `None` once access is granted.
//! - Getters `access()`, `show_banner()`, `limited_mode()`, `dismissed()`
//!   and `status_suffix() -> Option<&str>` (for example "Limited mode:
//!   protected folders not indexed"; `None` when granted).
//! - `scope_status(IndexScope, &Onboarding)`: the status-bar text about the
//!   index the window searches. `IndexScope::Upgrading` (the full index is
//!   being built beside the limited one) shows "Indexing protected
//!   folders…"; nothing ever asks the user to quit and reopen.

use std::cell::Cell;
use std::time::Duration;

use fx_fs::{AccessState, PermissionProbe};
use fx_ui_macos::onboarding::{
    IndexScope, LIMITED_MODE_SUFFIX, Onboarding, OnboardingEvent, OnboardingOutput, scope_status,
};

/// A probe whose answer the test flips, counting calls.
struct FakeProbe {
    state: Cell<AccessState>,
    probes: Cell<usize>,
    settings_opened: Cell<usize>,
}

impl FakeProbe {
    fn new(state: AccessState) -> FakeProbe {
        FakeProbe {
            state: Cell::new(state),
            probes: Cell::new(0),
            settings_opened: Cell::new(0),
        }
    }

    fn set(&self, state: AccessState) {
        self.state.set(state);
    }
}

impl PermissionProbe for FakeProbe {
    fn full_access(&self) -> AccessState {
        self.probes.set(self.probes.get() + 1);
        self.state.get()
    }

    fn open_settings(&self) {
        self.settings_opened.set(self.settings_opened.get() + 1);
    }
}

const MAX_RECHECK: Duration = Duration::from_secs(1);

fn launched(state: AccessState, dismissed: bool) -> (Onboarding, FakeProbe, OnboardingOutput) {
    let probe = FakeProbe::new(state);
    let mut ob = Onboarding::new(dismissed);
    let out = ob.handle(OnboardingEvent::Launch, &probe);
    (ob, probe, out)
}

fn assert_fast_recheck(out: &OnboardingOutput) {
    let d = out
        .recheck_after
        .expect("a recheck is scheduled while the banner shows");
    assert!(d > Duration::ZERO, "recheck interval must be positive");
    assert!(d <= MAX_RECHECK, "recheck every <= 1 s, got {d:?}");
}

#[test]
fn launch_granted_runs_full_scan_without_banner() {
    let (ob, probe, out) = launched(AccessState::Granted, false);
    assert!(probe.probes.get() >= 1, "launch probes access");
    assert_eq!(out.access, AccessState::Granted);
    assert!(!out.show_banner);
    assert!(!out.limited_mode);
    assert!(!out.rescan_excluded, "the first scan is already full");
    assert_eq!(out.recheck_after, None);
    assert_eq!(ob.access(), AccessState::Granted);
    assert!(!ob.show_banner());
    assert!(!ob.limited_mode());
    assert_eq!(ob.status_suffix(), None);
}

#[test]
fn launch_denied_shows_banner_and_limited_mode() {
    let (ob, _probe, out) = launched(AccessState::Denied, false);
    assert_eq!(out.access, AccessState::Denied);
    assert!(out.show_banner);
    assert!(out.limited_mode);
    assert!(!out.rescan_excluded);
    assert_fast_recheck(&out);
    assert!(ob.show_banner());
    assert!(ob.limited_mode());
    assert!(!ob.dismissed());
}

#[test]
fn launch_unknown_is_treated_as_limited() {
    let (ob, _probe, out) = launched(AccessState::Unknown, false);
    assert!(out.limited_mode, "never assume access we could not confirm");
    assert!(out.show_banner);
    assert!(!out.rescan_excluded);
    assert!(ob.status_suffix().is_some());
}

#[test]
fn status_suffix_in_limited_mode() {
    let (ob, _probe, _out) = launched(AccessState::Denied, false);
    let suffix = ob
        .status_suffix()
        .expect("limited mode has a status suffix");
    assert!(suffix.starts_with("Limited mode"), "{suffix}");
    assert!(suffix.to_lowercase().contains("not indexed"), "{suffix}");
}

#[test]
fn activation_reprobes_every_time() {
    let (mut ob, probe, _out) = launched(AccessState::Denied, false);
    for _ in 0..3 {
        let before = probe.probes.get();
        ob.handle(OnboardingEvent::BecameActive, &probe);
        assert!(probe.probes.get() > before, "each activation re-probes");
    }
}

#[test]
fn grant_detected_on_activation() {
    let (mut ob, probe, _out) = launched(AccessState::Denied, false);
    // The user grants access in System Settings, then switches back.
    probe.set(AccessState::Granted);
    let out = ob.handle(OnboardingEvent::BecameActive, &probe);
    assert_eq!(out.access, AccessState::Granted);
    assert!(!out.show_banner, "banner clears on activation");
    assert!(!out.limited_mode);
    assert!(
        out.rescan_excluded,
        "Denied->Granted rescans excluded folders"
    );
    assert_eq!(out.recheck_after, None, "no more rechecks once granted");
    assert_eq!(ob.status_suffix(), None);
}

#[test]
fn grant_detected_by_recheck_within_two_seconds() {
    let (mut ob, probe, out) = launched(AccessState::Denied, false);
    let activated = ob.handle(OnboardingEvent::BecameActive, &probe);
    assert_fast_recheck(&out);
    assert_fast_recheck(&activated);

    // Simulated clock: ticks fire at the scheduled interval while the
    // banner shows. A tick that sees Denied keeps rechecking.
    let mut elapsed = Duration::ZERO;
    let mut next = activated.recheck_after.unwrap();
    let tick = ob.handle(OnboardingEvent::RecheckTick, &probe);
    assert!(tick.show_banner);
    assert!(!tick.rescan_excluded);
    assert_fast_recheck(&tick);
    elapsed += next;
    next = tick.recheck_after.unwrap();

    // Access is granted while the app is active; the next tick sees it.
    probe.set(AccessState::Granted);
    let before = probe.probes.get();
    let tick = ob.handle(OnboardingEvent::RecheckTick, &probe);
    elapsed += next;
    assert!(
        probe.probes.get() > before,
        "a tick with the banner re-probes"
    );
    assert_eq!(tick.access, AccessState::Granted);
    assert!(!tick.show_banner);
    assert!(!tick.limited_mode);
    assert!(tick.rescan_excluded);
    assert_eq!(tick.recheck_after, None);
    assert!(elapsed < Duration::from_secs(2), "detected in {elapsed:?}");
}

#[test]
fn rescan_is_requested_only_once() {
    let (mut ob, probe, _out) = launched(AccessState::Denied, false);
    probe.set(AccessState::Granted);
    let first = ob.handle(OnboardingEvent::BecameActive, &probe);
    assert!(first.rescan_excluded);
    let again = ob.handle(OnboardingEvent::BecameActive, &probe);
    assert!(!again.rescan_excluded);
    let tick = ob.handle(OnboardingEvent::RecheckTick, &probe);
    assert!(!tick.rescan_excluded);
}

#[test]
fn grant_clicked_opens_settings_and_keeps_rechecking() {
    let (mut ob, probe, _out) = launched(AccessState::Denied, false);
    let out = ob.handle(OnboardingEvent::GrantClicked, &probe);
    assert_eq!(probe.settings_opened.get(), 1);
    assert!(out.limited_mode);
    assert!(!out.rescan_excluded);
    assert!(out.show_banner);
    assert_fast_recheck(&out);
}

#[test]
fn dismiss_hides_banner_but_stays_limited() {
    let (mut ob, probe, _out) = launched(AccessState::Denied, false);
    let out = ob.handle(OnboardingEvent::Dismissed, &probe);
    assert!(!out.show_banner);
    assert!(out.limited_mode);
    assert_eq!(out.recheck_after, None, "no rechecks without the banner");
    assert!(ob.dismissed());
    assert!(
        ob.status_suffix().is_some(),
        "status bar still says limited"
    );

    let out = ob.handle(OnboardingEvent::BecameActive, &probe);
    assert!(!out.show_banner, "dismissal survives activation");
    assert!(out.limited_mode);
}

#[test]
fn persisted_dismissal_suppresses_banner_at_launch() {
    let (ob, _probe, out) = launched(AccessState::Denied, true);
    assert!(!out.show_banner);
    assert!(out.limited_mode);
    assert!(ob.dismissed());
    assert!(ob.status_suffix().is_some());
}

#[test]
fn grant_after_dismissal_still_triggers_rescan() {
    let (mut ob, probe, _out) = launched(AccessState::Denied, true);
    probe.set(AccessState::Granted);
    let out = ob.handle(OnboardingEvent::BecameActive, &probe);
    assert!(out.rescan_excluded);
    assert!(!out.limited_mode);
    assert!(!out.show_banner);
    assert_eq!(ob.status_suffix(), None);
}

#[test]
fn revoked_access_returns_to_limited_mode() {
    let (mut ob, probe, _out) = launched(AccessState::Granted, false);
    probe.set(AccessState::Denied);
    let out = ob.handle(OnboardingEvent::BecameActive, &probe);
    assert_eq!(out.access, AccessState::Denied);
    assert!(out.limited_mode);
    assert!(out.show_banner);
    assert!(!out.rescan_excluded);
    assert_fast_recheck(&out);
}

// ------------------------------------------------- switch to the full index

/// Launches with `launch`, then activates once per state in `then` and
/// returns each activation's `rescan_excluded`.
fn rescans_after(launch: AccessState, then: &[AccessState]) -> Vec<bool> {
    let (mut ob, probe, out) = launched(launch, false);
    assert!(!out.rescan_excluded, "the launch never asks for the switch");
    then.iter()
        .map(|&state| {
            probe.set(state);
            ob.handle(OnboardingEvent::BecameActive, &probe)
                .rescan_excluded
        })
        .collect()
}

#[test]
fn a_grant_after_a_limited_launch_asks_for_the_switch_exactly_once() {
    use AccessState::{Denied, Granted};
    // Granted, revoked and granted again: the full index already runs (or
    // is being built), so the second grant must not open another one.
    assert_eq!(
        rescans_after(Denied, &[Denied, Granted, Granted, Denied, Granted]),
        [false, true, false, false, false]
    );
    // Unknown counts as limited too.
    assert_eq!(
        rescans_after(AccessState::Unknown, &[Granted, Granted]),
        [true, false]
    );

    // Ticks and dismissals after the grant never ask again either.
    let (mut ob, probe, _out) = launched(Denied, false);
    probe.set(Granted);
    assert!(
        ob.handle(OnboardingEvent::RecheckTick, &probe)
            .rescan_excluded
    );
    for event in [
        OnboardingEvent::RecheckTick,
        OnboardingEvent::BecameActive,
        OnboardingEvent::Dismissed,
        OnboardingEvent::GrantClicked,
    ] {
        assert!(!ob.handle(event, &probe).rescan_excluded, "{event:?}");
    }
}

#[test]
fn a_launch_with_access_never_asks_for_the_switch() {
    use AccessState::{Denied, Granted};
    // The index was built with access: revoking and granting again keeps it.
    assert_eq!(
        rescans_after(Granted, &[Denied, Granted, Denied, Granted]),
        [false, false, false, false]
    );
}

#[test]
fn scope_status_follows_the_index_the_window_searches() {
    use AccessState::{Denied, Granted};
    let (mut ob, probe, _out) = launched(Denied, false);
    assert_eq!(
        scope_status(IndexScope::Limited, &ob),
        Some(LIMITED_MODE_SUFFIX)
    );

    probe.set(Granted);
    assert!(
        ob.handle(OnboardingEvent::BecameActive, &probe)
            .rescan_excluded
    );
    let building = scope_status(IndexScope::Upgrading, &ob).expect("a status while building");
    assert_eq!(building, "Indexing protected folders\u{2026}");
    assert_eq!(
        scope_status(IndexScope::Full, &ob),
        None,
        "nothing to say once the full index is searched"
    );

    // Access revoked after the switch: limited again.
    probe.set(Denied);
    ob.handle(OnboardingEvent::BecameActive, &probe);
    assert_eq!(
        scope_status(IndexScope::Full, &ob),
        Some(LIMITED_MODE_SUFFIX)
    );
}

#[test]
fn no_status_asks_to_quit_and_reopen() {
    for launch in [AccessState::Denied, AccessState::Granted] {
        for now in [AccessState::Denied, AccessState::Granted] {
            let (mut ob, probe, _out) = launched(launch, false);
            probe.set(now);
            ob.handle(OnboardingEvent::BecameActive, &probe);
            for scope in [IndexScope::Limited, IndexScope::Upgrading, IndexScope::Full] {
                if let Some(text) = scope_status(scope, &ob) {
                    let text = text.to_lowercase();
                    assert!(
                        !text.contains("quit") && !text.contains("reopen"),
                        "{scope:?} after {launch:?} then {now:?}: {text}"
                    );
                }
            }
        }
    }
}
