//! Phase 0 gate tests.
//!
//! `scripts/phase0-gate.sh` is the one command the owner runs per machine
//! and cache state to measure the Phase 0 gate, and `docs/gates/phase-0.md`
//! is the gate report. These tests check that:
//!
//! - the script exists, uses `set -eu`, invokes every Phase 0 gate
//!   benchmark one at a time (no background jobs) and finishes with
//!   `fx-bench gate-summary` into `summary.md`;
//! - the script never deletes, renames or moves anything: no `rm`, `rmdir`,
//!   `unlink`, `mv` or `find -delete`, and no `purge` (cold cache means
//!   after a reboot; `sudo purge` is only documented in the report);
//! - on Unix, the script validates its arguments before doing any work:
//!   it refuses an `--out` directory that already exists (leaving it
//!   untouched), and rejects a missing `--machine` or a bad `--cache`
//!   without creating the output directory;
//! - the gate report has one section per Phase 0 checklist item in
//!   `docs/PLAN.md`, each with a status (Met / Missed / Unmeasured /
//!   Pending owner) and the four result columns, plus an overall verdict
//!   and the owner's steps.
//!
//! Only `std` is used.

mod common;

use common::{read, repo_root};
use std::path::PathBuf;

/// Path of the gate runner script, relative to the repository root.
const SCRIPT: &str = "scripts/phase0-gate.sh";

/// Path of the gate report, relative to the repository root.
const REPORT: &str = "docs/gates/phase-0.md";

/// Status values a gate item may carry.
const STATUSES: [&str; 4] = ["Met", "Missed", "Unmeasured", "Pending owner"];

/// Result columns every item's table must have.
const COLUMNS: [&str; 4] = [
    "M4 Pro warm",
    "M4 Pro cold",
    "8 GB Mac warm",
    "8 GB Mac cold",
];

/// One keyword group per Phase 0 checklist item, in `docs/PLAN.md` order.
/// A report section belongs to an item if it contains any of the group's
/// keywords (case-sensitive).
const ITEM_KEYWORDS: [&[&str]; 7] = [
    &["CLEANROOM"],
    &["getattrlistbulk"],
    &["memchr"],
    &["mmap"],
    &["FSEvents"],
    &["NSTableView", "16.7"],
    &["two machines"],
];

fn script_path() -> PathBuf {
    repo_root().join(SCRIPT)
}

fn script_text() -> String {
    let path = script_path();
    assert!(
        path.is_file(),
        "{SCRIPT} must exist (the Phase 0 gate runner)"
    );
    read(&path)
}

fn report_text() -> String {
    let path = repo_root().join(REPORT);
    assert!(
        path.is_file(),
        "{REPORT} must exist (the Phase 0 gate report)"
    );
    read(&path)
}

// ---------------------------------------------------------------------------
// Shell helpers.
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

/// If `line` starts a here-document (`<<EOF`, `<<-EOF`, `<<'EOF'`,
/// `<<"EOF"`), returns its terminator word.
fn heredoc_terminator(line: &str) -> Option<String> {
    let idx = line.find("<<")?;
    let rest = &line[idx + 2..];
    if rest.starts_with('<') {
        return None; // `<<<` here-string
    }
    let rest = rest.trim_start_matches('-').trim_start();
    let word: String = rest
        .chars()
        .filter(|c| *c != '\'' && *c != '"')
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!word.is_empty()).then_some(word)
}

/// Removes the contents of single- and double-quoted strings, so that
/// messages such as `echo "sudo purge is not run"` are not mistaken for
/// commands.
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
                  run_bench self-bench\n";
    let code = code_lines(script);
    let joined = code.join("\n");
    assert!(joined.contains("set -eu"));
    assert!(joined.contains("run_bench self-bench"));
    assert!(!joined.contains("only words here"));
    let bare: Vec<String> = code.iter().flat_map(|l| words(&strip_quoted(l))).collect();
    assert!(!bare.iter().any(|w| w == "rm" || w == "purge"), "{bare:?}");
    let bad: Vec<String> = words(&strip_quoted("mkdir -p x; rm -rf \"$OUT\""));
    assert!(bad.iter().any(|w| w == "rm"));
    assert!(words("x=$(rm y)").iter().any(|w| w == "rm"));
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
        let l = l.trim();
        l.strip_prefix("set -").is_some_and(|flags| {
            let flags = flags.split_whitespace().next().unwrap_or("");
            flags.contains('e') && flags.contains('u')
        })
    });
    assert!(strict, "{SCRIPT} must run with `set -eu`");
}

#[test]
fn script_accepts_the_documented_options() {
    let code = code_lines(&script_text()).join("\n");
    for opt in ["--machine", "--cache", "--out", "--skip-ui", "--skip-scan"] {
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
fn script_builds_in_release_and_invokes_every_gate_benchmark() {
    let code = code_lines(&script_text()).join("\n");
    assert!(code.contains("cargo"), "{SCRIPT} must build with cargo");
    assert!(code.contains("--release"), "{SCRIPT} must build in release");
    let required = [
        ("fx-bench gen over 5M names", "5000000"),
        ("fx-bench self-bench", "self-bench"),
        ("fx-query search_bench", "search_bench"),
        ("fx-store load_bench", "load_bench"),
        ("fx-fs-macos scan_bench", "scan_bench"),
        ("fx-fs-macos fsevents_bench", "fsevents_bench"),
        ("fsevents_bench latency", "latency"),
        ("fsevents_bench replay", "replay"),
        ("fx-ui-macos table_spike", "table_spike"),
        ("table_spike --autoscroll", "--autoscroll"),
        ("table_spike --launch-probe", "--launch-probe"),
        ("fx-ui-macos make_bundle", "make_bundle"),
        ("fx-bench gate-summary", "gate-summary"),
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
        "{SCRIPT} must invoke every Phase 0 gate benchmark; missing: {missing:?}"
    );
}

/// `table_spike` and the app's measurement modes exist only with
/// fx-ui-macos's `measure` feature, which release builds leave off, so every
/// build the script makes turns it on.
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

#[test]
fn script_never_deletes_moves_or_purges() {
    let text = script_text();
    let forbidden = ["rm", "rmdir", "unlink", "mv", "purge", "-delete", "srm"];
    let mut hits = Vec::new();
    for line in code_lines(&text) {
        let bare = strip_quoted(&line);
        for w in words(&bare) {
            if forbidden.contains(&w.as_str()) {
                hits.push(line.trim().to_owned());
            }
        }
    }
    assert!(
        hits.is_empty(),
        "{SCRIPT} must never delete, move or purge (no rm/rmdir/unlink/mv/purge/-delete): {hits:?}"
    );
}

#[test]
fn script_runs_benchmarks_one_at_a_time() {
    let text = script_text();
    let mut hits = Vec::new();
    for line in code_lines(&text) {
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

// ---------------------------------------------------------------------------
// Script behaviour (argument validation only; never runs a benchmark).
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod behaviour {
    use super::*;
    use std::fs;
    use std::process::{Command, Output, Stdio};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    /// A fresh, unused path under the system temp directory.
    fn temp_path(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "fx-phase0-gate-{tag}-{}-{nanos}",
            std::process::id()
        ))
    }

    /// Runs the script with `args` from the repository root. Argument
    /// errors must be reported before any build starts, so a run that
    /// takes longer than the limit is killed and fails the test.
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

        let out = run_script(&["--machine", "test", "--cache", "warm", "--out", &dir_str]);

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
        assert_eq!(
            sentinel_ok.as_deref(),
            Some("keep me"),
            "{SCRIPT} must not touch an existing --out directory"
        );
        assert_eq!(
            entries.len(),
            1,
            "{SCRIPT} must not write into an existing --out directory: {entries:?}"
        );
    }

    #[test]
    fn rejects_a_missing_machine_label_without_creating_out() {
        let dir = temp_path("nomachine");
        let dir_str = dir.to_str().expect("utf-8 temp path").to_owned();
        let out = run_script(&["--cache", "warm", "--out", &dir_str]);
        let created = dir.exists();
        let _ = fs::remove_dir_all(&dir);
        assert!(
            !out.status.success(),
            "{SCRIPT} must require --machine\n{}",
            describe(&out)
        );
        assert!(!created, "{SCRIPT} must not create --out on a usage error");
    }

    #[test]
    fn rejects_a_bad_cache_label_without_creating_out() {
        let dir = temp_path("badcache");
        let dir_str = dir.to_str().expect("utf-8 temp path").to_owned();
        let out = run_script(&[
            "--machine",
            "test",
            "--cache",
            "lukewarm",
            "--out",
            &dir_str,
        ]);
        let created = dir.exists();
        let _ = fs::remove_dir_all(&dir);
        assert!(
            !out.status.success(),
            "{SCRIPT} must accept only --cache warm or cold\n{}",
            describe(&out)
        );
        assert!(!created, "{SCRIPT} must not create --out on a usage error");
    }

    #[test]
    fn rejects_a_missing_out_dir() {
        let out = run_script(&["--machine", "test", "--cache", "warm"]);
        assert!(
            !out.status.success(),
            "{SCRIPT} must require --out\n{}",
            describe(&out)
        );
    }
}

// ---------------------------------------------------------------------------
// Gate report.
// ---------------------------------------------------------------------------

/// The Phase 0 checklist items from `docs/PLAN.md`.
fn plan_phase0_items() -> Vec<String> {
    let plan = read(&repo_root().join("docs/PLAN.md"));
    let mut items = Vec::new();
    let mut inside = false;
    for line in plan.lines() {
        let t = line.trim();
        if t.starts_with('#') {
            inside = t.trim_start_matches('#').trim().starts_with("Phase 0");
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

/// A heading-delimited section: from a heading up to the next heading of
/// the same or a shallower level.
struct Section {
    heading: String,
    body: String,
}

fn heading_level(line: &str) -> Option<usize> {
    let t = line.trim_start();
    let level = t.chars().take_while(|&c| c == '#').count();
    (level > 0 && t[level..].starts_with(' ')).then_some(level)
}

fn sections(md: &str) -> Vec<Section> {
    let lines: Vec<&str> = md.lines().collect();
    let mut out = Vec::new();
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
    for (n, &(start, level)) in heads.iter().enumerate() {
        let end = heads[n + 1..]
            .iter()
            .find(|&&(_, l)| l <= level)
            .map(|&(i, _)| i)
            .unwrap_or(lines.len());
        out.push(Section {
            heading: lines[start].trim().to_owned(),
            body: lines[start..end].join("\n"),
        });
    }
    out
}

/// True if `word` occurs in `text` with non-alphanumeric boundaries.
fn has_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let after = text[i + word.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

fn matches_item(body: &str, item: usize) -> bool {
    ITEM_KEYWORDS[item].iter().any(|k| body.contains(k))
}

/// A section qualifies as an item's own section if it names that item, has
/// a status and the four result columns, and is not an overview section
/// naming three or more items.
fn candidate(section: &Section, item: usize) -> bool {
    let body = &section.body;
    let named = (0..ITEM_KEYWORDS.len())
        .filter(|&i| matches_item(body, i))
        .count();
    matches_item(body, item)
        && named < 3
        && STATUSES.iter().any(|s| has_word(body, s))
        && COLUMNS.iter().all(|c| body.contains(c))
}

/// Assigns a distinct section to every item, by backtracking.
fn assign(cands: &[Vec<usize>], item: usize, used: &mut Vec<usize>) -> bool {
    if item == cands.len() {
        return true;
    }
    for &s in &cands[item] {
        if !used.contains(&s) {
            used.push(s);
            if assign(cands, item + 1, used) {
                return true;
            }
            used.pop();
        }
    }
    false
}

#[test]
fn item_keywords_match_the_plan_checklist() {
    let items = plan_phase0_items();
    assert_eq!(
        items.len(),
        ITEM_KEYWORDS.len(),
        "docs/PLAN.md Phase 0 checklist: {items:#?}"
    );
    for (i, item) in items.iter().enumerate() {
        for (k, group) in ITEM_KEYWORDS.iter().enumerate() {
            let hit = group.iter().any(|kw| item.contains(kw));
            assert_eq!(
                hit,
                i == k,
                "keyword group {group:?} vs Phase 0 item {i}: {item}"
            );
        }
    }
}

#[test]
fn report_has_one_section_with_status_and_results_per_checklist_item() {
    let report = report_text();
    let secs = sections(&report);
    let cands: Vec<Vec<usize>> = (0..ITEM_KEYWORDS.len())
        .map(|item| {
            let mut c: Vec<usize> = (0..secs.len())
                .filter(|&s| candidate(&secs[s], item))
                .collect();
            c.sort_by_key(|&s| secs[s].body.len());
            c
        })
        .collect();
    let lacking: Vec<&[&str]> = cands
        .iter()
        .enumerate()
        .filter(|(_, c)| c.is_empty())
        .map(|(i, _)| ITEM_KEYWORDS[i])
        .collect();
    assert!(
        lacking.is_empty(),
        "{REPORT} needs a section per Phase 0 checklist item with a status \
         ({STATUSES:?}) and result columns {COLUMNS:?}; none found for {lacking:?}.\n\
         Headings: {:?}",
        secs.iter().map(|s| s.heading.as_str()).collect::<Vec<_>>()
    );
    let mut used = Vec::new();
    assert!(
        assign(&cands, 0, &mut used),
        "{REPORT} must give each of the 7 Phase 0 items its own section"
    );
}

#[test]
fn report_has_an_overall_verdict() {
    let report = report_text().to_lowercase();
    assert!(
        report.contains("gate not passed") || report.contains("gate passed"),
        "{REPORT} must state an overall verdict (e.g. 'Gate not passed: measurements pending')"
    );
}

#[test]
fn report_tells_the_owner_how_to_measure() {
    let report = report_text();
    let lower = report.to_lowercase();
    assert!(
        report.contains(SCRIPT),
        "{REPORT} must give the command {SCRIPT}"
    );
    assert!(
        lower.contains("reboot"),
        "{REPORT} must describe the reboot procedure"
    );
    assert!(
        report.contains("summary.md"),
        "{REPORT} must say to paste summary.md into the report"
    );
    assert!(
        lower.contains("purge"),
        "{REPORT} must document `sudo purge` as a weaker, separately labelled approximation"
    );
    assert!(
        report.contains("1.7"),
        "{REPORT} must note that a missed scanner gate is carried into run 1.7"
    );
}
