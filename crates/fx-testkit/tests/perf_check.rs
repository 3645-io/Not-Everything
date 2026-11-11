//! Performance gate scripts and the workflow that checks PR descriptions.
//!
//! `scripts/perf-check.sh` runs one tier of the performance gate
//! (`docs/gates/performance.md`) and compares it with the baseline through
//! `fx-bench perf-compare`. `scripts/check-perf-report.sh` runs in CI
//! (`.github/workflows/perf-report.yml`) and fails a pull request that
//! changes shipped code without a passing summary in its description. These
//! tests check that:
//!
//! - `perf-check.sh` has a shell shebang and `set -eu`, builds in release
//!   with the app's `measure` feature, and finishes with `perf-compare`;
//! - it validates its arguments before taking the lock, building or
//!   creating anything, and never touches an existing `--out`;
//! - it deletes nothing but its own lock and, when it ends, its own scratch
//!   folder (it leaves `$TMPDIR` as it found it), runs only the app and
//!   `fx-bench churn` in the background (the app has to run while it is
//!   sampled),
//!   gives every app launch its own `--data-dir`, so the daily app's and the
//!   Preview app's data folders are never touched, and runs the file
//!   activity in a scratch folder the app indexes even without Full Disk
//!   Access;
//! - its quick-tier map from crates to steps is the one the agent rule
//!   (`.agents/rules/performance.md`) lists;
//! - the soak (C3) runs only when asked for with `--step soak` at the rc
//!   tier, in the scratch folder, and is in no default list of steps;
//! - `check-perf-report.sh` passes a pull request that changes no shipped
//!   code, or whose description has a passing summary of a run with no
//!   skipped step or the owner's approval to skip, fails one without, and
//!   never runs the description as shell;
//! - the workflow runs it on pull requests, again when the description is
//!   edited, passes the description through the environment and checks out
//!   without keeping the token.
//!
//! Only `std` is used.

mod common;

use common::shell::{
    Sandbox, assert_refused, code_lines, describe, run_with_timeout, strip_quoted, temp_path, words,
};
use common::{read, repo_root};
use std::fs;
use std::process::Command;

const SCRIPT: &str = "scripts/perf-check.sh";
const REPORT: &str = "scripts/check-perf-report.sh";
const WORKFLOW: &str = ".github/workflows/perf-report.yml";
const RULE: &str = ".agents/rules/performance.md";

fn script_text() -> String {
    read(&repo_root().join(SCRIPT))
}

fn code() -> Vec<String> {
    code_lines(&script_text())
}

#[test]
fn scripts_have_a_shell_shebang_and_strict_mode() {
    for path in [SCRIPT, REPORT] {
        let text = read(&repo_root().join(path));
        assert!(text.starts_with("#!/bin/sh"), "{path} must be a sh script");
        assert!(
            code_lines(&text).iter().any(|l| l.trim() == "set -eu"),
            "{path} must run with `set -eu`"
        );
    }
}

#[test]
fn perf_check_builds_with_the_measure_feature_and_compares() {
    let code = code().join("\n");
    assert!(
        code.contains("cargo build --release --locked --workspace --bins --examples --features fx-ui-macos/measure"),
        "{SCRIPT} must build everything in release with the app's measurement flags"
    );
    for needle in [
        "perf-compare",
        "summary.md",
        "matcher_bench",
        "startup_bench",
        "catchup_bench",
        "freshness_bench",
        "first_run_bench",
        "--gate-probe",
        "--summon-probe",
        "--sort-probe",
        "sample",
        "churn",
    ] {
        assert!(code.contains(needle), "{SCRIPT} must run {needle}");
    }
}

/// The only removals are of the script's own lock (its PID file and its
/// folder) and, when it ends, of its own scratch folder, by the exact path
/// it made, only while that path is still in `$TMPDIR`.
#[test]
fn perf_check_deletes_nothing_but_its_lock_and_scratch_folder() {
    const SCRATCH_REMOVAL: &str =
        "\"${TMPDIR:-/tmp}\"/not-everything-perf-*) rm -rf \"$SCRATCH\" ;;";
    let forbidden = [
        "rm", "rmdir", "unlink", "mv", "purge", "-delete", "srm", "sudo",
    ];
    let hits: Vec<String> = code()
        .into_iter()
        .filter(|line| {
            words(&strip_quoted(line))
                .iter()
                .any(|w| forbidden.contains(&w.as_str()))
        })
        .filter(|line| {
            !line.contains("\"$LOCK/pid\"")
                && !line.contains("rmdir \"$LOCK\"")
                && line.trim() != SCRATCH_REMOVAL
        })
        .map(|l| l.trim().to_owned())
        .collect();
    assert!(
        hits.is_empty(),
        "{SCRIPT} may remove only its own lock and scratch folder: {hits:?}"
    );
    let removals = code().into_iter().filter(|l| l.contains("rm -rf")).count();
    assert_eq!(
        removals, 1,
        "{SCRIPT} must remove its scratch folder in one place, guarded"
    );
}

/// Benchmarks run one at a time. The only background jobs are the app,
/// which must keep running while `fx-bench sample` samples it, and `fx-bench
/// churn`, the file activity it is sampled under.
#[test]
fn perf_check_runs_only_the_app_and_churn_in_the_background() {
    let mut hits = Vec::new();
    for line in code() {
        let bare = strip_quoted(&line);
        let trimmed = bare.trim_end();
        let backgrounded = trimmed.ends_with('&') && !trimmed.ends_with("&&");
        if backgrounded
            && !line.contains("\"$BIN/fx-ui-macos\"")
            && !line.contains("\"$BIN/fx-bench\" churn")
        {
            hits.push(line.trim().to_owned());
        }
        if line.contains("xargs -P") || words(&bare).iter().any(|w| w == "parallel") {
            hits.push(line.trim().to_owned());
        }
    }
    assert!(
        hits.is_empty(),
        "{SCRIPT} must run its benchmarks one at a time: {hits:?}"
    );
}

/// Every launch of the app gets a data folder inside the run's output, so a
/// measurement never reads or writes the daily app's index and settings.
#[test]
fn perf_check_gives_every_app_launch_its_own_data_folder() {
    let launches: Vec<String> = code()
        .into_iter()
        .filter(|l| l.contains("\"$BIN/fx-ui-macos\""))
        .collect();
    assert!(!launches.is_empty(), "{SCRIPT} must launch the app");
    for line in &launches {
        assert!(
            line.contains("--data-dir \"$OUT/"),
            "{SCRIPT}: every app launch needs --data-dir inside $OUT: {}",
            line.trim()
        );
    }
}

/// The scripted file activity goes to the scratch folder in `$TMPDIR`: an app
/// without Full Disk Access skips Documents, Desktop and Downloads, where
/// `--out` may be, so activity there would cost the app nothing.
#[test]
fn perf_check_runs_file_activity_where_the_app_indexes_it() {
    let text = code().join("\n");
    assert!(
        text.contains("SCRATCH=\"${TMPDIR:-/tmp}/not-everything-perf-"),
        "{SCRIPT} must make its scratch folder in $TMPDIR"
    );
    let churns: Vec<String> = code()
        .into_iter()
        .filter(|l| l.contains("\"$BIN/fx-bench\" churn"))
        .collect();
    assert!(!churns.is_empty(), "{SCRIPT} must run fx-bench churn");
    for line in &churns {
        assert!(
            line.contains("--dir \"$SCRATCH/"),
            "{SCRIPT}: churn must write in the scratch folder: {}",
            line.trim()
        );
    }
}

/// The quick tier's crate-to-steps map in the script and the table in the
/// agent rule are the same, so the rule never promises a step the script
/// doesn't run.
#[test]
fn quick_steps_match_the_agent_rule() {
    let script = script_text();
    let start = script
        .find("steps_for_crate() {")
        .expect("perf-check.sh must have steps_for_crate");
    let body: String = script[start..]
        .lines()
        .take_while(|l| l.trim() != "}")
        .collect::<Vec<_>>()
        .join("\n");
    let mut from_script: Vec<(String, String)> = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        let Some((crates, rest)) = line.split_once(") echo \"") else {
            continue;
        };
        let steps = rest.trim_end_matches(" ;;").trim_end_matches('"');
        if crates == "*" {
            continue;
        }
        for c in crates.split('|') {
            from_script.push((c.trim().to_owned(), steps.to_owned()));
        }
    }
    from_script.sort();

    let rule = read(&repo_root().join(RULE));
    let mut from_rule: Vec<(String, String)> = Vec::new();
    for line in rule.lines() {
        let cells: Vec<&str> = line.trim().split('|').map(str::trim).collect();
        // | `fx-query`, `fx-model` | `matcher` |
        if cells.len() != 4 || !cells[1].starts_with("`fx-") {
            continue;
        }
        let steps: Vec<String> = cells[2]
            .split(',')
            .map(|s| s.trim().trim_matches('`').to_owned())
            .collect();
        for c in cells[1].split(',') {
            from_rule.push((c.trim().trim_matches('`').to_owned(), steps.join(" ")));
        }
    }
    from_rule.sort();
    assert!(
        !from_rule.is_empty(),
        "{RULE} must list the quick steps per crate"
    );
    assert_eq!(
        from_script, from_rule,
        "{SCRIPT}'s steps_for_crate and the table in {RULE} must match"
    );
}

/// The soak (C3) runs for hours, so it runs only when asked for: never in
/// a default list of steps, always in the scratch folder in `$TMPDIR`, and
/// its `soak_differences` goes to the metrics.
#[test]
fn perf_check_runs_the_soak_only_when_asked() {
    let code = code();
    let soaks: Vec<&String> = code.iter().filter(|l| l.contains("\"$EX/soak\"")).collect();
    assert!(!soaks.is_empty(), "{SCRIPT} must run the soak example");
    for line in &soaks {
        assert!(
            line.contains("--dir \"$SCRATCH\"") && line.contains("--duration"),
            "{SCRIPT}: the soak must run for its duration in the scratch folder: {}",
            line.trim()
        );
    }
    let text = code.join("\n");
    assert!(
        text.contains("emit soak_differences"),
        "{SCRIPT} must report soak_differences"
    );
    let defaults: Vec<&String> = code
        .iter()
        .filter(|l| l.trim_start().starts_with("STEPS=\""))
        .collect();
    assert!(!defaults.is_empty(), "{SCRIPT} must set default steps");
    for line in defaults {
        assert!(
            !line.contains("soak"),
            "{SCRIPT}: the soak must not be a default step: {}",
            line.trim()
        );
    }
}

#[cfg(unix)]
mod refusals {
    use super::*;

    /// Runs the repository's script with only the system folders on PATH,
    /// so a run that got past its checks would stop at "cargo is not on
    /// PATH" instead of building.
    fn run(args: &[&str]) -> std::process::Output {
        let mut cmd = Command::new("sh");
        cmd.arg(repo_root().join(SCRIPT))
            .args(args)
            .env("PATH", common::shell::SYSTEM_PATH);
        run_with_timeout(&mut cmd)
    }

    #[test]
    fn refuses_bad_arguments_before_creating_anything() {
        let out_dir = temp_path("perf-check-refused");
        let out = out_dir.to_str().unwrap();
        let cases: [(&[&str], &str); 7] = [
            (&["--out", out], "--tier is required"),
            (
                &["--tier", "nightly", "--out", out],
                "--tier must be quick, pr or rc",
            ),
            (&["--tier", "pr"], "--out is required"),
            (
                &["--tier", "pr", "--out", out, "--only", "fx-query"],
                "--only is for the quick tier",
            ),
            (
                &["--tier", "pr", "--out", out, "--step", "everything"],
                "unknown --step",
            ),
            (
                &["--tier", "pr", "--out", out, "--bg-minutes", "ten"],
                "--bg-minutes must be a whole number",
            ),
            (
                &["--tier", "pr", "--out", out, "--cache", "lukewarm"],
                "--cache must be warm or cold",
            ),
        ];
        for (args, phrase) in cases {
            let res = run(args);
            assert_refused(&res, 2, phrase, &format!("{args:?}"));
            assert!(!out_dir.exists(), "{args:?} must not create --out");
        }
    }

    #[test]
    fn refuses_bad_soak_arguments_before_creating_anything() {
        let out_dir = temp_path("perf-check-soak-refused");
        let out = out_dir.to_str().unwrap();
        let cases: [(&[&str], &str); 5] = [
            (
                &["--tier", "pr", "--out", out, "--step", "soak"],
                "--step soak runs at the rc tier only",
            ),
            (
                &["--tier", "quick", "--out", out, "--step", "soak"],
                "--step soak runs at the rc tier only",
            ),
            (
                &[
                    "--tier",
                    "rc",
                    "--out",
                    out,
                    "--step",
                    "soak",
                    "--soak-minutes",
                    "ten",
                ],
                "--soak-minutes must be a whole number",
            ),
            (
                &[
                    "--tier",
                    "rc",
                    "--out",
                    out,
                    "--step",
                    "soak",
                    "--soak-minutes",
                    "0",
                ],
                "--soak-minutes must be above 0",
            ),
            (
                &["--tier", "rc", "--out", out, "--soak-minutes", "5"],
                "--soak-minutes needs --step soak",
            ),
        ];
        for (args, phrase) in cases {
            let res = run(args);
            assert_refused(&res, 2, phrase, &format!("{args:?}"));
            assert!(!out_dir.exists(), "{args:?} must not create --out");
        }
    }

    /// A valid soak request gets past every argument check: without cargo
    /// on PATH it stops there, before building or creating anything.
    #[test]
    fn accepts_the_soak_at_the_rc_tier() {
        let dir = temp_path("perf-check-soak-accepted");
        let res = run(&[
            "--tier",
            "rc",
            "--out",
            dir.to_str().unwrap(),
            "--step",
            "soak",
            "--soak-minutes",
            "1440",
        ]);
        assert_refused(&res, 2, "cargo is not on PATH", "a valid soak request");
        assert!(!dir.exists(), "--out must not be created without cargo");
    }

    #[test]
    fn never_touches_an_existing_out_folder() {
        let dir = temp_path("perf-check-existing");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("keep.txt"), "kept").unwrap();
        let res = run(&["--tier", "quick", "--out", dir.to_str().unwrap()]);
        assert_refused(&res, 2, "already exists", "an existing --out");
        assert_eq!(fs::read_to_string(dir.join("keep.txt")).unwrap(), "kept");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stops_before_building_without_cargo() {
        let dir = temp_path("perf-check-no-cargo");
        let res = run(&["--tier", "quick", "--out", dir.to_str().unwrap()]);
        assert_refused(&res, 2, "cargo is not on PATH", "no cargo on PATH");
        assert!(!dir.exists(), "--out must not be created without cargo");
    }

    /// A run that stops after making its scratch folder (here the build
    /// fails, through a `cargo` that only exits 1) leaves nothing in
    /// `$TMPDIR`: no lock and no scratch folder, whose files the app would
    /// otherwise index in every later run. `--out` keeps `run.txt`, which
    /// records the displays by size only.
    #[cfg(target_os = "macos")]
    #[test]
    fn removes_its_scratch_folder_and_lock_when_it_stops() {
        use std::os::unix::fs::PermissionsExt;
        let root = temp_path("perf-check-scratch");
        let bin = root.join("bin");
        let tmp = root.join("tmp");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(&tmp).unwrap();
        let cargo = bin.join("cargo");
        fs::write(&cargo, "#!/bin/sh\nexit 1\n").unwrap();
        fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755)).unwrap();
        let out = root.join("out");
        let mut cmd = Command::new("sh");
        cmd.arg(repo_root().join(SCRIPT))
            .args(["--tier", "quick", "--out", out.to_str().unwrap()])
            .env(
                "PATH",
                format!("{}:{}", bin.display(), common::shell::SYSTEM_PATH),
            )
            .env("TMPDIR", &tmp);
        let res = run_with_timeout(&mut cmd);
        assert_refused(&res, 2, "release build failed", "a failing build");
        let left: Vec<_> = fs::read_dir(&tmp)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert!(left.is_empty(), "$TMPDIR must be left empty: {left:?}");
        let run_txt = fs::read_to_string(out.join("run.txt")).unwrap();
        let displays = run_txt
            .lines()
            .find_map(|l| l.strip_prefix("displays: "))
            .unwrap_or_else(|| panic!("run.txt must record the displays:\n{run_txt}"));
        assert!(
            displays == "unknown"
                || displays.split("; ").all(|d| d
                    .chars()
                    .all(|c| c.is_ascii_digit() || " x.@()Hzpxlookslikemain".contains(c))),
            "the displays line holds sizes only, no names: {displays}"
        );
        fs::remove_dir_all(&root).unwrap();
    }
}

#[cfg(unix)]
mod report {
    use super::*;
    use std::process::Output;

    const PASSING: &str = "Tests pass.\n\nperf-compare: 27 pass, 0 fail, 0 not measured, \
         0 no baseline, 2 tracked, 0 skipped\n";

    /// A repository with `dev`, and a branch `work` checked out that
    /// changes `rel`.
    fn repo(tag: &str, rel: &str) -> Sandbox {
        let repo = Sandbox::new(tag);
        repo.copy_from_repo(REPORT);
        repo.write("Cargo.toml", "[workspace]\n");
        repo.write("crates/fx-query/src/lib.rs", "// v1\n");
        repo.write("crates/fx-bench/src/lib.rs", "// v1\n");
        repo.write("docs/notes.md", "v1\n");
        repo.commit_all("base");
        repo.git(&["branch", "dev"]);
        repo.git(&["checkout", "--quiet", "-b", "work"]);
        repo.write(rel, "// v2\n");
        repo.commit_all("change");
        repo
    }

    fn check(repo: &Sandbox, body: &str) -> Output {
        let mut cmd = Command::new("sh");
        cmd.arg(repo.dir.join(REPORT))
            .args(["dev", "HEAD"])
            .current_dir(&repo.dir)
            .env("PATH", common::shell::SYSTEM_PATH)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("PR_BODY", body);
        run_with_timeout(&mut cmd)
    }

    fn assert_passed(out: &Output) {
        assert_eq!(out.status.code(), Some(0), "{}", describe(out));
    }

    #[test]
    fn passes_without_shipped_code() {
        for rel in [
            "docs/notes.md",
            "crates/fx-bench/src/lib.rs",
            "crates/fx-query/tests/new.rs",
            "crates/fx-query/examples/bench.rs",
        ] {
            assert_passed(&check(&repo("report-none", rel), ""));
        }
    }

    #[test]
    fn fails_shipped_code_without_a_passing_summary() {
        for body in [
            "",
            "perf-compare: 25 pass, 2 fail, 0 not measured, 0 no baseline, 2 tracked, 0 skipped",
            "perf-compare: 25 pass, 0 fail, 2 not measured, 0 no baseline, 2 tracked, 0 skipped",
            // A run of some steps only (`--step`): the others never ran.
            "perf-compare: 10 pass, 0 fail, 0 not measured, 0 no baseline, 0 tracked, 19 skipped",
            // Without the skipped count a line can't show that every step ran.
            "perf-compare: 27 pass, 0 fail, 0 not measured, 0 no baseline, 2 tracked",
            "Performance: skipped",
        ] {
            let out = check(&repo("report-missing", "crates/fx-query/src/lib.rs"), body);
            assert_refused(&out, 1, "no\npassing performance summary", body);
        }
    }

    #[test]
    fn passes_shipped_code_with_a_summary_or_the_owners_approval() {
        assert_passed(&check(
            &repo("report-pass", "crates/fx-query/src/lib.rs"),
            PASSING,
        ));
        // A full run with a failure, and the failing step re-run alone and
        // passing (`.agents/rules/performance.md`: only a second failure
        // counts).
        assert_passed(&check(
            &repo("report-rerun", "crates/fx-query/src/lib.rs"),
            "Full run:\nperf-compare: 26 pass, 1 fail, 0 not measured, 0 no baseline, 2 tracked, \
             0 skipped\n\nS2 re-run alone:\nperf-compare: 1 pass, 0 fail, 0 not measured, \
             0 no baseline, 0 tracked, 28 skipped\n",
        ));
        assert_passed(&check(
            &repo("report-lock", "Cargo.lock"),
            "Bump a crate.\nPerformance: skipped, approved by the owner (lock file only)\n",
        ));
    }

    /// The description is data: shell syntax in it never runs.
    #[test]
    fn never_runs_the_description() {
        let marker = temp_path("report-injection");
        let body = format!("$(touch {m}) `touch {m}` ; touch {m}", m = marker.display());
        let out = check(
            &repo("report-injection", "crates/fx-query/src/lib.rs"),
            &body,
        );
        assert_eq!(out.status.code(), Some(1), "{}", describe(&out));
        assert!(!marker.exists(), "the description must never run as shell");
    }
}

#[test]
fn workflow_checks_descriptions_on_pull_requests() {
    let text = read(&repo_root().join(WORKFLOW));
    for needle in [
        "  pull_request:\n    types: [opened, edited, reopened, synchronize]",
        "runs-on: ubuntu-latest",
        "uses: actions/checkout@v7",
        "fetch-depth: 0",
        "persist-credentials: false",
        "PR_BODY: ${{ github.event.pull_request.body }}",
        "run: sh scripts/check-perf-report.sh \"$BASE_SHA\" \"$HEAD_SHA\"",
    ] {
        assert!(text.contains(needle), "{WORKFLOW} must have {needle:?}");
    }
    assert!(
        !text.contains("push:") && !text.contains("pull_request_target"),
        "{WORKFLOW} must run on pull requests only, without write access"
    );
    let run_lines: Vec<&str> = text.lines().filter(|l| l.contains("run:")).collect();
    for line in run_lines {
        assert!(
            !line.contains("${{"),
            "{WORKFLOW}: no expression may be pasted into a run line: {line:?}"
        );
    }
}
