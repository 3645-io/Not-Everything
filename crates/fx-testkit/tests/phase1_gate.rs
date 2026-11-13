//! Phase 1 gate tests.
//!
//! `scripts/phase1-gate.sh` is the one command the owner runs per machine
//! and cache state to measure the Phase 1 gate, and `docs/gates/phase-1.md`
//! is the gate record. These tests mirror `phase0_gate.rs` and check that:
//!
//! - the script exists, has a sh/bash shebang and `set -eu`, accepts
//!   `--machine`, `--cache warm|cold`, `--out`, `--store-dir` and
//!   `--reuse`, builds in release and invokes every Phase 1 benchmark one at
//!   a time (fx-query `matcher_bench` at 2M and 5M; fx-engine
//!   `startup_bench`, `catchup_bench`, `first_run_bench` and
//!   `freshness_bench` with 1,000 operations; the fx-ui-macos
//!   `--gate-probe`; the fx-query conformance tests; the fx-cli tests), then
//!   finishes with `fx-bench phase1-summary` into `summary.md`;
//! - the script never deletes, moves or purges anything and starts no
//!   background jobs;
//! - on Unix, the script validates its arguments before building anything
//!   and never touches an existing `--out` directory;
//! - the gate document maps every Phase 1 checklist item of `docs/PLAN.md`
//!   to a command, has a results table per machine and cache state, lists
//!   the items verified in CI and states an overall verdict;
//! - the fx-query conformance suites hold at least 300 cases.
//!
//! The file-mode executable bit is not asserted here; "executable" is
//! checked as "runs under the shell named by its shebang".
//!
//! Only `std` is used.

mod common;

use common::{read, repo_root};
use std::fs;
use std::path::{Path, PathBuf};

/// Path of the gate runner script, relative to the repository root.
const SCRIPT: &str = "scripts/phase1-gate.sh";

/// Path of the gate document, relative to the repository root.
const REPORT: &str = "docs/gates/phase-1.md";

/// Result columns: one per machine and cache state.
const COLUMNS: [&str; 4] = [
    "M4 Pro warm",
    "M4 Pro cold",
    "8 GB Mac warm",
    "8 GB Mac cold",
];

/// One keyword group per Phase 1 checklist item, in `docs/PLAN.md` order.
/// A line or section of the gate document belongs to an item if it
/// contains any of the group's keywords (case-sensitive).
const ITEM_KEYWORDS: [&[&str]; 8] = [
    &["Syntax", "conformance"],
    &["Goal 1", "Startup", "startup"],
    &["Goal 2", "matcher_bench"],
    &["Goal 3", "Freshness", "freshness"],
    &["Offline catch-up", "Catch-up", "catch-up", "catchup"],
    &["First run", "First-run", "first-run", "first_run"],
    &["Memory", "B/entry", "B per entry"],
    &["CLI", "fx-cli"],
];

/// Tokens that show a command is given for a criterion.
const COMMAND_TOKENS: [&str; 7] = [
    "cargo ",
    "phase1-gate.sh",
    "fx-bench",
    "_bench",
    "--gate-probe",
    "--test",
    "-p fx-",
];

/// Minimum number of conformance cases (PLAN.md Phase 1, "Syntax").
const MIN_CONFORMANCE_CASES: usize = 300;

fn script_path() -> PathBuf {
    repo_root().join(SCRIPT)
}

fn script_text() -> String {
    let path = script_path();
    assert!(
        path.is_file(),
        "{SCRIPT} must exist (the Phase 1 gate runner)"
    );
    read(&path)
}

fn report_text() -> String {
    let path = repo_root().join(REPORT);
    assert!(
        path.is_file(),
        "{REPORT} must exist (the Phase 1 gate record)"
    );
    read(&path)
}

// ---------------------------------------------------------------------------
// Shell helpers (same rules as phase0_gate.rs).
// ---------------------------------------------------------------------------

/// The script's executable lines: comment lines and here-document bodies
/// (usage text) removed, and trailing `#` comments stripped.
fn code_lines(script: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut heredoc_end: Option<String> = None;
    for raw in script.lines() {
        if let Some(end) = &heredoc_end {
            if raw.trim() == end {
                heredoc_end = None;
            }
            continue;
        }
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let line = strip_shell_comment(raw);
        if let Some(end) = heredoc_terminator(&line) {
            heredoc_end = Some(end);
        }
        out.push(line);
    }
    out
}

/// Removes a trailing ` # comment` outside quotes.
fn strip_shell_comment(line: &str) -> String {
    let mut quote: Option<char> = None;
    let mut prev = ' ';
    for (i, c) in line.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c == '#' && prev.is_whitespace() => return line[..i].to_owned(),
            None => {}
        }
        prev = c;
    }
    line.to_owned()
}

/// If `line` starts a here-document, returns its terminator word.
fn heredoc_terminator(line: &str) -> Option<String> {
    let idx = line.find("<<")?;
    let rest = &line[idx + 2..];
    if rest.starts_with('<') {
        return None;
    }
    let rest = rest.trim_start_matches('-').trim_start();
    let word: String = rest
        .chars()
        .filter(|c| *c != '\'' && *c != '"')
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!word.is_empty()).then_some(word)
}

/// Removes the contents of quoted strings.
fn strip_quoted(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut quote: Option<char> = None;
    for c in line.chars() {
        match quote {
            Some(q) if c == q => {
                quote = None;
                out.push(c);
            }
            Some(_) => {}
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                out.push(c);
            }
            None => out.push(c),
        }
    }
    out
}

/// Shell words of unquoted code, split on whitespace and operators.
fn words(line: &str) -> Vec<String> {
    line.split(|c: char| c.is_whitespace() || ";|&()`{}".contains(c))
        .filter(|w| !w.is_empty())
        .map(|w| w.trim_start_matches('$').to_owned())
        .collect()
}

/// True if `word` occurs in `text` with non-alphanumeric boundaries.
fn has_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let after = text[i + word.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

// ---------------------------------------------------------------------------
// Conformance case counting.
// ---------------------------------------------------------------------------

/// Counts conformance case entries in Rust source: calls `case(` with a
/// non-identifier character before them, excluding the `fn case(`
/// definition and comment lines.
fn count_case_entries(src: &str) -> usize {
    let mut n = 0;
    for line in src.lines() {
        let t = line.trim_start();
        if t.starts_with("//") {
            continue;
        }
        for (i, _) in line.match_indices("case(") {
            let before = line[..i].chars().next_back();
            if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                continue;
            }
            if line[..i].trim_end().ends_with("fn") {
                continue;
            }
            n += 1;
        }
    }
    n
}

/// All `.rs` files under `crates/fx-query/tests/` whose top-level name
/// starts with `conformance` (files and the shared `conformance/` module).
fn conformance_sources() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.filter_map(Result::ok) {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let tests = repo_root().join("crates/fx-query/tests");
    let mut out = Vec::new();
    for e in fs::read_dir(&tests)
        .unwrap_or_else(|e| panic!("cannot list {}: {e}", tests.display()))
        .filter_map(Result::ok)
    {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with("conformance") {
            continue;
        }
        let p = e.path();
        if p.is_dir() {
            walk(&p, &mut out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
    out.sort();
    out
}

// ---------------------------------------------------------------------------
// Tests of the helpers themselves.
// ---------------------------------------------------------------------------

#[test]
fn helpers_skip_comments_heredocs_and_quoted_text() {
    let script = "#!/bin/sh\n\
                  # rm everything? never.\n\
                  set -eu\n\
                  usage() {\n\
                  cat <<'EOF'\n\
                  rm and purge are only words here\n\
                  EOF\n\
                  }\n\
                  echo \"sudo purge is not run\" # rm in a comment\n\
                  run_step matcher_bench_2m \"$EX/matcher_bench\" --entries 2000000\n";
    let code = code_lines(script);
    let joined = code.join("\n");
    assert!(joined.contains("set -eu"));
    assert!(joined.contains("matcher_bench"));
    assert!(!joined.contains("only words here"));
    let bare: Vec<String> = code.iter().flat_map(|l| words(&strip_quoted(l))).collect();
    assert!(!bare.iter().any(|w| w == "rm" || w == "purge"), "{bare:?}");
    assert!(
        words(&strip_quoted("mkdir -p x; rm -rf \"$OUT\""))
            .iter()
            .any(|w| w == "rm")
    );
    assert!(has_word("--ops 1000 ", "1000"));
    assert!(!has_word("--ops 10000", "1000"));
}

#[test]
fn case_counter_counts_calls_not_definitions_or_comments() {
    let src = "pub fn case(group: &str, q: &str) -> Case { todo!() }\n\
               // case(G, \"commented\")\n\
               vec![\n\
                   case(G, \"a\").yes(&[\"a\"]),\n\
                   case(G, \"b\"), case(G, \"c\"),\n\
                   lowercase(x), run_case(y),\n\
               ]\n";
    assert_eq!(count_case_entries(src), 3);
}

// ---------------------------------------------------------------------------
// Conformance: ≥300 cases (verified in CI).
// ---------------------------------------------------------------------------

#[test]
fn conformance_suites_hold_at_least_300_cases() {
    let files = conformance_sources();
    assert!(
        !files.is_empty(),
        "crates/fx-query/tests must contain conformance*.rs suites"
    );
    let per_file: Vec<(String, usize)> = files
        .iter()
        .map(|p| (p.display().to_string(), count_case_entries(&read(p))))
        .collect();
    let total: usize = per_file.iter().map(|(_, n)| n).sum();
    assert!(
        total >= MIN_CONFORMANCE_CASES,
        "the fx-query conformance suites hold {total} cases, need ≥{MIN_CONFORMANCE_CASES}: {per_file:#?}"
    );
}

// ---------------------------------------------------------------------------
// Script content.
// ---------------------------------------------------------------------------

#[test]
fn script_has_a_shell_shebang_and_strict_mode() {
    let text = script_text();
    let first = text.lines().next().unwrap_or("");
    assert!(
        first.starts_with("#!") && (first.contains("sh") || first.contains("bash")),
        "{SCRIPT} must start with a sh or bash shebang, found {first:?}"
    );
    let strict = code_lines(&text).iter().any(|l| {
        l.trim().strip_prefix("set -").is_some_and(|flags| {
            let flags = flags.split_whitespace().next().unwrap_or("");
            flags.contains('e') && flags.contains('u')
        })
    });
    assert!(strict, "{SCRIPT} must run with `set -eu`");
}

#[test]
fn script_accepts_the_documented_options() {
    let code = code_lines(&script_text()).join("\n");
    for opt in ["--machine", "--cache", "--out", "--store-dir", "--reuse"] {
        assert!(
            code.contains(opt),
            "{SCRIPT} must handle the `{opt}` option"
        );
    }
    for cache in ["warm", "cold"] {
        assert!(
            code.contains(cache),
            "{SCRIPT} must accept the cache label `{cache}`"
        );
    }
}

#[test]
fn script_builds_in_release_and_invokes_every_phase1_benchmark() {
    let code = code_lines(&script_text()).join("\n");
    assert!(code.contains("cargo"), "{SCRIPT} must build with cargo");
    assert!(code.contains("--release"), "{SCRIPT} must build in release");
    let required = [
        ("fx-query matcher_bench", "matcher_bench"),
        ("matcher_bench at 2M entries", "2000000"),
        ("matcher_bench at 5M entries", "5000000"),
        ("fx-engine startup_bench", "startup_bench"),
        ("fx-engine catchup_bench", "catchup_bench"),
        ("fx-engine first_run_bench", "first_run_bench"),
        ("fx-engine freshness_bench", "freshness_bench"),
        ("freshness_bench --ops", "--ops"),
        ("fx-ui-macos gate probe", "--gate-probe"),
        ("fx-query conformance tests", "conformance"),
        ("fx-query crate", "fx-query"),
        ("fx-cli tests", "fx-cli"),
        ("fx-bench phase1-summary", "phase1-summary"),
        ("the summary file", "summary.md"),
        ("per-benchmark logs", ".log"),
    ];
    let missing: Vec<&str> = required
        .iter()
        .filter(|(_, needle)| !code.contains(needle))
        .map(|(what, _)| *what)
        .collect();
    assert!(
        missing.is_empty(),
        "{SCRIPT} must invoke every Phase 1 gate benchmark; missing: {missing:?}"
    );
    assert!(
        has_word(&code, "1000"),
        "{SCRIPT} must run freshness_bench with 1000 operations"
    );
    let tests_run = code_lines(&script_text())
        .iter()
        .any(|l| words(&strip_quoted(l)).iter().any(|w| w == "test"));
    assert!(
        tests_run,
        "{SCRIPT} must run the conformance and fx-cli tests with `cargo test`"
    );
}

/// The app's `--gate-probe` exists only with fx-ui-macos's `measure`
/// feature, which release builds leave off, so the script's build turns it
/// on.
#[test]
fn script_builds_with_the_measure_feature() {
    let builds: Vec<String> = code_lines(&script_text())
        .into_iter()
        .filter(|l| {
            let w = words(&strip_quoted(l));
            w.iter().any(|w| w == "cargo") && w.iter().any(|w| w == "build")
        })
        .collect();
    assert!(!builds.is_empty(), "{SCRIPT} must build with `cargo build`");
    for line in &builds {
        assert!(
            line.contains("--features fx-ui-macos/measure"),
            "{SCRIPT} must build with `--features fx-ui-macos/measure`: {}",
            line.trim()
        );
    }
}

/// Without `--data-dir`, the app opens the daily app's data folder: its
/// index, its settings and its shortcut. Every launch of the gate probe
/// names a folder of the run's own instead, as `perf-check.sh` does.
#[test]
fn every_app_launch_has_its_own_data_folder() {
    let launches: Vec<String> = code_lines(&script_text())
        .into_iter()
        .filter(|l| l.contains("/fx-ui-macos\""))
        .collect();
    assert!(!launches.is_empty(), "{SCRIPT} must launch fx-ui-macos");
    for line in &launches {
        assert!(
            line.contains("--data-dir"),
            "{SCRIPT} must give every fx-ui-macos launch its own `--data-dir`: {}",
            line.trim()
        );
    }
}

/// `fx-bench phase1-summary` reads every file in the results folder, and the
/// gate probe prints its `launch_ms=` line to standard output as well as to
/// its report. So each launch's output goes to a log in `gate_probe/`, which
/// the summary doesn't read: a launch counts once, from its report through
/// gate-metrics.txt, and the warm-up launch, which may first build the
/// probe's index, not at all.
#[test]
fn app_launch_output_stays_out_of_the_summary_folder() {
    let launches: Vec<String> = code_lines(&script_text())
        .into_iter()
        .filter(|l| l.contains("/fx-ui-macos\""))
        .collect();
    assert!(!launches.is_empty(), "{SCRIPT} must launch fx-ui-macos");
    for line in &launches {
        let line = line.trim();
        assert!(
            !line.starts_with("run_step "),
            "{SCRIPT} must not launch fx-ui-macos through run_step, whose log the summary reads: {line}"
        );
        assert!(
            line.contains(">>\"$OUT/gate_probe/"),
            "{SCRIPT} must send every fx-ui-macos launch's output to a log in gate_probe/: {line}"
        );
    }
}

#[test]
fn script_never_deletes_moves_or_purges() {
    let forbidden = ["rm", "rmdir", "unlink", "mv", "purge", "-delete", "srm"];
    let hits: Vec<String> = code_lines(&script_text())
        .into_iter()
        .filter(|line| {
            words(&strip_quoted(line))
                .iter()
                .any(|w| forbidden.contains(&w.as_str()))
        })
        .map(|l| l.trim().to_owned())
        .collect();
    assert!(
        hits.is_empty(),
        "{SCRIPT} must never delete, move or purge (no rm/rmdir/unlink/mv/purge/-delete): {hits:?}"
    );
}

#[test]
fn script_runs_benchmarks_one_at_a_time() {
    let mut hits = Vec::new();
    for line in code_lines(&script_text()) {
        let bare: Vec<char> = strip_quoted(&line).chars().collect();
        for (i, &c) in bare.iter().enumerate() {
            if c != '&' {
                continue;
            }
            let prev = if i > 0 { bare[i - 1] } else { ' ' };
            let next = bare.get(i + 1).copied().unwrap_or(' ');
            if prev != '&' && prev != '>' && next != '&' && next != '>' {
                hits.push(line.trim().to_owned());
            }
        }
        let w = words(&strip_quoted(&line));
        if w.iter().any(|w| w == "parallel") || line.contains("xargs -P") {
            hits.push(line.trim().to_owned());
        }
    }
    assert!(
        hits.is_empty(),
        "{SCRIPT} must run benchmarks one at a time (no background jobs): {hits:?}"
    );
}

/// True for a code line that opens an `if` testing for the cold cache
/// state, e.g. `if [ "$CACHE" = cold ]; then`.
fn is_cold_if(line: &str) -> bool {
    let t = line.trim();
    let compact: String = t.chars().filter(|c| *c != '"' && *c != '\'').collect();
    t.starts_with("if ")
        && (compact.contains("$CACHE = cold") || compact.contains("${CACHE} = cold"))
}

/// The variable a plain `NAME=value` assignment line sets, with its value.
fn assignment(line: &str) -> Option<(String, String)> {
    let t = line.trim().trim_end_matches(';').trim();
    let (name, value) = t.split_once('=')?;
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    ok.then(|| {
        (
            name.to_owned(),
            value.trim().trim_matches('"').trim_matches('\'').to_owned(),
        )
    })
}

#[test]
fn cold_pass_judges_a_single_cold_launch() {
    // Only the first launch after a reboot is cold: the cold branch that
    // sets STARTUP_RUNS=1 must also set LAUNCHES=1, and that must be the
    // last LAUNCHES assignment (after the full-scale and --quick defaults).
    let lines = code_lines(&script_text());
    let mut in_cold = 0usize; // nesting depth inside a cold `if` block
    let mut depth = 0usize;
    let mut cold_sets = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if t.starts_with("if ") {
            depth += 1;
            if in_cold == 0 && is_cold_if(t) {
                in_cold = depth;
            }
            continue;
        }
        if t == "fi" || t.starts_with("fi ") || t.starts_with("fi;") {
            if in_cold == depth {
                in_cold = 0;
            }
            depth = depth.saturating_sub(1);
            continue;
        }
        if in_cold != 0 {
            if let Some((name, value)) = assignment(t) {
                cold_sets.push((i, name, value));
            }
        }
    }
    let launches_1 = cold_sets
        .iter()
        .find(|(_, n, v)| n == "LAUNCHES" && v == "1")
        .map(|(i, _, _)| *i);
    let startup_1 = cold_sets
        .iter()
        .any(|(_, n, v)| n == "STARTUP_RUNS" && v == "1");
    assert!(
        startup_1,
        "{SCRIPT}: the `--cache cold` branch must set STARTUP_RUNS=1; cold assignments: {cold_sets:?}"
    );
    let Some(at) = launches_1 else {
        panic!(
            "{SCRIPT}: the `--cache cold` branch must set LAUNCHES=1 so the cold pass judges the \
             single cold launch; cold assignments: {cold_sets:?}"
        );
    };
    let later: Vec<&String> = lines
        .iter()
        .skip(at + 1)
        .filter(|l| assignment(l).is_some_and(|(n, _)| n == "LAUNCHES"))
        .collect();
    assert!(
        later.is_empty(),
        "{SCRIPT}: LAUNCHES=1 in the cold branch must not be overridden later: {later:?}"
    );
}

// ---------------------------------------------------------------------------
// Script behaviour (argument validation only; never runs a benchmark).
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod behaviour {
    use super::*;
    use std::process::{Command, Output, Stdio};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    fn temp_path(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "fx-phase1-gate-{tag}-{}-{nanos}",
            std::process::id()
        ))
    }

    /// Runs the script under the shell its shebang names. Argument errors
    /// must be reported before any build starts, so a run longer than the
    /// limit is killed and fails the test.
    fn run_script(args: &[&str]) -> Output {
        let text = script_text();
        let shell = if text.lines().next().unwrap_or("").contains("bash") {
            "bash"
        } else {
            "sh"
        };
        let mut child = Command::new(shell)
            .arg(script_path())
            .args(args)
            .current_dir(repo_root())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("cannot run {shell} {SCRIPT}: {e}"));
        let start = Instant::now();
        loop {
            if child.try_wait().expect("wait for script").is_some() {
                break;
            }
            if start.elapsed() > Duration::from_secs(60) {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "{SCRIPT} {args:?} did not exit within 60 s; it must validate its \
                     arguments before building or running anything"
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        child.wait_with_output().expect("collect script output")
    }

    fn describe(out: &Output) -> String {
        format!(
            "status {:?}\nstdout:\n{}\nstderr:\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    }

    #[test]
    fn refuses_an_existing_out_dir_and_leaves_it_untouched() {
        let dir = temp_path("exists");
        fs::create_dir_all(&dir).expect("create temp dir");
        let sentinel = dir.join("keep.txt");
        fs::write(&sentinel, "keep me").expect("write sentinel");
        let dir_str = dir.to_str().expect("utf-8 temp path").to_owned();

        let out = run_script(&["--machine", "ci", "--cache", "warm", "--out", &dir_str]);

        let entries: Vec<_> = fs::read_dir(&dir)
            .map(|rd| rd.filter_map(Result::ok).map(|e| e.file_name()).collect())
            .unwrap_or_default();
        let sentinel_ok = fs::read_to_string(&sentinel).ok();
        let _ = fs::remove_dir_all(&dir);

        assert!(
            !out.status.success(),
            "{SCRIPT} must refuse an --out directory that already exists\n{}",
            describe(&out)
        );
        assert_eq!(sentinel_ok.as_deref(), Some("keep me"));
        assert_eq!(
            entries.len(),
            1,
            "{SCRIPT} must not write into an existing --out directory: {entries:?}"
        );
    }

    fn assert_usage_error(tag: &str, args_before_out: &[&str], why: &str) {
        let dir = temp_path(tag);
        let dir_str = dir.to_str().expect("utf-8 temp path").to_owned();
        let mut args: Vec<&str> = args_before_out.to_vec();
        args.extend(["--out", dir_str.as_str()]);
        let out = run_script(&args);
        let created = dir.exists();
        let _ = fs::remove_dir_all(&dir);
        assert!(!out.status.success(), "{SCRIPT} {why}\n{}", describe(&out));
        assert!(!created, "{SCRIPT} must not create --out on a usage error");
    }

    #[test]
    fn rejects_a_missing_machine_label_without_creating_out() {
        assert_usage_error("nomachine", &["--cache", "warm"], "must require --machine");
    }

    #[test]
    fn rejects_a_missing_cache_label_without_creating_out() {
        assert_usage_error("nocache", &["--machine", "ci"], "must require --cache");
    }

    #[test]
    fn rejects_a_bad_cache_label_without_creating_out() {
        assert_usage_error(
            "badcache",
            &["--machine", "ci", "--cache", "lukewarm"],
            "must accept only --cache warm or cold",
        );
    }

    #[test]
    fn rejects_an_unknown_option_without_creating_out() {
        assert_usage_error(
            "unknown",
            &["--machine", "ci", "--cache", "warm", "--frobnicate"],
            "must reject an unknown option",
        );
    }

    #[test]
    fn rejects_a_missing_out_dir() {
        let out = run_script(&["--machine", "ci", "--cache", "warm"]);
        assert!(
            !out.status.success(),
            "{SCRIPT} must require --out\n{}",
            describe(&out)
        );
    }
}

// ---------------------------------------------------------------------------
// Gate document.
// ---------------------------------------------------------------------------

/// The Phase 1 checklist items from `docs/PLAN.md`.
fn plan_phase1_items() -> Vec<String> {
    let plan = read(&repo_root().join("docs/PLAN.md"));
    let mut items = Vec::new();
    let mut inside = false;
    for line in plan.lines() {
        let t = line.trim();
        if t.starts_with('#') {
            inside = t.trim_start_matches('#').trim().starts_with("Phase 1");
            continue;
        }
        if inside {
            if let Some(rest) = t
                .strip_prefix("- [ ] ")
                .or_else(|| t.strip_prefix("- [x] "))
                .or_else(|| t.strip_prefix("- [X] "))
            {
                items.push(rest.to_owned());
            }
        }
    }
    items
}

fn heading_level(line: &str) -> Option<usize> {
    let t = line.trim_start();
    let level = t.chars().take_while(|&c| c == '#').count();
    (level > 0 && t[level..].starts_with(' ')).then_some(level)
}

/// Heading-delimited sections `(heading, body)`: from a heading up to the
/// next heading of the same or a shallower level.
fn sections(md: &str) -> Vec<(String, String)> {
    let lines: Vec<&str> = md.lines().collect();
    let mut in_fence = false;
    let mut heads = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if l.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if !in_fence {
            if let Some(level) = heading_level(l) {
                heads.push((i, level));
            }
        }
    }
    heads
        .iter()
        .enumerate()
        .map(|(n, &(start, level))| {
            let end = heads[n + 1..]
                .iter()
                .find(|&&(_, l)| l <= level)
                .map(|&(i, _)| i)
                .unwrap_or(lines.len());
            (lines[start].trim().to_owned(), lines[start..end].join("\n"))
        })
        .collect()
}

fn matches_item(text: &str, item: usize) -> bool {
    ITEM_KEYWORDS[item].iter().any(|k| text.contains(k))
}

fn names_a_command(text: &str) -> bool {
    COMMAND_TOKENS.iter().any(|t| text.contains(t))
}

/// An item is mapped to a command if a single line (e.g. a table row) or a
/// section naming fewer than three items names both the item and a
/// command.
fn item_is_mapped(report: &str, item: usize) -> bool {
    let by_line = report
        .lines()
        .any(|l| matches_item(l, item) && names_a_command(l));
    let by_section = sections(report).iter().any(|(_, body)| {
        let named = (0..ITEM_KEYWORDS.len())
            .filter(|&i| matches_item(body, i))
            .count();
        named < 3 && matches_item(body, item) && names_a_command(body)
    });
    by_line || by_section
}

#[test]
fn item_keywords_match_the_plan_checklist() {
    let items = plan_phase1_items();
    assert_eq!(
        items.len(),
        ITEM_KEYWORDS.len(),
        "docs/PLAN.md Phase 1 checklist: {items:#?}"
    );
    for (i, item) in items.iter().enumerate() {
        for (k, group) in ITEM_KEYWORDS.iter().enumerate() {
            let hit = group.iter().any(|kw| item.contains(kw));
            assert_eq!(
                hit,
                i == k,
                "keyword group {group:?} vs Phase 1 item {i}: {item}"
            );
        }
    }
}

#[test]
fn report_maps_every_phase1_criterion_to_a_command() {
    let report = report_text();
    let items = plan_phase1_items();
    let unmapped: Vec<&str> = (0..ITEM_KEYWORDS.len())
        .filter(|&i| !item_is_mapped(&report, i))
        .map(|i| items.get(i).map(String::as_str).unwrap_or("?"))
        .collect();
    assert!(
        unmapped.is_empty(),
        "{REPORT} must list every Phase 1 criterion with the command that measures it; \
         unmapped: {unmapped:#?}"
    );
}

#[test]
fn report_has_results_per_machine_and_cache_state() {
    let report = report_text();
    let missing: Vec<&str> = COLUMNS
        .iter()
        .copied()
        .filter(|c| !report.contains(c))
        .collect();
    assert!(
        missing.is_empty(),
        "{REPORT} must have a results table with columns {COLUMNS:?}; missing: {missing:?}"
    );
    assert!(
        report.lines().any(|l| l.trim_start().starts_with('|')
            && COLUMNS.iter().filter(|c| l.contains(*c)).count() >= 2),
        "{REPORT} must lay the machine and cache results out as a Markdown table"
    );
}

#[test]
fn report_lists_the_items_verified_in_ci() {
    let report = report_text();
    let ci = sections(&report)
        .into_iter()
        .filter(|(h, _)| has_word(h, "CI"))
        .map(|(_, body)| body)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !ci.is_empty(),
        "{REPORT} must have a section whose heading names CI, listing the items verified in CI"
    );
    let lower = ci.to_lowercase();
    let needs = [
        ("the ≥300 conformance cases", ci.contains("300")),
        (
            "the CLI csv, json and txt formats",
            ["csv", "json", "txt"].iter().all(|f| lower.contains(f)),
        ),
        ("the CLI exit codes", lower.contains("exit code")),
        (
            "the memory B/entry estimate",
            ci.contains("B/entry") || lower.contains("per entry"),
        ),
    ];
    let missing: Vec<&str> = needs
        .iter()
        .filter(|(_, ok)| !ok)
        .map(|(w, _)| *w)
        .collect();
    assert!(
        missing.is_empty(),
        "{REPORT}'s CI section must cover: {missing:?}"
    );
}

#[test]
fn report_has_an_overall_verdict() {
    let report = report_text().to_lowercase();
    assert!(
        report.contains("pending owner measurement")
            || report.contains("gate not passed")
            || report.contains("gate passed"),
        "{REPORT} must state an overall verdict (e.g. 'pending owner measurement')"
    );
}

#[test]
fn report_tells_the_owner_how_to_measure() {
    let report = report_text();
    let lower = report.to_lowercase();
    for (needle, why) in [
        (SCRIPT, "give the command"),
        ("phase1-summary", "name fx-bench phase1-summary"),
        ("summary.md", "say to paste summary.md into the record"),
        ("--cache cold", "show the cold-cache invocation"),
        ("--store-dir", "explain --store-dir for the cold pass"),
    ] {
        assert!(report.contains(needle), "{REPORT} must {why} ({needle})");
    }
    assert!(
        lower.contains("reboot"),
        "{REPORT} must describe the cold run after a reboot"
    );
}
