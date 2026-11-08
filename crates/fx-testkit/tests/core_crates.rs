//! Core-crate presence tests.
//!
//! The platform-agnostic crates come from the "Proposed architecture" crate
//! table in `docs/PLAN.md`, each created as an empty placeholder until it
//! gets real code. These tests check that each one exists under `crates/`
//! with a manifest whose package name matches its directory, that the
//! library crates have `src/lib.rs`, that `fx-cli` has the `not-everything`
//! binary in `src/main.rs` and a library in `src/lib.rs`, and that every
//! crate keeps workspace-inherited metadata, workspace lints, no `build.rs`,
//! and a `src/lib.rs` (or `src/main.rs` for `fx-cli`) that starts with a
//! crate-level doc comment describing its responsibility.
//!
//! Placeholder crates must also stay empty: `src/lib.rs` holds only a `//!`
//! doc comment and `src/` holds no other file, they have no `src/main.rs`,
//! and their manifests have no dependency section at all. Crates listed in
//! [`IMPLEMENTED_CRATES`] have real code, so the placeholder-only checks no
//! longer apply to them and they may add a `src/main.rs` beside
//! `src/lib.rs`; all other checks still do.
//!
//! An implemented crate may declare only the dependencies its entry in
//! [`CORE_DEPENDENCY_ALLOWLIST`] lists, each in the listed kind of table
//! (normal, dev or build), never under a `[target.'cfg(..)'.*]` table except
//! for dev-dependencies the allowlist marks [`DepKind::MacosDev`], which must
//! sit under `[target.'cfg(target_os = "macos")'.dev-dependencies]`, and
//! never one of [`FORBIDDEN_DEPENDENCY_PREFIXES`]. An implemented crate
//! without an allowlist entry (`fx-bench`, `fx-model`, `fx-channel`) still
//! has no dependency section. `fx-cli`'s allowlist entry names workspace
//! path crates only. An allowlisted dependency need not be declared yet.
//!
//! Only `std` is used; manifests are read with simple string handling.

mod common;

use common::{
    Item, crate_dir, get, inherits_from_workspace, items, manifest, package_name, read,
    section_headers, table_keys, unquote,
};
use std::fs;

/// Platform-agnostic library crates from the architecture table, plus
/// `fx-channel` (the build channel, release or Preview, and the names that
/// follow from it).
const LIB_CRATES: [&str; 10] = [
    "fx-model",
    "fx-index",
    "fx-query",
    "fx-sort",
    "fx-store",
    "fx-engine",
    "fx-ipc",
    "fx-fs",
    "fx-bench",
    "fx-channel",
];

/// Crates that have left the placeholder state.
///
/// A crate joins this list in the change that first gives it real code. The
/// placeholder-only checks (a `src/lib.rs` holding only a `//!` doc
/// comment, no other file under `src/`, no `src/main.rs`, and no dependency
/// section) apply only to library crates not in this list. Every other
/// check in this file still applies to the crates listed here. A crate that
/// adds dependencies also needs an entry in [`CORE_DEPENDENCY_ALLOWLIST`]
/// listing exactly those dependencies.
const IMPLEMENTED_CRATES: &[&str] = &[
    "fx-bench",
    "fx-query",
    "fx-store",
    "fx-model",
    "fx-index",
    "fx-sort",
    "fx-fs",
    "fx-engine",
    "fx-channel",
    CLI_CRATE,
];

/// Whether `c` has been given real code and is exempt from the
/// placeholder-only checks.
fn is_implemented(c: &str) -> bool {
    IMPLEMENTED_CRATES.contains(&c)
}

/// The command-line crate: the `not-everything` binary in `src/main.rs`
/// plus a library in `src/lib.rs`. It is not in [`LIB_CRATES`] because its
/// crate doc comment lives in `src/main.rs`.
const CLI_CRATE: &str = "fx-cli";

/// Words (lower case) of which each crate's doc comment must mention at
/// least one, taken from the crate's responsibility in the architecture table.
const DOC_KEYWORDS: [(&str, &[&str]); 11] = [
    ("fx-model", &["type"]),
    ("fx-index", &["column"]),
    ("fx-query", &["pars"]),
    ("fx-sort", &["sort"]),
    ("fx-store", &["checkpoint"]),
    ("fx-engine", &["writer"]),
    ("fx-ipc", &["protocol"]),
    ("fx-fs", &["platform"]),
    ("fx-cli", &["cli", "command-line", "command line"]),
    ("fx-bench", &["benchmark"]),
    ("fx-channel", &["channel"]),
];

fn all_crates() -> Vec<&'static str> {
    let mut v: Vec<&str> = LIB_CRATES.to_vec();
    v.push(CLI_CRATE);
    v
}

/// Text of the crate-level `//!` doc comment, lower-cased.
fn crate_doc(source: &str) -> String {
    source
        .lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix("//!"))
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

// ---------------------------------------------------------------------------
// Dependency policy.
// ---------------------------------------------------------------------------

/// Which kind of dependency table a dependency is declared in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DepKind {
    Normal,
    Dev,
    Build,
    /// Allowlist-only kind: a dev-dependency that must be declared under
    /// `[target.'cfg(target_os = "macos")'.dev-dependencies]` (see
    /// [`MACOS_CFG`]), e.g. a macOS platform crate used by end-to-end tests.
    /// The manifest reader records such a dependency as [`DepKind::Dev`] with
    /// its `target` set; [`Dep::allow_kind`] maps it to this kind.
    MacosDev,
}

/// The squashed (whitespace-free) `cfg(..)` of macOS-only target tables.
const MACOS_CFG: &str = "cfg(target_os=\"macos\")";

/// The dependencies each implemented core crate may declare, by package name
/// and table kind. Crates not listed here may declare none.
///
/// Dependencies whose name starts with [`WORKSPACE_CRATE_PREFIX`] are
/// workspace crates and must be declared as `path = "../<name>"`; all others
/// must come from crates.io (no `path` or `git`).
const CORE_DEPENDENCY_ALLOWLIST: &[(&str, &[(&str, DepKind)])] = &[
    (
        "fx-query",
        &[
            // Unlicense OR MIT, used under MIT; `memmem::Finder` for the scan.
            ("memchr", DepKind::Normal),
            // MIT OR Apache-2.0; parallel chunked scan.
            ("rayon", DepKind::Normal),
            // MIT OR Apache-2.0; NFC/NFD tables for case and diacritic folding.
            ("unicode-normalization", DepKind::Normal),
            // MIT OR Apache-2.0; regex terms.
            ("regex", DepKind::Normal),
            // Synthetic names and the benchmark harness for tests and examples.
            ("fx-bench", DepKind::Dev),
        ],
    ),
    (
        "fx-store",
        &[
            // MIT OR Apache-2.0; read-only mmap of the index image.
            ("memmap2", DepKind::Normal),
            // Zlib OR Apache-2.0 OR MIT, used under MIT; zero-copy casts of
            // mapped bytes to u32/u64/i64 columns.
            ("bytemuck", DepKind::Normal),
            // Workspace crate; shared types (entry IDs, metadata) of the saved index.
            ("fx-model", DepKind::Normal),
            // Workspace crate; the `Snapshot` columns of the saved index.
            ("fx-index", DepKind::Normal),
            // Synthetic trees for tests and the load benchmark.
            ("fx-bench", DepKind::Dev),
            // First-result substring search in tests and the load benchmark.
            ("fx-query", DepKind::Dev),
            // MIT OR Apache-2.0 (already locked through memmap2); `kill(pid,
            // 0)`, so a leftover temp file of a writer that no longer runs is
            // removed at the next start.
            ("libc", DepKind::Normal),
            // Workspace crate (std only); the child processes of the
            // `kill -9` crash tests.
            ("fx-testkit", DepKind::Dev),
        ],
    ),
    // fx-model and fx-channel are std-only and
    // deliberately have no entry here, so, like fx-bench, they may not
    // declare any dependency section at all.
    (
        "fx-index",
        &[
            // Shared types (entry IDs, flags, metadata) from the model crate.
            ("fx-model", DepKind::Normal),
            // Synthetic trees for tests.
            ("fx-bench", DepKind::Dev),
        ],
    ),
    (
        "fx-sort",
        &[
            // Shared types (entry IDs, kinds, metadata) from the model crate.
            ("fx-model", DepKind::Normal),
            // The `SortSource` implementation for the index snapshot.
            ("fx-index", DepKind::Normal),
            // MIT OR Apache-2.0; parallel order building.
            ("rayon", DepKind::Normal),
            // Synthetic data for the sort benchmark example.
            ("fx-bench", DepKind::Dev),
        ],
    ),
    (
        "fx-fs",
        &[
            // Workspace crate; the `Entry`, ID and metadata types the platform
            // traits are built on.
            ("fx-model", DepKind::Normal),
            // MIT OR Apache-2.0; regex exclusion patterns.
            ("regex", DepKind::Normal),
        ],
    ),
    (
        "fx-engine",
        &[
            // Workspace crate; shared types, including `ResumeCursor`.
            ("fx-model", DepKind::Normal),
            // Workspace crate; the column store the writer thread owns.
            ("fx-index", DepKind::Normal),
            // Workspace crate; query parsing and matching for `SearchView`.
            ("fx-query", DepKind::Normal),
            // Workspace crate; the pre-sorted orders and batched merges.
            ("fx-sort", DepKind::Normal),
            // Workspace crate; checkpoints that save the cursor with the index.
            ("fx-store", DepKind::Normal),
            // Workspace crate; the `ChangeSource`, path-stat and reconcile logic.
            ("fx-fs", DepKind::Normal),
            // MIT OR Apache-2.0; snapshot publishing.
            ("arc-swap", DepKind::Normal),
            // MIT OR Apache-2.0; `localtime_r` and `struct tm` for the local
            // time zone's offset, instead of declaring them by hand.
            ("libc", DepKind::Normal),
            // Synthetic trees for tests and benchmarks.
            ("fx-bench", DepKind::Dev),
            // Workspace crate (std only); the child processes of the
            // `kill -9` crash tests.
            ("fx-testkit", DepKind::Dev),
            // macOS-only target dev-dependency; real FSEvents for the
            // end-to-end tests and the freshness example.
            ("fx-fs-macos", DepKind::MacosDev),
        ],
    ),
    (
        "fx-cli",
        &[
            // Workspace crates only; no third-party crate. The
            // read-only `StoreReader`, query toggles and sort keys.
            ("fx-engine", DepKind::Normal),
            ("fx-query", DepKind::Normal),
            ("fx-sort", DepKind::Normal),
            // The build channel: the command's name and data folder.
            ("fx-channel", DepKind::Normal),
            // Test-only: pins the CLI's default store path to the app's
            // `app_args::store_path` layout.
            ("fx-ui-macos", DepKind::Dev),
            // Test-only: indexes and stores built for the end-to-end tests.
            ("fx-engine", DepKind::Dev),
            ("fx-fs", DepKind::Dev),
            ("fx-store", DepKind::Dev),
            ("fx-index", DepKind::Dev),
            ("fx-model", DepKind::Dev),
            ("fx-sort", DepKind::Dev),
            ("fx-bench", DepKind::Dev),
        ],
    ),
];

/// Crate-name prefixes no core crate may depend on: UI toolkits and OS
/// bindings belong in the platform crates, and nothing may be named after
/// (or derived from) Everything.
const FORBIDDEN_DEPENDENCY_PREFIXES: [&str; 4] = ["objc2", "windows", "gtk", "everything"];

/// Name prefix of workspace crates, which must be path dependencies.
const WORKSPACE_CRATE_PREFIX: &str = "fx-";

fn dep_kind(key: &str) -> Option<DepKind> {
    match key {
        "dependencies" => Some(DepKind::Normal),
        "dev-dependencies" | "dev_dependencies" => Some(DepKind::Dev),
        "build-dependencies" | "build_dependencies" => Some(DepKind::Build),
        _ => None,
    }
}

/// One declared dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Dep {
    kind: DepKind,
    /// The `cfg(..)` of a `[target.<cfg>.*]` table, whitespace removed.
    target: Option<String>,
    /// The dependency key as written.
    key: String,
    /// Squashed `field=value` pairs. A bare version string `"1"` is recorded
    /// as `("version", "\"1\"")`.
    fields: Vec<(String, String)>,
}

impl Dep {
    /// The squashed value of `field`, if declared.
    fn field(&self, field: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == field)
            .map(|(_, v)| v.as_str())
    }

    /// The package name: `package = ".."` if renamed, otherwise the key.
    fn package(&self) -> String {
        self.field("package")
            .and_then(unquote)
            .map_or_else(|| self.key.clone(), str::to_owned)
    }

    /// The allowlist kind this declaration must match: its table kind for
    /// plain tables, [`DepKind::MacosDev`] for a dev-dependency under the
    /// macOS-only target table, and `None` for any other target table.
    fn allow_kind(&self) -> Option<DepKind> {
        match (&self.target, self.kind) {
            (None, kind) => Some(kind),
            (Some(cfg), DepKind::Dev) if cfg == MACOS_CFG => Some(DepKind::MacosDev),
            (Some(_), _) => None,
        }
    }

    /// A human-readable location, e.g. `[dependencies] memchr`.
    fn describe(&self) -> String {
        let table = match self.kind {
            DepKind::Normal => "dependencies",
            DepKind::Dev | DepKind::MacosDev => "dev-dependencies",
            DepKind::Build => "build-dependencies",
        };
        match &self.target {
            Some(cfg) => format!("[target.'{cfg}'.{table}] {}", self.key),
            None => format!("[{table}] {}", self.key),
        }
    }
}

/// Splits a squashed string at every `sep` outside quoted strings and
/// outside `[..]`/`{..}` nesting.
fn split_top_level(s: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if q == '"' && c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '[' | '{' => depth += 1,
            ']' | '}' => depth -= 1,
            c if c == sep && depth == 0 => {
                parts.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

/// Splits a squashed `key=value` entry at its first top-level `=`.
fn split_entry(line: &str) -> Option<(&str, &str)> {
    let parts = split_top_level(line, '=');
    if parts.len() < 2 {
        return None;
    }
    let key = parts[0];
    Some((key, &line[key.len() + 1..]))
}

/// If `keys` (a full dotted path) lies inside a dependency table, returns
/// the kind, the target cfg and the rest of the path after the table.
fn dep_scope(keys: &[String]) -> Option<(DepKind, Option<String>, &[String])> {
    if let Some(kind) = keys.first().and_then(|k| dep_kind(k)) {
        return Some((kind, None, &keys[1..]));
    }
    if keys.first().map(String::as_str) == Some("target") && keys.len() >= 3 {
        if let Some(kind) = dep_kind(&keys[2]) {
            let cfg: String = keys[1].chars().filter(|c| !c.is_whitespace()).collect();
            return Some((kind, Some(cfg), &keys[3..]));
        }
    }
    None
}

/// Every dependency the manifest declares, in any dependency table (plain,
/// dev, build or target-specific), including `[dependencies.foo]`
/// sub-tables, dotted keys such as `foo.version = "1"` inside a dependency
/// table, and dotted keys such as `dependencies.foo = "1"` at the top level.
/// Entries for the same dependency are merged.
fn dependencies(manifest: &str) -> Vec<Dep> {
    let mut deps: Vec<Dep> = Vec::new();
    let mut add =
        |kind: DepKind, target: Option<String>, key: &str, fields: Vec<(String, String)>| {
            if let Some(d) = deps
                .iter_mut()
                .find(|d| d.kind == kind && d.target == target && d.key == key)
            {
                d.fields.extend(fields);
            } else {
                deps.push(Dep {
                    kind,
                    target,
                    key: key.to_owned(),
                    fields,
                });
            }
        };
    let mut header: Vec<String> = Vec::new();
    for item in items(manifest) {
        match item {
            Item::Header(name) => {
                header = table_keys(&name);
                if let Some((kind, target, rest)) = dep_scope(&header) {
                    if let Some(key) = rest.first() {
                        // `[dependencies.foo]` declares `foo` itself.
                        add(kind, target, key, Vec::new());
                    }
                }
            }
            Item::Entry(line) => {
                let Some((key, value)) = split_entry(&line) else {
                    continue;
                };
                let mut full = header.clone();
                full.extend(table_keys(key));
                let Some((kind, target, rest)) = dep_scope(&full) else {
                    continue;
                };
                let Some(name) = rest.first() else {
                    continue;
                };
                let fields = if rest.len() > 1 {
                    vec![(rest[1..].join("."), value.to_owned())]
                } else if let Some(inner) =
                    value.strip_prefix('{').and_then(|v| v.strip_suffix('}'))
                {
                    split_top_level(inner, ',')
                        .into_iter()
                        .filter(|f| !f.is_empty())
                        .filter_map(split_entry)
                        .map(|(k, v)| (table_keys(k).join("."), v.to_owned()))
                        .collect()
                } else {
                    vec![("version".to_owned(), value.to_owned())]
                };
                add(kind, target, name, fields);
            }
        }
    }
    deps
}

/// Every dependency table header in the manifest, e.g. `dependencies`,
/// `dev-dependencies.foo` or `target.'cfg(unix)'.dependencies`, even when
/// the table is empty.
fn dependency_sections(manifest: &str) -> Vec<String> {
    section_headers(manifest)
        .into_iter()
        .filter(|h| dep_scope(&table_keys(h)).is_some())
        .collect()
}

/// The allowlist entry of core crate `name`, if any.
fn allowlist_for(name: &str) -> Option<&'static [(&'static str, DepKind)]> {
    CORE_DEPENDENCY_ALLOWLIST
        .iter()
        .find(|(c, _)| *c == name)
        .map(|(_, list)| *list)
}

/// Checks the dependencies of core crate `name` against
/// [`IMPLEMENTED_CRATES`], [`CORE_DEPENDENCY_ALLOWLIST`] and
/// [`FORBIDDEN_DEPENDENCY_PREFIXES`], returning one message per violation.
fn dependency_violations(name: &str, manifest: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let allowed = match allowlist_for(name) {
        Some(list) if is_implemented(name) => list,
        _ => {
            // Placeholders, and implemented crates without an allowlist
            // entry, have no dependency section at all.
            for h in dependency_sections(manifest) {
                problems.push(format!(
                    "crates/{name}/Cargo.toml: unexpected [{h}] section"
                ));
            }
            for dep in dependencies(manifest) {
                problems.push(format!(
                    "crates/{name}/Cargo.toml: {}: {name} may declare no dependencies",
                    dep.describe()
                ));
            }
            return problems;
        }
    };
    for dep in dependencies(manifest) {
        let package = dep.package();
        let what = format!("crates/{name}/Cargo.toml: {}", dep.describe());
        if FORBIDDEN_DEPENDENCY_PREFIXES
            .iter()
            .any(|p| package.starts_with(p) || dep.key.starts_with(p))
        {
            problems.push(format!(
                "{what}: objc2, windows, gtk and everything crates are not allowed in core crates"
            ));
            continue;
        }
        if package != dep.key {
            problems.push(format!("{what}: renamed dependencies are not allowed"));
            continue;
        }
        let Some(allow_kind) = dep.allow_kind() else {
            problems.push(format!(
                "{what}: core crates are platform-agnostic; no target-specific dependencies \
                 (only allowlisted dev-dependencies under [target.'{MACOS_CFG}'.dev-dependencies])"
            ));
            continue;
        };
        if !allowed
            .iter()
            .any(|&(n, kind)| n == package && kind == allow_kind)
        {
            problems.push(format!(
                "{what}: not on the dependency allowlist for {name} ({allow_kind:?} dependency)"
            ));
            continue;
        }
        if dep.field("git").is_some() {
            problems.push(format!("{what}: must not be a git dependency"));
        }
        if package.starts_with(WORKSPACE_CRATE_PREFIX) {
            let want = format!("\"../{package}\"");
            if dep.field("path") != Some(want.as_str()) {
                problems.push(format!(
                    "{what}: must be the workspace path crate `path = {want}`"
                ));
            }
        } else if dep.field("path").is_some() {
            problems.push(format!("{what}: must come from crates.io, not a path"));
        }
    }
    problems
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[test]
fn every_core_crate_directory_exists_with_a_manifest() {
    let missing: Vec<String> = all_crates()
        .into_iter()
        .filter(|c| !crate_dir(c).join("Cargo.toml").is_file())
        .map(|c| format!("crates/{c}/Cargo.toml"))
        .collect();
    assert!(
        missing.is_empty(),
        "missing core crate manifests:\n  {}",
        missing.join("\n  ")
    );
}

#[test]
fn package_name_matches_directory_name() {
    for c in all_crates() {
        let m = manifest(c);
        assert_eq!(
            package_name(&m).as_deref(),
            Some(c),
            "crates/{c}/Cargo.toml must declare `name = \"{c}\"` under [package]"
        );
    }
}

#[test]
fn implemented_crates_are_known_library_crates() {
    // Every implemented crate has a library target: the LIB_CRATES and the
    // library-plus-binary CLI crate.
    for c in IMPLEMENTED_CRATES {
        assert!(
            LIB_CRATES.contains(c) || *c == CLI_CRATE,
            "IMPLEMENTED_CRATES lists {c}, which is not in LIB_CRATES or {CLI_CRATE}"
        );
    }
}

#[test]
fn library_crates_have_lib_rs() {
    for c in LIB_CRATES {
        let dir = crate_dir(c);
        assert!(
            dir.join("src").join("lib.rs").is_file(),
            "library crate {c} must have crates/{c}/src/lib.rs"
        );
        // Implemented crates may become library-plus-binary crates (fx-bench
        // has the `fx-bench` binary); placeholders may not.
        if !is_implemented(c) {
            assert!(
                !dir.join("src").join("main.rs").exists(),
                "placeholder library crate {c} must not have a src/main.rs"
            );
        }
    }
}

#[test]
fn fx_cli_is_a_binary_crate_with_main_rs() {
    let dir = crate_dir(CLI_CRATE);
    // The library half the binary is built on.
    assert!(
        dir.join("src").join("lib.rs").is_file(),
        "fx-cli must have crates/fx-cli/src/lib.rs"
    );
    let main = dir.join("src").join("main.rs");
    assert!(main.is_file(), "fx-cli must have crates/fx-cli/src/main.rs");
    let src = read(&main);
    assert!(
        src.contains("fn main()"),
        "crates/fx-cli/src/main.rs must define `fn main()`"
    );
    // The command is named after the product: the
    // crate keeps the `fx-` prefix and a [[bin]] target names the binary
    // exactly `not-everything`.
    let m = manifest(CLI_CRATE);
    let headers = section_headers(&m);
    assert!(
        headers.iter().any(|h| h == "[bin]"),
        "fx-cli must declare a [[bin]] target named \"not-everything\""
    );
    assert_eq!(
        get(&m, "[bin]", "name").as_deref(),
        Some("\"not-everything\""),
        "fx-cli's [[bin]] target must be named \"not-everything\""
    );
}

#[test]
fn manifests_inherit_workspace_metadata_and_lints() {
    let mut problems = Vec::new();
    for c in all_crates() {
        let m = manifest(c);
        for field in ["version", "edition", "license", "publish", "rust-version"] {
            if !inherits_from_workspace(&m, field) {
                problems.push(format!("{c}: `{field}.workspace = true` missing"));
            }
        }
        if get(&m, "lints", "workspace").as_deref() != Some("true") {
            problems.push(format!("{c}: `[lints] workspace = true` missing"));
        }
    }
    assert!(
        problems.is_empty(),
        "manifest problems:\n  {}",
        problems.join("\n  ")
    );
}

#[test]
fn core_dependency_allowlist_covers_only_implemented_crates_and_no_forbidden_crate() {
    for (c, list) in CORE_DEPENDENCY_ALLOWLIST {
        assert!(
            is_implemented(c),
            "CORE_DEPENDENCY_ALLOWLIST lists {c}, which is not in IMPLEMENTED_CRATES"
        );
        for (dep, _) in *list {
            assert!(
                !FORBIDDEN_DEPENDENCY_PREFIXES
                    .iter()
                    .any(|p| dep.starts_with(p)),
                "CORE_DEPENDENCY_ALLOWLIST allows forbidden crate {dep} for {c}"
            );
        }
    }
}

#[test]
fn core_crates_declare_only_allowlisted_dependencies() {
    let mut problems = Vec::new();
    for c in all_crates() {
        problems.extend(dependency_violations(c, &manifest(c)));
        if crate_dir(c).join("build.rs").exists() {
            problems.push(format!("{c}: unexpected build.rs"));
        }
    }
    assert!(
        problems.is_empty(),
        "core crate dependency policy violated:\n  {}",
        problems.join("\n  ")
    );
}

#[test]
fn library_sources_hold_only_a_crate_doc_comment() {
    for c in LIB_CRATES.into_iter().filter(|c| !is_implemented(c)) {
        let path = crate_dir(c).join("src").join("lib.rs");
        let src = read(&path);
        let code: Vec<&str> = src
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with("//!"))
            .collect();
        assert!(
            code.is_empty(),
            "crates/{c}/src/lib.rs must hold only a `//!` doc comment; found:\n  {}",
            code.join("\n  ")
        );
        let src_dir = crate_dir(c).join("src");
        let extra: Vec<String> = fs::read_dir(&src_dir)
            .unwrap_or_else(|e| panic!("cannot list {}: {e}", src_dir.display()))
            .map(|e| {
                e.expect("readable entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|n| n != "lib.rs")
            .collect();
        assert!(
            extra.is_empty(),
            "crates/{c}/src must contain only lib.rs; found {extra:?}"
        );
    }
}

#[test]
fn every_crate_has_a_doc_comment_describing_its_responsibility() {
    for (c, keywords) in DOC_KEYWORDS {
        let file = if c == CLI_CRATE { "main.rs" } else { "lib.rs" };
        let src = read(&crate_dir(c).join("src").join(file));
        assert!(
            src.trim_start().starts_with("//!"),
            "crates/{c}/src/{file} must start with a `//!` crate-level doc comment"
        );
        let doc = crate_doc(&src);
        assert!(
            keywords.iter().any(|k| doc.contains(k)),
            "the doc comment of {c} should summarise its responsibility from the \
             architecture table (expected it to mention one of {keywords:?}); got {doc:?}"
        );
    }
}

#[test]
fn checker_self_test() {
    let m = "[package]\nname = \"fx-x\" # c\nversion.workspace = true\n\
             edition = { workspace = true }\n\n[lints]\nworkspace = true\n\n\
             [target.'cfg(unix)'.dependencies]\nfoo = \"1\"\n\n[[bin]]\nname = \"fx-cli\"\n";
    assert_eq!(package_name(m).as_deref(), Some("fx-x"));
    assert!(inherits_from_workspace(m, "version"));
    assert!(inherits_from_workspace(m, "edition"));
    assert!(!inherits_from_workspace(m, "license"));
    assert_eq!(get(m, "lints", "workspace").as_deref(), Some("true"));
    let headers = section_headers(m);
    assert!(
        headers.iter().any(|h| h.ends_with("dependencies")),
        "{headers:?}"
    );
    assert_eq!(get(m, "[bin]", "name").as_deref(), Some("\"fx-cli\""));
}

// ---------------------------------------------------------------------------
// Self-tests: the dependency checker must catch what it should.
// ---------------------------------------------------------------------------

const EMPTY: &str = r#"
[package]
name = "fx-x"   # comment with [dependencies] in it
version.workspace = true

[lints]
workspace = true
"#;

fn violations(name: &str, extra: &str) -> Vec<String> {
    dependency_violations(name, &format!("{EMPTY}\n{extra}"))
}

#[test]
fn dependency_reader_records_kind_target_and_fields() {
    assert!(dependencies(EMPTY).is_empty());
    assert!(dependency_sections(EMPTY).is_empty());
    let m = format!(
        "{EMPTY}\n[dependencies]\nmemchr = {{ version = \"2\", default-features = false }}\n\
         rayon = \"1\"\n\
         [dev-dependencies.fx-bench]\npath = \"../fx-bench\"\n\
         [build-dependencies]\ncc.version = \"1\"\n\
         [target.'cfg(target_os = \"macos\")'.dependencies]\nlibc = \"0.2\"\n"
    );
    let deps = dependencies(&m);
    assert_eq!(deps.len(), 5, "{deps:?}");
    assert_eq!(deps[0].kind, DepKind::Normal);
    assert_eq!(deps[0].key, "memchr");
    assert_eq!(deps[0].field("version"), Some("\"2\""));
    assert_eq!(deps[0].field("default-features"), Some("false"));
    assert_eq!(deps[1].field("version"), Some("\"1\""));
    assert_eq!(deps[2].kind, DepKind::Dev);
    assert_eq!(deps[2].field("path"), Some("\"../fx-bench\""));
    assert_eq!(deps[3].kind, DepKind::Build);
    assert_eq!(deps[3].field("version"), Some("\"1\""));
    assert_eq!(deps[4].target.as_deref(), Some("cfg(target_os=\"macos\")"));
    let dotted = "dependencies.memchr = \"2\"\n[package]\nname = \"fx-x\"\n";
    assert_eq!(dependencies(dotted).len(), 1);
    let empty_table = format!("{EMPTY}\n[dev-dependencies]\n# none yet\n");
    assert!(dependencies(&empty_table).is_empty());
    assert_eq!(dependency_sections(&empty_table), vec!["dev-dependencies"]);
    let other = format!("{EMPTY}\n[features]\ndefault = []\n[lib]\npath = \"src/lib.rs\"\n");
    assert!(dependencies(&other).is_empty());
    assert!(dependency_sections(&other).is_empty());
}

#[test]
fn allowlist_accepts_fx_query_dependencies() {
    for extra in [
        "",
        "[dependencies]\nmemchr = \"2\"\n",
        "[dependencies]\nmemchr = \"2.7\"\nrayon = \"1\"\n",
        "[dependencies]\nrayon = { version = \"1.10\", default-features = false }\n",
        "[dependencies.memchr]\nversion = \"2\"\n",
        "[dev-dependencies]\nfx-bench = { path = \"../fx-bench\" }\n",
        "[dev-dependencies.fx-bench]\npath = \"../fx-bench\"\n",
        "[dependencies]\nmemchr = \"2\"\nrayon = \"1\"\n\n\
         [dev-dependencies]\nfx-bench = { path = \"../fx-bench\" }\n",
    ] {
        assert_eq!(
            violations("fx-query", extra),
            Vec::<String>::new(),
            "{extra:?}"
        );
    }
}

#[test]
fn allowlist_accepts_fx_index_dependencies_and_rejects_third_party_ones() {
    for extra in [
        "",
        "[dependencies]\nfx-model = { path = \"../fx-model\" }\n",
        "[dev-dependencies]\nfx-bench = { path = \"../fx-bench\" }\n",
        "[dependencies]\nfx-model = { path = \"../fx-model\" }\n\n\
         [dev-dependencies]\nfx-bench = { path = \"../fx-bench\" }\n",
    ] {
        assert_eq!(
            violations("fx-index", extra),
            Vec::<String>::new(),
            "{extra:?}"
        );
    }
    for extra in [
        "[dependencies]\nmemchr = \"2\"\n",
        "[dependencies]\narc-swap = \"1\"\n",
        "[dev-dependencies]\nfx-model = { path = \"../fx-model\" }\n",
        "[dependencies]\nfx-bench = { path = \"../fx-bench\" }\n",
        "[dependencies]\nfx-model = \"0.1\"\n",
    ] {
        assert!(
            !violations("fx-index", extra).is_empty(),
            "fx-index should reject {extra:?}"
        );
    }
}

#[test]
fn allowlist_accepts_fx_sort_dependencies_and_rejects_others() {
    for extra in [
        "",
        "[dependencies]\nfx-model = { path = \"../fx-model\" }\n",
        "[dependencies]\nfx-index = { path = \"../fx-index\" }\n",
        "[dependencies]\nrayon = \"1\"\n",
        "[dev-dependencies]\nfx-bench = { path = \"../fx-bench\" }\n",
        "[dependencies]\nfx-model = { path = \"../fx-model\" }\n\
         fx-index = { path = \"../fx-index\" }\nrayon = \"1.10\"\n\n\
         [dev-dependencies]\nfx-bench = { path = \"../fx-bench\" }\n",
    ] {
        assert_eq!(
            violations("fx-sort", extra),
            Vec::<String>::new(),
            "{extra:?}"
        );
    }
    for extra in [
        "[dependencies]\nmemchr = \"2\"\n",
        "[dependencies]\nserde = \"1\"\n",
        "[dev-dependencies]\nrayon = \"1\"\n",
        "[dependencies]\nfx-bench = { path = \"../fx-bench\" }\n",
        "[dependencies]\nfx-index = \"0.1\"\n",
        "[dependencies]\nfx-model = { path = \"../elsewhere\" }\n",
        "[dependencies]\nfx-query = { path = \"../fx-query\" }\n",
        "[target.'cfg(unix)'.dependencies]\nrayon = \"1\"\n",
    ] {
        assert!(
            !violations("fx-sort", extra).is_empty(),
            "fx-sort should reject {extra:?}"
        );
    }
}

#[test]
fn allowlist_accepts_fx_fs_dependencies_and_rejects_others() {
    for extra in [
        "",
        "[dependencies]\nfx-model = { path = \"../fx-model\" }\n",
        "[dependencies]\nregex = \"1\"\n",
        "[dependencies]\nfx-model = { path = \"../fx-model\" }\n\
         regex = { version = \"1.11\", default-features = false, features = [\"std\"] }\n",
    ] {
        assert_eq!(
            violations("fx-fs", extra),
            Vec::<String>::new(),
            "{extra:?}"
        );
    }
    for extra in [
        "[dependencies]\nlibc = \"0.2\"\n",
        "[dependencies]\nobjc2 = \"0.6\"\n",
        "[dev-dependencies]\ntempfile = \"3\"\n",
        "[dev-dependencies]\nregex = \"1\"\n",
        "[dependencies]\nregex = { git = \"https://example.com/regex\" }\n",
        "[dependencies]\nregex = { path = \"../regex\" }\n",
        "[dependencies]\nfx-model = \"0.1\"\n",
        "[dependencies]\nfx-model = { path = \"../elsewhere\" }\n",
        "[dependencies]\nfx-index = { path = \"../fx-index\" }\n",
        "[target.'cfg(target_os = \"macos\")'.dependencies]\nregex = \"1\"\n",
    ] {
        assert!(
            !violations("fx-fs", extra).is_empty(),
            "fx-fs should reject {extra:?}"
        );
    }
}

#[test]
fn dependency_reader_records_target_dev_dependencies() {
    let m = format!(
        "{EMPTY}\n[target.'cfg(target_os = \"macos\")'.dev-dependencies]\n\
         fx-fs-macos = {{ path = \"../fx-fs-macos\" }}\n\
         [target.\"cfg(unix)\".dev-dependencies.tempfile]\nversion = \"3\"\n"
    );
    let deps = dependencies(&m);
    assert_eq!(deps.len(), 2, "{deps:?}");
    assert_eq!(deps[0].kind, DepKind::Dev);
    assert_eq!(deps[0].key, "fx-fs-macos");
    assert_eq!(deps[0].target.as_deref(), Some(MACOS_CFG));
    assert_eq!(deps[0].field("path"), Some("\"../fx-fs-macos\""));
    assert_eq!(deps[0].allow_kind(), Some(DepKind::MacosDev));
    assert_eq!(deps[1].kind, DepKind::Dev);
    assert_eq!(deps[1].key, "tempfile");
    assert_eq!(deps[1].target.as_deref(), Some("cfg(unix)"));
    assert_eq!(deps[1].allow_kind(), None);
    assert_eq!(
        dependency_sections(&m).len(),
        2,
        "{:?}",
        dependency_sections(&m)
    );
}

#[test]
fn allowlist_accepts_fx_engine_dependencies_and_rejects_others() {
    for extra in [
        "",
        "[dependencies]\nfx-model = { path = \"../fx-model\" }\n\
         fx-index = { path = \"../fx-index\" }\nfx-query = { path = \"../fx-query\" }\n\
         fx-sort = { path = \"../fx-sort\" }\nfx-store = { path = \"../fx-store\" }\n\
         fx-fs = { path = \"../fx-fs\" }\narc-swap = \"1\"\n",
        "[dev-dependencies]\nfx-bench = { path = \"../fx-bench\" }\n",
        "[target.'cfg(target_os = \"macos\")'.dev-dependencies]\n\
         fx-fs-macos = { path = \"../fx-fs-macos\" }\n",
        "[target.'cfg(target_os=\"macos\")'.dev-dependencies.fx-fs-macos]\n\
         path = \"../fx-fs-macos\"\n",
    ] {
        assert_eq!(
            violations("fx-engine", extra),
            Vec::<String>::new(),
            "{extra:?}"
        );
    }
    for extra in [
        // Not on the allowlist, in any table.
        "[dependencies]\nserde = \"1\"\n",
        "[dependencies]\nrayon = \"1\"\n",
        "[dependencies]\nfx-ipc = { path = \"../fx-ipc\" }\n",
        "[dev-dependencies]\ntempfile = \"3\"\n",
        "[target.'cfg(target_os = \"macos\")'.dev-dependencies]\ntempfile = \"3\"\n",
        "[target.'cfg(target_os = \"macos\")'.dev-dependencies]\nobjc2 = \"0.6\"\n",
        // The macOS crate only as a macOS-only target dev-dependency.
        "[dependencies]\nfx-fs-macos = { path = \"../fx-fs-macos\" }\n",
        "[dev-dependencies]\nfx-fs-macos = { path = \"../fx-fs-macos\" }\n",
        "[target.'cfg(target_os = \"macos\")'.dependencies]\n\
         fx-fs-macos = { path = \"../fx-fs-macos\" }\n",
        "[target.'cfg(unix)'.dev-dependencies]\nfx-fs-macos = { path = \"../fx-fs-macos\" }\n",
        "[target.'cfg(target_os = \"macos\")'.dev-dependencies]\n\
         fx-fs-macos = { path = \"../elsewhere\" }\n",
        "[target.'cfg(target_os = \"macos\")'.dev-dependencies]\nfx-fs-macos = \"0.1\"\n",
        // Allowlisted normal dependencies stay out of target tables.
        "[target.'cfg(target_os = \"macos\")'.dev-dependencies]\n\
         fx-bench = { path = \"../fx-bench\" }\n",
        "[target.'cfg(target_os = \"macos\")'.dependencies]\narc-swap = \"1\"\n",
        // Wrong kind or source.
        "[dev-dependencies]\narc-swap = \"1\"\n",
        "[dependencies]\nfx-bench = { path = \"../fx-bench\" }\n",
        "[dependencies]\narc-swap = { path = \"../arc-swap\" }\n",
        "[dependencies]\nfx-fs = \"0.1\"\n",
    ] {
        assert!(
            !violations("fx-engine", extra).is_empty(),
            "fx-engine should reject {extra:?}"
        );
    }
    // Crates without a MacosDev entry may not use the macOS target table.
    let macos_bench = "[target.'cfg(target_os = \"macos\")'.dev-dependencies]\n\
                       fx-bench = { path = \"../fx-bench\" }\n";
    assert!(!violations("fx-query", macos_bench).is_empty());
}

#[test]
fn allowlist_accepts_fx_cli_workspace_dependencies_and_rejects_others() {
    for extra in [
        "",
        "[dependencies]\nfx-engine = { path = \"../fx-engine\" }\n\
         fx-query = { path = \"../fx-query\" }\nfx-sort = { path = \"../fx-sort\" }\n",
        "[dependencies]\nfx-channel = { path = \"../fx-channel\" }\n",
        "[dev-dependencies]\nfx-ui-macos = { path = \"../fx-ui-macos\" }\n",
        "[dev-dependencies]\nfx-ui-macos = { path = \"../fx-ui-macos\" }\n\
         fx-engine = { path = \"../fx-engine\" }\nfx-fs = { path = \"../fx-fs\" }\n\
         fx-store = { path = \"../fx-store\" }\nfx-index = { path = \"../fx-index\" }\n\
         fx-model = { path = \"../fx-model\" }\nfx-sort = { path = \"../fx-sort\" }\n\
         fx-bench = { path = \"../fx-bench\" }\n",
    ] {
        assert_eq!(
            violations("fx-cli", extra),
            Vec::<String>::new(),
            "{extra:?}"
        );
    }
    for extra in [
        // Third-party crates stay banned, in any table.
        "[dependencies]\nclap = \"4\"\n",
        "[dependencies]\nserde_json = \"1\"\n",
        "[dev-dependencies]\ntempfile = \"3\"\n",
        "[build-dependencies]\ncc = \"1\"\n",
        "[dependencies]\nobjc2 = \"0.6\"\n",
        // Test-only crates are not normal dependencies.
        "[dependencies]\nfx-ui-macos = { path = \"../fx-ui-macos\" }\n",
        "[dependencies]\nfx-fs = { path = \"../fx-fs\" }\n",
        "[dependencies]\nfx-bench = { path = \"../fx-bench\" }\n",
        "[dependencies]\nfx-model = { path = \"../fx-model\" }\n",
        "[dependencies]\nfx-index = { path = \"../fx-index\" }\n",
        "[dependencies]\nfx-store = { path = \"../fx-store\" }\n",
        // Workspace crates not on the list.
        "[dependencies]\nfx-ipc = { path = \"../fx-ipc\" }\n",
        "[dev-dependencies]\nfx-channel = { path = \"../fx-channel\" }\n",
        "[dependencies]\nfx-channel = \"0.2\"\n",
        "[dev-dependencies]\nfx-fs-macos = { path = \"../fx-fs-macos\" }\n",
        // Wrong source, rename or table.
        "[dependencies]\nfx-engine = \"0.1\"\n",
        "[dependencies]\nfx-engine = { path = \"../elsewhere\" }\n",
        "[dependencies]\nfx-engine = { git = \"https://example.com/fx\" }\n",
        "[dependencies]\nengine = { package = \"fx-engine\", path = \"../fx-engine\" }\n",
        "[target.'cfg(unix)'.dependencies]\nfx-engine = { path = \"../fx-engine\" }\n",
        "[target.'cfg(target_os = \"macos\")'.dev-dependencies]\n\
         fx-ui-macos = { path = \"../fx-ui-macos\" }\n",
    ] {
        assert!(
            !violations("fx-cli", extra).is_empty(),
            "fx-cli should reject {extra:?}"
        );
    }
}

#[test]
fn allowlist_rejects_unknown_forbidden_and_misplaced_dependencies() {
    for extra in [
        // Not on the allowlist.
        "[dependencies]\nserde = \"1\"\n",
        "[dev-dependencies]\ntempfile = \"3\"\n",
        // Forbidden prefixes.
        "[dependencies]\nobjc2 = \"0.6\"\n",
        "[dependencies]\nwindows-sys = \"0.59\"\n",
        "[dependencies]\ngtk4 = \"0.9\"\n",
        "[dependencies]\neverything-sdk = \"0.1\"\n",
        // Renames.
        "[dependencies]\nmemchr = { package = \"regex\", version = \"1\" }\n",
        "[dependencies]\nm = { package = \"memchr\", version = \"2\" }\n",
        // Wrong kind.
        "[build-dependencies]\nmemchr = \"2\"\n",
        "[dev-dependencies]\nrayon = \"1\"\n",
        "[dependencies]\nfx-bench = { path = \"../fx-bench\" }\n",
        // Target-specific tables.
        "[target.'cfg(unix)'.dependencies]\nmemchr = \"2\"\n",
        // Wrong source.
        "[dependencies]\nmemchr = { git = \"https://example.com/memchr\" }\n",
        "[dependencies]\nrayon = { path = \"../rayon\" }\n",
        "[dev-dependencies]\nfx-bench = \"0.1\"\n",
        "[dev-dependencies]\nfx-bench = { path = \"../elsewhere\" }\n",
    ] {
        assert!(
            !violations("fx-query", extra).is_empty(),
            "should reject {extra:?}"
        );
    }
}

#[test]
fn crates_without_an_allowlist_entry_have_no_dependency_section() {
    // fx-bench, fx-model and fx-channel are implemented but have no
    // allowlist entry. None may declare anything, not even an empty table.
    for name in ["fx-bench", "fx-model", "fx-channel"] {
        assert_eq!(violations(name, ""), Vec::<String>::new(), "{name}");
        for extra in [
            "[dependencies]\nmemchr = \"2\"\n",
            "[dev-dependencies]\nfx-bench = { path = \"../fx-bench\" }\n",
            "[dependencies]\n",
            "[build-dependencies]\n# none\n",
            "[target.'cfg(unix)'.dependencies]\n",
        ] {
            assert!(
                !violations(name, extra).is_empty(),
                "{name} should reject {extra:?}"
            );
        }
        let dotted = "dependencies.memchr = \"2\"\n[package]\nname = \"fx-x\"\n";
        assert!(!dependency_violations(name, dotted).is_empty(), "{name}");
    }
}
