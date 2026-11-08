//! Contract tests for the Phase 1 gate probe of the `fx-ui-macos` app: the
//! `--gate-probe <out>` option and the platform-agnostic
//! `fx_ui_macos::gate_probe` module behind it, built only with the
//! `measure` feature (`cargo test -p fx-ui-macos --features measure`).
//!
//! Started with `--gate-probe <out>`, the app opens the real engine over its
//! fx-store index for `--root` (never `SyntheticSource`), measures three Phase
//! 1 criteria and writes them to `<out>`:
//!
//! - goal 1: process start to the first result row painted, one sample per
//!   launch (the gate script runs 20 launches and takes the p95), budget
//!   `STARTUP_BUDGET` = 2.0 s;
//! - goal 2: keystroke to table repaint for every step of a scripted typing
//!   sequence (`default_keystroke_script`), p95 budget `KEYSTROKE_BUDGET` =
//!   50 ms;
//! - autoscroll frame times over the real engine, p95 budget
//!   `frame_stats::DEFAULT_BUDGET` (16.7 ms).
//!
//! The report is text with one `GATE <criterion>: PASS|FAIL (<detail>)` line
//! per criterion, so the fx-bench gate summary
//! (`fx_bench::gate::parse_gate_lines`) reads it. A criterion with no samples
//! is a FAIL line, never a missing line, and percentiles are nearest-rank
//! (`fx_bench::Stats`). At a budget exactly is a pass.
//!
//! The module's pure parts (argument parsing, the keystroke script, timing
//! aggregation and report rendering) are tested here on any host; the AppKit
//! glue that feeds them is not.
//!
//! The probe only reads the index and writes its own report file, a new
//! one: it never replaces an existing file, so the read-only rules are
//! unchanged.

#![cfg(feature = "measure")]

use std::path::PathBuf;
use std::time::Duration;

use fx_bench::Stats;
use fx_bench::gate::{GateLine, Verdict, parse_gate_lines};
use fx_ui_macos::app_args::{AppArgs, USAGE};
use fx_ui_macos::frame_stats::DEFAULT_BUDGET;
use fx_ui_macos::gate_probe::{
    DEFAULT_QUERIES, GateProbe, KEYSTROKE_BUDGET, STARTUP_BUDGET, default_keystroke_script,
    keystroke_script,
};

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// The gate lines of `probe`'s report, as the fx-bench summary parses them.
fn gate_lines(probe: &GateProbe) -> Vec<GateLine> {
    parse_gate_lines(&probe.report_text())
}

/// The single parsed gate line whose criterion mentions `keyword`
/// (case-insensitive).
fn line_for(probe: &GateProbe, keyword: &str) -> GateLine {
    let report = probe.report_text();
    let matching: Vec<GateLine> = parse_gate_lines(&report)
        .into_iter()
        .filter(|l| l.criterion.to_lowercase().contains(keyword))
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "expected exactly one gate line about {keyword:?} in:\n{report}"
    );
    matching.into_iter().next().unwrap()
}

/// A probe with every criterion comfortably inside its budget.
fn passing_probe() -> GateProbe {
    let mut p = GateProbe::new(2_000_000);
    p.set_first_result(ms(800));
    for i in 1..=40 {
        p.record_keystroke(ms(i % 20 + 1));
    }
    for i in 1..=200 {
        p.record_frame(Duration::from_micros(4_000 + (i % 50) * 100));
    }
    p
}

// ---------------------------------------------------------------------------
// Budgets
// ---------------------------------------------------------------------------

#[test]
fn budgets_are_the_phase_1_goals() {
    assert_eq!(STARTUP_BUDGET, Duration::from_secs(2));
    assert_eq!(KEYSTROKE_BUDGET, ms(50));
    assert_eq!(DEFAULT_BUDGET, Duration::from_micros(16_700));
}

// ---------------------------------------------------------------------------
// --gate-probe argument parsing
// ---------------------------------------------------------------------------

#[test]
fn gate_probe_is_off_by_default() {
    let a = AppArgs::parse(Vec::<String>::new()).expect("no arguments parse");
    assert_eq!(a.gate_probe, None);
    assert_eq!(AppArgs::default().gate_probe, None);
}

#[test]
fn gate_probe_takes_the_output_path() {
    let a = AppArgs::parse(args(&["--gate-probe", "/tmp/gate out/ui.log"])).expect("parses");
    assert_eq!(a.gate_probe, Some(PathBuf::from("/tmp/gate out/ui.log")));
    assert_eq!(a.synthetic, None, "the probe runs on the real engine");
}

#[test]
fn gate_probe_combines_with_a_root_in_any_order() {
    let a = AppArgs::parse(args(&["--gate-probe", "ui.log", "--root", "/Volumes/Data"]))
        .expect("parses");
    let b = AppArgs::parse(args(&["--root", "/Volumes/Data", "--gate-probe", "ui.log"]))
        .expect("parses");
    assert_eq!(a, b);
    assert_eq!(a.root, PathBuf::from("/Volumes/Data"));
    assert_eq!(a.gate_probe, Some(PathBuf::from("ui.log")));
}

#[test]
fn gate_probe_needs_a_non_empty_path() {
    for bad in [&["--gate-probe"][..], &["--gate-probe", ""][..]] {
        match AppArgs::parse(args(bad)) {
            Ok(a) => panic!("{bad:?} should be rejected, parsed as {a:?}"),
            Err(e) => assert!(!e.to_string().trim().is_empty(), "{bad:?}: empty message"),
        }
    }
}

#[test]
fn gate_probe_refuses_the_synthetic_source() {
    for bad in [
        &["--gate-probe", "ui.log", "--synthetic", "2000000"][..],
        &["--synthetic", "2000000", "--gate-probe", "ui.log"][..],
    ] {
        match AppArgs::parse(args(bad)) {
            Ok(a) => panic!("{bad:?}: the gate probe must measure the real engine, got {a:?}"),
            Err(e) => assert!(!e.to_string().trim().is_empty(), "{bad:?}: empty message"),
        }
    }
}

#[test]
fn usage_documents_the_gate_probe() {
    assert!(
        USAGE.contains("--gate-probe"),
        "USAGE should mention --gate-probe:\n{USAGE}"
    );
}

/// On macOS, a bad probe command line exits 2 before any window opens and
/// writes no report.
#[cfg(target_os = "macos")]
#[test]
fn binary_rejects_a_bad_gate_probe_with_exit_code_2() {
    let out_file = std::env::temp_dir().join(format!(
        "fx-ui-gate-probe-rejected-{}.log",
        std::process::id()
    ));
    let out_str = out_file.to_string_lossy().into_owned();
    for bad in [
        vec!["--gate-probe".to_string()],
        vec![
            "--gate-probe".to_string(),
            out_str.clone(),
            "--synthetic".to_string(),
            "10".to_string(),
        ],
    ] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_fx-ui-macos"))
            .args(&bad)
            .output()
            .expect("run fx-ui-macos");
        assert_eq!(
            out.status.code(),
            Some(2),
            "{bad:?}: status {:?}",
            out.status
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("usage"), "{bad:?}: stderr {stderr:?}");
        assert!(!out_file.exists(), "{bad:?}: a rejected run wrote a report");
    }
}

// ---------------------------------------------------------------------------
// The scripted keystroke sequence
// ---------------------------------------------------------------------------

#[test]
fn keystroke_script_types_each_query_one_character_at_a_time() {
    assert_eq!(
        keystroke_script(&["ab", "c", "de"]),
        vec!["a", "ab", "c", "d", "de"]
    );
}

#[test]
fn keystroke_script_steps_by_characters_not_bytes() {
    assert_eq!(keystroke_script(&["é✓x"]), vec!["é", "é✓", "é✓x"]);
}

#[test]
fn keystroke_script_skips_empty_queries() {
    assert_eq!(keystroke_script(&["", "a", ""]), vec!["a"]);
    assert!(keystroke_script(&[]).is_empty());
}

#[test]
fn default_script_types_every_default_query() {
    assert!(
        DEFAULT_QUERIES.len() >= 3,
        "a scripted sequence needs several queries: {DEFAULT_QUERIES:?}"
    );
    assert!(DEFAULT_QUERIES.iter().all(|q| !q.is_empty()));
    let script = default_keystroke_script();
    assert_eq!(script, keystroke_script(DEFAULT_QUERIES));
    for q in DEFAULT_QUERIES {
        assert!(
            script.iter().any(|s| s == q),
            "the script never reaches {q:?}"
        );
    }
}

#[test]
fn default_script_has_enough_keystrokes_for_a_p95() {
    let script = default_keystroke_script();
    assert!(
        script.len() >= 20,
        "{} keystrokes are too few for a p95",
        script.len()
    );
    assert!(script.iter().all(|s| !s.is_empty()));
}

// ---------------------------------------------------------------------------
// Timing aggregation
// ---------------------------------------------------------------------------

#[test]
fn a_new_probe_has_no_samples() {
    let p = GateProbe::new(2_000_000);
    assert_eq!(p.first_result(), None);
    assert_eq!(p.keystroke_stats(), None);
    assert_eq!(p.frames().count(), 0);
    assert!(!p.passes(), "an empty probe never passes");
}

#[test]
fn records_the_first_result_time() {
    let mut p = GateProbe::new(10);
    p.set_first_result(ms(1234));
    assert_eq!(p.first_result(), Some(ms(1234)));
}

#[test]
fn keystroke_stats_are_nearest_rank() {
    let mut p = GateProbe::new(10);
    let samples: Vec<Duration> = (1..=100).rev().map(ms).collect();
    for &d in &samples {
        p.record_keystroke(d);
    }
    let s = p.keystroke_stats().expect("100 samples");
    assert_eq!(Some(s), Stats::from_samples(&samples));
    assert_eq!(s.count, 100);
    assert_eq!(s.p50, ms(50));
    assert_eq!(s.p95, ms(95));
    assert_eq!(s.max, ms(100));
}

#[test]
fn frames_are_collected_as_frame_stats() {
    let mut p = GateProbe::new(10);
    for i in 1..=20 {
        p.record_frame(ms(i));
    }
    assert_eq!(p.frames().count(), 20);
    assert_eq!(p.frames().p95(), Some(ms(19)));
    assert_eq!(p.frames().budget(), DEFAULT_BUDGET);
}

// ---------------------------------------------------------------------------
// Verdicts and the report
// ---------------------------------------------------------------------------

#[test]
fn report_has_one_parsable_gate_line_per_criterion() {
    let p = passing_probe();
    let lines = gate_lines(&p);
    assert_eq!(lines.len(), 3, "{}", p.report_text());
    for keyword in ["first result", "keystroke", "frame"] {
        let l = line_for(&p, keyword);
        assert_eq!(l.verdict, Verdict::Pass, "{keyword}: {l:?}");
    }
    assert!(p.passes());
}

#[test]
fn report_names_the_index_size() {
    let p = passing_probe();
    let report = p.report_text();
    assert!(
        report.contains("2000000"),
        "the report should say how many entries the index has:\n{report}"
    );
}

#[test]
fn slow_first_result_fails_only_the_startup_gate() {
    let mut p = passing_probe();
    p.set_first_result(ms(2_001));
    assert_eq!(line_for(&p, "first result").verdict, Verdict::Fail);
    assert_eq!(line_for(&p, "keystroke").verdict, Verdict::Pass);
    assert_eq!(line_for(&p, "frame").verdict, Verdict::Pass);
    assert!(!p.passes());
}

#[test]
fn first_result_at_the_budget_passes() {
    let mut p = passing_probe();
    p.set_first_result(STARTUP_BUDGET);
    assert_eq!(line_for(&p, "first result").verdict, Verdict::Pass);
}

#[test]
fn keystroke_gate_judges_the_p95() {
    // 100 samples: 94 fast and 6 slow keystrokes put the nearest-rank p95 on
    // a slow one; 95 fast and 5 slow keep it fast.
    let mut slow = GateProbe::new(10);
    let mut ok = GateProbe::new(10);
    for i in 0..100 {
        slow.record_keystroke(if i < 94 { ms(10) } else { ms(51) });
        ok.record_keystroke(if i < 95 { ms(10) } else { ms(500) });
    }
    assert_eq!(line_for(&slow, "keystroke").verdict, Verdict::Fail);
    assert_eq!(line_for(&ok, "keystroke").verdict, Verdict::Pass);

    let mut edge = GateProbe::new(10);
    edge.record_keystroke(KEYSTROKE_BUDGET);
    assert_eq!(line_for(&edge, "keystroke").verdict, Verdict::Pass);
}

#[test]
fn keystroke_detail_reports_the_p95() {
    let mut p = GateProbe::new(2_000_000);
    for i in 1..=100 {
        p.record_keystroke(ms(i));
    }
    let l = line_for(&p, "keystroke");
    let detail = l.detail.unwrap_or_default();
    assert!(
        detail.contains("p95=95"),
        "detail should give p95: {detail:?}"
    );
}

#[test]
fn frame_gate_judges_the_p95_against_one_60hz_frame() {
    let mut slow = GateProbe::new(10);
    for i in 0..100 {
        slow.record_frame(if i < 90 { ms(5) } else { ms(30) });
    }
    let l = line_for(&slow, "frame");
    assert_eq!(l.verdict, Verdict::Fail);
    assert!(
        l.detail.unwrap_or_default().contains("p95=30"),
        "detail should give p95"
    );

    let mut edge = GateProbe::new(10);
    edge.record_frame(DEFAULT_BUDGET);
    assert_eq!(line_for(&edge, "frame").verdict, Verdict::Pass);
}

#[test]
fn missing_samples_are_fail_lines_not_missing_lines() {
    let p = GateProbe::new(2_000_000);
    let lines = gate_lines(&p);
    assert_eq!(lines.len(), 3, "{}", p.report_text());
    for keyword in ["first result", "keystroke", "frame"] {
        assert_eq!(line_for(&p, keyword).verdict, Verdict::Fail, "{keyword}");
    }
}

#[test]
fn report_lines_use_the_criterion_form() {
    let p = passing_probe();
    let report = p.report_text();
    let gate: Vec<&str> = report
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("GATE"))
        .collect();
    assert_eq!(gate.len(), 3, "{report}");
    for l in gate {
        assert!(
            l.starts_with("GATE ") && (l.contains(": PASS") || l.contains(": FAIL")),
            "expected `GATE <criterion>: PASS|FAIL (...)`, got {l:?}"
        );
    }
}

/// A fresh, empty folder under the system temp directory.
fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "fx-ui-gate-probe-{tag}-{}-{}",
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
fn write_report_writes_the_report_text() {
    let dir = temp_dir("write");
    let path = dir.join("ui-probe.log");
    let p = passing_probe();
    p.write_report(&path).expect("write report");
    let text = std::fs::read_to_string(&path).expect("read report");
    assert_eq!(text, p.report_text());
    assert_eq!(parse_gate_lines(&text).len(), 3);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
}

/// The report goes to a new file: a path that already holds a file is an
/// `AlreadyExists` error, and the file keeps its contents.
#[test]
fn write_report_never_replaces_a_file() {
    let dir = temp_dir("exists");
    let path = dir.join("ui-probe.log");
    std::fs::write(&path, "keep me").expect("write the existing file");
    let e = passing_probe()
        .write_report(&path)
        .expect_err("an existing file is never replaced");
    let kept = std::fs::read_to_string(&path).expect("read the existing file");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
    assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists, "{e}");
    assert_eq!(kept, "keep me");
}
