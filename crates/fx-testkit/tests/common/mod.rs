//! Helpers shared by the integration tests in `tests/`.
//!
//! Each test file includes this module with `mod common;`. Only `std` is
//! used: manifests are read with a minimal TOML reader that understands what
//! the workspace manifests need (tables, array tables, dotted keys, inline
//! tables, multi-line arrays, comments and quoted strings) and nothing more.
//! [`shell`] holds the helpers of the shell-script tests.

// Not every test file uses every helper.
#![allow(dead_code)]

pub mod shell;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------------------------------------------------------------------
// Repository access.
// ---------------------------------------------------------------------------

/// The repository root (two levels above this crate's manifest directory).
pub fn repo_root() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    root.canonicalize()
        .unwrap_or_else(|e| panic!("cannot resolve repo root {}: {e}", root.display()))
}

/// The directory of the workspace crate `name`, i.e. `crates/<name>`.
pub fn crate_dir(name: &str) -> PathBuf {
    repo_root().join("crates").join(name)
}

/// Reads a file to a string, panicking with the path on failure.
pub fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// The text of `crates/<name>/Cargo.toml`.
pub fn manifest(name: &str) -> String {
    read(&crate_dir(name).join("Cargo.toml"))
}

/// Whether the repository root is the top of a git work tree, so the tests
/// that check every tracked file have a list of them. A source archive has
/// no `.git`, and one unpacked inside another repository would list nothing,
/// so either way those tests have nothing to check: this prints a note
/// naming `what` was skipped, and they pass.
pub fn in_git_work_tree(what: &str) -> bool {
    let root = repo_root();
    let top = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&root)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
        .and_then(|top| top.canonicalize().ok());
    let inside = top.as_deref() == Some(root.as_path());
    if !inside {
        eprintln!(
            "note: {} is not a git work tree, so {what} has nothing to check",
            root.display()
        );
    }
    inside
}

/// `git ls-files -z` with `args`, run from the repository root: one entry
/// per tracked file. Call it only after [`in_git_work_tree`].
pub fn git_ls_files(args: &[&str]) -> Vec<String> {
    let out = Command::new("git")
        .args(["ls-files", "-z"])
        .args(args)
        .current_dir(repo_root())
        .output()
        .expect("run git ls-files");
    assert!(out.status.success(), "git ls-files {args:?} failed");
    String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect()
}

// ---------------------------------------------------------------------------
// Minimal TOML helpers.
// ---------------------------------------------------------------------------

/// Pairs each character of `s` with its byte index and whether it belongs to
/// a quoted string, delimiters included. Handles `"basic"` strings with
/// backslash escapes and `'literal'` strings.
fn classify(s: &str) -> Vec<(usize, char, bool)> {
    let mut out = Vec::with_capacity(s.len());
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        match quote {
            Some(q) => {
                out.push((i, c, true));
                if escaped {
                    escaped = false;
                } else if q == '"' && c == '\\' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                }
            }
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                out.push((i, c, true));
            }
            None => out.push((i, c, false)),
        }
    }
    out
}

/// Removes a trailing `#` comment, ignoring `#` inside quoted strings.
pub fn strip_comment(line: &str) -> &str {
    match classify(line)
        .into_iter()
        .find(|&(_, c, quoted)| c == '#' && !quoted)
    {
        Some((i, _, _)) => &line[..i],
        None => line,
    }
}

/// Removes all whitespace outside quoted strings.
pub fn squash(s: &str) -> String {
    classify(s)
        .into_iter()
        .filter(|&(_, c, quoted)| quoted || !c.is_whitespace())
        .map(|(_, c, _)| c)
        .collect()
}

/// Net count of opening minus closing `[`/`{` brackets outside strings.
fn bracket_depth_change(s: &str) -> i32 {
    classify(s)
        .into_iter()
        .filter(|&(_, _, quoted)| !quoted)
        .map(|(_, c, _)| match c {
            '[' | '{' => 1,
            ']' | '}' => -1,
            _ => 0,
        })
        .sum()
}

/// Returns the squashed table name if `line` is a table header: `package`
/// for `[package]` and `[bin]` for the array table `[[bin]]`. Key lines
/// never start with `[`; a header may contain `=` inside a quoted key such
/// as `'cfg(target_os = "macos")'`.
pub fn table_header(line: &str) -> Option<String> {
    strip_comment(line)
        .trim()
        .strip_prefix('[')
        .and_then(|l| l.strip_suffix(']'))
        .map(squash)
}

/// Splits a squashed dotted table name or key into its parts, honouring
/// quotes (`target.'cfg(unix)'.dependencies` has three parts).
pub fn table_keys(name: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in name.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => quote = Some(c),
            (None, '.') => keys.push(std::mem::take(&mut cur)),
            (None, c) => cur.push(c),
        }
    }
    keys.push(cur);
    keys
}

/// One logical line of a manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// A table header, as returned by [`table_header`].
    Header(String),
    /// A `key = value` entry, comments removed and whitespace outside
    /// strings squashed. Multi-line arrays and inline tables are joined
    /// onto one line.
    Entry(String),
}

/// Splits a manifest into headers and entries, in order.
pub fn items(manifest: &str) -> Vec<Item> {
    let mut out = Vec::new();
    let mut pending = String::new();
    let mut depth: i32 = 0;
    for raw in manifest.lines() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if depth == 0 {
            if let Some(name) = table_header(line) {
                out.push(Item::Header(name));
                continue;
            }
        }
        pending.push_str(line);
        depth += bracket_depth_change(line);
        if depth <= 0 {
            depth = 0;
            out.push(Item::Entry(squash(&pending)));
            pending.clear();
        }
    }
    if !pending.is_empty() {
        out.push(Item::Entry(squash(&pending)));
    }
    out
}

/// Returns the table names of all headers in order, e.g. `package`,
/// `lints`, `dependencies`, and `[bin]` for `[[bin]]`.
pub fn section_headers(manifest: &str) -> Vec<String> {
    items(manifest)
        .into_iter()
        .filter_map(|item| match item {
            Item::Header(name) => Some(name),
            Item::Entry(_) => None,
        })
        .collect()
}

/// Returns the squashed `key=value` entries of a `[section]` table.
pub fn section_lines(manifest: &str, section: &str) -> Vec<String> {
    let mut in_section = false;
    let mut lines = Vec::new();
    for item in items(manifest) {
        match item {
            Item::Header(name) => in_section = name == section,
            Item::Entry(line) if in_section => lines.push(line),
            Item::Entry(_) => {}
        }
    }
    lines
}

/// Looks up `key` in `[section]` and returns its squashed raw value.
/// Also understands dotted keys such as `license.workspace`.
pub fn get(manifest: &str, section: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    section_lines(manifest, section)
        .into_iter()
        .find_map(|l| l.strip_prefix(&prefix).map(str::to_owned))
}

/// Strips surrounding double or single quotes from a squashed value.
pub fn unquote(v: &str) -> Option<&str> {
    if v.len() < 2 {
        return None;
    }
    ['"', '\'']
        .into_iter()
        .find_map(|q| v.strip_prefix(q).and_then(|v| v.strip_suffix(q)))
}

/// The unquoted `[package] name`.
pub fn package_name(manifest: &str) -> Option<String> {
    get(manifest, "package", "name")
        .as_deref()
        .and_then(unquote)
        .map(str::to_owned)
}

/// True if `field` in `[package]` is inherited from the workspace, written
/// either as `field.workspace = true` or `field = { workspace = true }`.
pub fn inherits_from_workspace(manifest: &str, field: &str) -> bool {
    get(manifest, "package", &format!("{field}.workspace")).as_deref() == Some("true")
        || get(manifest, "package", field).as_deref() == Some("{workspace=true}")
}
