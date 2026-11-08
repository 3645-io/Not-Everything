//! Phase 1 gate support in fx-bench: the `gate1` module and the
//! `fx-bench phase1-summary` subcommand.
//!
//! # Contract pinned by these tests
//!
//! Module `fx_bench::gate1` (Phase 0's `fx_bench::gate` is unchanged):
//!
//! - `criteria() -> Vec<Criterion>`; `Criterion` has `id` and
//!   `description` (both string-like). One criterion per row of the Phase 1
//!   gate in `docs/PLAN.md`, with these stable ids and result-file metrics:
//!
//!   | id                | metric(s)                          | rule                         |
//!   |-------------------|------------------------------------|------------------------------|
//!   | `syntax`          | `syntax_cases`, `syntax_failures`  | max cases >= 300, failures 0 |
//!   | `goal1_launch`    | `launch_ms`                        | p95 <= 2000 (stretch <= 1000)|
//!   | `goal2_engine_2m` | `engine_2m_ms`                     | p95 <= 16                    |
//!   | `goal2_engine_5m` | `engine_5m_ms`                     | p95 <= 30                    |
//!   | `goal2_keystroke` | `keystroke_ms`                     | p95 <= 50                    |
//!   | `goal2_regex`     | `regex_2m_ms`                      | p95 <= 100                   |
//!   | `goal2_fullpath`  | `fullpath_2m_ms`                   | p95 <= 100                   |
//!   | `goal3_freshness` | `freshness_ms`                     | p99 <= 1000 (stretch p95 <= 250) |
//!   | `catchup_equal`   | `catchup_equal` (flag)             | every value true             |
//!   | `catchup_time`    | `catchup_ms`                       | worst (max) <= 10000         |
//!   | `catchup_usable`  | `catchup_usable_ms`                | worst (max) <= 2000          |
//!   | `first_run`       | `first_run_ms`                     | worst (max) <= 15000         |
//!   | `memory`          | `bytes_per_entry`                  | worst (max) <= 100           |
//!   | `cli`             | `cli_ok` (flag)                    | every value true             |
//!
//!   Percentiles are nearest-rank (as in the harness's `Stats`). Thresholds
//!   are inclusive and compare the exact sample values (a measured 16 ms
//!   p95 passes a 16 ms budget even after any `Duration` conversion for
//!   `stats`). A criterion whose metric has no value is
//!   `Status::NotMeasured`; a missing `syntax_failures` with
//!   `syntax_cases` present counts as 0 failures.
//! - `Results` (Default): samples per metric. `add_sample(&str, f64)`,
//!   `add_flag(&str, bool)`, `samples(&str) -> &[f64]` (empty when
//!   absent), `flag(&str) -> Option<bool>` (`Some(false)` if any recorded
//!   value is false), `merge(&mut self, other: Results)`.
//! - `parse_results(&str) -> Results` reads benchmark output line by line
//!   (each line trimmed first): a line
//!   `<key>=<value>` (key of `[a-z0-9_]`, value a number or `true`/`false`)
//!   adds one sample; a line `{"metric":"<key>","value":<number|bool>, ...}`
//!   (JSON lines, extra fields allowed) adds one sample; all other lines
//!   (prose, `GATE ...` lines) are ignored. Repeated keys accumulate.
//! - `read_results_dir(&Path) -> io::Result<Results>` parses every regular
//!   file in the directory except `summary.md`.
//! - `evaluate(&Results) -> Vec<CriterionResult>`, one per criterion, in
//!   `criteria()` order. `CriterionResult` has `id`, `status: Status`,
//!   `measured: Option<f64>` (the value compared with the threshold),
//!   `stats: Option<Stats>` (harness nearest-rank stats for timing
//!   metrics, ms samples converted to `Duration`) and
//!   `stretch_met: Option<bool>` (only for criteria with a stretch goal).
//! - `Status::{Pass, Fail, NotMeasured}`, `as_str()` = `PASS`, `FAIL`,
//!   `NOT MEASURED`.
//! - `all_pass(&[CriterionResult]) -> bool`: true only when every row passes.
//! - `render_summary(&[CriterionResult], machine: &str, cache: CacheState)
//!   -> String`: markdown containing the literal text `Machine: <label>`
//!   and `Cache: <warm|cold|unknown>` (no markup between the colon and the
//!   value), and a table with one row per criterion showing its id and
//!   status label.
//!
//! - `evaluate_gate(&Results, CacheState) -> Vec<CriterionResult>`: the gate
//!   verdict `phase1-summary` reports (same rows and order as `evaluate`,
//!   which stays the scale-agnostic rule evaluation). On top of the rules:
//!   - Cold cache: `goal1_launch` (and any startup metric) is judged by the
//!     worst sample (max), not the nearest-rank p95, so one slow cold launch
//!     can never be dropped into a PASS.
//!   - Scale: each row reads the scale its benchmark ran at from a result
//!     metric. When that metric is missing or below the gate scale the row
//!     is `NotMeasured` (never Pass or Fail) and its `detail` says
//!     `below gate scale (N)` with the measured N when there is one. When the
//!     scale is met, `detail` shows the measured scale.
//!
//!     | id                         | scale metric          | gate scale        |
//!     |----------------------------|-----------------------|-------------------|
//!     | `goal1_launch`             | `ui_entries`          | >= 2,000,000      |
//!     | `goal1_launch` (warm only) | count of `launch_ms`  | >= 20 launches    |
//!     | `goal2_engine_2m`          | `engine_2m_entries`   | >= 2,000,000      |
//!     | `goal2_engine_5m`          | `engine_5m_entries`   | >= 5,000,000      |
//!     | `goal2_regex`              | `regex_2m_entries`    | >= 2,000,000      |
//!     | `goal2_fullpath`           | `fullpath_2m_entries` | >= 2,000,000      |
//!     | `goal3_freshness`          | `freshness_ops`       | >= 1,000 ops      |
//!     | `catchup_*`                | `catchup_changes`     | >= 10,000 changes |
//!     | `first_run`                | `first_run_entries`   | ~2,000,000        |
//!
//!     `memory` (the B/entry estimate), `syntax` and `cli` have no scale
//!     requirement. The benches also emit `startup_entries`.
//!
//! Binary: `fx-bench phase1-summary --machine LABEL --cache
//! warm|cold|unknown DIR` reads DIR with `read_results_dir`, judges it with
//! `evaluate_gate` for the given cache state, writes
//! `DIR/summary.md`, and exits 0 when every criterion passes, 1 when the
//! summary was written but some criterion failed or was not measured, and 2
//! on a usage error or an unreadable DIR.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fx_bench::CacheState;
use fx_bench::gate1::{self, CriterionResult, Results, Status};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const ALL_IDS: [&str; 14] = [
    "syntax",
    "goal1_launch",
    "goal2_engine_2m",
    "goal2_engine_5m",
    "goal2_keystroke",
    "goal2_regex",
    "goal2_fullpath",
    "goal3_freshness",
    "catchup_equal",
    "catchup_time",
    "catchup_usable",
    "first_run",
    "memory",
    "cli",
];

fn row<'a>(rows: &'a [CriterionResult], id: &str) -> &'a CriterionResult {
    rows.iter()
        .find(|r| r.id == id)
        .unwrap_or_else(|| panic!("no criterion row with id {id}"))
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-3
}

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// A results text where every criterion passes.
fn passing_text() -> String {
    let mut t = String::from("syntax_cases=312\nsyntax_failures=0\n");
    for _ in 0..20 {
        t.push_str("launch_ms=800\n");
    }
    for (k, v) in [
        ("engine_2m_ms", 10.0),
        ("engine_5m_ms", 20.0),
        ("keystroke_ms", 30.0),
        ("regex_2m_ms", 60.0),
        ("fullpath_2m_ms", 70.0),
        ("freshness_ms", 120.0),
    ] {
        for _ in 0..5 {
            t.push_str(&format!("{k}={v}\n"));
        }
    }
    t.push_str(
        "catchup_equal=true\ncatchup_ms=4000\ncatchup_usable_ms=900\n\
         first_run_ms=9000\nbytes_per_entry=72.5\ncli_ok=true\n",
    );
    t.push_str(FULL_SCALE);
    t
}

/// The scale metrics of a full-scale gate run.
const FULL_SCALE: &str = "ui_entries=2000000\nstartup_entries=2000000\n\
                          engine_2m_entries=2000000\nengine_5m_entries=5000000\n\
                          regex_2m_entries=2000000\nfullpath_2m_entries=2000000\n\
                          freshness_ops=1000\ncatchup_changes=10000\n\
                          first_run_entries=2100000\n";

/// A `--quick` run: every timing well inside its budget, but at the quick
/// scale (20k entries, 3 launches, 100 ops, 1,000 changes).
fn quick_text() -> String {
    let mut t = String::from("syntax_cases=312\nsyntax_failures=0\n");
    for _ in 0..3 {
        t.push_str("launch_ms=300\nkeystroke_ms=5\n");
    }
    for (k, v) in [
        ("engine_2m_ms", 1.0),
        ("engine_5m_ms", 2.0),
        ("regex_2m_ms", 3.0),
        ("fullpath_2m_ms", 4.0),
        ("freshness_ms", 50.0),
    ] {
        for _ in 0..5 {
            t.push_str(&format!("{k}={v}\n"));
        }
    }
    t.push_str(
        "catchup_equal=true\ncatchup_ms=400\ncatchup_usable_ms=90\n\
         first_run_ms=900\nbytes_per_entry=72.5\ncli_ok=true\n\
         ui_entries=20000\nstartup_entries=20000\n\
         engine_2m_entries=20000\nengine_5m_entries=50000\n\
         regex_2m_entries=20000\nfullpath_2m_entries=20000\n\
         freshness_ops=100\ncatchup_changes=1000\nfirst_run_entries=20000\n",
    );
    t
}

/// The summary.md table line of criterion `id`.
fn summary_line<'a>(md: &'a str, id: &str) -> &'a str {
    md.lines()
        .find(|l| {
            let t = l.trim_start();
            t.starts_with('|')
                && t[1..]
                    .split('|')
                    .next()
                    .is_some_and(|cell| cell.trim().trim_matches('`') == id)
        })
        .unwrap_or_else(|| panic!("summary has no row for {id}:\n{md}"))
}

fn mentions_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| text.contains(n))
}

fn fx_bench(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fx-bench"))
        .args(args)
        .output()
        .expect("run fx-bench binary")
}

fn describe(out: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// A unique scratch directory under the system temp dir, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "fx-bench-phase1-summary-{tag}-{}-{}-{nanos}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).expect("create scratch dir");
        Scratch(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write(&self, name: &str, text: &str) {
        fs::write(self.0.join(name), text).expect("write results file");
    }

    fn summary(&self) -> String {
        fs::read_to_string(self.0.join("summary.md")).expect("summary.md written")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------------------
// Criteria table
// ---------------------------------------------------------------------------

#[test]
fn criteria_cover_every_phase1_gate_row() {
    let crit = gate1::criteria();
    for id in ALL_IDS {
        assert!(
            crit.iter().any(|c| c.id == id),
            "criteria() lacks {id}; has {:?}",
            crit.iter().map(|c| c.id.to_string()).collect::<Vec<_>>()
        );
    }
    for c in &crit {
        assert!(
            !c.description.is_empty(),
            "criterion {} has no description",
            c.id
        );
    }
    let mut ids: Vec<String> = crit.iter().map(|c| c.id.to_string()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), crit.len(), "criterion ids must be unique");
}

#[test]
fn evaluate_returns_one_row_per_criterion_in_order() {
    let rows = gate1::evaluate(&Results::default());
    let crit = gate1::criteria();
    assert_eq!(rows.len(), crit.len());
    for (r, c) in rows.iter().zip(&crit) {
        assert_eq!(r.id.to_string(), c.id.to_string());
    }
}

// ---------------------------------------------------------------------------
// Parsing result files
// ---------------------------------------------------------------------------

#[test]
fn parses_key_value_lines_and_ignores_other_output() {
    let text = "engine bench at 2000000 entries\n\
                GATE engine p95: PASS (9 ms)\n\
                GATE: PASS p95=5.00ms budget=16.7ms\n\
                engine_2m_ms=9.5\n\
                  engine_2m_ms=11\n\
                cli_ok=true\n\
                Not A Key=3\n\
                bytes_per_entry=abc\n";
    let r = gate1::parse_results(text);
    assert_eq!(r.samples("engine_2m_ms"), &[9.5, 11.0]);
    assert_eq!(r.flag("cli_ok"), Some(true));
    assert!(r.samples("bytes_per_entry").is_empty());
    assert!(r.samples("p95").is_empty(), "GATE lines are not samples");
    assert!(r.samples("missing").is_empty());
    assert_eq!(r.flag("catchup_equal"), None);
}

#[test]
fn parses_json_lines() {
    let text = "{\"metric\":\"launch_ms\",\"value\":812.5}\n\
                {\"metric\": \"launch_ms\", \"value\": 900, \"cache\": \"warm\"}\n\
                {\"metric\":\"catchup_equal\",\"value\":false}\n";
    let r = gate1::parse_results(text);
    assert_eq!(r.samples("launch_ms"), &[812.5, 900.0]);
    assert_eq!(r.flag("catchup_equal"), Some(false));
}

#[test]
fn a_single_false_flag_wins() {
    let mut r = Results::default();
    r.add_flag("cli_ok", true);
    r.add_flag("cli_ok", false);
    r.add_flag("cli_ok", true);
    assert_eq!(r.flag("cli_ok"), Some(false));
}

#[test]
fn merge_accumulates_samples() {
    let mut a = gate1::parse_results("launch_ms=1\nlaunch_ms=2\n");
    let b = gate1::parse_results("launch_ms=3\nmemory_note=1\n");
    a.merge(b);
    assert_eq!(a.samples("launch_ms"), &[1.0, 2.0, 3.0]);
}

#[test]
fn read_results_dir_reads_every_file_but_summary() {
    let dir = Scratch::new("read");
    dir.write("launch.log", "launch_ms=100\nlaunch_ms=200\n");
    dir.write(
        "engine.jsonl",
        "{\"metric\":\"engine_2m_ms\",\"value\":7}\n",
    );
    dir.write("summary.md", "launch_ms=999999\n");
    let r = gate1::read_results_dir(dir.path()).expect("read results dir");
    let mut launch = r.samples("launch_ms").to_vec();
    launch.sort_by(f64::total_cmp);
    assert_eq!(launch, vec![100.0, 200.0]);
    assert_eq!(r.samples("engine_2m_ms"), &[7.0]);
}

#[test]
fn read_results_dir_errors_on_missing_dir() {
    let dir = Scratch::new("missing");
    assert!(gate1::read_results_dir(&dir.path().join("nope")).is_err());
}

// ---------------------------------------------------------------------------
// Percentiles and verdicts
// ---------------------------------------------------------------------------

#[test]
fn timing_criteria_use_nearest_rank_percentiles() {
    let mut r = Results::default();
    // 1..=20 ms in shuffled order: p50 = 10, p95 = 19, p99 = 20.
    for v in [
        7, 20, 1, 13, 4, 18, 10, 2, 16, 5, 19, 11, 3, 15, 8, 12, 6, 17, 9, 14,
    ] {
        r.add_sample("launch_ms", f64::from(v));
        r.add_sample("freshness_ms", f64::from(v));
    }
    let rows = gate1::evaluate(&r);

    let launch = row(&rows, "goal1_launch");
    let s = launch.stats.expect("timing criterion has stats");
    assert_eq!(s.count, 20);
    assert!(close(ms(s.p50), 10.0), "p50 {:?}", s.p50);
    assert!(close(ms(s.p95), 19.0), "p95 {:?}", s.p95);
    assert!(close(ms(s.p99), 20.0), "p99 {:?}", s.p99);
    assert!(
        close(launch.measured.expect("measured"), 19.0),
        "goal 1 uses p95"
    );
    assert_eq!(launch.status, Status::Pass);

    let fresh = row(&rows, "goal3_freshness");
    assert!(
        close(fresh.measured.expect("measured"), 20.0),
        "goal 3 uses p99"
    );
    assert_eq!(fresh.status, Status::Pass);
}

#[test]
fn unmeasured_criteria_are_not_measured() {
    let rows = gate1::evaluate(&Results::default());
    for r in &rows {
        assert_eq!(r.status, Status::NotMeasured, "{}", r.id);
        assert!(r.measured.is_none(), "{}", r.id);
    }
    assert!(!gate1::all_pass(&rows));
}

#[test]
fn every_criterion_passes_on_good_numbers() {
    let rows = gate1::evaluate(&gate1::parse_results(&passing_text()));
    for r in &rows {
        assert_eq!(r.status, Status::Pass, "{} should pass", r.id);
    }
    assert!(gate1::all_pass(&rows));
}

#[test]
fn thresholds_are_inclusive() {
    let text = "launch_ms=2000\nengine_2m_ms=16\nengine_5m_ms=30\nkeystroke_ms=50\n\
                regex_2m_ms=100\nfullpath_2m_ms=100\nfreshness_ms=1000\n\
                catchup_ms=10000\ncatchup_usable_ms=2000\nfirst_run_ms=15000\n\
                bytes_per_entry=100\nsyntax_cases=300\n";
    let rows = gate1::evaluate(&gate1::parse_results(text));
    for id in [
        "goal1_launch",
        "goal2_engine_2m",
        "goal2_engine_5m",
        "goal2_keystroke",
        "goal2_regex",
        "goal2_fullpath",
        "goal3_freshness",
        "catchup_time",
        "catchup_usable",
        "first_run",
        "memory",
        "syntax",
    ] {
        assert_eq!(row(&rows, id).status, Status::Pass, "{id} at its threshold");
    }
}

#[test]
fn values_over_the_threshold_fail() {
    let text = "launch_ms=2000.5\nengine_2m_ms=16.1\nengine_5m_ms=31\nkeystroke_ms=51\n\
                regex_2m_ms=101\nfullpath_2m_ms=150\nfreshness_ms=1001\n\
                catchup_ms=10001\ncatchup_usable_ms=2001\nfirst_run_ms=15001\n\
                bytes_per_entry=100.5\nsyntax_cases=299\ncatchup_equal=false\ncli_ok=false\n";
    let rows = gate1::evaluate(&gate1::parse_results(text));
    for id in ALL_IDS {
        assert_eq!(
            row(&rows, id).status,
            Status::Fail,
            "{id} over its threshold"
        );
    }
    assert!(!gate1::all_pass(&rows));
}

#[test]
fn syntax_needs_300_cases_and_no_failures() {
    let rows = gate1::evaluate(&gate1::parse_results(
        "syntax_cases=312\nsyntax_failures=1\n",
    ));
    assert_eq!(row(&rows, "syntax").status, Status::Fail);
    let rows = gate1::evaluate(&gate1::parse_results("syntax_cases=312\n"));
    assert_eq!(row(&rows, "syntax").status, Status::Pass);
    let rows = gate1::evaluate(&gate1::parse_results("syntax_failures=0\n"));
    assert_eq!(row(&rows, "syntax").status, Status::NotMeasured);
}

#[test]
fn one_shot_criteria_use_the_worst_sample() {
    let rows = gate1::evaluate(&gate1::parse_results(
        "bytes_per_entry=90\nbytes_per_entry=110\nbytes_per_entry=95\n",
    ));
    let mem = row(&rows, "memory");
    assert_eq!(mem.status, Status::Fail);
    assert!(close(mem.measured.expect("measured"), 110.0));
}

#[test]
fn p95_ignores_a_single_outlier_in_20_but_p99_does_not() {
    let mut text = String::new();
    for _ in 0..19 {
        text.push_str("engine_2m_ms=5\nfreshness_ms=100\n");
    }
    text.push_str("engine_2m_ms=500\nfreshness_ms=5000\n");
    let rows = gate1::evaluate(&gate1::parse_results(&text));
    assert_eq!(row(&rows, "goal2_engine_2m").status, Status::Pass);
    assert_eq!(row(&rows, "goal3_freshness").status, Status::Fail);
}

#[test]
fn stretch_goals_are_reported_but_do_not_change_the_verdict() {
    let rows = gate1::evaluate(&gate1::parse_results("launch_ms=800\nfreshness_ms=200\n"));
    assert_eq!(row(&rows, "goal1_launch").stretch_met, Some(true));
    assert_eq!(row(&rows, "goal3_freshness").stretch_met, Some(true));

    let rows = gate1::evaluate(&gate1::parse_results("launch_ms=1500\nfreshness_ms=900\n"));
    let launch = row(&rows, "goal1_launch");
    assert_eq!(launch.status, Status::Pass);
    assert_eq!(launch.stretch_met, Some(false));
    let fresh = row(&rows, "goal3_freshness");
    assert_eq!(fresh.status, Status::Pass);
    assert_eq!(fresh.stretch_met, Some(false));

    // Criteria without a stretch goal never report one.
    assert_eq!(row(&rows, "memory").stretch_met, None);
}

#[test]
fn status_labels() {
    assert_eq!(Status::Pass.as_str(), "PASS");
    assert_eq!(Status::Fail.as_str(), "FAIL");
    assert_eq!(Status::NotMeasured.as_str(), "NOT MEASURED");
}

// ---------------------------------------------------------------------------
// Gate verdicts: cold launches and gate scale
// ---------------------------------------------------------------------------

/// Rows that need a gate scale; `memory`, `syntax` and `cli` do not.
const SCALED_IDS: [&str; 9] = [
    "goal1_launch",
    "goal2_engine_2m",
    "goal2_engine_5m",
    "goal2_regex",
    "goal2_fullpath",
    "goal3_freshness",
    "catchup_equal",
    "catchup_time",
    "catchup_usable",
];

fn gate(text: &str, cache: CacheState) -> Vec<CriterionResult> {
    gate1::evaluate_gate(&gate1::parse_results(text), cache)
}

#[test]
fn evaluate_gate_returns_one_row_per_criterion_in_order() {
    let crit = gate1::criteria();
    for cache in [CacheState::Warm, CacheState::Cold, CacheState::Unknown] {
        let rows = gate1::evaluate_gate(&Results::default(), cache);
        assert_eq!(rows.len(), crit.len());
        for (r, c) in rows.iter().zip(&crit) {
            assert_eq!(r.id.to_string(), c.id.to_string());
            assert_eq!(r.status, Status::NotMeasured, "{} with no results", r.id);
        }
    }
}

#[test]
fn full_scale_good_numbers_pass_the_gate_warm() {
    let rows = gate(&passing_text(), CacheState::Warm);
    for r in &rows {
        assert_eq!(r.status, Status::Pass, "{} should pass: {}", r.id, r.detail);
    }
    assert!(gate1::all_pass(&rows));
}

#[test]
fn cold_launch_with_one_slow_sample_over_2s_fails() {
    let rows = gate("launch_ms=2500\nui_entries=2000000\n", CacheState::Cold);
    let launch = row(&rows, "goal1_launch");
    assert_eq!(launch.status, Status::Fail, "{}", launch.detail);
    assert!(close(launch.measured.expect("measured"), 2500.0));
}

#[test]
fn cold_launch_is_judged_by_the_worst_sample_not_p95() {
    // 20 fast launches and one slow one: the nearest-rank p95 of 21 samples
    // is a fast one, so a warm pass passes; a cold pass must not drop it.
    let mut text = String::from("ui_entries=2000000\n");
    for _ in 0..20 {
        text.push_str("launch_ms=800\n");
    }
    text.push_str("launch_ms=2500\n");

    let cold = gate(&text, CacheState::Cold);
    let launch = row(&cold, "goal1_launch");
    assert_eq!(launch.status, Status::Fail, "{}", launch.detail);
    assert!(close(launch.measured.expect("measured"), 2500.0));

    let warm = gate(&text, CacheState::Warm);
    let launch = row(&warm, "goal1_launch");
    assert_eq!(launch.status, Status::Pass, "{}", launch.detail);
    assert!(close(launch.measured.expect("measured"), 800.0));
}

#[test]
fn a_single_cold_launch_within_budget_passes() {
    let rows = gate("launch_ms=1500\nui_entries=2000000\n", CacheState::Cold);
    let launch = row(&rows, "goal1_launch");
    assert_eq!(launch.status, Status::Pass, "{}", launch.detail);
}

#[test]
fn warm_launch_needs_20_launches() {
    let rows = gate(
        "launch_ms=800\nlaunch_ms=800\nlaunch_ms=800\nui_entries=2000000\n",
        CacheState::Warm,
    );
    let launch = row(&rows, "goal1_launch");
    assert_eq!(launch.status, Status::NotMeasured, "{}", launch.detail);
    assert!(
        launch.detail.contains("below gate scale"),
        "{}",
        launch.detail
    );
}

#[test]
fn quick_scale_rows_are_not_measured_below_gate_scale() {
    let rows = gate(&quick_text(), CacheState::Warm);
    for id in SCALED_IDS.iter().copied().chain(["first_run"]) {
        let r = row(&rows, id);
        assert_eq!(
            r.status,
            Status::NotMeasured,
            "{id} at quick scale: {}",
            r.detail
        );
        assert!(r.detail.contains("below gate scale"), "{id}: {}", r.detail);
    }
    let engine = row(&rows, "goal2_engine_2m");
    assert!(
        mentions_any(&engine.detail, &["20000", "20,000", "20k", "20 k"]),
        "the note names the measured scale: {}",
        engine.detail
    );
    // The CI-checkable rows are still judged at small scale.
    for id in ["memory", "syntax", "cli"] {
        assert_eq!(row(&rows, id).status, Status::Pass, "{id}");
    }
    assert!(!gate1::all_pass(&rows));
}

#[test]
fn below_scale_never_fails_or_passes() {
    let rows = gate(
        "engine_2m_ms=500\nengine_2m_entries=20000\n\
         freshness_ms=5000\nfreshness_ops=100\n",
        CacheState::Warm,
    );
    for id in ["goal2_engine_2m", "goal3_freshness"] {
        assert_eq!(row(&rows, id).status, Status::NotMeasured, "{id}");
    }
}

#[test]
fn missing_scale_is_not_measured() {
    let text = passing_text().replace(FULL_SCALE, "");
    assert!(
        !text.contains("_entries="),
        "test text has no scale metrics"
    );
    let rows = gate(&text, CacheState::Warm);
    for id in SCALED_IDS.iter().copied().chain(["first_run"]) {
        assert_eq!(
            row(&rows, id).status,
            Status::NotMeasured,
            "{id} without a scale metric"
        );
    }
    assert_eq!(row(&rows, "memory").status, Status::Pass);
}

#[test]
fn full_scale_detail_shows_the_measured_scale() {
    let rows = gate(&passing_text(), CacheState::Warm);
    for (id, needles) in [
        (
            "goal2_engine_2m",
            &["2000000", "2,000,000", "2M", "2 M"][..],
        ),
        (
            "goal2_engine_5m",
            &["5000000", "5,000,000", "5M", "5 M"][..],
        ),
        ("goal3_freshness", &["1000", "1,000"][..]),
        ("catchup_time", &["10000", "10,000", "10k", "10 k"][..]),
    ] {
        let r = row(&rows, id);
        assert!(mentions_any(&r.detail, needles), "{id}: {}", r.detail);
    }
}

#[test]
fn cli_judges_a_cold_pass_by_the_slowest_launch() {
    let dir = Scratch::new("cli-cold");
    dir.write(
        "results.log",
        &format!("{}launch_ms=2500\n", passing_text()),
    );
    let dir_s = dir.path().to_str().expect("utf-8 temp path");
    let run =
        |cache: &str| fx_bench(&["phase1-summary", "--machine", "ci", "--cache", cache, dir_s]);

    let cold = run("cold");
    assert_eq!(cold.status.code(), Some(1), "{}", describe(&cold));
    let md = dir.summary();
    let line = summary_line(&md, "goal1_launch");
    assert!(line.contains("FAIL"), "{line}");

    let warm = run("warm");
    assert_eq!(warm.status.code(), Some(0), "{}", describe(&warm));
}

#[test]
fn cli_marks_a_quick_scale_run_below_gate_scale() {
    let dir = Scratch::new("cli-quick");
    dir.write("gate-metrics.txt", &quick_text());
    let dir_s = dir.path().to_str().expect("utf-8 temp path");
    let out = fx_bench(&[
        "phase1-summary",
        "--machine",
        "ci",
        "--cache",
        "warm",
        dir_s,
    ]);
    assert_eq!(out.status.code(), Some(1), "{}", describe(&out));
    let md = dir.summary();
    for id in ["goal2_engine_2m", "goal2_engine_5m", "goal1_launch"] {
        let line = summary_line(&md, id);
        assert!(line.contains("NOT MEASURED"), "{line}");
        assert!(line.contains("below gate scale"), "{line}");
        assert!(!line.contains("PASS"), "{line}");
        assert!(!line.contains("FAIL"), "{line}");
    }
    assert!(summary_line(&md, "memory").contains("PASS"), "{md}");
}

// ---------------------------------------------------------------------------
// summary.md rendering
// ---------------------------------------------------------------------------

#[test]
fn summary_records_machine_cache_and_every_criterion() {
    let rows = gate1::evaluate(&gate1::parse_results(
        "launch_ms=800\nbytes_per_entry=150\ncli_ok=true\n",
    ));
    let md = gate1::render_summary(&rows, "M4 Pro 24GB", CacheState::Cold);
    assert!(md.contains("Machine: M4 Pro 24GB"), "{md}");
    assert!(md.contains("Cache: cold"), "{md}");
    for id in ALL_IDS {
        assert!(md.contains(id), "summary lacks {id}:\n{md}");
    }
    assert!(md.contains("PASS"), "{md}");
    assert!(md.contains("FAIL"), "{md}");
    assert!(md.contains("NOT MEASURED"), "{md}");
    assert!(md.contains('|'), "summary is a markdown table:\n{md}");

    let warm = gate1::render_summary(&rows, "ci", CacheState::Warm);
    assert!(warm.contains("Machine: ci"), "{warm}");
    assert!(warm.contains("Cache: warm"), "{warm}");
}

// ---------------------------------------------------------------------------
// fx-bench phase1-summary
// ---------------------------------------------------------------------------

#[test]
fn cli_writes_summary_md_and_exits_0_when_all_pass() {
    let dir = Scratch::new("cli-pass");
    dir.write("results.log", &passing_text());
    let dir_s = dir.path().to_str().expect("utf-8 temp path");
    let out = fx_bench(&[
        "phase1-summary",
        "--machine",
        "ci",
        "--cache",
        "warm",
        dir_s,
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    let md = dir.summary();
    assert!(md.contains("Machine: ci"), "{md}");
    assert!(md.contains("Cache: warm"), "{md}");
    for id in ALL_IDS {
        assert!(md.contains(id), "summary lacks {id}:\n{md}");
    }
}

#[test]
fn cli_exits_1_when_some_criterion_is_not_measured_or_fails() {
    let dir = Scratch::new("cli-partial");
    dir.write("memory.log", "bytes_per_entry=80\n");
    dir.write("cli.jsonl", "{\"metric\":\"cli_ok\",\"value\":true}\n");
    let dir_s = dir.path().to_str().expect("utf-8 temp path");
    let out = fx_bench(&[
        "phase1-summary",
        "--machine",
        "ci",
        "--cache",
        "cold",
        dir_s,
    ]);
    assert_eq!(out.status.code(), Some(1), "{}", describe(&out));
    let md = dir.summary();
    assert!(md.contains("NOT MEASURED"), "{md}");
    assert!(md.contains("Cache: cold"), "{md}");
}

#[test]
fn cli_rerun_ignores_its_own_summary() {
    let dir = Scratch::new("cli-rerun");
    dir.write("results.log", &passing_text());
    let dir_s = dir.path().to_str().expect("utf-8 temp path");
    let args = [
        "phase1-summary",
        "--machine",
        "ci",
        "--cache",
        "warm",
        dir_s,
    ];
    let first = fx_bench(&args);
    assert_eq!(first.status.code(), Some(0), "{}", describe(&first));
    let md1 = dir.summary();
    let second = fx_bench(&args);
    assert_eq!(second.status.code(), Some(0), "{}", describe(&second));
    assert_eq!(md1, dir.summary());
}

#[test]
fn cli_usage_errors_exit_2() {
    let dir = Scratch::new("cli-usage");
    let dir_s = dir.path().to_str().expect("utf-8 temp path");
    let missing = dir.path().join("does-not-exist");
    let missing_s = missing.to_str().expect("utf-8 temp path");
    for args in [
        vec!["phase1-summary"],
        vec!["phase1-summary", "--machine", "ci", dir_s],
        vec!["phase1-summary", "--cache", "warm", dir_s],
        vec![
            "phase1-summary",
            "--machine",
            "ci",
            "--cache",
            "tepid",
            dir_s,
        ],
        vec!["phase1-summary", "--machine", "ci", "--cache", "warm"],
        vec![
            "phase1-summary",
            "--machine",
            "ci",
            "--cache",
            "warm",
            missing_s,
        ],
    ] {
        let out = fx_bench(&args);
        assert_eq!(out.status.code(), Some(2), "{args:?}\n{}", describe(&out));
    }
    assert!(!missing.exists(), "an unreadable DIR must not be created");
}

#[test]
fn phase0_gate_summary_still_works() {
    let dir = Scratch::new("phase0");
    dir.write("a.log", "GATE live p95 <= 50 ms: PASS (12 ms)\n");
    let log = dir.path().join("a.log");
    let log_s = log.to_str().expect("utf-8 temp path");
    let out = fx_bench(&["gate-summary", "--machine", "m", "--cache", "warm", log_s]);
    assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
}
