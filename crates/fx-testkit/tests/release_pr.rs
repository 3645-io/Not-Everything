//! Release pull request checks, and the CI workflow that runs them.
//!
//! `scripts/check-release-pr.sh [BASE [HEAD]]` runs in CI on every pull
//! request into `main` (`.github/workflows/ci.yml`). These tests check that:
//!
//! - the workflow runs it for pull requests into `main`, and by hand
//!   (`workflow_dispatch`) on an `rc/X.Y.Z` branch, on a checkout with the
//!   full history and tags (`fetch-depth: 0`);
//! - the version check passes only when the Cargo version at the PR head is
//!   higher than every released version (the `release/vX.Y.Z` tags; release
//!   candidates and other tags do not count), unless the PR changes only
//!   docs and CI files (`*.md`, `docs/`, `.github/`);
//! - the RC gate passes only when the PR head is the commit of the latest
//!   `release/v<version>-RC-N` tag and contains `BASE`.
//!
//! And that the workflow runs on pull requests and by hand, never on
//! pushes; checks out with `actions/checkout@v7` without keeping the token
//! in the checkout; and checks the doc links on macOS.
//!
//! The behaviour tests run a copy of the script in a throwaway git
//! repository. Only `std` is used.

mod common;

use common::shell::code_lines;
use common::{read, repo_root};

/// Path of the check script, relative to the repository root.
const SCRIPT: &str = "scripts/check-release-pr.sh";

/// Path of the CI workflow, relative to the repository root.
const CI: &str = ".github/workflows/ci.yml";

fn ci_text() -> String {
    read(&repo_root().join(CI))
}

/// The lines of the job `name` (a key under `jobs:`), without its key line.
fn job(ci: &str, name: &str) -> String {
    let start = ci
        .find(&format!("\n  {name}:"))
        .unwrap_or_else(|| panic!("{CI} must have a {name} job"));
    ci[start + 1..]
        .lines()
        .skip(1)
        .take_while(|l| l.is_empty() || l.starts_with("    "))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn script_has_a_shell_shebang_and_strict_mode() {
    let text = read(&repo_root().join(SCRIPT));
    assert!(
        text.starts_with("#!/bin/sh"),
        "{SCRIPT} must be a sh script"
    );
    assert!(
        code_lines(&text).iter().any(|l| l.trim() == "set -eu"),
        "{SCRIPT} must run with `set -eu`"
    );
}

/// The job runs the script on pull requests into `main`, and by hand on an
/// `rc/` branch (after tagging a release candidate), with the whole history
/// and every tag, on the PR's head commit or the branch's.
#[test]
fn ci_runs_the_check_on_pull_requests_into_main() {
    let job = job(&ci_text(), "release-pr");
    for needle in [
        "(github.event_name == 'pull_request' && github.base_ref == 'main') ||",
        "(github.event_name == 'workflow_dispatch' && startsWith(github.ref_name, 'rc/'))",
        "fetch-depth: 0",
        "ref: ${{ github.event.pull_request.head.sha || github.sha }}",
        "sh scripts/check-release-pr.sh origin/main",
    ] {
        assert!(
            job.contains(needle),
            "the release-pr job must have {needle:?}:\n{job}"
        );
    }
}

/// CI runs on pull requests and by hand, never on pushes.
#[test]
fn ci_runs_on_pull_requests_and_by_hand() {
    let ci = ci_text();
    let start = ci
        .find("\non:\n")
        .unwrap_or_else(|| panic!("{CI} must have an `on:` block"));
    let triggers: Vec<&str> = ci[start + 5..]
        .lines()
        .take_while(|l| l.is_empty() || l.starts_with("  "))
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    assert_eq!(
        triggers,
        ["pull_request:", "workflow_dispatch:"],
        "{CI} must run on pull requests and by hand only"
    );
}

/// Every checkout uses `actions/checkout@v7` and leaves no token in the
/// checkout (`persist-credentials: false`): no step pushes.
#[test]
fn ci_checks_out_without_keeping_the_token() {
    let ci = ci_text();
    let lines: Vec<&str> = ci.lines().collect();
    let mut checkouts = 0;
    for (i, line) in lines.iter().enumerate() {
        let Some(at) = line.find("uses: actions/checkout@") else {
            continue;
        };
        checkouts += 1;
        assert!(
            line.trim_end().ends_with("actions/checkout@v7"),
            "{CI}: use actions/checkout@v7: {line:?}"
        );
        // The step's other lines are indented past its `- uses:`.
        let step: Vec<&str> = lines[i + 1..]
            .iter()
            .take_while(|l| l.len() - l.trim_start().len() >= at)
            .copied()
            .collect();
        assert!(
            step.iter()
                .any(|l| l.trim() == "persist-credentials: false"),
            "{CI}: the checkout on line {} must set persist-credentials: false",
            i + 1
        );
    }
    assert_eq!(checkouts, 3, "{CI}: one checkout per job");
}

/// The macOS job fails on a broken intra-doc link, private items included.
#[test]
fn ci_checks_the_doc_links_on_macos() {
    let job = job(&ci_text(), "check");
    for needle in [
        "macos-latest",
        "RUSTDOCFLAGS: -D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links",
        "cargo doc --workspace --no-deps --document-private-items --locked",
    ] {
        assert!(
            job.contains(needle),
            "the check job must have {needle:?}:\n{job}"
        );
    }
}

#[cfg(unix)]
mod behaviour {
    use super::*;
    use common::shell::{Sandbox, assert_refused, describe};
    use std::process::Output;

    fn cargo_toml(version: &str) -> String {
        format!("[workspace]\nmembers = []\n\n[workspace.package]\nversion = \"{version}\"\n")
    }

    /// `main` at 0.1.3, released as `release/v0.1.3`, and a `rc/0.2.0`
    /// branch that bumps the version and changes code, checked out.
    fn rc_repo(tag: &str) -> Sandbox {
        let repo = Sandbox::new(tag);
        repo.copy_from_repo(SCRIPT);
        repo.write("Cargo.toml", &cargo_toml("0.1.3"));
        repo.write("src/lib.rs", "// v0.1.3\n");
        repo.commit_all("0.1.3");
        repo.git(&["tag", "release/v0.1.3"]);
        repo.git(&["checkout", "--quiet", "-b", "rc/0.2.0"]);
        repo.write("Cargo.toml", &cargo_toml("0.2.0"));
        repo.write("src/lib.rs", "// v0.2.0\n");
        repo.commit_all("0.2.0");
        repo
    }

    fn check(repo: &Sandbox) -> Output {
        repo.run(SCRIPT, &["main", "HEAD"])
    }

    fn assert_passed(out: &Output) {
        assert_eq!(out.status.code(), Some(0), "{}", describe(out));
    }

    #[test]
    fn passes_for_the_latest_rc_with_a_higher_version() {
        let repo = rc_repo("pass");
        repo.git(&["tag", "-a", "release/v0.2.0-RC-1", "-m", "rc 1"]);
        assert_passed(&check(&repo));
    }

    #[test]
    fn passes_with_older_release_candidates_and_releases_around() {
        let repo = rc_repo("pass-many");
        repo.git(&["tag", "release/v0.1.3-RC-1", "main"]);
        repo.git(&["tag", "release/v0.1.2", "main"]);
        repo.git(&["tag", "-a", "release/v0.2.0-RC-1", "-m", "rc 1"]);
        repo.write("src/fix.rs", "// fix\n");
        repo.commit_all("fix");
        repo.git(&["tag", "-a", "release/v0.2.0-RC-2", "-m", "rc 2"]);
        assert_passed(&check(&repo));
    }

    /// Releases are tagged `release/vX.Y.Z`; a plain `vX.Y.Z` tag is not a
    /// release, so it doesn't raise the version a PR must beat.
    #[test]
    fn a_plain_v_tag_is_not_a_release() {
        let repo = rc_repo("plain-v");
        repo.git(&["tag", "v0.9.0", "main"]);
        repo.git(&["tag", "-a", "release/v0.2.0-RC-1", "-m", "rc 1"]);
        let out = check(&repo);
        assert_passed(&out);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("last release (0.1.3)"),
            "{}",
            describe(&out)
        );
    }

    #[test]
    fn refuses_a_version_that_is_not_higher_than_the_last_release() {
        // Not bumped past release/v0.1.3.
        let repo = rc_repo("not-bumped");
        repo.write("Cargo.toml", &cargo_toml("0.1.3"));
        repo.commit_all("undo the bump");
        repo.git(&["tag", "-a", "release/v0.1.3-RC-1", "-m", "rc 1"]);
        assert_refused(&check(&repo), 1, "0.1.3", "a version already released");

        // Lower than a release/vX.Y.Z tag.
        let repo = rc_repo("lower");
        repo.git(&["tag", "release/v0.10.0", "main"]);
        repo.git(&["tag", "-a", "release/v0.2.0-RC-1", "-m", "rc 1"]);
        assert_refused(
            &check(&repo),
            1,
            "0.10.0",
            "a version below the last release",
        );

        // Equal to a release/vX.Y.Z tag.
        let repo = rc_repo("equal");
        repo.git(&["tag", "release/v0.2.0", "main"]);
        repo.git(&["tag", "-a", "release/v0.2.0-RC-1", "-m", "rc 1"]);
        assert_refused(
            &check(&repo),
            1,
            "higher",
            "a version equal to the last release",
        );
    }

    /// A PR that changes only docs and CI files needs no new version, but
    /// still goes through a release candidate.
    #[test]
    fn docs_and_ci_changes_need_no_new_version() {
        let repo = Sandbox::new("docs-only");
        repo.copy_from_repo(SCRIPT);
        repo.write("Cargo.toml", &cargo_toml("0.2.0"));
        repo.write("src/lib.rs", "// v0.2.0\n");
        repo.commit_all("0.2.0");
        repo.git(&["tag", "release/v0.2.0"]);
        repo.git(&["checkout", "--quiet", "-b", "rc/0.2.0"]);
        repo.write("README.md", "# Docs\n");
        repo.write("docs/guide.md", "Guide\n");
        repo.write(".github/workflows/ci.yml", "name: CI\n");
        repo.commit_all("docs");
        repo.git(&["tag", "-a", "release/v0.2.0-RC-1", "-m", "rc 1"]);
        assert_passed(&check(&repo));

        repo.write("src/lib.rs", "// changed\n");
        repo.commit_all("code");
        repo.git(&["tag", "-a", "release/v0.2.0-RC-2", "-m", "rc 2"]);
        assert_refused(
            &check(&repo),
            1,
            "higher",
            "a code change needs a new version",
        );
    }

    #[test]
    fn refuses_a_head_without_a_release_candidate() {
        let repo = rc_repo("no-rc");
        assert_refused(&check(&repo), 1, "release/v0.2.0-RC-", "no RC tag");
    }

    #[test]
    fn refuses_a_head_that_is_not_the_latest_rc() {
        // Commits after the RC.
        let repo = rc_repo("after-rc");
        repo.git(&["tag", "-a", "release/v0.2.0-RC-1", "-m", "rc 1"]);
        repo.write("src/late.rs", "// late\n");
        repo.commit_all("late");
        assert_refused(
            &check(&repo),
            1,
            "release/v0.2.0-RC-1",
            "commits after the RC",
        );

        // A newer RC elsewhere.
        let repo = rc_repo("older-rc");
        repo.git(&["tag", "-a", "release/v0.2.0-RC-2", "-m", "rc 2", "main"]);
        repo.git(&["tag", "-a", "release/v0.2.0-RC-1", "-m", "rc 1"]);
        assert_refused(&check(&repo), 1, "release/v0.2.0-RC-2", "not the latest RC");
    }

    #[test]
    fn refuses_a_head_that_does_not_contain_main() {
        let repo = rc_repo("behind-main");
        repo.git(&["tag", "-a", "release/v0.2.0-RC-1", "-m", "rc 1"]);
        repo.git(&["checkout", "--quiet", "main"]);
        repo.write("hotfix.txt", "hotfix\n");
        repo.commit_all("hotfix on main");
        repo.git(&["checkout", "--quiet", "rc/0.2.0"]);
        assert_refused(&check(&repo), 1, "does not contain main", "main moved on");
    }

    #[test]
    fn rejects_bad_arguments() {
        let repo = rc_repo("usage");
        for args in [&["-x"][..], &["main", "HEAD", "extra"]] {
            let out = repo.run(SCRIPT, args);
            assert_refused(&out, 2, "usage:", &format!("{args:?}"));
        }
        let out = repo.run(SCRIPT, &["no-such-branch", "HEAD"]);
        assert_refused(&out, 1, "no-such-branch", "an unknown base");
    }
}
