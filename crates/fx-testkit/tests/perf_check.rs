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
//! - it refuses to start on a locked screen, before taking the lock and
//!   again once it holds it, and records the screen's state in `run.txt`
//!   (with fake `ioreg`, `cargo` and benchmarks, so nothing is built);
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

/// B7 ("Disk writes a day") adds the index saves the background phases are
/// too short to see (`docs/gates/performance.md`): `fx-bench saves` projects
/// them from the store this run's app saved, its entry count and the
/// phase's scripted file activity, and B7 is the measured writes plus that
/// projection, never the measured writes alone.
#[test]
fn perf_check_adds_the_projected_index_saves_to_b7() {
    let code = code();
    let text = code.join("\n");
    for needle in [
        "\"$BIN/fx-bench\" saves",
        "-get-result-count",
        "churn_creates",
        "churn_deletes",
        "churn_seconds",
        "--measured-per-day",
    ] {
        assert!(
            text.contains(needle),
            "{SCRIPT}: B7's projection needs {needle}"
        );
    }
    assert!(
        text.contains("\"$OUT\"/bg-data/stores"),
        "{SCRIPT}: the saved store must be the run's own, in its data folder"
    );
    let b7: Vec<&String> = code
        .iter()
        .filter(|l| l.contains("emit disk_written_per_day_mb"))
        .collect();
    assert_eq!(b7.len(), 1, "{SCRIPT}: B7 is emitted in one place: {b7:?}");
    assert!(
        b7[0].contains("_disk_written_with_saves_per_day_bytes"),
        "{SCRIPT}: B7 must add the projected saves: {}",
        b7[0].trim()
    );
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
    /// records the displays by size only. The screen reads as unlocked
    /// (a fake `ioreg`), so the test passes on a locked Mac too.
    #[cfg(target_os = "macos")]
    #[test]
    fn removes_its_scratch_folder_and_lock_when_it_stops() {
        let fakes = crate::fakes::Fakes::new("perf-check-scratch");
        fakes.cargo(1, "");
        fakes.ioreg("false");
        let res = fakes.run(common::shell::SYSTEM_PATH, &["--tier", "quick"]);
        assert_refused(&res, 2, "release build failed", "a failing build");
        let left = fakes.tmp_left();
        assert!(left.is_empty(), "$TMPDIR must be left empty: {left:?}");
        let displays = fakes.run_txt_value("displays");
        assert!(
            displays == "unknown"
                || displays.split("; ").all(|d| d
                    .chars()
                    .all(|c| c.is_ascii_digit() || " x.@()Hzpxlookslikemain".contains(c))),
            "the displays line holds sizes only, no names: {displays}"
        );
    }
}

/// Runs of the real script with fake tools, so nothing is built or
/// measured: a throwaway folder whose `bin/` comes first on `PATH` (a
/// `cargo`, an `ioreg`), whose `tmp/` is `$TMPDIR` (the lock and the
/// scratch folder go there), whose `target/` is `CARGO_TARGET_DIR` (fake
/// benchmarks) and whose `out/` is `--out`. The fake tools append what they
/// were run with to `calls.log`. The folder is removed on drop.
#[cfg(target_os = "macos")]
mod fakes {
    use super::*;
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::process::Output;

    pub struct Fakes {
        pub root: PathBuf,
    }

    impl Fakes {
        pub fn new(tag: &str) -> Fakes {
            let root = temp_path(tag);
            fs::create_dir_all(root.join("bin")).unwrap();
            fs::create_dir_all(root.join("tmp")).unwrap();
            Fakes { root }
        }

        pub fn out(&self) -> PathBuf {
            self.root.join("out")
        }

        fn log(&self) -> PathBuf {
            self.root.join("calls.log")
        }

        /// Writes an executable `sh` script with `body` at `rel`.
        pub fn tool(&self, rel: &str, body: &str) {
            let path = self.root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }

        /// A `cargo` that logs `cargo <args>`, runs the shell code `then`
        /// and exits with `code`.
        pub fn cargo(&self, code: i32, then: &str) {
            let log = self.log();
            self.tool(
                "bin/cargo",
                &format!("echo \"cargo $*\" >>'{}'\n{then}\nexit {code}", log.display()),
            );
        }

        /// An `ioreg` that logs `ioreg <args> lock=held|free` (whether the
        /// run's lock folder exists) and prints the I/O Registry's root as
        /// `ioreg -n Root -d1` does, cut to the console users, with the
        /// screen locked while the shell test `locked_if` succeeds.
        pub fn ioreg(&self, locked_if: &str) {
            let log = self.log();
            self.tool(
                "bin/ioreg",
                &format!(
                    r#"if [ -d "$TMPDIR/not-everything-perf.lock" ]; then lock=held; else lock=free; fi
echo "ioreg $* lock=$lock" >>'{log}'
if {locked_if}; then key='"CGSSessionScreenIsLocked"=Yes,'; else key=; fi
cat <<EOF
+-o Root  <class IORegistryEntry, id 0x100000100, retain 36>
    {{
      "IOKitBuildVersion" = "Darwin Kernel Version 27.0.0"
      "IOConsoleUsers" = ({{"kCGSSessionOnConsoleKey"=Yes,${{key}}"kCGSSessionUserIDKey"=501}})
    }}
EOF"#,
                    log = log.display()
                ),
            );
        }

        /// Runs the repository's script with `args` and `--out`, the fake
        /// tools first on `PATH` and then `path`.
        pub fn run(&self, path: &str, args: &[&str]) -> Output {
            let mut cmd = Command::new("sh");
            cmd.arg(repo_root().join(SCRIPT))
                .args(args)
                .arg("--out")
                .arg(self.out())
                .env("PATH", format!("{}:{path}", self.root.join("bin").display()))
                .env("TMPDIR", self.root.join("tmp"))
                .env("CARGO_TARGET_DIR", self.root.join("target"));
            run_with_timeout(&mut cmd)
        }

        /// What the fake tools were run with, in order.
        pub fn calls(&self) -> Vec<String> {
            fs::read_to_string(self.log())
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        }

        /// What the run left in `$TMPDIR`.
        pub fn tmp_left(&self) -> Vec<OsString> {
            fs::read_dir(self.root.join("tmp"))
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect()
        }

        pub fn run_txt(&self) -> String {
            read(&self.out().join("run.txt"))
        }

        /// The value of the `run.txt` line `<key>: <value>`.
        pub fn run_txt_value(&self, key: &str) -> String {
            let run_txt = self.run_txt();
            let prefix = format!("{key}: ");
            let values: Vec<&str> = run_txt
                .lines()
                .filter_map(|l| l.strip_prefix(prefix.as_str()))
                .collect();
            assert_eq!(
                values.len(),
                1,
                "run.txt must have one line {prefix:?}:\n{run_txt}"
            );
            values[0].to_owned()
        }
    }

    impl Drop for Fakes {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

/// A locked screen stops a run before it starts: on one, the app's window
/// can't come forward, so every probe that summons it times out. On 13 Nov
/// 2026 the screen locked two minutes into an rc run, all 50 shortcut
/// presses of F3 timed out at 5 s, and the 75-minute run was wasted. The
/// script reads the screen's state with `ioreg -n Root -d1` before it takes
/// the lock and again once it holds it, records it in `run.txt` at the start
/// and at the end, and notes each step after which it was locked. Without
/// `ioreg` the state is unknown, and the run goes on.
#[cfg(target_os = "macos")]
mod screen {
    use super::*;
    use crate::fakes::Fakes;
    use common::shell::SYSTEM_PATH;
    use std::path::Path;

    const LOCKED: &str = "the screen is locked";
    const FREE: &str = "ioreg -n Root -d1 lock=free";
    const HELD: &str = "ioreg -n Root -d1 lock=held";

    #[test]
    fn refuses_a_locked_screen_before_taking_the_lock() {
        let fakes = Fakes::new("perf-check-locked");
        fakes.cargo(1, "");
        fakes.ioreg("true");
        let res = fakes.run(SYSTEM_PATH, &["--tier", "rc"]);
        assert_refused(&res, 2, LOCKED, "a locked screen");
        assert_refused(&res, 2, "nothing was changed", "a locked screen");
        assert!(
            !fakes.out().exists(),
            "--out must not be created on a locked screen"
        );
        let left = fakes.tmp_left();
        assert!(
            left.is_empty(),
            "no lock or scratch folder on a locked screen: {left:?}"
        );
        assert_eq!(
            fakes.calls(),
            [FREE],
            "one check, before the lock, and no build"
        );
    }

    /// Waiting for another run's lock can take an hour: a screen that
    /// locked meanwhile is refused once the lock is held, and the lock is
    /// given back.
    #[test]
    fn refuses_a_screen_that_locked_while_it_waited_for_the_lock() {
        let fakes = Fakes::new("perf-check-locked-later");
        fakes.cargo(1, "");
        fakes.ioreg("[ -d \"$TMPDIR/not-everything-perf.lock\" ]");
        let res = fakes.run(SYSTEM_PATH, &["--tier", "pr"]);
        assert_refused(&res, 2, LOCKED, "a screen locked once the lock is held");
        assert_refused(&res, 2, "nothing was changed", "a screen locked later");
        assert!(!fakes.out().exists(), "--out must not be created");
        let left = fakes.tmp_left();
        assert!(left.is_empty(), "the lock must be given back: {left:?}");
        assert_eq!(fakes.calls(), [FREE, HELD], "no build");
    }

    #[test]
    fn checks_the_screen_before_the_lock_and_the_build_and_records_it() {
        let fakes = Fakes::new("perf-check-unlocked");
        fakes.cargo(1, "");
        fakes.ioreg("false");
        let res = fakes.run(SYSTEM_PATH, &["--tier", "pr"]);
        assert_refused(&res, 2, "release build failed", "a failing build");
        let calls = fakes.calls();
        assert!(
            calls.len() == 4
                && calls[..2] == [FREE, HELD]
                && calls[2].starts_with("cargo build --release")
                && calls[3].starts_with("ioreg -n Root -d1 "),
            "the screen is checked before the lock, once it is held and before \
             the build, and at the end: {calls:?}"
        );
        assert_eq!(fakes.run_txt_value("screen at the start"), "unlocked");
        assert_eq!(fakes.run_txt_value("screen at the end"), "unlocked");
    }

    /// The end of a run is recorded however it ends: here the screen locks
    /// during a build that fails.
    #[test]
    fn records_a_screen_that_locked_during_the_run() {
        let fakes = Fakes::new("perf-check-locked-during");
        let marker = fakes.root.join("locked");
        fakes.cargo(1, &format!("touch '{}'", marker.display()));
        fakes.ioreg(&format!("[ -e '{}' ]", marker.display()));
        let res = fakes.run(SYSTEM_PATH, &["--tier", "pr"]);
        assert_refused(&res, 2, "release build failed", "a failing build");
        assert_eq!(fakes.run_txt_value("screen at the start"), "unlocked");
        assert_eq!(fakes.run_txt_value("screen at the end"), "locked");
    }

    /// With fake benchmarks, the screen locks during the catchup step: its
    /// note names catchup, and the matcher step before it has none.
    #[test]
    fn notes_each_step_after_which_the_screen_was_locked() {
        let fakes = Fakes::new("perf-check-locked-step");
        let marker = fakes.root.join("locked");
        fakes.cargo(0, "");
        fakes.ioreg(&format!("[ -e '{}' ]", marker.display()));
        fakes.tool("target/release/examples/matcher_bench", "exit 0");
        fakes.tool(
            "target/release/examples/catchup_bench",
            &format!("touch '{}'", marker.display()),
        );
        fakes.tool("target/release/fx-bench", "exit 0");
        let res = fakes.run(
            SYSTEM_PATH,
            &["--tier", "pr", "--step", "catchup", "--step", "matcher"],
        );
        assert_eq!(res.status.code(), Some(0), "{}", describe(&res));
        let run_txt = fakes.run_txt();
        let notes: Vec<&str> = run_txt
            .lines()
            .filter(|l| l.contains("screen locked after"))
            .collect();
        assert!(
            notes.len() == 1 && notes[0].contains("the catchup step"),
            "one note, naming the catchup step:\n{run_txt}"
        );
        assert_eq!(fakes.run_txt_value("screen at the start"), "unlocked");
        assert_eq!(fakes.run_txt_value("screen at the end"), "locked");
    }

    /// Without `ioreg`, or with one that fails, the screen's state is
    /// unknown: the run is not refused, and `run.txt` says so.
    #[test]
    fn runs_when_the_screen_state_is_unknown() {
        // No ioreg anywhere on PATH (it lives in /usr/sbin).
        let path = "/usr/bin:/bin";
        for dir in path.split(':') {
            assert!(
                !Path::new(dir).join("ioreg").exists(),
                "{dir} has an ioreg, so this case can't run without one"
            );
        }
        let missing = Fakes::new("perf-check-no-ioreg");
        missing.cargo(1, "");
        let failing = Fakes::new("perf-check-ioreg-fails");
        failing.cargo(1, "");
        failing.tool("bin/ioreg", "exit 1");
        for (fakes, path, why) in [
            (&missing, path, "no ioreg"),
            (&failing, SYSTEM_PATH, "a failing ioreg"),
        ] {
            let res = fakes.run(path, &["--tier", "pr"]);
            assert_refused(&res, 2, "release build failed", why);
            assert_eq!(fakes.run_txt_value("screen at the start"), "unknown", "{why}");
            assert_eq!(fakes.run_txt_value("screen at the end"), "unknown", "{why}");
        }
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
