//! The re-sort probe: what `fx-ui-macos --sort-probe <out>` measures
//! (`measure` builds only).
//!
//! With every entry listed (the window's starting empty query) and the list
//! steady ([`list_settled`]), the probe clicks the column titles as the
//! user would: every sortable column twice per round ([`sort_clicks`]), so
//! from the window's starting sort (Name ascending) every click re-sorts
//! and each round sorts by every column in both directions. Each click is
//! timed from the call a header click makes (the column title's action) to
//! the first table draw after the re-sorted page is applied, as the gate
//! probe times a keystroke. The p95 over [`SORT_ROUNDS`] rounds is judged
//! against [`SORT_BUDGET`].
//!
//! [`WARM_UP_ROUNDS`] untimed rounds go first. The synthetic source builds
//! each column's sort order the first time it is asked for, which the engine
//! never does (its orders are kept with the index), so timing that would
//! measure the synthetic rows rather than the window. Both sources sort, so
//! the probe runs with `--synthetic` as well as with `--root`.
//!
//! A click whose repaint does not come within [`STEP_TIMEOUT`] is recorded
//! as that slow. The report is a [`LatencyProbe`] of kind [`SORT`].

#![forbid(unsafe_code)]

use std::time::{Duration, Instant};

use crate::columns::Column;
use crate::gate_probe::{Observed, STEP_TIMEOUT};
use crate::latency_probe::{LIST_TIMEOUT, LatencyKind, LatencyProbe, list_settled};

/// The gate: re-sort to repaint, p95 at most 50 ms.
pub const SORT_BUDGET: Duration = Duration::from_millis(50);

/// The rounds the probe times.
pub const SORT_ROUNDS: usize = 5;

/// The untimed rounds before them.
pub const WARM_UP_ROUNDS: usize = 1;

/// The re-sort probe's kind: `sort_*` metric keys.
pub const SORT: LatencyKind = LatencyKind {
    name: "sort",
    timed: "ui re-sort to repaint",
    budget: SORT_BUDGET,
};

/// The column titles clicked in `rounds` rounds: each column of
/// [`Column::ALL`] twice per round, in order. A click on a new column sorts
/// by it ascending and the second flips it, so each round visits every
/// column in both directions and every click changes the sort.
pub fn sort_clicks(rounds: usize) -> Vec<Column> {
    (0..rounds)
        .flat_map(|_| Column::ALL.into_iter().flat_map(|c| [c, c]))
        .collect()
}

/// What the window should do on this tick of a [`SortRun`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortAction {
    /// Nothing yet: wait for the list or a repaint.
    Wait,
    /// Note the time, click this column's title (send its action), and pass
    /// the time to [`SortRun::clicked`].
    Click(Column),
    /// Write the report and quit.
    Finish,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Listing,
    Clicking { index: usize, at: Option<Instant> },
    Done,
}

/// The order of one re-sort probe run: wait for a steady list (at most
/// [`LIST_TIMEOUT`]), then click through the warm-up and the timed rounds,
/// each click waiting for its repaint.
#[derive(Clone, Debug)]
pub struct SortRun {
    probe: LatencyProbe,
    started: Instant,
    clicks: Vec<Column>,
    untimed: usize,
    phase: Phase,
}

impl SortRun {
    /// A run of `warm_up` untimed rounds and then `rounds` timed ones, its
    /// list timeout counted from `started`.
    pub fn new(started: Instant, warm_up: usize, rounds: usize) -> SortRun {
        SortRun {
            probe: LatencyProbe::new(SORT, 0),
            started,
            clicks: sort_clicks(warm_up + rounds),
            untimed: sort_clicks(warm_up).len(),
            phase: Phase::Listing,
        }
    }

    /// A run of [`WARM_UP_ROUNDS`] and [`SORT_ROUNDS`].
    pub fn with_defaults(started: Instant) -> SortRun {
        SortRun::new(started, WARM_UP_ROUNDS, SORT_ROUNDS)
    }

    /// The samples so far.
    pub fn probe(&self) -> &LatencyProbe {
        &self.probe
    }

    /// Whether the run has finished.
    pub fn is_done(&self) -> bool {
        self.phase == Phase::Done
    }

    /// The window sent the click for [`SortAction::Click`] at `at`: the
    /// sample starts there.
    pub fn clicked(&mut self, at: Instant) {
        if let Phase::Clicking { at: Some(t), .. } = &mut self.phase {
            *t = at;
        }
    }

    /// Advances the run with what the window observed at `now` and whether
    /// its source is `indexing`, and says what to do next.
    pub fn tick(&mut self, now: Instant, seen: Observed, indexing: bool) -> SortAction {
        loop {
            match self.phase {
                Phase::Listing => {
                    let waited = now.saturating_duration_since(self.started);
                    if !list_settled(now, &seen, indexing) && waited < LIST_TIMEOUT {
                        return SortAction::Wait;
                    }
                    self.probe.set_entries(seen.rows);
                    self.phase = if seen.rows == 0 {
                        // Nothing to sort: no samples, and the window exits 1.
                        Phase::Done
                    } else {
                        Phase::Clicking { index: 0, at: None }
                    };
                }
                Phase::Clicking { index, at: None } => {
                    let Some(&column) = self.clicks.get(index) else {
                        self.phase = Phase::Done;
                        continue;
                    };
                    self.phase = Phase::Clicking {
                        index,
                        at: Some(now),
                    };
                    return SortAction::Click(column);
                }
                Phase::Clicking {
                    index,
                    at: Some(at),
                } => {
                    let latency = match seen.painted_since(at) {
                        Some(drawn) => drawn.saturating_duration_since(at),
                        None if now.saturating_duration_since(at) >= STEP_TIMEOUT => {
                            now.saturating_duration_since(at)
                        }
                        None => return SortAction::Wait,
                    };
                    if index >= self.untimed {
                        self.probe.record(latency);
                    }
                    self.phase = Phase::Clicking {
                        index: index + 1,
                        at: None,
                    };
                }
                Phase::Done => return SortAction::Finish,
            }
        }
    }
}
