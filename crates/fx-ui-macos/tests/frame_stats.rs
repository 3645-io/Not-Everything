//! Contract tests for `fx_ui_macos::frame_stats::FrameStats`: nearest-rank
//! frame-time percentiles, the over-budget count and the GATE line.
//! Built with the `measure` feature only.

#![cfg(feature = "measure")]

use std::time::Duration;

use fx_ui_macos::frame_stats::FrameStats;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// 1..=100 ms in a scrambled order (37 is coprime to 100).
fn one_to_hundred_scrambled() -> FrameStats {
    let mut s = FrameStats::new();
    for k in 0..100u64 {
        s.record(ms((k * 37) % 100 + 1));
    }
    s
}

fn gate_line(report: &str) -> &str {
    report
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .expect("report has a last line")
}

/// Parses the `p95=<x>ms` value of a GATE line, in milliseconds.
fn gate_p95_ms(line: &str) -> f64 {
    let rest = line
        .split("p95=")
        .nth(1)
        .unwrap_or_else(|| panic!("no p95= in {line:?}"));
    let value = rest
        .split("ms")
        .next()
        .unwrap_or_else(|| panic!("no ms unit in {line:?}"));
    value
        .parse()
        .unwrap_or_else(|_| panic!("bad p95 {value:?} in {line:?}"))
}

#[test]
fn default_budget_is_16_7_ms() {
    assert_eq!(FrameStats::new().budget(), Duration::from_micros(16_700));
}

#[test]
fn empty_stats_have_no_percentiles_and_fail_the_gate() {
    let s = FrameStats::new();
    assert_eq!(s.count(), 0);
    assert_eq!(s.p50(), None);
    assert_eq!(s.p95(), None);
    assert_eq!(s.p99(), None);
    assert_eq!(s.max(), None);
    assert_eq!(s.over_budget(), 0);
    assert_eq!(s.over_budget_fraction(), 0.0);
    assert!(!s.passes());
    let report = s.report_text();
    assert!(
        gate_line(&report).starts_with("GATE: FAIL"),
        "empty run must not pass: {report}"
    );
}

#[test]
fn percentiles_use_nearest_rank() {
    let s = one_to_hundred_scrambled();
    assert_eq!(s.count(), 100);
    assert_eq!(s.p50(), Some(ms(50)));
    assert_eq!(s.p95(), Some(ms(95)));
    assert_eq!(s.p99(), Some(ms(99)));
    assert_eq!(s.max(), Some(ms(100)));
}

#[test]
fn nearest_rank_on_small_sets_is_a_measured_sample() {
    let mut s = FrameStats::new();
    for n in [4, 1, 3, 2] {
        s.record(ms(n));
    }
    // ranks: p50 -> ceil(2.0) = 2, p95 -> ceil(3.8) = 4, p99 -> 4.
    assert_eq!(s.p50(), Some(ms(2)));
    assert_eq!(s.p95(), Some(ms(4)));
    assert_eq!(s.p99(), Some(ms(4)));
    assert_eq!(s.max(), Some(ms(4)));

    let mut one = FrameStats::new();
    one.record(Duration::from_micros(8_250));
    assert_eq!(one.count(), 1);
    assert_eq!(one.p50(), Some(Duration::from_micros(8_250)));
    assert_eq!(one.p95(), Some(Duration::from_micros(8_250)));
    assert_eq!(one.p99(), Some(Duration::from_micros(8_250)));
}

#[test]
fn over_budget_counts_frames_strictly_above_the_budget() {
    let s = one_to_hundred_scrambled();
    // 17..=100 ms exceed 16.7 ms.
    assert_eq!(s.over_budget(), 84);
    assert!((s.over_budget_fraction() - 0.84).abs() < 1e-9);

    let mut edge = FrameStats::with_budget(ms(10));
    assert_eq!(edge.budget(), ms(10));
    edge.record(ms(10));
    edge.record(ms(11));
    edge.record(ms(9));
    edge.record(Duration::from_micros(10_001));
    assert_eq!(edge.over_budget(), 2, "a frame exactly on budget is fine");
    assert!((edge.over_budget_fraction() - 0.5).abs() < 1e-9);
}

#[test]
fn gate_passes_when_p95_is_within_budget() {
    let mut s = FrameStats::new();
    // 96 fast frames and 4 slow ones: p95 is a fast frame.
    for _ in 0..96 {
        s.record(ms(5));
    }
    for _ in 0..4 {
        s.record(ms(40));
    }
    assert_eq!(s.p95(), Some(ms(5)));
    assert_eq!(s.over_budget(), 4);
    assert!(s.passes());

    let line_owner = s.report_text();
    let line = gate_line(&line_owner);
    assert!(line.starts_with("GATE: PASS p95="), "{line:?}");
    assert!(line.ends_with("ms budget=16.7ms"), "{line:?}");
    assert!((gate_p95_ms(line) - 5.0).abs() < 0.01, "{line:?}");
}

#[test]
fn gate_passes_exactly_at_budget() {
    let mut s = FrameStats::new();
    for _ in 0..20 {
        s.record(Duration::from_micros(16_700));
    }
    assert!(s.passes());
    assert!(gate_line(&s.report_text()).starts_with("GATE: PASS"));
}

#[test]
fn gate_fails_when_p95_exceeds_budget() {
    let s = one_to_hundred_scrambled();
    assert!(!s.passes());
    let report = s.report_text();
    let line = gate_line(&report);
    assert!(line.starts_with("GATE: FAIL p95="), "{line:?}");
    assert!(line.ends_with("ms budget=16.7ms"), "{line:?}");
    assert!((gate_p95_ms(line) - 95.0).abs() < 0.01, "{line:?}");
}

#[test]
fn gate_line_reports_sub_millisecond_precision() {
    let mut s = FrameStats::new();
    s.record(Duration::from_micros(12_340));
    let report = s.report_text();
    let line = gate_line(&report);
    assert!((gate_p95_ms(line) - 12.34).abs() < 0.006, "{line:?}");
}

#[test]
fn report_has_exactly_one_gate_line_and_lists_the_statistics() {
    let s = one_to_hundred_scrambled();
    let report = s.report_text();
    let gates = report.lines().filter(|l| l.contains("GATE:")).count();
    assert_eq!(gates, 1, "{report}");
    for word in ["p50", "p95", "p99", "max", "100"] {
        assert!(report.contains(word), "missing {word:?} in {report}");
    }
    // The over-budget count appears in the report.
    assert!(report.contains("84"), "{report}");
}
