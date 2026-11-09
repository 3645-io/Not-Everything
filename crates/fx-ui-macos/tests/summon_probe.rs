//! Contract tests for the shortcut probe of the `fx-ui-macos` app
//! (`--summon-probe <out>`, `measure` builds only): the platform-agnostic
//! `fx_ui_macos::summon_probe` state machine and the report of
//! `fx_ui_macos::latency_probe` it writes.
//!
//! Once the window lists its results steadily, the probe hides the window
//! the way Escape does, waits until it is off screen and the app is no
//! longer active, then calls what the global hotkey's handler calls and
//! times until the window is ready: on screen, key in the active app, the
//! search field holding the focus, and its display pass drawn and committed.
//! It repeats that `SUMMON_CYCLES` (50) times and judges the p95 against
//! `SUMMON_BUDGET` (100 ms).
//!
//! The report is:
//!
//! ```text
//! fx-ui-macos summon probe: <n> entries
//! GATE ui shortcut to window ready p95 <= 100 ms: PASS|FAIL (p95=<x>ms p50=<y>ms n=<k>)
//! summon_p95_ms=<x>
//! summon_p50_ms=<y>
//! summon_samples=<k>
//! summon_entries=<n>
//! ```
//!
//! Without samples the GATE line is a FAIL and the p95 and p50 lines are
//! left out. The report goes to a new file, never over an existing one.

#![cfg(feature = "measure")]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use fx_bench::gate::{Verdict, parse_gate_lines};
use fx_ui_macos::gate_probe::Observed;
use fx_ui_macos::latency_probe::{LIST_TIMEOUT, LatencyProbe, QUIET};
use fx_ui_macos::summon_probe::{
    HIDE_TIMEOUT, READY_TIMEOUT, SETTLE, SUMMON, SUMMON_BUDGET, SUMMON_CYCLES, SummonAction,
    SummonRun, SummonSeen, WindowState,
};

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn at(t0: Instant, n: u64) -> Instant {
    t0 + ms(n)
}

/// A probe with 50 samples of 1 to 50 ms over 200,000 entries.
fn fifty_samples() -> LatencyProbe {
    let mut p = LatencyProbe::new(SUMMON, 200_000);
    for i in (1..=50).rev() {
        p.record(ms(i));
    }
    p
}

// ---------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------

#[test]
fn the_budget_and_cycles_are_the_gate_s() {
    assert_eq!(SUMMON_BUDGET, ms(100));
    assert_eq!(SUMMON_CYCLES, 50);
    assert_eq!(SUMMON.budget, SUMMON_BUDGET);
    assert_eq!(SUMMON.name, "summon");
}

#[test]
fn the_report_has_the_header_the_gate_line_and_the_metric_lines() {
    assert_eq!(
        fifty_samples().report_text(),
        "fx-ui-macos summon probe: 200000 entries\n\
         GATE ui shortcut to window ready p95 <= 100 ms: PASS (p95=48.00ms p50=25.00ms n=50)\n\
         summon_p95_ms=48.00\n\
         summon_p50_ms=25.00\n\
         summon_samples=50\n\
         summon_entries=200000\n"
    );
}

#[test]
fn the_gate_line_parses_as_one_criterion() {
    let lines = parse_gate_lines(&fifty_samples().report_text());
    assert_eq!(lines.len(), 1);
    assert_eq!(
        lines[0].criterion,
        "ui shortcut to window ready p95 <= 100 ms"
    );
    assert_eq!(lines[0].verdict, Verdict::Pass);
    assert_eq!(
        lines[0].detail.as_deref(),
        Some("p95=48.00ms p50=25.00ms n=50")
    );
}

#[test]
fn the_gate_judges_the_p95_and_a_value_at_the_budget_passes() {
    let mut edge = LatencyProbe::new(SUMMON, 10);
    edge.record(SUMMON_BUDGET);
    assert!(edge.passes());
    let mut slow = LatencyProbe::new(SUMMON, 10);
    for i in 0..100 {
        slow.record(if i < 94 { ms(10) } else { ms(101) });
    }
    assert!(!slow.passes());
    let line = &parse_gate_lines(&slow.report_text())[0];
    assert_eq!(line.verdict, Verdict::Fail);
}

#[test]
fn no_samples_is_a_fail_line_without_made_up_numbers() {
    let p = LatencyProbe::new(SUMMON, 7);
    assert!(!p.passes());
    assert_eq!(
        p.report_text(),
        "fx-ui-macos summon probe: 7 entries\n\
         GATE ui shortcut to window ready p95 <= 100 ms: FAIL (no samples)\n\
         summon_samples=0\n\
         summon_entries=7\n"
    );
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "fx-ui-summon-probe-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[test]
fn the_report_goes_to_a_new_file_only() {
    let dir = temp_dir("write");
    let path = dir.join("summon.txt");
    let p = fifty_samples();
    p.write_report(&path).expect("write a new report");
    let written = std::fs::read_to_string(&path).expect("read it back");
    let again = p.write_report(&path);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
    assert_eq!(written, p.report_text());
    assert_eq!(
        again.expect_err("never replaced").kind(),
        std::io::ErrorKind::AlreadyExists
    );
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

/// Results painted at `t0 + 100 ms`, `rows` rows, not indexing.
fn listed(t0: Instant, rows: usize) -> Observed {
    Observed {
        first_results: Some(at(t0, 100)),
        last_applied: Some(at(t0, 100)),
        last_draw: Some(at(t0, 110)),
        rows,
    }
}

fn seen(results: Observed, window: WindowState) -> SummonSeen {
    SummonSeen {
        results,
        indexing: false,
        window,
    }
}

const SHOWN_READY: WindowState = WindowState {
    on_screen: true,
    key: true,
    app_active: true,
    field_focused: true,
};

const HIDDEN: WindowState = WindowState {
    on_screen: false,
    key: false,
    app_active: false,
    field_focused: true,
};

#[test]
fn the_window_is_ready_only_when_all_four_hold() {
    assert!(SHOWN_READY.ready());
    for not in [
        WindowState {
            on_screen: false,
            ..SHOWN_READY
        },
        WindowState {
            key: false,
            ..SHOWN_READY
        },
        WindowState {
            app_active: false,
            ..SHOWN_READY
        },
        WindowState {
            field_focused: false,
            ..SHOWN_READY
        },
    ] {
        assert!(!not.ready(), "{not:?}");
    }
}

#[test]
fn a_cycle_hides_waits_shows_and_times_to_the_drawn_window() {
    let t0 = Instant::now();
    let mut run = SummonRun::new(t0, 2);
    let rows = listed(t0, 500);

    // Nothing listed yet, then not quiet for long enough.
    assert_eq!(
        run.tick(at(t0, 50), seen(Observed::default(), SHOWN_READY)),
        SummonAction::Wait
    );
    let quiet = 100 + QUIET.as_millis() as u64;
    assert_eq!(
        run.tick(at(t0, quiet - 1), seen(rows, SHOWN_READY)),
        SummonAction::Wait
    );
    assert_eq!(
        run.tick(at(t0, quiet), seen(rows, SHOWN_READY)),
        SummonAction::Hide
    );
    assert_eq!(run.probe().entries(), 500);

    // Still on screen, then off screen with the app inactive: settle.
    let hide = quiet;
    assert_eq!(
        run.tick(at(t0, hide + 5), seen(rows, SHOWN_READY)),
        SummonAction::Wait
    );
    let off = hide + 20;
    assert_eq!(
        run.tick(at(t0, off), seen(rows, HIDDEN)),
        SummonAction::Wait
    );
    let settle = SETTLE.as_millis() as u64;
    assert_eq!(
        run.tick(at(t0, off + settle - 1), seen(rows, HIDDEN)),
        SummonAction::Wait
    );
    assert_eq!(
        run.tick(at(t0, off + settle), seen(rows, HIDDEN)),
        SummonAction::Show
    );
    let shown = off + settle + 1;
    run.shown(at(t0, shown));

    // On screen but not yet key, then ready: draw, then the sample.
    let not_key = WindowState {
        key: false,
        app_active: false,
        ..SHOWN_READY
    };
    assert_eq!(
        run.tick(at(t0, shown + 10), seen(rows, not_key)),
        SummonAction::Wait
    );
    assert_eq!(
        run.tick(at(t0, shown + 30), seen(rows, SHOWN_READY)),
        SummonAction::Draw
    );
    run.drawn(at(t0, shown + 33));
    assert_eq!(run.probe().samples(), &[ms(33)]);

    // The second cycle.
    let next = shown + 40;
    assert_eq!(
        run.tick(at(t0, next), seen(rows, SHOWN_READY)),
        SummonAction::Hide
    );
    assert_eq!(
        run.tick(at(t0, next + 10), seen(rows, HIDDEN)),
        SummonAction::Wait
    );
    assert_eq!(
        run.tick(at(t0, next + 10 + settle), seen(rows, HIDDEN)),
        SummonAction::Show
    );
    let shown2 = next + 10 + settle;
    run.shown(at(t0, shown2));
    assert_eq!(
        run.tick(at(t0, shown2 + 20), seen(rows, SHOWN_READY)),
        SummonAction::Draw
    );
    run.drawn(at(t0, shown2 + 21));
    assert_eq!(
        run.tick(at(t0, shown2 + 30), seen(rows, SHOWN_READY)),
        SummonAction::Finish
    );
    assert!(run.is_done());
    assert_eq!(run.probe().samples(), &[ms(33), ms(21)]);
    assert_eq!(run.skipped(), 0);
}

#[test]
fn a_window_that_will_not_hide_skips_its_cycle() {
    let t0 = Instant::now();
    let mut run = SummonRun::new(t0, 1);
    let rows = listed(t0, 10);
    let start = 100 + QUIET.as_millis() as u64;
    assert_eq!(
        run.tick(at(t0, start), seen(rows, SHOWN_READY)),
        SummonAction::Hide
    );
    let late = start + HIDE_TIMEOUT.as_millis() as u64;
    assert_eq!(
        run.tick(at(t0, late - 1), seen(rows, SHOWN_READY)),
        SummonAction::Wait
    );
    // Showing now would toggle the visible window away instead.
    assert_eq!(
        run.tick(at(t0, late), seen(rows, SHOWN_READY)),
        SummonAction::Finish
    );
    assert_eq!(run.skipped(), 1);
    assert!(run.probe().samples().is_empty());
}

#[test]
fn an_app_that_stays_active_is_shown_after_the_hide_timeout() {
    let t0 = Instant::now();
    let mut run = SummonRun::new(t0, 1);
    let rows = listed(t0, 10);
    let start = 100 + QUIET.as_millis() as u64;
    assert_eq!(
        run.tick(at(t0, start), seen(rows, SHOWN_READY)),
        SummonAction::Hide
    );
    let still_active = WindowState {
        app_active: true,
        ..HIDDEN
    };
    let late = start + HIDE_TIMEOUT.as_millis() as u64;
    assert_eq!(
        run.tick(at(t0, late - 1), seen(rows, still_active)),
        SummonAction::Wait
    );
    assert_eq!(
        run.tick(at(t0, late), seen(rows, still_active)),
        SummonAction::Wait
    );
    let settle = SETTLE.as_millis() as u64;
    assert_eq!(
        run.tick(at(t0, late + settle), seen(rows, still_active)),
        SummonAction::Show
    );
    assert_eq!(run.skipped(), 0);
}

#[test]
fn a_window_that_never_gets_ready_is_a_slow_sample() {
    let t0 = Instant::now();
    let mut run = SummonRun::new(t0, 1);
    let rows = listed(t0, 10);
    let start = 100 + QUIET.as_millis() as u64;
    assert_eq!(
        run.tick(at(t0, start), seen(rows, SHOWN_READY)),
        SummonAction::Hide
    );
    assert_eq!(
        run.tick(at(t0, start + 1), seen(rows, HIDDEN)),
        SummonAction::Wait
    );
    let show = start + 1 + SETTLE.as_millis() as u64;
    assert_eq!(
        run.tick(at(t0, show), seen(rows, HIDDEN)),
        SummonAction::Show
    );
    run.shown(at(t0, show));
    let late = show + READY_TIMEOUT.as_millis() as u64;
    assert_eq!(
        run.tick(at(t0, late - 1), seen(rows, HIDDEN)),
        SummonAction::Wait
    );
    assert_eq!(
        run.tick(at(t0, late), seen(rows, HIDDEN)),
        SummonAction::Finish
    );
    assert_eq!(run.probe().samples(), &[READY_TIMEOUT]);
    assert!(!run.probe().passes());
}

#[test]
fn a_list_that_never_settles_starts_after_the_list_timeout() {
    let t0 = Instant::now();
    let mut run = SummonRun::new(t0, 1);
    let indexing = SummonSeen {
        results: listed(t0, 42),
        indexing: true,
        window: SHOWN_READY,
    };
    assert_eq!(run.tick(at(t0, 10_000), indexing), SummonAction::Wait);
    assert_eq!(run.tick(t0 + LIST_TIMEOUT, indexing), SummonAction::Hide);
    assert_eq!(run.probe().entries(), 42);
}

#[test]
fn the_default_run_times_fifty_cycles() {
    let t0 = Instant::now();
    let mut run = SummonRun::with_defaults(t0);
    let rows = listed(t0, 3);
    let mut now = 100 + QUIET.as_millis() as u64;
    let mut hidden = false;
    for _ in 0..10_000 {
        let window = if hidden { HIDDEN } else { SHOWN_READY };
        match run.tick(at(t0, now), seen(rows, window)) {
            SummonAction::Wait => {}
            SummonAction::Hide => hidden = true,
            SummonAction::Show => {
                run.shown(at(t0, now));
                hidden = false;
            }
            SummonAction::Draw => run.drawn(at(t0, now + 5)),
            SummonAction::Finish => break,
        }
        now += 50;
    }
    assert!(run.is_done());
    assert_eq!(run.probe().samples().len(), SUMMON_CYCLES);
    assert!(run.probe().passes());
}
