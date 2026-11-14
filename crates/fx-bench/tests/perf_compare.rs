//! `fx-bench perf-compare`, driven through the built binary: the baseline
//! file and metrics files it reads, the verdict of each metric, the report
//! and the exit codes. The parser's line-by-line rules and the aggregates
//! are also unit-tested in `src/perf.rs`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

fn fx_bench(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fx-bench"))
        .args(args)
        .output()
        .expect("run fx-bench binary")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn describe(out: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        stdout(out),
        stderr(out)
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
            "fx-bench-perf-compare-{tag}-{}-{}-{nanos}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).expect("create scratch dir");
        Scratch(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// Writes `text` to `name` in the scratch directory and returns its path
    /// as a string.
    fn write(&self, name: &str, text: &str) -> String {
        let p = self.0.join(name);
        fs::write(&p, text).expect("write fixture");
        p.to_str().expect("utf-8 temp path").to_owned()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const META: &str = "\
# Performance baseline used by the tests.
[meta]
machine = \"m4pro\"
cache = \"warm\"
build = \"0.3.1\"
date = \"2026-11-09\"
";

/// One metric table with the required keys, `better` and `tiers` as
/// given, plus the extra `key = value` lines in `extra`.
fn table(id: &str, key: &str, better: &str, tiers: &str, extra: &str) -> String {
    format!(
        "\n[{id}]\nname = \"Metric {id}\"\nkey = \"{key}\"\nunit = \"ms\"\n\
         better = \"{better}\"\naggregate = \"median\"\ntiers = \"{tiers}\"\n{extra}"
    )
}

/// Runs `perf-compare` on a baseline made of [`META`] and `tables`, with one
/// metrics file per entry of `metrics`, at `tier`, adding `extra` flags.
fn compare(tier: &str, tables: &[String], metrics: &[&str], extra: &[&str]) -> Output {
    let s = Scratch::new("run");
    let baseline = s.write("baseline.toml", &format!("{META}{}", tables.concat()));
    let files: Vec<String> = metrics
        .iter()
        .enumerate()
        .map(|(i, text)| s.write(&format!("metrics-{i}.txt"), text))
        .collect();
    let mut args = vec!["perf-compare", "--baseline", &baseline, "--tier", tier];
    args.extend_from_slice(extra);
    args.extend(files.iter().map(String::as_str));
    fx_bench(&args)
}

/// The cells of the table row whose ID cell is `id`.
fn row(text: &str, id: &str) -> Vec<String> {
    let prefix = format!("| {id} |");
    let line = text
        .lines()
        .find(|l| l.starts_with(&prefix))
        .unwrap_or_else(|| panic!("no row for {id}:\n{text}"));
    line.trim()
        .trim_matches('|')
        .split(" | ")
        .map(|c| c.trim().to_owned())
        .collect()
}

/// The Verdict cell of `id`'s row.
fn verdict(text: &str, id: &str) -> String {
    let cells = row(text, id);
    assert_eq!(cells.len(), 7, "row {id} must have 7 cells: {cells:?}");
    cells[6].clone()
}

fn summary_line(text: &str) -> String {
    text.lines()
        .find(|l| l.starts_with("perf-compare: "))
        .unwrap_or_else(|| panic!("no summary line:\n{text}"))
        .to_owned()
}

fn assert_exit(out: &Output, code: i32) {
    assert_eq!(out.status.code(), Some(code), "{}", describe(out));
}

// ---------------------------------------------------------------------------
// Report format
// ---------------------------------------------------------------------------

#[test]
fn report_has_header_table_and_summary_line() {
    let t = table(
        "S1",
        "engine_2m_ms",
        "lower",
        "quick pr rc",
        "baseline = 10\n",
    );
    let out = compare("pr", &[t], &["engine_2m_ms=11\n"], &[]);
    assert_exit(&out, 1);
    let text = stdout(&out);
    for want in ["pr", "m4pro", "warm", "0.3.1", "2026-11-09"] {
        assert!(text.contains(want), "header lacks {want}:\n{text}");
    }
    assert!(
        text.contains("| ID | Metric | Baseline | Now | Change | Target | Verdict |"),
        "{text}"
    );
    let cells = row(&text, "S1");
    assert_eq!(cells[1], "Metric S1");
    assert_eq!(cells[2], "10 ms");
    assert_eq!(cells[3], "11 ms");
    assert_eq!(cells[4], "+10.0%");
    assert_eq!(cells[5], "-");
    assert!(cells[6].starts_with("FAIL"), "{cells:?}");
    assert_eq!(
        summary_line(&text),
        "perf-compare: 0 pass, 1 fail, 0 not measured, 0 no baseline, 0 tracked, 0 skipped, 0 known gaps"
    );
}

#[test]
fn change_is_signed() {
    let t = table(
        "S1",
        "a_ms",
        "lower",
        "pr",
        "baseline = 10\nallow_pct = 50\n",
    );
    let out = compare("pr", &[t], &["a_ms=9\n"], &[]);
    assert_exit(&out, 0);
    assert_eq!(row(&stdout(&out), "S1")[4], "-10.0%");
}

// ---------------------------------------------------------------------------
// Targets and regressions
// ---------------------------------------------------------------------------

#[test]
fn a_target_miss_fails() {
    // Within the regression allowance (15 + 20% = 18) but over the target.
    let t = table(
        "S1",
        "engine_2m_ms",
        "lower",
        "pr rc",
        "target = 16\nbaseline = 15\nallow_pct = 20\n",
    );
    let out = compare("pr", &[t], &["engine_2m_ms=17\n"], &[]);
    assert_exit(&out, 1);
    let text = stdout(&out);
    let v = verdict(&text, "S1");
    assert!(v.starts_with("FAIL"), "{v}");
    assert!(v.contains("target"), "{v}");
    assert!(!v.contains("regression"), "{v}");
    assert_eq!(row(&text, "S1")[5], "<= 16 ms");
}

/// At the pr and rc tiers a metric with a target is judged on it alone, so
/// a baseline that misses the target no longer holds it: a miss fails
/// unless the gate file records it as a known gap.
#[test]
fn a_target_the_baseline_misses_still_fails() {
    let t = table(
        "M1",
        "footprint_mb",
        "lower",
        "pr rc",
        "target = 200\nbaseline = 1500\nallow_pct = 5\n",
    );
    for tier in ["pr", "rc"] {
        let out = compare(
            tier,
            std::slice::from_ref(&t),
            &["footprint_mb=1550\n"],
            &[],
        );
        assert_exit(&out, 1);
        assert_eq!(verdict(&stdout(&out), "M1"), "FAIL (target)");
    }
    let out = compare("pr", &[t], &["footprint_mb=150\n"], &[]);
    assert_exit(&out, 0);
    assert_eq!(verdict(&stdout(&out), "M1"), "PASS");
}

/// A known gap is a missed target the gate file names with the work that
/// fixes it. It is reported and counted but doesn't fail the gate; once the
/// target is met, the report says the gap can be removed, so it blocks again.
#[test]
fn a_known_gap_misses_without_failing() {
    let t = table(
        "M1",
        "footprint_mb",
        "lower",
        "pr rc",
        "target = 200\nknown_gap = \"Lower memory\"\n",
    );
    let out = compare("rc", std::slice::from_ref(&t), &["footprint_mb=250\n"], &[]);
    assert_exit(&out, 0);
    let text = stdout(&out);
    assert_eq!(verdict(&text, "M1"), "KNOWN GAP (Lower memory)");
    assert_eq!(
        summary_line(&text),
        "perf-compare: 0 pass, 0 fail, 0 not measured, 0 no baseline, 0 tracked, 0 skipped, 1 known gaps"
    );
    let out = compare("rc", &[t], &["footprint_mb=150\n"], &[]);
    assert_exit(&out, 0);
    let text = stdout(&out);
    assert_eq!(verdict(&text, "M1"), "PASS (known gap met at this tier)");
    assert!(
        summary_line(&text).starts_with("perf-compare: 1 pass, 0 fail,"),
        "{text}"
    );
}

/// The JSON report names the known gap and counts it.
#[test]
fn a_known_gap_shows_in_json() {
    let t = table(
        "H1",
        "rate",
        "higher",
        "pr",
        "target = 100\nknown_gap = \"Faster rate\"\n",
    );
    let out = compare("pr", &[t], &["rate=48\n"], &["--format", "json"]);
    assert_exit(&out, 0);
    let text = stdout(&out);
    for want in [
        "\"verdict\":\"KNOWN GAP\"",
        "\"known_gap\":\"Faster rate\"",
        "\"known_gaps\":1",
        "\"result\":\"PASS\"",
    ] {
        assert!(text.contains(want), "json lacks {want}:\n{text}");
    }
}

/// Without a baseline the target is the only check, so a miss fails.
#[test]
fn a_target_miss_without_a_baseline_fails() {
    let t = table("B3", "burst_pct", "lower", "pr", "target = 100\n");
    let out = compare("pr", &[t], &["burst_pct=150\n"], &[]);
    assert_exit(&out, 1);
    assert_eq!(verdict(&stdout(&out), "B3"), "FAIL (target)");
}

/// With a target, the pr and rc tiers judge the target alone: 17 ms is far
/// worse than the 9.3 ms baseline but within 20 ms, so it passes, near the
/// limit.
#[test]
fn with_a_target_the_baseline_is_not_judged() {
    let t = table(
        "S1",
        "engine_2m_ms",
        "lower",
        "pr rc",
        "target = 20\nbaseline = 9.3\nallow_pct = 10\n",
    );
    for tier in ["pr", "rc"] {
        let out = compare(tier, std::slice::from_ref(&t), &["engine_2m_ms=17\n"], &[]);
        assert_exit(&out, 0);
        assert_eq!(
            verdict(&stdout(&out), "S1"),
            "PASS (near limit: 85% of target)"
        );
    }
}

/// A pass at 80% of its target or more is flagged, so a slow drift shows
/// before it fails; an `equal` target has no limit to be near.
#[test]
fn a_pass_at_80_percent_of_its_target_is_near_the_limit() {
    let t = table("S5", "a_ms", "lower", "pr", "target = 10\n");
    for (value, want) in [
        ("7.9", "PASS"),
        ("8", "PASS (near limit: 80% of target)"),
        ("9.5", "PASS (near limit: 95% of target)"),
        ("10", "PASS (near limit: 100% of target)"),
    ] {
        let out = compare(
            "pr",
            std::slice::from_ref(&t),
            &[&format!("a_ms={value}\n")],
            &[],
        );
        assert_exit(&out, 0);
        assert_eq!(verdict(&stdout(&out), "S5"), want, "{value}");
    }
    let h = table("H1", "rate", "higher", "pr", "target = 1000\n");
    for (value, want) in [
        ("1100", "PASS (near limit: 91% of target)"),
        ("1300", "PASS"),
    ] {
        let out = compare(
            "pr",
            std::slice::from_ref(&h),
            &[&format!("rate={value}\n")],
            &[],
        );
        assert_eq!(verdict(&stdout(&out), "H1"), want, "{value}");
    }
    let e = table("C1", "missed", "equal", "pr", "target = 0\n");
    let out = compare("pr", &[e], &["missed=0\n"], &[]);
    assert_eq!(verdict(&stdout(&out), "C1"), "PASS");
    // Nor has a yes-or-no metric.
    let b = table("C2", "equal_flag", "higher", "pr", "target = 1\n")
        .replace("unit = \"ms\"", "unit = \"bool\"");
    let out = compare("pr", &[b], &["equal_flag=true\n"], &[]);
    assert_eq!(verdict(&stdout(&out), "C2"), "PASS");
}

/// `per_key` divides a metric by another key's median, scaled by
/// `per_scale`: memory per million files, or one memory as a multiple of
/// another. Without the other key's samples the metric is NOT MEASURED.
#[test]
fn per_key_divides_by_another_key() {
    let t = table(
        "M1",
        "footprint_mb",
        "lower",
        "pr",
        "target = 60\nper_key = \"index_entries\"\nper_scale = 1000000\n",
    );
    let out = compare(
        "pr",
        std::slice::from_ref(&t),
        &["footprint_mb=96.5\nindex_entries=2077758\n"],
        &[],
    );
    assert_exit(&out, 0);
    let text = stdout(&out);
    assert!(row(&text, "M1")[3].starts_with("46.44"), "{text}");
    // 46.4 is 77% of the target: a plain pass.
    assert_eq!(verdict(&text, "M1"), "PASS");

    let out = compare("pr", &[t], &["footprint_mb=96.5\n"], &[]);
    assert_exit(&out, 1);
    assert_eq!(
        verdict(&stdout(&out), "M1"),
        "NOT MEASURED (no index_entries samples)"
    );

    let ratio = table(
        "M3",
        "peak_mb",
        "lower",
        "pr",
        "target = 2\nper_key = \"hidden_mb\"\nper_scale = 1\n",
    );
    let out = compare("pr", &[ratio], &["peak_mb=388\nhidden_mb=96.5\n"], &[]);
    assert_exit(&out, 1);
    let text = stdout(&out);
    assert!(row(&text, "M3")[3].starts_with("4.02"), "{text}");
    assert_eq!(verdict(&text, "M3"), "FAIL (target)");
}

/// The M1 target, the slowest supported Mac's, is shown beside the gate's.
#[test]
fn the_m1_target_shows_beside_the_target() {
    let t = table("S1", "a_ms", "lower", "pr", "target = 6\nm1_target = 16\n");
    let out = compare("pr", &[t], &["a_ms=5\n"], &[]);
    assert_eq!(row(&stdout(&out), "S1")[5], "<= 6 ms (M1: <= 16 ms)");
}

#[test]
fn the_floor_widens_a_small_allowance() {
    // Baseline 1.0 ms with 10% and a 1 ms floor allows up to 2.0 ms.
    let t = table(
        "S1",
        "a_ms",
        "lower",
        "pr",
        "baseline = 1.0\nallow_pct = 10\nallow_floor = 1\n",
    );
    let pass = compare("pr", std::slice::from_ref(&t), &["a_ms=2.0\n"], &[]);
    assert_exit(&pass, 0);
    assert_eq!(verdict(&stdout(&pass), "S1"), "PASS");

    let fail = compare("pr", &[t], &["a_ms=2.01\n"], &[]);
    assert_exit(&fail, 1);
    let v = verdict(&stdout(&fail), "S1");
    assert!(v.starts_with("FAIL") && v.contains("regression"), "{v}");
}

#[test]
fn the_percentage_wins_when_larger_than_the_floor() {
    let t = table(
        "S1",
        "a_ms",
        "lower",
        "pr",
        "baseline = 100\nallow_pct = 10\nallow_floor = 1\n",
    );
    assert_exit(
        &compare("pr", std::slice::from_ref(&t), &["a_ms=110\n"], &[]),
        0,
    );
    assert_exit(&compare("pr", &[t], &["a_ms=110.5\n"], &[]), 1);
}

#[test]
fn without_allowance_any_worsening_fails() {
    let t = table("S1", "a_ms", "lower", "pr", "baseline = 5\n");
    assert_exit(
        &compare("pr", std::slice::from_ref(&t), &["a_ms=5\n"], &[]),
        0,
    );
    assert_exit(&compare("pr", &[t], &["a_ms=5.001\n"], &[]), 1);
}

#[test]
fn higher_is_better() {
    // Target: at least 1000. At the pr tier the target alone decides.
    let t = table(
        "B1",
        "rate_per_s",
        "higher",
        "pr",
        "target = 1000\nbaseline = 1200\nallow_pct = 10\n",
    );
    let ok = compare("pr", std::slice::from_ref(&t), &["rate_per_s=1050\n"], &[]);
    assert_exit(&ok, 0);
    let text = stdout(&ok);
    assert_eq!(row(&text, "B1")[5], ">= 1000 ms");
    assert_eq!(verdict(&text, "B1"), "PASS (near limit: 95% of target)");

    let missed = compare("pr", &[t], &["rate_per_s=990\n"], &[]);
    assert_exit(&missed, 1);
    assert_eq!(verdict(&stdout(&missed), "B1"), "FAIL (target)");
}

#[test]
fn equal_relies_on_the_target() {
    let t = table(
        "C1",
        "catchup_equal",
        "equal",
        "pr",
        "target = 1\nbaseline = 1\n",
    );
    let ok = compare(
        "pr",
        std::slice::from_ref(&t),
        &["catchup_equal=true\n"],
        &[],
    );
    assert_exit(&ok, 0);
    assert_eq!(verdict(&stdout(&ok), "C1"), "PASS");

    let bad = compare("pr", &[t], &["catchup_equal=false\n"], &[]);
    assert_exit(&bad, 1);
    let v = verdict(&stdout(&bad), "C1");
    assert!(v.starts_with("FAIL") && v.contains("target"), "{v}");

    // No regression check: a value far from the baseline that meets the
    // target passes.
    let t = table("C2", "n", "equal", "pr", "target = 7\nbaseline = 5\n");
    let out = compare("pr", &[t], &["n=7\n"], &[]);
    assert_exit(&out, 0);
    assert_eq!(verdict(&stdout(&out), "C2"), "PASS");
}

#[test]
fn bool_units_show_true_and_false() {
    let t = "\n[C1]\nname = \"Catch-up equals a fresh scan\"\nkey = \"catchup_equal\"\n\
             unit = \"bool\"\nbetter = \"equal\"\naggregate = \"min\"\ntiers = \"pr\"\n\
             target = 1\n"
        .to_owned();
    let out = compare(
        "pr",
        &[t],
        &["catchup_equal=true\ncatchup_equal=false\n"],
        &[],
    );
    assert_exit(&out, 1);
    let cells = row(&stdout(&out), "C1");
    assert_eq!(cells[3], "false");
    assert_eq!(cells[5], "= true");
}

// ---------------------------------------------------------------------------
// Aggregates
// ---------------------------------------------------------------------------

#[test]
fn every_aggregate_combines_samples_across_files() {
    let mut tables = Vec::new();
    for (id, agg) in [
        ("A1", "median"),
        ("A2", "p95"),
        ("A3", "max"),
        ("A4", "min"),
        ("A5", "mean"),
        ("A6", "last"),
    ] {
        tables.push(format!(
            "\n[{id}]\nname = \"{agg}\"\nkey = \"x_ms\"\nunit = \"ms\"\nbetter = \"lower\"\n\
             aggregate = \"{agg}\"\ntiers = \"pr\"\n"
        ));
    }
    // Sorted samples: 1 2 4 7 10. The last line read is 7.
    let out = compare(
        "pr",
        &tables,
        &[
            "x_ms=10\nx_ms=1\n# a comment\n\nx_ms=4\n",
            "x_ms=2\nx_ms=7\n",
        ],
        &["--print-values"],
    );
    assert_exit(&out, 0);
    let text = stdout(&out);
    for want in [
        "A1 x_ms = 4",
        "A2 x_ms = 10",
        "A3 x_ms = 10",
        "A4 x_ms = 1",
        "A5 x_ms = 4.8",
        "A6 x_ms = 7",
    ] {
        assert!(
            text.lines().any(|l| l == want),
            "missing value line {want:?}:\n{text}"
        );
    }
}

#[test]
fn print_values_skips_unmeasured_metrics() {
    let tables = [
        table("S1", "a_ms", "lower", "pr", ""),
        table("S2", "b_ms", "lower", "pr", ""),
        table("S3", "c_ms", "lower", "rc", ""),
    ];
    let out = compare("pr", &tables, &["a_ms=3\nc_ms=1\n"], &["--print-values"]);
    let text = stdout(&out);
    assert!(text.lines().any(|l| l == "S1 a_ms = 3"), "{text}");
    assert!(!text.contains("S2 b_ms ="), "{text}");
    assert!(!text.contains("S3 c_ms ="), "{text}");
}

// ---------------------------------------------------------------------------
// Missing metrics, scale, ungated metrics, --partial
// ---------------------------------------------------------------------------

#[test]
fn a_missing_metric_fails_the_run() {
    let tables = [
        table("S1", "a_ms", "lower", "pr", "baseline = 5\n"),
        table("S2", "b_ms", "lower", "pr", "baseline = 5\n"),
    ];
    let out = compare("pr", &tables, &["a_ms=4\n"], &[]);
    assert_exit(&out, 1);
    let text = stdout(&out);
    assert_eq!(verdict(&text, "S1"), "PASS");
    let v = verdict(&text, "S2");
    assert!(v.starts_with("NOT MEASURED"), "{v}");
    assert!(v.contains("b_ms"), "the reason names the key: {v}");
    assert!(summary_line(&text).contains("1 not measured"), "{text}");
}

#[test]
fn below_scale_is_not_measured_at_pr_and_rc() {
    let t = table(
        "S1",
        "engine_2m_ms",
        "lower",
        "quick pr rc",
        "scale_key = \"engine_2m_entries\"\nscale_min = 2000000\nbaseline = 9\n\
         quick_baseline = 1\nallow_pct = 10\n",
    );
    let small = "engine_2m_ms=1\nengine_2m_entries=100000\n";
    for tier in ["pr", "rc"] {
        let out = compare(tier, std::slice::from_ref(&t), &[small], &[]);
        assert_exit(&out, 1);
        let v = verdict(&stdout(&out), "S1");
        assert!(v.starts_with("NOT MEASURED"), "{tier}: {v}");
        assert!(v.contains("scale"), "{tier}: {v}");
    }
    // The scale key missing while scale_min is set is not measured either.
    let out = compare("pr", std::slice::from_ref(&t), &["engine_2m_ms=1\n"], &[]);
    assert_exit(&out, 1);
    assert!(verdict(&stdout(&out), "S1").starts_with("NOT MEASURED"));

    // At gate scale it is judged.
    let out = compare(
        "pr",
        std::slice::from_ref(&t),
        &["engine_2m_ms=9\nengine_2m_entries=2000000\n"],
        &[],
    );
    assert_exit(&out, 0);
    assert_eq!(verdict(&stdout(&out), "S1"), "PASS");

    // The quick tier has no scale check.
    let out = compare("quick", &[t], &[small], &[]);
    assert_exit(&out, 0);
    assert_eq!(verdict(&stdout(&out), "S1"), "PASS");
}

#[test]
fn an_ungated_metric_never_fails() {
    let tables = [
        table(
            "M1",
            "a_ms",
            "lower",
            "pr",
            "baseline = 5\ntarget = 4\ngated = false\n",
        ),
        table("M2", "b_ms", "lower", "pr", "gated = false\n"),
    ];
    let out = compare("pr", &tables, &["a_ms=50\n"], &[]);
    assert_exit(&out, 0);
    let text = stdout(&out);
    let v = verdict(&text, "M1");
    assert!(v.starts_with("TRACKED") && v.contains("FAIL"), "{v}");
    let v = verdict(&text, "M2");
    assert!(
        v.starts_with("TRACKED") && v.contains("NOT MEASURED"),
        "{v}"
    );
    assert!(summary_line(&text).contains("2 tracked"), "{text}");
}

#[test]
fn partial_skips_metrics_absent_from_every_file() {
    let tables = [
        table("S1", "a_ms", "lower", "pr", "baseline = 5\n"),
        table("S2", "b_ms", "lower", "pr", "baseline = 5\n"),
    ];
    let out = compare("pr", &tables, &["a_ms=4\n"], &["--partial"]);
    assert_exit(&out, 0);
    let text = stdout(&out);
    assert_eq!(verdict(&text, "S1"), "PASS");
    assert_eq!(verdict(&text, "S2"), "SKIPPED");
    assert_eq!(
        summary_line(&text),
        "perf-compare: 1 pass, 0 fail, 0 not measured, 0 no baseline, 0 tracked, 1 skipped, 0 known gaps"
    );
}

#[test]
fn partial_still_reports_a_present_metric_below_scale() {
    let t = table(
        "S1",
        "engine_2m_ms",
        "lower",
        "pr",
        "scale_key = \"engine_2m_entries\"\nscale_min = 2000000\nbaseline = 9\n",
    );
    let out = compare(
        "pr",
        &[t],
        &["engine_2m_ms=1\nengine_2m_entries=10\n"],
        &["--partial"],
    );
    assert_exit(&out, 1);
    assert!(verdict(&stdout(&out), "S1").starts_with("NOT MEASURED"));
}

// ---------------------------------------------------------------------------
// Baselines per tier
// ---------------------------------------------------------------------------

#[test]
fn the_quick_tier_uses_quick_baseline_and_ignores_targets() {
    let t = table(
        "S1",
        "a_ms",
        "lower",
        "quick pr",
        // The pr baseline meets the target, so the target is enforced there
        // (a baseline that misses it would leave only the allowance).
        "target = 1.2\nbaseline = 1.1\nquick_baseline = 1.0\nallow_floor = 0.5\n",
    );
    // 1.4 is over the target but within quick_baseline + floor (1.5).
    let quick = compare("quick", std::slice::from_ref(&t), &["a_ms=1.4\n"], &[]);
    assert_exit(&quick, 0);
    let text = stdout(&quick);
    assert_eq!(verdict(&text, "S1"), "PASS");
    let cells = row(&text, "S1");
    assert_eq!(cells[2], "1 ms", "the quick baseline is shown");
    assert_eq!(
        cells[5], "(<= 1.2 ms)",
        "an unchecked target is in parentheses"
    );

    let regressed = compare("quick", std::slice::from_ref(&t), &["a_ms=1.6\n"], &[]);
    assert_exit(&regressed, 1);
    assert!(verdict(&stdout(&regressed), "S1").contains("regression"));

    // The same value at the pr tier misses the target.
    let pr = compare("pr", &[t], &["a_ms=1.4\n"], &[]);
    assert_exit(&pr, 1);
    let v = verdict(&stdout(&pr), "S1");
    assert!(v.contains("target") && !v.contains("regression"), "{v}");
}

#[test]
fn no_baseline_for_the_tier_is_not_a_failure() {
    let t = table("S1", "a_ms", "lower", "quick pr", "baseline = 5\n");
    let out = compare("quick", std::slice::from_ref(&t), &["a_ms=50\n"], &[]);
    assert_exit(&out, 0);
    let text = stdout(&out);
    assert_eq!(verdict(&text, "S1"), "NO BASELINE");
    assert!(summary_line(&text).contains("1 no baseline"), "{text}");

    // A missed target still fails without a baseline.
    let t = table("S2", "b_ms", "lower", "pr", "target = 3\n");
    let out = compare("pr", &[t], &["b_ms=4\n"], &[]);
    assert_exit(&out, 1);
}

#[test]
fn only_metrics_of_the_tier_are_reported() {
    let tables = [
        table("S1", "a_ms", "lower", "pr", ""),
        table("S2", "b_ms", "lower", "rc", "baseline = 1\n"),
    ];
    let out = compare("pr", &tables, &["a_ms=1\n"], &[]);
    assert_exit(&out, 0);
    let text = stdout(&out);
    assert!(text.contains("| S1 |"), "{text}");
    assert!(!text.contains("| S2 |"), "{text}");
}

#[test]
fn the_pr_tier_uses_pr_baseline_when_there_is_one() {
    // The pr tier measures the background under file activity and the rc
    // tier without it, so a background metric carries a baseline for each.
    // Without a target, the baseline judges the metric.
    let t = table(
        "B2",
        "a_ms",
        "lower",
        "pr rc",
        "baseline = 10\npr_baseline = 60\nallow_pct = 10\n",
    );
    // 62 is within 60 + 10% at the pr tier...
    let pr = compare("pr", std::slice::from_ref(&t), &["a_ms=62\n"], &[]);
    assert_exit(&pr, 0);
    let text = stdout(&pr);
    assert_eq!(verdict(&text, "B2"), "PASS");
    assert_eq!(row(&text, "B2")[2], "60 ms", "the pr baseline is shown");
    assert_eq!(row(&text, "B2")[4], "+3.3%");

    // ...and far over 10 + 10% at the rc tier.
    let rc = compare("rc", &[t], &["a_ms=62\n"], &[]);
    assert_exit(&rc, 1);
    let text = stdout(&rc);
    assert!(verdict(&text, "B2").contains("regression"), "{text}");
    assert_eq!(row(&text, "B2")[2], "10 ms");
}

#[test]
fn a_pr_baseline_that_meets_the_target_holds_it_at_the_pr_tier() {
    let t = table(
        "M1",
        "a_ms",
        "lower",
        "pr rc",
        "target = 200\nbaseline = 120\npr_baseline = 130\nallow_floor = 100\n",
    );
    // 210 is within 130 + 100 but over the target.
    let out = compare("pr", &[t], &["a_ms=210\n"], &[]);
    assert_exit(&out, 1);
    let v = verdict(&stdout(&out), "M1");
    assert!(v.contains("target") && !v.contains("regression"), "{v}");
}

#[test]
fn without_a_pr_baseline_the_pr_tier_uses_the_baseline() {
    let t = table(
        "S1",
        "a_ms",
        "lower",
        "pr rc",
        "baseline = 10\nallow_pct = 10\n",
    );
    let out = compare("pr", &[t], &["a_ms=12\n"], &[]);
    assert_exit(&out, 1);
    let text = stdout(&out);
    assert_eq!(row(&text, "S1")[2], "10 ms");
    assert!(verdict(&text, "S1").contains("regression"), "{text}");
}

#[test]
fn the_pr_tier_header_names_the_pr_baselines_build_and_date() {
    // Keys after META's last line still belong to [meta].
    let meta = "pr_build = \"0.4.0\"\npr_date = \"2026-11-12\"\n".to_owned();
    let t = table(
        "B1",
        "a_ms",
        "lower",
        "pr rc",
        "baseline = 1\npr_baseline = 2\n",
    );
    let pr = compare("pr", &[meta.clone(), t.clone()], &["a_ms=2\n"], &[]);
    assert_exit(&pr, 0);
    let text = stdout(&pr);
    assert!(text.contains("PR-tier baseline build: 0.4.0"), "{text}");
    assert!(text.contains("PR-tier baseline date: 2026-11-12"), "{text}");

    let rc = compare("rc", &[meta, t], &["a_ms=1\n"], &[]);
    assert_exit(&rc, 0);
    assert!(!stdout(&rc).contains("PR-tier"), "{}", stdout(&rc));
}

#[test]
fn a_pr_baseline_needs_the_pr_tier_and_a_number() {
    let rc_only = table(
        "B1",
        "a_ms",
        "lower",
        "rc",
        "baseline = 1\npr_baseline = 2\n",
    );
    let out = compare("rc", &[rc_only], &["a_ms=1\n"], &[]);
    assert_exit(&out, 2);
    let err = stderr(&out);
    assert!(err.contains("baseline.toml:"), "{err}");
    assert!(
        err.contains("pr_baseline") && err.contains("tiers"),
        "{err}"
    );

    let text = table("B1", "a_ms", "lower", "pr", "pr_baseline = \"2\"\n");
    let out = compare("pr", &[text], &["a_ms=1\n"], &[]);
    assert_exit(&out, 2);
    assert!(stderr(&out).contains("pr_baseline"), "{}", stderr(&out));
}

// ---------------------------------------------------------------------------
// Exceptions and bad input
// ---------------------------------------------------------------------------

#[test]
fn an_exception_must_name_a_pr() {
    let with = |exception: &str| {
        table(
            "S1",
            "a_ms",
            "lower",
            "pr",
            &format!("baseline = 5\nexception = \"{exception}\"\n"),
        )
    };
    let ok = compare(
        "pr",
        &[with("Raised by #42 after the store rewrite")],
        &["a_ms=5\n"],
        &[],
    );
    assert_exit(&ok, 0);
    assert!(stdout(&ok).contains("#42"), "the exception is shown");

    for bad in ["accepted by the owner", "", "see # 12"] {
        let out = compare("pr", &[with(bad)], &["a_ms=5\n"], &[]);
        assert_exit(&out, 2);
        let err = stderr(&out);
        assert!(err.contains("baseline.toml:"), "{err}");
        assert!(err.contains("exception"), "{err}");
    }
}

#[test]
fn a_malformed_baseline_exits_2_naming_file_and_line() {
    // META has 6 lines and the table text starts with a blank line, so the
    // unknown `[meta]` key is on line 8.
    let out = compare("pr", &["\nbogus = 1\n".to_owned()], &["a=1\n"], &[]);
    assert_exit(&out, 2);
    let err = stderr(&out);
    assert!(err.contains("baseline.toml:8"), "{err}");

    let bad_key = table("S1", "a_ms", "lower", "pr", "targte = 5\n");
    let out = compare("pr", &[bad_key], &["a_ms=1\n"], &[]);
    assert_exit(&out, 2);
    let err = stderr(&out);
    assert!(err.contains("baseline.toml:15"), "{err}");
    assert!(err.contains("targte"), "{err}");
}

#[test]
fn table_ids_may_use_letters_digits_underscore_and_dash() {
    let tables = [
        table("F6", "a_ms", "lower", "pr", ""),
        table("F6u", "b_ms", "lower", "pr", ""),
        table("x_1-b", "c_ms", "lower", "pr", ""),
    ];
    let out = compare("pr", &tables, &["a_ms=1\nb_ms=1\nc_ms=1\n"], &[]);
    assert_exit(&out, 0);
    let text = stdout(&out);
    for id in ["F6", "F6u", "x_1-b"] {
        assert_eq!(verdict(&text, id), "NO BASELINE");
    }
}

#[test]
fn a_malformed_metrics_line_exits_2_naming_file_and_line() {
    let t = table("S1", "a_ms", "lower", "pr", "");
    let out = compare("pr", &[t], &["a_ms=1\n\n# fine\na_ms=fast\n"], &[]);
    assert_exit(&out, 2);
    let err = stderr(&out);
    assert!(err.contains("metrics-0.txt:4"), "{err}");
}

#[test]
fn unreadable_files_exit_2() {
    let s = Scratch::new("unreadable");
    let baseline = s.write("baseline.toml", META);
    let metrics = s.write("m.txt", "a=1\n");
    let missing = s.path().join("nope.txt");
    let missing = missing.to_str().unwrap();
    let out = fx_bench(&[
        "perf-compare",
        "--baseline",
        missing,
        "--tier",
        "pr",
        &metrics,
    ]);
    assert_exit(&out, 2);
    let out = fx_bench(&[
        "perf-compare",
        "--baseline",
        &baseline,
        "--tier",
        "pr",
        missing,
    ]);
    assert_exit(&out, 2);
}

#[test]
fn bad_arguments_exit_2_with_usage() {
    let s = Scratch::new("args");
    let baseline = s.write("baseline.toml", META);
    let metrics = s.write("m.txt", "a=1\n");
    for args in [
        vec!["perf-compare", "--tier", "pr", &metrics],
        vec!["perf-compare", "--baseline", &baseline, &metrics],
        vec![
            "perf-compare",
            "--baseline",
            &baseline,
            "--tier",
            "nightly",
            &metrics,
        ],
        vec!["perf-compare", "--baseline", &baseline, "--tier", "pr"],
        vec![
            "perf-compare",
            "--baseline",
            &baseline,
            "--tier",
            "pr",
            "--format",
            "xml",
            &metrics,
        ],
        vec![
            "perf-compare",
            "--baseline",
            &baseline,
            "--tier",
            "pr",
            "--bogus",
            &metrics,
        ],
        vec![
            "perf-compare",
            "--baseline",
            &baseline,
            "--tier",
            "pr",
            "--format",
            "json",
            "--print-values",
            &metrics,
        ],
    ] {
        let out = fx_bench(&args);
        assert_exit(&out, 2);
        assert!(
            stderr(&out).contains("usage"),
            "{args:?}: {}",
            describe(&out)
        );
    }
}

// ---------------------------------------------------------------------------
// JSON and --out
// ---------------------------------------------------------------------------

#[test]
fn json_output_has_the_same_fields() {
    let tables = [
        table(
            "S1",
            "a_ms",
            "lower",
            "pr",
            "baseline = 10\ntarget = 20\nallow_pct = 20\n",
        ),
        table("S2", "b_ms", "lower", "pr", "baseline = 10\n"),
    ];
    let out = compare("pr", &tables, &["a_ms=11\n"], &["--format", "json"]);
    assert_exit(&out, 1);
    let text = stdout(&out);
    let text = text.trim();
    assert!(text.starts_with('{') && text.ends_with('}'), "{text}");
    for want in [
        "\"tier\":\"pr\"",
        "\"machine\":\"m4pro\"",
        "\"id\":\"S1\"",
        "\"key\":\"a_ms\"",
        "\"baseline\":10",
        "\"now\":11",
        "\"change_pct\":10",
        "\"target\":20",
        "\"verdict\":\"PASS\"",
        "\"verdict\":\"NOT MEASURED\"",
        "\"not_measured\":1",
        "\"skipped\":0",
        "\"known_gaps\":0",
        "\"known_gap\":null",
        "\"result\":\"FAIL\"",
    ] {
        assert!(text.contains(want), "json lacks {want}:\n{text}");
    }
}

#[test]
fn out_writes_a_new_file_and_refuses_to_overwrite() {
    let s = Scratch::new("out");
    let baseline = s.write(
        "baseline.toml",
        &format!(
            "{META}{}",
            table("S1", "a_ms", "lower", "pr", "baseline = 2\n")
        ),
    );
    let metrics = s.write("m.txt", "a_ms=2\n");
    let report = s.path().join("report.md");
    let report = report.to_str().unwrap();
    let args = [
        "perf-compare",
        "--baseline",
        &baseline,
        "--tier",
        "pr",
        "--out",
        report,
        &metrics,
    ];
    let out = fx_bench(&args);
    assert_exit(&out, 0);
    let written = fs::read_to_string(report).expect("report written");
    assert_eq!(written, stdout(&out));

    fs::write(report, "keep").unwrap();
    let again = fx_bench(&args);
    assert_exit(&again, 2);
    assert_eq!(fs::read_to_string(report).unwrap(), "keep");
}

#[test]
fn help_lists_the_new_subcommands() {
    let out = fx_bench(&["help"]);
    assert_exit(&out, 0);
    let text = stdout(&out);
    for sub in ["perf-compare", "sample", "churn"] {
        assert!(text.contains(&format!("fx-bench {sub}")), "{text}");
    }
}
