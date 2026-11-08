//! Privacy policy tests: the repository is public, so nothing private goes
//! into it. Every tracked text file is checked for
//!
//! - email addresses other than the project's public contact, noreply
//!   addresses and examples (an address inside a link is part of the link);
//! - the home folder of whoever runs the tests (`$HOME`), and the scratch
//!   paths tools derive from it (`/Users/jo` becomes `-Users-jo-`); tests
//!   and docs use sample users such as `/Users/me`, so a run whose home is
//!   one of them ([`SAMPLE_HOMES`]) skips this check;
//! - links to documents readers can't open: private Claude, Atlassian,
//!   Figma, Google Docs, Notion and Slack pages;
//! - signing details: a certificate's SHA-1 fingerprint (40 upper-case hex
//!   digits), or a team ID written into a script or project file.
//!
//! The patterns are general, so this file names nobody's details. On the
//! owner's Mac the last test also reads `.agents/local/private-patterns.txt`
//! (gitignored, never committed): one exact string per line, such as a
//! name or a certificate's, matched without regard to case, blank lines and
//! `#` comments skipped. Elsewhere that test has nothing to check.
//!
//! The release script keeps the builder's home folder out of the binaries
//! (`tests/release_macos.rs`). Outside a git work tree (a source archive)
//! there is nothing to check and the tests pass with a note. Only `std` is
//! used.

mod common;

use common::{git_ls_files, in_git_work_tree, repo_root};
use std::fs;

/// This file spells out the patterns it looks for.
const SELF: &str = "crates/fx-testkit/tests/privacy_policy.rs";

/// The project's public contact and the noreply addresses commits use.
const ALLOWED_EMAILS: [&str; 3] = ["ne@3645.io", "noreply@anthropic.com", "noreply@github.com"];

/// Domains whose addresses are never anyone's private address.
const ALLOWED_EMAIL_DOMAINS: [&str; 2] = ["users.noreply.github.com", "example.com"];

/// File extensions, so `icon_128x128@2x.png` is not read as an address.
const FILE_EXTENSIONS: [&str; 12] = [
    "png", "jpg", "jpeg", "gif", "svg", "pdf", "icns", "rs", "md", "txt", "json", "toml",
];

/// Hosts of documents that only their owners and invitees can open.
const PRIVATE_LINKS: [&str; 7] = [
    "claude.ai/",
    "atlassian.net",
    "figma.com/",
    "docs.google.com",
    "drive.google.com",
    "notion.so/",
    "slack.com/",
];

/// The owner's local list of exact private strings.
const LOCAL_PATTERNS: &str = ".agents/local/private-patterns.txt";

/// The sample home folders tests and docs use. Run under one of them (a
/// test account or a container, say), the home-folder check can't tell the
/// runner's home from the samples, so it is skipped.
const SAMPLE_HOMES: [&str; 16] = [
    "/Users/me",
    "/Users/tester",
    "/Users/user",
    "/Users/alice",
    "/Users/bob",
    "/Users/jo",
    "/Users/joe",
    "/Users/zo",
    "/Users/zoe",
    "/Users/someone",
    "/Users/other",
    "/Users/you",
    "/Users/a",
    "/Users/b",
    "/Users/x",
    "/home/me",
];

/// Whether `home` is one of [`SAMPLE_HOMES`], in any case (macOS matches
/// folder names without regard to case).
fn is_sample_home(home: &str) -> bool {
    let home = home.trim_end_matches('/');
    SAMPLE_HOMES.iter().any(|s| s.eq_ignore_ascii_case(home))
}

/// Every tracked file that holds UTF-8 text, as (path, text), except this
/// one; `None` outside a git work tree.
fn tracked_texts() -> Option<Vec<(String, String)>> {
    if !in_git_work_tree("the privacy policy check") {
        return None;
    }
    let root = repo_root();
    Some(
        git_ls_files(&[])
            .into_iter()
            .filter(|p| p != SELF)
            .filter_map(|p| {
                let bytes = fs::read(root.join(&p)).ok()?;
                String::from_utf8(bytes).ok().map(|t| (p, t))
            })
            .collect(),
    )
}

/// `findings` as one line per finding, for an assertion message.
fn report(what: &str, findings: &[String]) -> String {
    format!("{what}:\n  {}", findings.join("\n  "))
}

/// The email addresses in `text` that are not allowed, skipping any inside
/// a link or a git remote's URL (`ssh://git@host/...`, `git@host:owner/...`).
fn private_emails(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for word in text.split(|c: char| c.is_whitespace() || "()<>[]`\"',;".contains(c)) {
        if word.starts_with("http") || word.contains("://") || !word.contains('@') {
            continue;
        }
        let Some((local, domain)) = word.split_once('@') else {
            continue;
        };
        let local = local.trim_start_matches(|c: char| !c.is_ascii_alphanumeric());
        let domain = domain.trim_end_matches(|c: char| !c.is_ascii_alphanumeric());
        // A host followed by a path is a remote, not an address.
        if domain.contains([':', '/']) {
            continue;
        }
        let tld = domain.rsplit('.').next().unwrap_or("");
        let looks_like_address = !local.is_empty()
            && domain.contains('.')
            && tld.len() >= 2
            && tld.chars().all(|c| c.is_ascii_alphabetic())
            && !FILE_EXTENSIONS.contains(&tld.to_ascii_lowercase().as_str());
        if !looks_like_address {
            continue;
        }
        let address = format!("{local}@{domain}").to_ascii_lowercase();
        let allowed = ALLOWED_EMAILS.contains(&address.as_str())
            || ALLOWED_EMAIL_DOMAINS
                .iter()
                .any(|d| domain.eq_ignore_ascii_case(d));
        if !allowed {
            found.push(address);
        }
    }
    found
}

/// Where `text` holds the home folder `home` (`/Users/jo`, also as the
/// scratch-path form `-Users-jo-` tools derive from it): the start of each
/// place, as a short excerpt with the folder blanked out.
fn home_paths(text: &str, home: &str) -> Vec<String> {
    let home = home.trim_end_matches('/');
    // A home folder must be at least two levels deep to be anyone's.
    if home.matches('/').count() < 2 {
        return Vec::new();
    }
    let dashed = format!("{}-", home.replace('/', "-"));
    let mut found = Vec::new();
    for needle in [format!("{home}/"), format!("{home}\""), dashed] {
        for _ in text.match_indices(needle.as_str()) {
            found.push("the home folder".to_owned());
        }
    }
    // The folder itself at the end of a word (`/Users/jo` alone).
    for (i, _) in text.match_indices(home) {
        let next = text[i + home.len()..].chars().next();
        if next.is_none_or(|c| !c.is_alphanumeric() && c != '/' && c != '-' && c != '"') {
            found.push("the home folder".to_owned());
        }
    }
    found
}

/// Links in `text` to documents readers can't open.
fn private_links(text: &str) -> Vec<String> {
    PRIVATE_LINKS
        .iter()
        .filter(|host| text.contains(*host))
        .map(|host| (*host).to_owned())
        .collect()
}

/// Certificate fingerprints and literal team IDs in `text`.
fn signing_details(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    // 40 upper-case hex digits: a SHA-1 fingerprint (commit IDs are lower
    // case).
    let mut run = String::new();
    for c in text.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_hexdigit() && !c.is_ascii_lowercase() {
            run.push(c);
            continue;
        }
        let edge = c.is_ascii_alphanumeric();
        if run.len() == 40 && !edge && run.chars().any(|d| d.is_ascii_alphabetic()) {
            found.push(format!("fingerprint {run}"));
        }
        run.clear();
    }
    for key in ["TEAM_ID=", "DEVELOPMENT_TEAM = "] {
        for (i, _) in text.match_indices(key) {
            let next = text[i + key.len()..].chars().next();
            if next.is_some_and(|c| c.is_ascii_alphanumeric()) {
                let shown: String = text[i..].chars().take(key.len() + 10).collect();
                found.push(shown);
            }
        }
    }
    found
}

/// Runs `check` on every tracked text file; returns `path: finding` lines,
/// or `None` outside a git work tree.
fn scan(check: impl Fn(&str) -> Vec<String>) -> Option<Vec<String>> {
    Some(
        tracked_texts()?
            .iter()
            .flat_map(|(path, text)| check(text).into_iter().map(move |f| format!("{path}: {f}")))
            .collect(),
    )
}

#[test]
fn tracked_files_hold_no_private_email_addresses() {
    let Some(found) = scan(private_emails) else {
        return;
    };
    assert!(
        found.is_empty(),
        "{}",
        report(
            "email addresses other than ne@3645.io, noreply and examples",
            &found
        )
    );
}

#[test]
fn tracked_files_hold_no_home_folder_paths() {
    let Ok(home) = std::env::var("HOME") else {
        return;
    };
    if is_sample_home(&home) {
        eprintln!("note: $HOME is the sample home {home}, so the home-folder check is skipped");
        return;
    }
    let Some(found) = scan(|text| home_paths(text, &home)) else {
        return;
    };
    assert!(
        found.is_empty(),
        "{}",
        report(
            "the home folder of whoever runs the tests (use a sample user such as /Users/me)",
            &found
        )
    );
}

#[test]
fn tracked_files_link_no_private_documents() {
    let Some(found) = scan(private_links) else {
        return;
    };
    assert!(
        found.is_empty(),
        "{}",
        report("links to documents readers can't open", &found)
    );
}

#[test]
fn tracked_files_hold_no_signing_details() {
    let Some(found) = scan(signing_details) else {
        return;
    };
    assert!(
        found.is_empty(),
        "{}",
        report(
            "signing details (the release script reads them from NE_SIGN_IDENTITY)",
            &found
        )
    );
}

#[test]
fn tracked_files_hold_none_of_the_owners_private_strings() {
    let Ok(list) = fs::read_to_string(repo_root().join(LOCAL_PATTERNS)) else {
        // Not the owner's Mac: there is nothing to check.
        return;
    };
    let patterns: Vec<String> = list
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_lowercase)
        .collect();
    let Some(found) = scan(|text| {
        let text = text.to_lowercase();
        patterns
            .iter()
            .enumerate()
            .filter(|(_, p)| text.contains(p.as_str()))
            // The line number only: the report must not repeat the string.
            .map(|(n, _)| format!("pattern {} of {LOCAL_PATTERNS}", n + 1))
            .collect()
    }) else {
        return;
    };
    assert!(
        found.is_empty(),
        "{}",
        report("the owner's private strings", &found)
    );
}

// ---------------------------------------------------------------------------
// The checks themselves.
// ---------------------------------------------------------------------------

#[test]
fn the_checks_find_what_they_look_for() {
    assert_eq!(
        private_emails("mail jo.doe@corp.co.uk now"),
        ["jo.doe@corp.co.uk"]
    );
    assert!(private_emails("ne@3645.io a@users.noreply.github.com b@example.com").is_empty());
    assert!(private_emails("icon_128x128@2x.png").is_empty());
    assert!(private_emails("https://lore.kernel.org/x/1-jo@corp.com/").is_empty());
    assert!(private_emails("git@github.com:owner/repo.git").is_empty());
    assert!(private_emails("ssh://git@github.com/owner/repo.git").is_empty());
    assert_eq!(private_emails("<jo@corp.com>:"), ["jo@corp.com"]);
    assert_eq!(home_paths("cd /Users/jo/src", "/Users/jo").len(), 1);
    assert_eq!(home_paths("cd /Users/jo", "/Users/jo/").len(), 1);
    assert_eq!(home_paths("x-Users-jo-Library", "/Users/jo").len(), 1);
    assert!(home_paths("/Users/joe/a /Users/me /Users/jo-x", "/Users/jo").is_empty());
    assert!(home_paths("/Users/jo/src", "/").is_empty());
    assert_eq!(
        private_links("see https://claude.ai/artifact/x"),
        ["claude.ai/"]
    );
    let fingerprint = "0123456789ABCDEF0123456789ABCDEF01234567";
    assert_eq!(signing_details(fingerprint).len(), 1);
    assert!(signing_details(&fingerprint.to_lowercase()).is_empty());
    assert_eq!(signing_details("TEAM_ID=AB12CD34EF").len(), 1);
    assert!(signing_details("TEAM_ID=$(team_of \"$IDENTITY\")").is_empty());
}

#[test]
fn sample_homes_are_recognised() {
    for home in [
        "/Users/me",
        "/Users/me/",
        "/Users/Me",
        "/Users/tester",
        "/home/me",
    ] {
        assert!(is_sample_home(home), "{home}");
    }
    for home in ["/Users/emma", "/Users/meg", "/Users", "/", "/home/runner"] {
        assert!(!is_sample_home(home), "{home}");
    }
}
