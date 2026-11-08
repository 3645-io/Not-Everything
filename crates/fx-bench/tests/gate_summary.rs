//! `fx-bench gate-summary` and the `gate` module: parsing the `GATE ...`
//! lines every Phase 0 benchmark prints, and turning log files into one
//! results table (markdown or JSON) with an exit code.
//!
//! Line formats covered (copied from the benchmarks' own `println!`s):
//! - (a) `GATE <criterion>: PASS|FAIL[ (<detail>)]` (fsevents_bench,
//!   load_bench);
//! - (b) `GATE: PASS|FAIL p95=<x> budget=<y>ms` (fx-ui-macos frame_stats);
//! - search_bench's `gate_line`: `gate p95<=10ms over N names on T threads:
//!   PASS|FAIL (worst p95 X ms, query "q")`;
//! - scan_bench's text gates: `PASS|FAIL|SKIP: <name> (<detail>)`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fx_bench::{GateLine, Verdict, parse_gate_lines};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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
            "fx-bench-gate-summary-{tag}-{}-{}-{nanos}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).expect("create scratch dir");
        Scratch(dir)
    }

    fn write(&self, name: &str, text: &str) -> PathBuf {
        let p = self.0.join(name);
        fs::write(&p, text).expect("write log file");
        p
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn path_str(p: &Path) -> &str {
    p.to_str().expect("utf-8 temp path")
}

/// Data rows of the first markdown table in `text`: header and the
/// `|---|` separator are skipped; cells are trimmed.
fn table(text: &str) -> (Vec<String>, Vec<Vec<String>>) {
    let mut lines = text
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('|'))
        .map(|l| {
            l.trim_matches('|')
                .split('|')
                .map(|c| c.trim().to_string())
                .collect::<Vec<String>>()
        });
    let header = lines.next().unwrap_or_default();
    let rows: Vec<Vec<String>> = lines
        .filter(|cells| {
            !cells
                .iter()
                .all(|c| !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':'))
        })
        .collect();
    (header, rows)
}

fn has_cell(row: &[String], value: &str) -> bool {
    row.iter().any(|c| c == value)
}

fn single(text: &str) -> GateLine {
    let mut lines = parse_gate_lines(text);
    assert_eq!(lines.len(), 1, "expected one gate line from {text:?}");
    lines.remove(0)
}

// Exact lines as the benchmarks print them.
const FSEVENTS_LIVE_PASS: &str = "GATE live p95 <= 50 ms: PASS";
const FSEVENTS_REPLAY_FAIL: &str = "GATE replay exact: FAIL";
const LOAD_PASS: &str = "GATE load-to-first-result p95 <= 50 ms: PASS (12.3 ms)";
const FRAME_PASS: &str = "GATE: PASS p95=5.00ms budget=16.7ms";
const FRAME_FAIL: &str = "GATE: FAIL p95=21.40ms budget=16.7ms";
const SEARCH_PASS: &str = "gate p95<=10ms over 1000000 names on 8 threads: PASS \
                           (worst p95 3.210 ms, query \"foo\")";
const SCAN_FAIL: &str = "FAIL: 1-thread throughput >= 180000 entries/s (65000 entries/s at p50)";
const SCAN_ROOT_PASS: &str = "PASS: whole-root scan of / <= 10 s at best thread count \
                              (p50 8.000 s at 4 threads, 1900000 entries)";
const SCAN_SKIP: &str = "SKIP: 1-thread throughput >= 180000 entries/s (1 thread not in --threads)";

// ---------------------------------------------------------------------------
// Parsing: format (a)
// ---------------------------------------------------------------------------

#[test]
fn format_a_pass_without_detail() {
    let g = single(FSEVENTS_LIVE_PASS);
    assert_eq!(g.criterion, "live p95 <= 50 ms");
    assert_eq!(g.verdict, Verdict::Pass);
    assert_eq!(g.detail, None);
}

#[test]
fn format_a_fail_without_detail() {
    let g = single(FSEVENTS_REPLAY_FAIL);
    assert_eq!(g.criterion, "replay exact");
    assert_eq!(g.verdict, Verdict::Fail);
    assert_eq!(g.detail, None);
}

#[test]
fn format_a_with_detail() {
    let g = single(LOAD_PASS);
    assert_eq!(g.criterion, "load-to-first-result p95 <= 50 ms");
    assert_eq!(g.verdict, Verdict::Pass);
    assert_eq!(g.detail.as_deref(), Some("12.3 ms"));

    let g = single("GATE load-to-first-result p95 <= 50 ms: FAIL (73.0 ms)");
    assert_eq!(g.verdict, Verdict::Fail);
    assert_eq!(g.detail.as_deref(), Some("73.0 ms"));
}

#[test]
fn leading_whitespace_is_trimmed() {
    let g = single("   GATE replay exact: PASS  ");
    assert_eq!(g.criterion, "replay exact");
    assert_eq!(g.verdict, Verdict::Pass);
}

// ---------------------------------------------------------------------------
// Parsing: format (b)
// ---------------------------------------------------------------------------

#[test]
fn format_b_pass_and_fail() {
    let g = single(FRAME_PASS);
    assert_eq!(g.verdict, Verdict::Pass);
    assert_eq!(g.criterion, "p95=5.00ms budget=16.7ms");

    let g = single(FRAME_FAIL);
    assert_eq!(g.verdict, Verdict::Fail);
    assert_eq!(g.criterion, "p95=21.40ms budget=16.7ms");
}

#[test]
fn format_b_without_text_is_frame_p95() {
    let g = single("GATE: FAIL");
    assert_eq!(g.verdict, Verdict::Fail);
    assert_eq!(g.criterion, "frame p95");

    let g = single("GATE: PASS");
    assert_eq!(g.verdict, Verdict::Pass);
    assert_eq!(g.criterion, "frame p95");
}

// ---------------------------------------------------------------------------
// Parsing: search_bench and scan_bench
// ---------------------------------------------------------------------------

#[test]
fn search_bench_gate_line() {
    let g = single(SEARCH_PASS);
    assert_eq!(g.verdict, Verdict::Pass);
    assert_eq!(g.criterion, "p95<=10ms over 1000000 names on 8 threads");
    assert_eq!(
        g.detail.as_deref(),
        Some("worst p95 3.210 ms, query \"foo\"")
    );

    let g = single(
        "gate p95<=10ms over 1000000 names on 1 threads: FAIL (worst p95 14.000 ms, query \"a\")",
    );
    assert_eq!(g.verdict, Verdict::Fail);
    assert_eq!(g.criterion, "p95<=10ms over 1000000 names on 1 threads");
}

#[test]
fn scan_bench_gate_lines() {
    let g = single(SCAN_FAIL);
    assert_eq!(g.verdict, Verdict::Fail);
    assert_eq!(g.criterion, "1-thread throughput >= 180000 entries/s");
    assert_eq!(g.detail.as_deref(), Some("65000 entries/s at p50"));

    let g = single(SCAN_ROOT_PASS);
    assert_eq!(g.verdict, Verdict::Pass);
    assert_eq!(
        g.criterion,
        "whole-root scan of / <= 10 s at best thread count"
    );
    assert_eq!(
        g.detail.as_deref(),
        Some("p50 8.000 s at 4 threads, 1900000 entries")
    );
}

#[test]
fn scan_bench_skip_is_reported_but_not_a_pass() {
    let lines = parse_gate_lines(SCAN_SKIP);
    assert_eq!(lines.len(), 1, "a SKIP gate must not be dropped: {lines:?}");
    assert_ne!(lines[0].verdict, Verdict::Pass);
}

// ---------------------------------------------------------------------------
// Parsing: whole logs, unparsed lines
// ---------------------------------------------------------------------------

#[test]
fn whole_log_keeps_order_and_ignores_other_lines() {
    let log = format!(
        "fsevents_bench: machine arm64, cache warm\n\
         p50 12.000 ms p95 30.000 ms\n\
         {FSEVENTS_LIVE_PASS}\n\
         note: the gate passes when ...\n\
         {FSEVENTS_REPLAY_FAIL}\n\
         {LOAD_PASS}\n"
    );
    let lines = parse_gate_lines(&log);
    let got: Vec<(&str, &Verdict)> = lines
        .iter()
        .map(|g| (g.criterion.as_str(), &g.verdict))
        .collect();
    assert_eq!(
        got,
        vec![
            ("live p95 <= 50 ms", &Verdict::Pass),
            ("replay exact", &Verdict::Fail),
            ("load-to-first-result p95 <= 50 ms", &Verdict::Pass),
        ]
    );
}

#[test]
fn text_without_gate_lines_parses_to_nothing() {
    assert!(parse_gate_lines("").is_empty());
    assert!(parse_gate_lines("machine: macos aarch64\np50 1 ms\nscan matches: 3\n").is_empty());
}

#[test]
fn gate_line_without_verdict_is_unparsed_not_dropped() {
    let lines = parse_gate_lines("GATE live p95 <= 50 ms: crashed\nGATE\n");
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines.iter().all(|g| g.verdict == Verdict::Unparsed),
        "{lines:?}"
    );
}

// ---------------------------------------------------------------------------
// Subcommand: markdown output and exit codes
// ---------------------------------------------------------------------------

#[test]
fn all_pass_gives_markdown_table_and_exit_0() {
    let s = Scratch::new("pass");
    let a = s.write(
        "fsevents.log",
        &format!("header\n{FSEVENTS_LIVE_PASS}\nGATE replay exact: PASS\n"),
    );
    let b = s.write("ui.log", &format!("frames: 600\n{FRAME_PASS}\n"));
    let out = fx_bench(&[
        "gate-summary",
        "--machine",
        "m4pro",
        "--cache",
        "warm",
        path_str(&a),
        path_str(&b),
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", describe(&out));

    let text = stdout(&out);
    let (header, rows) = table(&text);
    let header_lc = header.join(" ").to_lowercase();
    for col in ["criterion", "verdict", "machine", "cache"] {
        assert!(header_lc.contains(col), "header lacks {col}: {header:?}");
    }
    assert_eq!(rows.len(), 3, "one row per gate line:\n{text}");
    for row in &rows {
        assert!(has_cell(row, "PASS"), "{row:?}");
        assert!(has_cell(row, "m4pro"), "{row:?}");
        assert!(has_cell(row, "warm"), "{row:?}");
    }
    assert!(has_cell(&rows[0], "fsevents.log"), "{:?}", rows[0]);
    assert!(has_cell(&rows[0], "live p95 <= 50 ms"), "{:?}", rows[0]);
    assert!(has_cell(&rows[1], "replay exact"), "{:?}", rows[1]);
    assert!(has_cell(&rows[2], "ui.log"), "{:?}", rows[2]);
}

#[test]
fn explicit_md_format_matches_default() {
    let s = Scratch::new("md");
    let a = s.write("load.log", &format!("{LOAD_PASS}\n"));
    let base = ["gate-summary", "--machine", "m8", "--cache", "cold"];
    let default = fx_bench(&[&base[..], &[path_str(&a)][..]].concat());
    let md = fx_bench(&[&base[..], &["--format", "md", path_str(&a)][..]].concat());
    assert_eq!(default.status.code(), Some(0), "{}", describe(&default));
    assert_eq!(md.status.code(), Some(0), "{}", describe(&md));
    assert_eq!(stdout(&default), stdout(&md));

    let (_, rows) = table(&stdout(&md));
    assert_eq!(rows.len(), 1);
    assert!(has_cell(&rows[0], "load.log"), "{:?}", rows[0]);
    assert!(
        has_cell(&rows[0], "12.3 ms"),
        "detail column: {:?}",
        rows[0]
    );
    assert!(has_cell(&rows[0], "m8"), "{:?}", rows[0]);
    assert!(has_cell(&rows[0], "cold"), "{:?}", rows[0]);
}

#[test]
fn any_fail_exits_1() {
    let s = Scratch::new("fail");
    let a = s.write("load.log", &format!("{LOAD_PASS}\n"));
    let b = s.write("scan.log", &format!("{SCAN_FAIL}\n{SCAN_ROOT_PASS}\n"));
    let out = fx_bench(&[
        "gate-summary",
        "--machine",
        "m4pro",
        "--cache",
        "cold",
        path_str(&a),
        path_str(&b),
    ]);
    assert_eq!(out.status.code(), Some(1), "{}", describe(&out));
    let (_, rows) = table(&stdout(&out));
    assert_eq!(rows.len(), 3, "{}", stdout(&out));
    let fail = rows
        .iter()
        .find(|r| has_cell(r, "1-thread throughput >= 180000 entries/s"))
        .expect("scan row");
    assert!(has_cell(fail, "FAIL"), "{fail:?}");
    assert!(has_cell(fail, "scan.log"), "{fail:?}");
}

#[test]
fn file_without_gate_line_is_missing_and_exits_1() {
    let s = Scratch::new("missing");
    let a = s.write("load.log", &format!("{LOAD_PASS}\n"));
    let b = s.write("crashed.log", "thread 'main' panicked at ...\n");
    let out = fx_bench(&[
        "gate-summary",
        "--machine",
        "m4pro",
        "--cache",
        "warm",
        path_str(&a),
        path_str(&b),
    ]);
    assert_eq!(out.status.code(), Some(1), "{}", describe(&out));
    let (_, rows) = table(&stdout(&out));
    assert_eq!(rows.len(), 2, "{}", stdout(&out));
    let missing = rows
        .iter()
        .find(|r| has_cell(r, "crashed.log"))
        .expect("row for file with no GATE line");
    assert!(has_cell(missing, "MISSING"), "{missing:?}");
}

#[test]
fn empty_file_is_missing() {
    let s = Scratch::new("empty");
    let a = s.write("empty.log", "");
    let out = fx_bench(&[
        "gate-summary",
        "--machine",
        "m4pro",
        "--cache",
        "unknown",
        path_str(&a),
    ]);
    assert_eq!(out.status.code(), Some(1), "{}", describe(&out));
    let (_, rows) = table(&stdout(&out));
    assert_eq!(rows.len(), 1);
    assert!(has_cell(&rows[0], "MISSING"), "{:?}", rows[0]);
    assert!(has_cell(&rows[0], "unknown"), "{:?}", rows[0]);
}

#[test]
fn unparsed_gate_line_exits_1_and_gets_a_row() {
    let s = Scratch::new("unparsed");
    let a = s.write("load.log", &format!("{LOAD_PASS}\n"));
    let b = s.write("odd.log", "GATE live p95 <= 50 ms: crashed\n");
    let out = fx_bench(&[
        "gate-summary",
        "--machine",
        "m4pro",
        "--cache",
        "warm",
        path_str(&a),
        path_str(&b),
    ]);
    assert_eq!(out.status.code(), Some(1), "{}", describe(&out));
    let (_, rows) = table(&stdout(&out));
    assert_eq!(rows.len(), 2, "{}", stdout(&out));
    let odd = rows
        .iter()
        .find(|r| has_cell(r, "odd.log"))
        .expect("row for unparsed line");
    assert!(!has_cell(odd, "PASS"), "{odd:?}");
    assert!(!has_cell(odd, "MISSING"), "{odd:?}");
}

#[test]
fn skip_gate_exits_1() {
    let s = Scratch::new("skip");
    let a = s.write("scan.log", &format!("{SCAN_SKIP}\n"));
    let out = fx_bench(&[
        "gate-summary",
        "--machine",
        "m4pro",
        "--cache",
        "warm",
        path_str(&a),
    ]);
    assert_eq!(out.status.code(), Some(1), "{}", describe(&out));
    let (_, rows) = table(&stdout(&out));
    assert_eq!(rows.len(), 1, "{}", stdout(&out));
    assert!(!has_cell(&rows[0], "PASS"), "{:?}", rows[0]);
}

#[test]
fn all_benchmark_formats_in_one_log() {
    let s = Scratch::new("mixed");
    let log = [
        FSEVENTS_LIVE_PASS,
        LOAD_PASS,
        FRAME_PASS,
        SEARCH_PASS,
        SCAN_ROOT_PASS,
    ]
    .join("\nother output\n");
    let a = s.write("all.log", &log);
    let out = fx_bench(&[
        "gate-summary",
        "--machine",
        "m4pro",
        "--cache",
        "warm",
        path_str(&a),
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    let (_, rows) = table(&stdout(&out));
    assert_eq!(rows.len(), 5, "{}", stdout(&out));
}

// ---------------------------------------------------------------------------
// Subcommand: JSON output
// ---------------------------------------------------------------------------

#[test]
fn json_output_has_rows_and_escapes_strings() {
    let s = Scratch::new("json");
    let a = s.write("search.log", &format!("{SEARCH_PASS}\n"));
    let b = s.write("none.log", "no gate here\n");
    let out = fx_bench(&[
        "gate-summary",
        "--machine",
        "m4pro",
        "--cache",
        "cold",
        "--format",
        "json",
        path_str(&a),
        path_str(&b),
    ]);
    assert_eq!(out.status.code(), Some(1), "{}", describe(&out));
    let text = stdout(&out);
    let t = text.trim();
    assert!(
        (t.starts_with('[') && t.ends_with(']')) || (t.starts_with('{') && t.ends_with('}')),
        "JSON array or object expected:\n{text}"
    );
    assert!(
        !t.contains("|---"),
        "JSON mode must not print the table:\n{text}"
    );
    for key in [
        "\"criterion\"",
        "\"verdict\"",
        "\"detail\"",
        "\"machine\"",
        "\"cache\"",
    ] {
        assert!(t.contains(key), "missing key {key}:\n{text}");
    }
    for value in [
        "\"search.log\"",
        "\"none.log\"",
        "\"p95<=10ms over 1000000 names on 8 threads\"",
        "\"PASS\"",
        "\"MISSING\"",
        "\"m4pro\"",
        "\"cold\"",
    ] {
        assert!(t.contains(value), "missing value {value}:\n{text}");
    }
    // The query is quoted in the detail; the quotes must be escaped.
    assert!(
        t.contains("query \\\"foo\\\""),
        "quotes not escaped:\n{text}"
    );
}

// ---------------------------------------------------------------------------
// Subcommand: argument errors and unreadable files (exit 2)
// ---------------------------------------------------------------------------

fn assert_exit_2(args: &[&str], want_usage: bool) {
    let out = fx_bench(args);
    assert_eq!(
        out.status.code(),
        Some(2),
        "args {args:?} must exit 2\n{}",
        describe(&out)
    );
    assert!(
        !stderr(&out).trim().is_empty(),
        "args {args:?}: no error on stderr"
    );
    if want_usage {
        assert!(
            stderr(&out).to_lowercase().contains("usage"),
            "args {args:?} must print usage:\n{}",
            stderr(&out)
        );
    }
}

#[test]
fn bad_arguments_exit_2() {
    let s = Scratch::new("args");
    let a = s.write("load.log", &format!("{LOAD_PASS}\n"));
    let log = path_str(&a);
    // Missing --machine / --cache.
    assert_exit_2(&["gate-summary", "--cache", "warm", log], true);
    assert_exit_2(&["gate-summary", "--machine", "m4pro", log], true);
    // Bad --cache and --format values.
    assert_exit_2(
        &["gate-summary", "--machine", "m", "--cache", "hot", log],
        true,
    );
    assert_exit_2(
        &[
            "gate-summary",
            "--machine",
            "m",
            "--cache",
            "warm",
            "--format",
            "csv",
            log,
        ],
        true,
    );
    // No log files.
    assert_exit_2(&["gate-summary", "--machine", "m", "--cache", "warm"], true);
    // Unknown flag; flag without value.
    assert_exit_2(
        &[
            "gate-summary",
            "--machine",
            "m",
            "--cache",
            "warm",
            "--bogus",
            log,
        ],
        true,
    );
    assert_exit_2(&["gate-summary", "--cache", "warm", log, "--machine"], true);
}

#[test]
fn unreadable_file_exits_2() {
    let s = Scratch::new("unreadable");
    let a = s.write("load.log", &format!("{LOAD_PASS}\n"));
    let gone = s.0.join("does-not-exist.log");
    assert_exit_2(
        &[
            "gate-summary",
            "--machine",
            "m",
            "--cache",
            "warm",
            path_str(&a),
            path_str(&gone),
        ],
        false,
    );
    // A directory is not a readable log file either.
    assert_exit_2(
        &[
            "gate-summary",
            "--machine",
            "m",
            "--cache",
            "warm",
            path_str(&s.0),
        ],
        false,
    );
}

#[test]
fn help_lists_gate_summary() {
    let out = fx_bench(&["help"]);
    assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    assert!(stdout(&out).contains("gate-summary"), "{}", stdout(&out));
}

// ---------------------------------------------------------------------------
// Read-only
// ---------------------------------------------------------------------------

#[test]
fn gate_summary_never_modifies_its_inputs() {
    let s = Scratch::new("readonly");
    let text_a = format!("{FSEVENTS_LIVE_PASS}\n{FSEVENTS_REPLAY_FAIL}\n");
    let text_b = "nothing\n";
    let a = s.write("a.log", &text_a);
    let b = s.write("b.log", text_b);
    for format in ["md", "json"] {
        let out = fx_bench(&[
            "gate-summary",
            "--machine",
            "m",
            "--cache",
            "warm",
            "--format",
            format,
            path_str(&a),
            path_str(&b),
        ]);
        assert_eq!(out.status.code(), Some(1), "{}", describe(&out));
    }
    assert_eq!(fs::read_to_string(&a).expect("a still readable"), text_a);
    assert_eq!(fs::read_to_string(&b).expect("b still readable"), text_b);
    let mut entries: Vec<String> = fs::read_dir(&s.0)
        .expect("read scratch dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    entries.sort();
    assert_eq!(entries, vec!["a.log".to_string(), "b.log".to_string()]);
}
