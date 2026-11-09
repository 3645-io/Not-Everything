//! Contract tests for the re-sort probe of the `fx-ui-macos` app
//! (`--sort-probe <out>`, `measure` builds only): the platform-agnostic
//! `fx_ui_macos::sort_probe` state machine and its report.
//!
//! Once every entry is listed (the empty query) and the list is steady, the
//! probe clicks the column titles: every sortable column twice per round,
//! so each round sorts by every column in both directions. One warm-up
//! round goes first and is not timed (the synthetic source builds each sort
//! order on first use); then `SORT_ROUNDS` (5) rounds are timed, each click
//! from the call a header click makes to the first table draw after the
//! re-sorted page is applied. The p95 is judged against `SORT_BUDGET`
//! (50 ms).
//!
//! The report is:
//!
//! ```text
//! fx-ui-macos sort probe: <n> entries
//! GATE ui re-sort to repaint p95 <= 50 ms: PASS|FAIL (p95=<x>ms p50=<y>ms n=<k>)
//! sort_p95_ms=<x>
//! sort_p50_ms=<y>
//! sort_samples=<k>
//! sort_entries=<n>
//! ```

#![cfg(feature = "measure")]

use std::collections::HashSet;
use std::time::{Duration, Instant};

use fx_bench::gate::{Verdict, parse_gate_lines};
use fx_sort::Direction;
use fx_ui_macos::columns::{Column, SortState};
use fx_ui_macos::gate_probe::{Observed, STEP_TIMEOUT};
use fx_ui_macos::latency_probe::{LIST_TIMEOUT, LatencyProbe, QUIET};
use fx_ui_macos::sort_probe::{
    SORT, SORT_BUDGET, SORT_ROUNDS, SortAction, SortRun, WARM_UP_ROUNDS, sort_clicks,
};

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn at(t0: Instant, n: u64) -> Instant {
    t0 + ms(n)
}

#[test]
fn the_budget_and_rounds_are_the_gate_s() {
    assert_eq!(SORT_BUDGET, ms(50));
    assert_eq!(SORT_ROUNDS, 5);
    assert_eq!(WARM_UP_ROUNDS, 1);
    assert_eq!(SORT.budget, SORT_BUDGET);
    assert_eq!(SORT.name, "sort");
}

#[test]
fn a_round_clicks_every_column_twice() {
    use Column::*;
    assert_eq!(
        sort_clicks(1),
        vec![Name, Name, Path, Path, Size, Size, Modified, Modified]
    );
    assert_eq!(sort_clicks(3).len(), 3 * 2 * Column::ALL.len());
    assert!(sort_clicks(0).is_empty());
}

/// From the window's starting sort (Name ascending), every click changes
/// the sort, and every round sorts by every column in both directions.
#[test]
fn every_click_re_sorts_and_each_round_covers_every_order() {
    let mut sort = SortState::default();
    let per_round = 2 * Column::ALL.len();
    for (round, clicks) in sort_clicks(SORT_ROUNDS + WARM_UP_ROUNDS)
        .chunks(per_round)
        .enumerate()
    {
        let mut seen = HashSet::new();
        for &column in clicks {
            let before = sort;
            sort.click(column);
            assert_ne!(sort, before, "round {round}: clicking {column:?}");
            seen.insert(sort);
        }
        for column in Column::ALL {
            for direction in [Direction::Ascending, Direction::Descending] {
                assert!(
                    seen.contains(&SortState { column, direction }),
                    "round {round} misses {column:?} {direction:?}"
                );
            }
        }
    }
}

#[test]
fn the_report_has_the_header_the_gate_line_and_the_metric_lines() {
    let mut p = LatencyProbe::new(SORT, 200_000);
    for i in 1..=40 {
        p.record(ms(i));
    }
    assert_eq!(
        p.report_text(),
        "fx-ui-macos sort probe: 200000 entries\n\
         GATE ui re-sort to repaint p95 <= 50 ms: PASS (p95=38.00ms p50=20.00ms n=40)\n\
         sort_p95_ms=38.00\n\
         sort_p50_ms=20.00\n\
         sort_samples=40\n\
         sort_entries=200000\n"
    );
    let lines = parse_gate_lines(&p.report_text());
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].criterion, "ui re-sort to repaint p95 <= 50 ms");
    assert_eq!(lines[0].verdict, Verdict::Pass);
}

#[test]
fn a_slow_p95_fails() {
    let mut p = LatencyProbe::new(SORT, 10);
    for i in 0..40 {
        p.record(if i < 37 { ms(5) } else { ms(51) });
    }
    assert_eq!(parse_gate_lines(&p.report_text())[0].verdict, Verdict::Fail);
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

/// All `rows` rows painted at `t0 + 100 ms`.
fn listed(t0: Instant, rows: usize) -> Observed {
    Observed {
        first_results: Some(at(t0, 100)),
        last_applied: Some(at(t0, 100)),
        last_draw: Some(at(t0, 110)),
        rows,
    }
}

#[test]
fn the_run_waits_for_a_steady_list_then_times_each_click() {
    let t0 = Instant::now();
    let mut run = SortRun::new(t0, 0, 1);
    let mut seen = listed(t0, 1000);
    let start = 100 + QUIET.as_millis() as u64;

    assert_eq!(
        run.tick(at(t0, 10), Observed::default(), false),
        SortAction::Wait
    );
    assert_eq!(run.tick(at(t0, start), seen, true), SortAction::Wait);
    assert_eq!(run.tick(at(t0, start - 1), seen, false), SortAction::Wait);
    assert_eq!(
        run.tick(at(t0, start), seen, false),
        SortAction::Click(Column::Name)
    );
    assert_eq!(run.probe().entries(), 1000);
    run.clicked(at(t0, start + 1));

    // The page is applied but not drawn yet, then drawn.
    seen.last_applied = Some(at(t0, start + 8));
    assert_eq!(run.tick(at(t0, start + 9), seen, false), SortAction::Wait);
    seen.last_draw = Some(at(t0, start + 12));
    assert_eq!(
        run.tick(at(t0, start + 13), seen, false),
        SortAction::Click(Column::Name)
    );
    assert_eq!(run.probe().samples(), &[ms(11)]);

    let mut clicks = 2;
    let mut now = start + 20;
    loop {
        run.clicked(at(t0, now));
        seen.last_applied = Some(at(t0, now + 2));
        seen.last_draw = Some(at(t0, now + 4));
        match run.tick(at(t0, now + 5), seen, false) {
            SortAction::Click(_) => clicks += 1,
            SortAction::Finish => break,
            SortAction::Wait => panic!("the click was painted"),
        }
        now += 10;
    }
    assert_eq!(clicks, 2 * Column::ALL.len());
    assert!(run.is_done());
    assert_eq!(run.probe().samples().len(), 2 * Column::ALL.len());
    assert!(run.probe().passes());
}

#[test]
fn warm_up_clicks_are_not_timed() {
    let t0 = Instant::now();
    let mut run = SortRun::new(t0, 1, 1);
    let mut seen = listed(t0, 10);
    let mut now = 100 + QUIET.as_millis() as u64;
    let mut clicks = Vec::new();
    loop {
        match run.tick(at(t0, now), seen, false) {
            SortAction::Click(column) => {
                clicks.push(column);
                run.clicked(at(t0, now));
                seen.last_applied = Some(at(t0, now + 1));
                seen.last_draw = Some(at(t0, now + 3));
            }
            SortAction::Finish => break,
            SortAction::Wait => panic!("every click was painted"),
        }
        now += 10;
    }
    assert_eq!(clicks, sort_clicks(2));
    assert_eq!(run.probe().samples().len(), sort_clicks(1).len());
}

#[test]
fn a_click_never_painted_is_a_slow_sample() {
    let t0 = Instant::now();
    let mut run = SortRun::new(t0, 0, 1);
    let seen = listed(t0, 10);
    let start = 100 + QUIET.as_millis() as u64;
    assert_eq!(
        run.tick(at(t0, start), seen, false),
        SortAction::Click(Column::Name)
    );
    run.clicked(at(t0, start));
    let late = start + STEP_TIMEOUT.as_millis() as u64;
    assert_eq!(run.tick(at(t0, late - 1), seen, false), SortAction::Wait);
    assert_eq!(
        run.tick(at(t0, late), seen, false),
        SortAction::Click(Column::Name)
    );
    assert_eq!(run.probe().samples(), &[STEP_TIMEOUT]);
}

#[test]
fn nothing_to_sort_finishes_without_samples() {
    let t0 = Instant::now();
    let mut run = SortRun::with_defaults(t0);
    let empty = listed(t0, 0);
    let start = 100 + QUIET.as_millis() as u64;
    assert_eq!(run.tick(at(t0, start), empty, false), SortAction::Finish);
    assert!(run.probe().samples().is_empty());
}

#[test]
fn a_list_still_indexing_starts_after_the_list_timeout() {
    let t0 = Instant::now();
    let mut run = SortRun::with_defaults(t0);
    let seen = listed(t0, 5);
    assert_eq!(run.tick(at(t0, 60_000), seen, true), SortAction::Wait);
    assert_eq!(
        run.tick(t0 + LIST_TIMEOUT, seen, true),
        SortAction::Click(Column::Name)
    );
}

#[test]
fn the_default_run_times_five_rounds() {
    let t0 = Instant::now();
    let mut run = SortRun::with_defaults(t0);
    let mut seen = listed(t0, 200);
    let mut now = 100 + QUIET.as_millis() as u64;
    loop {
        match run.tick(at(t0, now), seen, false) {
            SortAction::Click(_) => {
                run.clicked(at(t0, now));
                seen.last_applied = Some(at(t0, now + 1));
                seen.last_draw = Some(at(t0, now + 2));
            }
            SortAction::Finish => break,
            SortAction::Wait => {}
        }
        now += 10;
    }
    assert_eq!(
        run.probe().samples().len(),
        SORT_ROUNDS * 2 * Column::ALL.len()
    );
}
