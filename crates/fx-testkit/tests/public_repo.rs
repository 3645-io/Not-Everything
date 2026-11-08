//! Public repository tests: what the published tree must hold, and never
//! hold. Every tracked text file is checked.
//!
//! - Decision records stay on the owner's machine. `docs/decisions/` is
//!   not tracked and `.gitignore` lists it, and no tracked file cites a
//!   record, since readers can't open one: no "decision", "decisions" or
//!   "decision record(s)" followed by a record number (any case, also
//!   across a wrapped comment line), and no path `docs/decisions` except in
//!   `.gitignore` and the agent rules under `.agents/`, which describe the
//!   folder as local. Say the reason in plain words instead.
//! - Every tracked shell script is executable.
//!
//! Outside a git work tree (a source archive) there is nothing to check and
//! the tests pass with a note. Only `std` is used.

mod common;

use common::{git_ls_files, in_git_work_tree, read, repo_root};
use std::fs;

/// This file spells out the decision citations it looks for, so the
/// citation check skips it.
const SELF: &str = "crates/fx-testkit/tests/public_repo.rs";

/// The folder of the decision records, which stays local.
const DECISIONS: &str = "docs/decisions";

/// Every tracked file that holds UTF-8 text, as (path, text); `None`
/// outside a git work tree.
fn tracked_texts(what: &str) -> Option<Vec<(String, String)>> {
    if !in_git_work_tree(what) {
        return None;
    }
    let root = repo_root();
    Some(
        git_ls_files(&[])
            .into_iter()
            .filter_map(|p| {
                let bytes = fs::read(root.join(&p)).ok()?;
                String::from_utf8(bytes).ok().map(|t| (p, t))
            })
            .collect(),
    )
}

/// The 1-based line of byte offset `at` in `text`.
fn line_of(text: &str, at: usize) -> usize {
    text[..at].matches('\n').count() + 1
}

/// Whether `path` may name the decision records' folder: `.gitignore`,
/// which must list it, and the agent rules, which say it is local.
fn may_name_the_folder(path: &str) -> bool {
    path == ".gitignore" || path.starts_with(".agents/")
}

/// Where `text` cites a decision record, as (line, what was found). The
/// folder's path counts unless `folder_allowed`.
fn decision_citations(text: &str, folder_allowed: bool) -> Vec<(usize, String)> {
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut found = Vec::new();
    if !folder_allowed {
        for (at, _) in lower.match_indices(DECISIONS) {
            found.push((line_of(text, at), DECISIONS.to_owned()));
        }
    }
    for (at, _) in lower.match_indices("decision") {
        let starts_word =
            at == 0 || !(bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_');
        if !starts_word {
            continue;
        }
        let mut i = at + "decision".len();
        if bytes.get(i) == Some(&b's') {
            i += 1;
        }
        let after_word = skip_separators(bytes, i);
        if after_word > i && lower[after_word..].starts_with("record") {
            i = after_word + "record".len();
            if bytes.get(i) == Some(&b's') {
                i += 1;
            }
        }
        let number = skip_separators(bytes, i);
        if number == i {
            continue;
        }
        let digits = bytes[number..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        // A date (2026-11-08) is not a record number.
        let date = bytes.get(number + digits) == Some(&b'-')
            && bytes
                .get(number + digits + 1)
                .is_some_and(u8::is_ascii_digit);
        if digits == 4 && !date {
            let end = number + digits;
            found.push((
                line_of(text, at),
                lower[at..end]
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
            ));
        }
    }
    found
}

/// The index after the run of separators starting at `i`: white space,
/// comment markers (`#`, `//`, `//!`, `*`) and the punctuation a citation
/// may carry (`-`, `:`, `(`, `[`, a backtick).
fn skip_separators(bytes: &[u8], mut i: usize) -> usize {
    while bytes
        .get(i)
        .is_some_and(|b| b.is_ascii_whitespace() || b"#/!*-:([`".contains(b))
    {
        i += 1;
    }
    i
}

/// `findings` as one line per finding, for an assertion message.
fn report(what: &str, findings: &[String]) -> String {
    format!("{what}:\n  {}", findings.join("\n  "))
}

#[test]
fn the_decision_records_stay_local() {
    if !in_git_work_tree("the decision records' check") {
        return;
    }
    let tracked: Vec<String> = git_ls_files(&[DECISIONS]);
    assert!(
        tracked.is_empty(),
        "{DECISIONS}/ must not be tracked (git rm -r --cached {DECISIONS}):\n  {}",
        tracked.join("\n  ")
    );
    let ignore = read(&repo_root().join(".gitignore"));
    let listed = ignore.lines().map(str::trim).any(|l| {
        let l = l.trim_start_matches('/').trim_end_matches('/');
        l == DECISIONS
    });
    assert!(listed, ".gitignore must list {DECISIONS}/");
}

#[test]
fn no_tracked_file_cites_a_decision_record() {
    let Some(texts) = tracked_texts("the decision citation check") else {
        return;
    };
    let found: Vec<String> = texts
        .iter()
        .filter(|(path, _)| path != SELF)
        .flat_map(|(path, text)| {
            decision_citations(text, may_name_the_folder(path))
                .into_iter()
                .map(move |(line, what)| format!("{path}:{line}: {what}"))
        })
        .collect();
    assert!(
        found.is_empty(),
        "{}",
        report(
            "citations of the local decision records (say the reason in plain words)",
            &found
        )
    );
}

/// The scripts are run as `scripts/<name>.sh` in the docs and the rules, so
/// each one must keep its executable bit in git.
#[test]
fn tracked_shell_scripts_are_executable() {
    if !in_git_work_tree("the script mode check") {
        return;
    }
    let not_executable: Vec<String> = git_ls_files(&["-s"])
        .into_iter()
        .filter_map(|entry| {
            let (meta, path) = entry.split_once('\t')?;
            let mode = meta.split(' ').next()?;
            (path.ends_with(".sh") && mode != "100755").then(|| format!("{path} ({mode})"))
        })
        .collect();
    assert!(
        not_executable.is_empty(),
        "{}",
        report(
            "shell scripts without the executable bit (git update-index --chmod=+x)",
            &not_executable
        )
    );
}

// ---------------------------------------------------------------------------
// The checks themselves.
// ---------------------------------------------------------------------------

#[test]
fn the_citation_check_finds_record_numbers_in_any_form() {
    let cite = |text: &str| decision_citations(text, false);
    // A record's path names the folder and the record.
    assert_eq!(
        cite("see docs/decisions/0006-branching.md"),
        [
            (1, "docs/decisions".to_owned()),
            (1, "decisions/0006".to_owned())
        ]
    );
    assert_eq!(cite("(decision 0006)"), [(1, "decision 0006".to_owned())]);
    assert_eq!(cite("Decision record 0008 says").len(), 1);
    assert_eq!(cite("decision records 0004 and 0006").len(), 1);
    assert_eq!(cite("as decisions 0003 and 0004 say").len(), 1);
    assert_eq!(cite("DECISION 0001").len(), 1);
    assert_eq!(cite("decision-0002"), [(1, "decision-0002".to_owned())]);
    assert_eq!(cite("decision `0005`").len(), 1);
    // Wrapped across comment lines, reported on the line it starts.
    assert_eq!(cite("x\n# allowlist (decision\n  # 0008)")[0].0, 2);
    assert_eq!(cite("//! the record (decision\n//! 0006).").len(), 1);
    assert_eq!(cite("/// decision record\n/// 0008").len(), 1);
}

#[test]
fn the_citation_check_leaves_other_uses_alone() {
    let cite = |text: &str| decision_citations(text, false);
    for text in [
        "the owner's decision (8 Nov 2026)",
        "the Decision column",
        "a decision 2026-11-08",
        "indecision 0006",
        "decision 12345",
        "decision 006",
        "decision0006",
        "the decisions were made",
    ] {
        assert!(cite(text).is_empty(), "{text:?}: {:?}", cite(text));
    }
}

#[test]
fn the_folder_may_be_named_only_in_gitignore_and_the_agent_rules() {
    assert!(may_name_the_folder(".gitignore"));
    assert!(may_name_the_folder(".agents/CLAUDE.md"));
    assert!(may_name_the_folder(".agents/rules/privacy.md"));
    assert!(!may_name_the_folder("README.md"));
    assert!(!may_name_the_folder("docs/.gitignore"));
    assert!(!may_name_the_folder("crates/x/.agents/notes.md"));
    // There, the folder passes, but a record never does, by path or number.
    assert!(decision_citations("docs/decisions/\n", true).is_empty());
    assert_eq!(
        decision_citations("docs/decisions/0008-public.md", true),
        [(1, "decisions/0008".to_owned())]
    );
    assert_eq!(decision_citations("see decision 0008", true).len(), 1);
    assert_eq!(decision_citations("/docs/decisions/\n", false).len(), 1);
}
