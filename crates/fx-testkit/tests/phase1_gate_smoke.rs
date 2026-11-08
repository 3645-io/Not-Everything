//! Phase 1 gate smoke run, ignored by default.
//!
//! Runs `scripts/phase1-gate.sh --machine ci --cache warm --quick --skip-ui`
//! end to end: a release build in its own target directory
//! (`target/phase1-gate-smoke`, so it never waits on the outer cargo's
//! lock), every benchmark at small entry counts, the conformance and fx-cli
//! tests, and `fx-bench phase1-summary`. It checks that `summary.md` and
//! `gate-metrics.txt` are written and carry the CI-scale results. It takes
//! minutes, so it only runs on request:
//!
//! ```text
//! cargo test -p fx-testkit --test phase1_gate_smoke -- --ignored --nocapture
//! ```
//!
//! The results directory is kept under the system temp directory and its
//! path is printed. Only `std` is used.

mod common;

#[cfg(unix)]
#[test]
#[ignore = "runs every Phase 1 benchmark at small scale (minutes); run with --ignored"]
fn quick_gate_run_writes_summary_md() {
    use common::{read, repo_root};
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    let root = repo_root();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let out = std::env::temp_dir().join(format!(
        "fx-phase1-gate-smoke-{}-{nanos}",
        std::process::id()
    ));
    let out_str = out.to_str().expect("utf-8 temp path");

    let status = Command::new("sh")
        .arg(root.join("scripts/phase1-gate.sh"))
        .args([
            "--machine",
            "ci",
            "--cache",
            "warm",
            "--quick",
            "--skip-ui",
            "--out",
            out_str,
        ])
        .env("CARGO_TARGET_DIR", root.join("target/phase1-gate-smoke"))
        .current_dir(&root)
        .status()
        .expect("run scripts/phase1-gate.sh");
    println!("results in {out_str}, script exit {:?}", status.code());

    // Exit 0 (all pass) or 1 (some criterion failed or not measured, as
    // the skipped gate probe guarantees); never 2 (usage or I/O error).
    assert!(
        matches!(status.code(), Some(0 | 1)),
        "phase1-gate.sh exited with {:?}; see {out_str}",
        status.code()
    );
    let summary_path = out.join("summary.md");
    assert!(summary_path.is_file(), "no summary.md in {out_str}");
    let summary = read(&summary_path);
    println!("{summary}");
    for id in ["syntax", "goal1_launch", "goal3_freshness", "memory", "cli"] {
        assert!(
            summary.contains(id),
            "summary.md lacks the `{id}` row:\n{summary}"
        );
    }
    assert!(
        summary.contains("Run checks"),
        "summary.md lacks the run checks"
    );

    let metrics = read(&out.join("gate-metrics.txt"));
    println!("gate-metrics.txt:\n{metrics}");
    let cases: usize = metrics
        .lines()
        .find_map(|l| l.strip_prefix("syntax_cases="))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or_else(|| panic!("no syntax_cases in gate-metrics.txt:\n{metrics}"));
    assert!(cases >= 300, "syntax_cases={cases}, need >= 300");
    for line in ["syntax_failures=0", "cli_ok=true", "catchup_equal=true"] {
        assert!(
            metrics.lines().any(|l| l == line),
            "gate-metrics.txt lacks `{line}`:\n{metrics}"
        );
    }
    for key in ["engine_2m_ms=", "engine_5m_ms=", "bytes_per_entry="] {
        assert!(
            metrics.lines().any(|l| l.starts_with(key)),
            "gate-metrics.txt lacks `{key}`:\n{metrics}"
        );
    }
}
