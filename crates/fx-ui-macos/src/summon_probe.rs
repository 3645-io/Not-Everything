//! The shortcut probe: what `fx-ui-macos --summon-probe <out>` measures
//! (`measure` builds only).
//!
//! It times the global shortcut's path to a window the user can type into,
//! [`SUMMON_CYCLES`] times, and judges the p95 against [`SUMMON_BUDGET`].
//! Each cycle, driven by [`SummonRun`]:
//!
//! 1. hides the window as Escape does, and waits until it is off screen and
//!    the app has handed the focus back ([`HIDE_TIMEOUT`]), then lets the
//!    app settle for [`SETTLE`];
//! 2. calls what the global hotkey's handler calls (the window controller's
//!    `toggle_window`, which shows the hidden window) and notes the time;
//! 3. waits until the window is ready ([`WindowState::ready`]: on screen,
//!    the key window of the active app, the search field holding the
//!    focus), then has the window run its display pass and commit it to the
//!    window server, and stops the clock when that returns.
//!
//! "Drawn" is that display pass: what AppKit would draw and commit at the
//! end of the run-loop turn, run as soon as the window is ready so its end
//! can be timed. The window server puts the committed frame on screen at
//! its next refresh, up to one frame later, which the sample leaves out.
//!
//! The sample starts at the hotkey handler, not at the key press: posting
//! real key presses to the system needs the Accessibility permission, which
//! a measurement run should not ask for. So the time from the key press to
//! the app's handler (the system's hotkey dispatch and the app's wake-up)
//! is not in the sample.
//!
//! A cycle whose window will not go off screen within [`HIDE_TIMEOUT`] is
//! skipped ([`SummonRun::skipped`]): showing then would hide the window
//! instead. A window not ready within [`READY_TIMEOUT`] is recorded as that
//! slow. The report is a [`LatencyProbe`] of kind [`SUMMON`].

#![forbid(unsafe_code)]

use std::time::{Duration, Instant};

use crate::gate_probe::Observed;
use crate::latency_probe::{LIST_TIMEOUT, LatencyKind, LatencyProbe, list_settled};

/// The gate: global shortcut to a ready window, p95 at most 100 ms.
pub const SUMMON_BUDGET: Duration = Duration::from_millis(100);

/// The hide-and-show cycles the probe times.
pub const SUMMON_CYCLES: usize = 50;

/// The shortcut probe's kind: `summon_*` metric keys.
pub const SUMMON: LatencyKind = LatencyKind {
    name: "summon",
    timed: "ui shortcut to window ready",
    budget: SUMMON_BUDGET,
};

/// How long a hidden window may take to go off screen and the app to hand
/// back the focus. An app that stays active is shown anyway once the window
/// is off screen; a window still on screen skips its cycle.
pub const HIDE_TIMEOUT: Duration = Duration::from_secs(3);

/// The pause between the hide and the show, so the show is not timed
/// against work the hide left behind.
pub const SETTLE: Duration = Duration::from_millis(100);

/// How long a shown window may take to get ready before the probe records
/// it as that slow and moves on.
pub const READY_TIMEOUT: Duration = Duration::from_secs(5);

/// The window's state as the probe sees it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowState {
    /// The window is on screen (ordered in, the app not hidden).
    pub on_screen: bool,
    /// The window is the key window.
    pub key: bool,
    /// The app is the active app.
    pub app_active: bool,
    /// The search field holds the focus (its field editor is the window's
    /// first responder).
    pub field_focused: bool,
}

impl WindowState {
    /// Whether the user could type into the search field: the window is on
    /// screen, key in the active app, and the field has the focus.
    pub fn ready(self) -> bool {
        self.on_screen && self.key && self.app_active && self.field_focused
    }
}

/// What the window reports on every [`SummonRun::tick`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SummonSeen {
    /// The results list's times and rows.
    pub results: Observed,
    /// Whether the source is still indexing.
    pub indexing: bool,
    /// The window's state.
    pub window: WindowState,
}

/// What the window should do on this tick of a [`SummonRun`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SummonAction {
    /// Nothing yet.
    Wait,
    /// Hide the window as Escape does.
    Hide,
    /// Note the time, call what the hotkey handler calls, and pass the time
    /// to [`SummonRun::shown`].
    Show,
    /// Run the window's display pass, commit it, and pass the time it
    /// returned to [`SummonRun::drawn`].
    Draw,
    /// Write the report and quit.
    Finish,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Listing,
    Hiding { asked: Option<Instant> },
    Settling { since: Instant },
    Showing { shown: Instant },
    Drawing { shown: Instant },
    Done,
}

/// The order of one shortcut probe run: wait for a steady list
/// ([`list_settled`], at most [`LIST_TIMEOUT`]), then hide-and-show cycles
/// until `cycles` have been tried.
#[derive(Clone, Debug)]
pub struct SummonRun {
    probe: LatencyProbe,
    started: Instant,
    cycles: usize,
    tried: usize,
    skipped: usize,
    phase: Phase,
}

impl SummonRun {
    /// A run of `cycles` cycles, its list timeout counted from `started`.
    pub fn new(started: Instant, cycles: usize) -> SummonRun {
        SummonRun {
            probe: LatencyProbe::new(SUMMON, 0),
            started,
            cycles,
            tried: 0,
            skipped: 0,
            phase: Phase::Listing,
        }
    }

    /// A run of [`SUMMON_CYCLES`] cycles.
    pub fn with_defaults(started: Instant) -> SummonRun {
        SummonRun::new(started, SUMMON_CYCLES)
    }

    /// The samples so far.
    pub fn probe(&self) -> &LatencyProbe {
        &self.probe
    }

    /// The cycles skipped because the window would not go off screen.
    pub fn skipped(&self) -> usize {
        self.skipped
    }

    /// Whether the run has finished.
    pub fn is_done(&self) -> bool {
        self.phase == Phase::Done
    }

    /// The window called the hotkey handler at `at`, for
    /// [`SummonAction::Show`]: the sample starts there.
    pub fn shown(&mut self, at: Instant) {
        if let Phase::Showing { shown } = &mut self.phase {
            *shown = at;
        }
    }

    /// The window's display pass returned at `at`, for
    /// [`SummonAction::Draw`]: the sample ends there.
    pub fn drawn(&mut self, at: Instant) {
        if let Phase::Drawing { shown } = self.phase {
            self.probe.record(at.saturating_duration_since(shown));
            self.next_cycle();
        }
    }

    fn next_cycle(&mut self) {
        self.tried += 1;
        self.phase = if self.tried >= self.cycles {
            Phase::Done
        } else {
            Phase::Hiding { asked: None }
        };
    }

    /// Advances the run with what the window reports at `now`, and says
    /// what to do next.
    pub fn tick(&mut self, now: Instant, seen: SummonSeen) -> SummonAction {
        loop {
            match self.phase {
                Phase::Listing => {
                    let waited = now.saturating_duration_since(self.started);
                    if !list_settled(now, &seen.results, seen.indexing) && waited < LIST_TIMEOUT {
                        return SummonAction::Wait;
                    }
                    self.probe.set_entries(seen.results.rows);
                    self.phase = if self.cycles == 0 {
                        Phase::Done
                    } else {
                        Phase::Hiding { asked: None }
                    };
                }
                Phase::Hiding { asked: None } => {
                    self.phase = Phase::Hiding { asked: Some(now) };
                    return SummonAction::Hide;
                }
                Phase::Hiding { asked: Some(asked) } => {
                    let w = seen.window;
                    let late = now.saturating_duration_since(asked) >= HIDE_TIMEOUT;
                    if !w.on_screen && (!w.app_active || late) {
                        self.phase = Phase::Settling { since: now };
                    } else if late {
                        // Still on screen: the hotkey would hide it.
                        self.skipped += 1;
                        self.next_cycle();
                    } else {
                        return SummonAction::Wait;
                    }
                }
                Phase::Settling { since } => {
                    if now.saturating_duration_since(since) < SETTLE {
                        return SummonAction::Wait;
                    }
                    self.phase = Phase::Showing { shown: now };
                    return SummonAction::Show;
                }
                Phase::Showing { shown } => {
                    if seen.window.ready() {
                        self.phase = Phase::Drawing { shown };
                        return SummonAction::Draw;
                    }
                    let waited = now.saturating_duration_since(shown);
                    if waited < READY_TIMEOUT {
                        return SummonAction::Wait;
                    }
                    self.probe.record(waited);
                    self.next_cycle();
                }
                // The window draws within the tick that asked it to; ask
                // again if it did not say so.
                Phase::Drawing { .. } => return SummonAction::Draw,
                Phase::Done => return SummonAction::Finish,
            }
        }
    }
}
