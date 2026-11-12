//! Platform crate presence tests.
//!
//! The "Proposed architecture" crate table in `docs/PLAN.md` lists six
//! platform-specific crates. Each starts as an empty placeholder, so the
//! whole workspace builds and tests on any host, including macOS CI. These
//! tests check that:
//!
//! - each of the six crate directories exists under `crates/` with a
//!   `Cargo.toml` whose package name matches the directory name,
//! - each manifest inherits version, edition, rust-version, license and
//!   publish from the workspace and sets `[lints] workspace = true`,
//! - each crate is a library (`src/lib.rs`) whose crate-level `//!` doc
//!   comment names the target OS, with no `src/main.rs` except in the native
//!   UI shell crates listed in [`UI_SHELL_CRATES_WITH_BINARY`], which are the
//!   product's executables (`fx-ui-macos` holds the macOS app's).
//!
//! Placeholder crates must also stay empty: no dependency in any table
//! (`[dependencies]`, dev, build or `[target.'cfg(..)'.dependencies]`), a
//! `src/lib.rs` holding only a `//!` doc comment, no other file under `src/`
//! and no `examples/`, `tests/` or `benches/` directory or `build.rs`.
//!
//! Crates listed in [`IMPLEMENTED_PLATFORM_CRATES`] have real code: the
//! macOS scanner (`fx-fs-macos`) and the AppKit app (`fx-ui-macos`). They
//! are exempt from the placeholder-only checks and may have `examples/`,
//! `tests/` and `src/` modules, but may declare only the dependencies in
//! [`PLATFORM_DEPENDENCY_ALLOWLIST`], and never one of
//! [`FORBIDDEN_DEPENDENCY_PREFIXES`] (`objc2`, `windows`, `gtk`) unless
//! [`FORBIDDEN_PREFIX_EXEMPTIONS`] exempts that prefix for that crate. The
//! only exemption is `objc2` for `fx-ui-macos`, the AppKit shell, and its
//! allowlist permits the `objc2*` crates only under
//! `cfg(target_os = "macos")`.
//!
//! Only `std` is used; manifests are parsed with simple string handling.

mod common;

use common::{Item, crate_dir, get, inherits_from_workspace, items, manifest, read, table_keys};

/// The platform crates from the architecture table, with the words that
/// name each crate's target OS (any one of them must appear in the crate
/// doc comment).
const PLATFORM_CRATES: [(&str, &[&str]); 6] = [
    ("fx-fs-macos", &["macOS"]),
    ("fx-fs-windows", &["Windows"]),
    ("fx-fs-linux", &["Linux"]),
    ("fx-ui-macos", &["macOS"]),
    ("fx-ui-win32", &["Windows"]),
    ("fx-ui-gtk", &["Linux", "Ubuntu"]),
];

/// Platform crates that have left the placeholder state.
///
/// Add a platform crate here in the change that first gives it real code,
/// and give it an entry in [`PLATFORM_DEPENDENCY_ALLOWLIST`] listing exactly
/// the dependencies that change adds. The placeholder-only checks (a
/// `src/lib.rs` holding only a `//!` doc comment, no other source files, no
/// `examples/`/`tests/`/`benches/`, and no dependencies at all) apply only to
/// platform crates not in this list. Every other check in this file still
/// applies to the crates listed here.
const IMPLEMENTED_PLATFORM_CRATES: &[&str] = &["fx-fs-macos", "fx-ui-macos"];

/// Whether `c` has been given real code and is exempt from the
/// placeholder-only checks.
fn is_implemented(c: &str) -> bool {
    IMPLEMENTED_PLATFORM_CRATES.contains(&c)
}

/// Native UI shell crates that may ship a `src/main.rs` binary target next to
/// their library.
///
/// Per the crate table in `docs/PLAN.md`, these crates are the product's
/// executables (`fx-ui-macos` holds the macOS app's), so they may have a
/// `src/main.rs`. They must still have a `src/lib.rs` whose crate doc
/// comment names the OS. Backend crates such as `fx-fs-macos` stay
/// library-only. Placeholder crates in this list are still held to the
/// placeholder-only checks (`src/` holding only `lib.rs`) until they join
/// [`IMPLEMENTED_PLATFORM_CRATES`].
const UI_SHELL_CRATES_WITH_BINARY: &[&str] = &["fx-ui-macos", "fx-ui-win32", "fx-ui-gtk"];

/// Whether `c` is a native UI shell crate allowed a `src/main.rs`.
fn may_ship_binary(c: &str) -> bool {
    UI_SHELL_CRATES_WITH_BINARY.contains(&c)
}

/// The `cfg` of `[target.'cfg(target_os = "macos")'.dependencies]`, with
/// whitespace removed.
const MACOS_CFG: &str = "cfg(target_os=\"macos\")";

/// Crate-name prefixes no platform crate may depend on yet: the UI toolkit
/// and OS binding crates come with the work that builds the native UIs and
/// the Windows/Linux sources, together with an update of this list.
const FORBIDDEN_DEPENDENCY_PREFIXES: [&str; 3] = ["objc2", "windows", "gtk"];

/// `(crate, prefix)` pairs exempting one platform crate from one entry of
/// [`FORBIDDEN_DEPENDENCY_PREFIXES`]. The exempt crate may still declare only
/// the dependencies its [`PLATFORM_DEPENDENCY_ALLOWLIST`] entry lists.
///
/// `fx-ui-macos` is the AppKit shell and needs the `objc2` crates.
/// Every other crate, including `fx-fs-macos`, stays objc2-free; `windows`
/// and `gtk` stay forbidden everywhere.
const FORBIDDEN_PREFIX_EXEMPTIONS: &[(&str, &str)] = &[("fx-ui-macos", "objc2")];

/// The forbidden prefix `dep_name` matches for platform crate `krate`, if
/// any, after applying [`FORBIDDEN_PREFIX_EXEMPTIONS`].
fn forbidden_prefix(krate: &str, dep_name: &str) -> Option<&'static str> {
    FORBIDDEN_DEPENDENCY_PREFIXES
        .iter()
        .copied()
        .filter(|p| !FORBIDDEN_PREFIX_EXEMPTIONS.contains(&(krate, *p)))
        .find(|p| dep_name.starts_with(p))
}

/// One dependency an implemented platform crate is allowed to declare.
struct AllowedDep {
    /// Package name (renames via `package = ".."` are not allowed).
    name: &'static str,
    /// The kind of dependency table it may appear in.
    kind: DepKind,
    /// The `cfg(..)` targets it may appear under; `None` is the plain table.
    targets: &'static [Option<&'static str>],
    /// `Some(path)` for a workspace path crate, which must declare exactly
    /// this `path`; `None` for a crates.io crate, which must not declare
    /// `path` or `git`.
    path: Option<&'static str>,
}

/// The dependencies each implemented platform crate may declare. Crates not
/// listed here may declare none.
const PLATFORM_DEPENDENCY_ALLOWLIST: &[(&str, &[AllowedDep])] = &[
    (
        "fx-fs-macos",
        &[
            // MIT OR Apache-2.0; the walker's FFI (getattrlistbulk, setiopolicy_np).
            AllowedDep {
                name: "libc",
                kind: DepKind::Normal,
                targets: &[None, Some(MACOS_CFG)],
                path: None,
            },
            // Workspace crate (MIT); the platform traits (`VolumeEnumerator`,
            // `ChangeSource`, `PermissionProbe`) and the exclusion engine the
            // production scanner implements and applies.
            AllowedDep {
                name: "fx-fs",
                kind: DepKind::Normal,
                targets: &[None, Some(MACOS_CFG)],
                path: Some("../fx-fs"),
            },
            // Workspace crate (MIT); the `Entry` type the scanner emits.
            AllowedDep {
                name: "fx-model",
                kind: DepKind::Normal,
                targets: &[None, Some(MACOS_CFG)],
                path: Some("../fx-model"),
            },
            // Synthetic trees for the walker tests and the scan benchmark.
            AllowedDep {
                name: "fx-bench",
                kind: DepKind::Dev,
                targets: &[None, Some(MACOS_CFG)],
                path: Some("../fx-bench"),
            },
            // Workspace crate (std only); the disk images of the
            // external-volume tests (run 2.4).
            AllowedDep {
                name: "fx-testkit",
                kind: DepKind::Dev,
                targets: &[None, Some(MACOS_CFG)],
                path: Some("../fx-testkit"),
            },
        ],
    ),
    (
        "fx-ui-macos",
        &[
            // MIT; Objective-C runtime bindings (message sending, class
            // declaration for the table data source and delegates).
            AllowedDep {
                name: "objc2",
                kind: DepKind::Normal,
                targets: &[Some(MACOS_CFG)],
                path: None,
            },
            // MIT; Foundation bindings (NSString, NSNotification, run loop)
            // used by the AppKit layer.
            AllowedDep {
                name: "objc2-foundation",
                kind: DepKind::Normal,
                targets: &[Some(MACOS_CFG)],
                path: None,
            },
            // Zlib OR Apache-2.0 OR MIT, used under MIT; AppKit bindings
            // (NSWindow, NSSearchField, view-based NSTableView, status bar).
            AllowedDep {
                name: "objc2-app-kit",
                kind: DepKind::Normal,
                targets: &[Some(MACOS_CFG)],
                path: None,
            },
            // MIT; SMAppService bindings for the Launch at Login item. Like
            // the other objc2 crates, only under the macOS cfg.
            AllowedDep {
                name: "objc2-service-management",
                kind: DepKind::Normal,
                targets: &[Some(MACOS_CFG)],
                path: None,
            },
            // MIT OR Apache-2.0, used under MIT; the global summon hotkey via
            // Carbon RegisterEventHotKey, which needs no Accessibility prompt.
            AllowedDep {
                name: "global-hotkey",
                kind: DepKind::Normal,
                targets: &[Some(MACOS_CFG)],
                path: None,
            },
            // Workspace crate (MIT); synthetic file names for the 2M virtual
            // rows of the table spike.
            AllowedDep {
                name: "fx-bench",
                kind: DepKind::Normal,
                targets: &[None, Some(MACOS_CFG)],
                path: Some("../fx-bench"),
            },
            // Workspace crate (MIT); synthetic names in the model tests.
            AllowedDep {
                name: "fx-bench",
                kind: DepKind::Dev,
                targets: &[None, Some(MACOS_CFG)],
                path: Some("../fx-bench"),
            },
            // Workspace crate (MIT); the engine the results controller queries
            // and implements `ResultsSource` for.
            AllowedDep {
                name: "fx-engine",
                kind: DepKind::Normal,
                targets: &[None, Some(MACOS_CFG)],
                path: Some("../fx-engine"),
            },
            // Workspace crate (MIT); parsed queries for the controller.
            AllowedDep {
                name: "fx-query",
                kind: DepKind::Normal,
                targets: &[None, Some(MACOS_CFG)],
                path: Some("../fx-query"),
            },
            // Workspace crate (MIT); the private-folder helper and modes the
            // data folder shares with the store.
            AllowedDep {
                name: "fx-store",
                kind: DepKind::Normal,
                targets: &[None, Some(MACOS_CFG)],
                path: Some("../fx-store"),
            },
            // Workspace crate (MIT); sort keys for the result columns.
            AllowedDep {
                name: "fx-sort",
                kind: DepKind::Normal,
                targets: &[None, Some(MACOS_CFG)],
                path: Some("../fx-sort"),
            },
            // Workspace crate (MIT); `EntryId` and `EntryKind` for reading rows
            // from the engine snapshot.
            AllowedDep {
                name: "fx-model",
                kind: DepKind::Normal,
                targets: &[None, Some(MACOS_CFG)],
                path: Some("../fx-model"),
            },
            // Workspace crate (MIT); platform traits and exclusions wired up by
            // the app binary.
            AllowedDep {
                name: "fx-fs",
                kind: DepKind::Normal,
                targets: &[None, Some(MACOS_CFG)],
                path: Some("../fx-fs"),
            },
            // Workspace crate (MIT); the production macOS scanner the app
            // binary runs.
            AllowedDep {
                name: "fx-fs-macos",
                kind: DepKind::Normal,
                targets: &[None, Some(MACOS_CFG)],
                path: Some("../fx-fs-macos"),
            },
            // Workspace crate (MIT, std only); the build channel, which sets
            // the app's name, bundle ID, data folder and default hotkey.
            AllowedDep {
                name: "fx-channel",
                kind: DepKind::Normal,
                targets: &[None, Some(MACOS_CFG)],
                path: Some("../fx-channel"),
            },
        ],
    ),
];

// ---------------------------------------------------------------------------
// Dependency detection.
// ---------------------------------------------------------------------------

/// Which kind of dependency table a dependency is declared in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DepKind {
    Normal,
    Dev,
    Build,
}

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
            .and_then(common::unquote)
            .map_or_else(|| self.key.clone(), str::to_owned)
    }

    /// A human-readable location, e.g. `[dependencies] libc`.
    fn describe(&self) -> String {
        let table = match self.kind {
            DepKind::Normal => "dependencies",
            DepKind::Dev => "dev-dependencies",
            DepKind::Build => "build-dependencies",
        };
        match &self.target {
            Some(cfg) => format!("[target.'{cfg}'.{table}] {}", self.key),
            None => format!("[{table}] {}", self.key),
        }
    }
}

/// Splits a squashed string at every `sep` that is outside quoted strings
/// and outside `[..]`/`{..}` nesting.
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
    let key = parts.first()?;
    if parts.len() < 2 {
        return None;
    }
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

/// Returns a description of every dependency the manifest declares (see
/// [`dependencies`]).
fn declared_dependencies(manifest: &str) -> Vec<String> {
    dependencies(manifest).iter().map(Dep::describe).collect()
}

/// Checks the dependencies of platform crate `name` against
/// [`PLATFORM_DEPENDENCY_ALLOWLIST`] and [`FORBIDDEN_DEPENDENCY_PREFIXES`],
/// returning one message per violation.
fn dependency_violations(name: &str, manifest: &str) -> Vec<String> {
    let allowed: &[AllowedDep] = PLATFORM_DEPENDENCY_ALLOWLIST
        .iter()
        .find(|(c, _)| *c == name)
        .map_or(&[], |(_, list)| list);
    let mut problems = Vec::new();
    for dep in dependencies(manifest) {
        let package = dep.package();
        let what = format!("crates/{name}/Cargo.toml: {}", dep.describe());
        if let Some(prefix) =
            forbidden_prefix(name, &package).or_else(|| forbidden_prefix(name, &dep.key))
        {
            problems.push(format!(
                "{what}: `{prefix}*` crates are not allowed in {name}"
            ));
            continue;
        }
        if package != dep.key {
            problems.push(format!("{what}: renamed dependencies are not allowed"));
            continue;
        }
        let Some(rule) = allowed
            .iter()
            .find(|a| a.name == package && a.kind == dep.kind)
        else {
            problems.push(format!(
                "{what}: not on the dependency allowlist for {name} ({:?} dependency)",
                dep.kind
            ));
            continue;
        };
        if !rule.targets.contains(&dep.target.as_deref()) {
            problems.push(format!("{what}: may only appear under {:?}", rule.targets));
        }
        match rule.path {
            Some(path) => {
                let want = format!("\"{path}\"");
                if dep.field("path") != Some(want.as_str()) {
                    problems.push(format!("{what}: must be the path crate `path = {want}`"));
                }
                if dep.field("git").is_some() {
                    problems.push(format!("{what}: must not be a git dependency"));
                }
            }
            None => {
                if dep.field("path").is_some() || dep.field("git").is_some() {
                    problems.push(format!(
                        "{what}: must come from crates.io, not a path or git source"
                    ));
                }
            }
        }
    }
    problems
}

// ---------------------------------------------------------------------------
// Tests against the real repository.
// ---------------------------------------------------------------------------

#[test]
fn every_platform_crate_directory_has_a_manifest() {
    let missing: Vec<String> = PLATFORM_CRATES
        .iter()
        .filter(|(name, _)| !crate_dir(name).join("Cargo.toml").is_file())
        .map(|(name, _)| format!("crates/{name}/Cargo.toml"))
        .collect();
    assert!(
        missing.is_empty(),
        "platform crates from the architecture table are missing:\n  {}",
        missing.join("\n  ")
    );
}

#[test]
fn implemented_platform_crates_are_known_platform_crates() {
    for c in IMPLEMENTED_PLATFORM_CRATES {
        assert!(
            PLATFORM_CRATES.iter().any(|(name, _)| name == c),
            "IMPLEMENTED_PLATFORM_CRATES lists {c}, which is not in PLATFORM_CRATES"
        );
    }
}

#[test]
fn dependency_allowlist_covers_only_implemented_crates_and_no_forbidden_crate() {
    for (c, list) in PLATFORM_DEPENDENCY_ALLOWLIST {
        assert!(
            is_implemented(c),
            "PLATFORM_DEPENDENCY_ALLOWLIST lists {c}, which is not in IMPLEMENTED_PLATFORM_CRATES"
        );
        for dep in *list {
            assert!(
                forbidden_prefix(c, dep.name).is_none(),
                "PLATFORM_DEPENDENCY_ALLOWLIST allows forbidden crate {} for {c}",
                dep.name
            );
            // Exempted toolkit crates may only appear under the macOS cfg.
            if FORBIDDEN_DEPENDENCY_PREFIXES
                .iter()
                .any(|p| dep.name.starts_with(p))
            {
                assert_eq!(
                    dep.targets,
                    &[Some(MACOS_CFG)],
                    "{} for {c} must be allowed only under {MACOS_CFG}",
                    dep.name
                );
                assert_eq!(dep.kind, DepKind::Normal, "{} for {c}", dep.name);
                assert_eq!(dep.path, None, "{} for {c}", dep.name);
            }
        }
    }
}

#[test]
fn forbidden_prefix_exemptions_are_exactly_objc2_for_fx_ui_macos() {
    assert_eq!(FORBIDDEN_PREFIX_EXEMPTIONS, &[("fx-ui-macos", "objc2")]);
    for (c, prefix) in FORBIDDEN_PREFIX_EXEMPTIONS {
        assert!(is_implemented(c), "exemption for unimplemented crate {c}");
        assert!(
            FORBIDDEN_DEPENDENCY_PREFIXES.contains(prefix),
            "exemption for unknown prefix {prefix}"
        );
    }
    // `windows` and `gtk` stay forbidden everywhere; objc2 everywhere else.
    for (c, _) in PLATFORM_CRATES {
        assert_eq!(forbidden_prefix(c, "windows-sys"), Some("windows"), "{c}");
        assert_eq!(forbidden_prefix(c, "gtk4"), Some("gtk"), "{c}");
        let objc2 = forbidden_prefix(c, "objc2-app-kit");
        if c == "fx-ui-macos" {
            assert_eq!(objc2, None);
        } else {
            assert_eq!(objc2, Some("objc2"), "{c}");
        }
    }
}

#[test]
fn platform_crate_package_names_match_directories() {
    for (name, _) in PLATFORM_CRATES {
        let m = manifest(name);
        assert_eq!(
            get(&m, "package", "name").as_deref(),
            Some(format!("\"{name}\"").as_str()),
            "crates/{name}/Cargo.toml must declare [package] name = \"{name}\""
        );
    }
}

#[test]
fn placeholder_platform_crates_declare_no_dependencies() {
    let mut all = Vec::new();
    for (name, _) in PLATFORM_CRATES {
        if is_implemented(name) {
            continue;
        }
        for dep in declared_dependencies(&manifest(name)) {
            all.push(format!("crates/{name}/Cargo.toml: {dep}"));
        }
    }
    assert!(
        all.is_empty(),
        "placeholder platform crates must have no dependencies yet (no objc2, windows-sys or gtk):\n  {}",
        all.join("\n  ")
    );
}

#[test]
fn implemented_platform_crates_declare_only_allowlisted_dependencies() {
    let mut all = Vec::new();
    for c in IMPLEMENTED_PLATFORM_CRATES {
        all.extend(dependency_violations(c, &manifest(c)));
    }
    assert!(all.is_empty(), "{}", all.join("\n"));
}

#[test]
fn platform_crates_inherit_workspace_package_fields_and_lints() {
    let mut problems = Vec::new();
    for (name, _) in PLATFORM_CRATES {
        let m = manifest(name);
        for field in ["version", "edition", "rust-version", "license", "publish"] {
            if !inherits_from_workspace(&m, field) {
                problems.push(format!(
                    "crates/{name}: `{field}` must be inherited (`{field}.workspace = true`)"
                ));
            }
        }
        if get(&m, "lints", "workspace").as_deref() != Some("true") {
            problems.push(format!(
                "crates/{name}: must set `[lints] workspace = true`"
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn platform_crates_are_libraries_with_a_doc_comment_naming_the_os() {
    let mut problems = Vec::new();
    for (name, os_words) in PLATFORM_CRATES {
        let dir = crate_dir(name);
        if dir.join("src").join("main.rs").exists() && !may_ship_binary(name) {
            problems.push(format!(
                "crates/{name} must be a library crate, found src/main.rs"
            ));
        }
        let path = dir.join("src").join("lib.rs");
        if !path.is_file() {
            problems.push(format!("crates/{name}: missing src/lib.rs"));
            continue;
        }
        let src = read(&path);
        let mut doc = String::new();
        for line in src.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if let Some(text) = line.strip_prefix("//!") {
                doc.push_str(text);
                doc.push('\n');
            } else if !line.starts_with("//") && !is_implemented(name) {
                problems.push(format!(
                    "crates/{name}/src/lib.rs must hold only a `//!` doc comment, found code: {line:?}"
                ));
            }
        }
        if doc.trim().is_empty() {
            problems.push(format!(
                "crates/{name}/src/lib.rs needs a crate-level `//!` doc comment"
            ));
        } else if !os_words.iter().any(|w| doc.contains(w)) {
            problems.push(format!(
                "crates/{name}/src/lib.rs doc comment must name the target OS (one of {os_words:?})"
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn only_ui_shell_crates_may_ship_a_binary() {
    for c in UI_SHELL_CRATES_WITH_BINARY {
        assert!(
            c.starts_with("fx-ui-") && PLATFORM_CRATES.iter().any(|(n, _)| n == c),
            "{c} is not a native UI shell crate from the platform crate table"
        );
    }
    for backend in ["fx-fs-macos", "fx-fs-windows", "fx-fs-linux"] {
        assert!(
            !may_ship_binary(backend),
            "{backend} must stay library-only"
        );
    }
}

#[test]
fn placeholder_platform_crates_hold_no_other_files() {
    let mut problems = Vec::new();
    for (name, _) in PLATFORM_CRATES {
        if is_implemented(name) {
            continue;
        }
        let dir = crate_dir(name);
        for extra in ["examples", "tests", "benches", "build.rs"] {
            if dir.join(extra).exists() {
                problems.push(format!("crates/{name}/{extra} must not exist yet"));
            }
        }
        if let Ok(entries) = std::fs::read_dir(dir.join("src")) {
            for entry in entries.flatten() {
                if entry.file_name() != "lib.rs" {
                    problems.push(format!(
                        "crates/{name}/src must hold only lib.rs, found {}",
                        entry.file_name().to_string_lossy()
                    ));
                }
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

// ---------------------------------------------------------------------------
// Self-tests: the dependency checker must actually catch dependencies.
// ---------------------------------------------------------------------------

const EMPTY: &str = r#"
[package]
name = "fx-x"   # comment with [dependencies] in it
version.workspace = true

[lints]
workspace = true
"#;

#[test]
fn dependency_checker_accepts_manifest_without_dependencies() {
    assert_eq!(declared_dependencies(EMPTY), Vec::<String>::new());
    let empty_table = format!("{EMPTY}\n[dependencies]\n# none yet\n");
    assert_eq!(declared_dependencies(&empty_table), Vec::<String>::new());
}

#[test]
fn dependency_checker_finds_every_dependency_form() {
    for (extra, expect) in [
        ("[dependencies]\nobjc2 = \"0.6\"\n", "objc2"),
        ("[ dependencies ]\nfoo = { path = \"../foo\" }\n", "foo"),
        ("[dependencies.gtk4]\nversion = \"0.9\"\n", "gtk4"),
        ("[dev-dependencies]\nbar = \"1\"\n", "bar"),
        ("[build-dependencies]\ncc = \"1\"\n", "cc"),
        (
            "[target.'cfg(windows)'.dependencies]\nwindows-sys = \"0.59\"\n",
            "windows-sys",
        ),
        (
            "[target.'cfg(target_os = \"macos\")'.dependencies]\nobjc2 = \"0.6\"\n",
            "objc2",
        ),
    ] {
        let m = format!("{EMPTY}\n{extra}");
        let found = declared_dependencies(&m);
        assert!(
            found.iter().any(|d| d.contains(expect)),
            "expected to find {expect:?} in {extra:?}, got {found:?}"
        );
    }
    let dotted = "dependencies.objc2 = \"0.6\"\n[package]\nname = \"fx-x\"\n";
    assert_eq!(declared_dependencies(dotted).len(), 1);
    let dotted_target =
        "target.'cfg(target_os = \"macos\")'.dependencies.objc2 = \"0.6\"\n[package]\n";
    assert_eq!(declared_dependencies(dotted_target).len(), 1);
}

#[test]
fn dependency_checker_ignores_non_dependency_tables() {
    let m = format!("{EMPTY}\n[features]\ndefault = []\n[lib]\npath = \"src/lib.rs\"\n");
    assert_eq!(declared_dependencies(&m), Vec::<String>::new());
}

#[test]
fn dependency_parser_records_kind_target_and_fields() {
    let m = format!(
        "{EMPTY}\n[target.'cfg(target_os = \"macos\")'.dependencies]\nlibc = {{ version = \"0.2\", features = [\"extra_traits\", \"std\"] }}\n\
         [dev-dependencies.fx-bench]\npath = \"../fx-bench\"\n\
         [build-dependencies]\ncc.version = \"1\"\ncc.optional = true\n"
    );
    let deps = dependencies(&m);
    assert_eq!(deps.len(), 3, "{deps:?}");
    assert_eq!(deps[0].kind, DepKind::Normal);
    assert_eq!(deps[0].target.as_deref(), Some(MACOS_CFG));
    assert_eq!(deps[0].key, "libc");
    assert_eq!(deps[0].field("version"), Some("\"0.2\""));
    assert_eq!(
        deps[0].field("features"),
        Some("[\"extra_traits\",\"std\"]")
    );
    assert_eq!(deps[1].kind, DepKind::Dev);
    assert_eq!(deps[1].target, None);
    assert_eq!(deps[1].field("path"), Some("\"../fx-bench\""));
    assert_eq!(deps[2].kind, DepKind::Build);
    assert_eq!(deps[2].field("version"), Some("\"1\""));
    assert_eq!(deps[2].field("optional"), Some("true"));
}

// ---------------------------------------------------------------------------
// Self-tests: the allowlist must accept exactly what it lists.
// ---------------------------------------------------------------------------

fn violations(extra: &str) -> Vec<String> {
    dependency_violations("fx-fs-macos", &format!("{EMPTY}\n{extra}"))
}

#[test]
fn allowlist_accepts_libc_and_dev_fx_bench() {
    for extra in [
        "[dependencies]\nlibc = \"0.2\"\n",
        "[target.'cfg(target_os = \"macos\")'.dependencies]\nlibc = \"0.2\"\n",
        "[target.'cfg(target_os = \"macos\")'.dependencies]\nlibc = { version = \"0.2\", default-features = false }\n",
        "[dependencies.libc]\nversion = \"0.2\"\n",
        "[dev-dependencies]\nfx-bench = { path = \"../fx-bench\" }\n",
        "[dev-dependencies.fx-bench]\npath = \"../fx-bench\"\n",
        "[target.'cfg(target_os = \"macos\")'.dependencies]\nlibc = \"0.2\"\n\
         [dev-dependencies]\nfx-bench = { path = \"../fx-bench\" }\n",
    ] {
        assert_eq!(violations(extra), Vec::<String>::new(), "{extra:?}");
    }
    assert_eq!(violations(""), Vec::<String>::new());
}

#[test]
fn allowlist_accepts_fx_fs_and_fx_model_path_crates() {
    for extra in [
        "[dependencies]\nfx-fs = { path = \"../fx-fs\" }\nfx-model = { path = \"../fx-model\" }\n",
        "[target.'cfg(target_os = \"macos\")'.dependencies]\nfx-fs = { path = \"../fx-fs\" }\n\
         fx-model = { path = \"../fx-model\" }\nlibc = \"0.2\"\n",
        "[dependencies.fx-fs]\npath = \"../fx-fs\"\n",
    ] {
        assert_eq!(violations(extra), Vec::<String>::new(), "{extra:?}");
    }
    for extra in [
        // Only the workspace path crates.
        "[dependencies]\nfx-fs = \"0.1\"\n",
        "[dependencies]\nfx-model = { path = \"../elsewhere\" }\n",
        "[dependencies]\nfx-fs = { git = \"https://example.com/fx-fs\" }\n",
        // Only as normal dependencies, under the plain or macOS table.
        "[dev-dependencies]\nfx-fs = { path = \"../fx-fs\" }\n",
        "[build-dependencies]\nfx-model = { path = \"../fx-model\" }\n",
        "[target.'cfg(windows)'.dependencies]\nfx-fs = { path = \"../fx-fs\" }\n",
        // No other workspace crates.
        "[dependencies]\nfx-index = { path = \"../fx-index\" }\n",
    ] {
        assert!(!violations(extra).is_empty(), "should reject {extra:?}");
    }
}

#[test]
fn allowlist_rejects_objc2_and_unknown_crates() {
    for extra in [
        "[dependencies]\nobjc2 = \"0.6\"\n",
        "[target.'cfg(target_os = \"macos\")'.dependencies]\nobjc2 = \"0.6\"\n",
        "[dependencies]\nobjc2-foundation = \"0.3\"\n",
        "[dependencies]\nwindows-sys = \"0.59\"\n",
        "[dependencies]\ngtk4 = \"0.9\"\n",
        "[dependencies]\nserde = \"1\"\n",
        "[dev-dependencies]\ntempfile = \"3\"\n",
        "[dependencies]\nsys = { package = \"objc2\", version = \"0.6\" }\n",
        "[dependencies]\nlibc = { package = \"nix\", version = \"0.29\" }\n",
    ] {
        assert!(!violations(extra).is_empty(), "should reject {extra:?}");
    }
}

#[test]
fn allowlist_rejects_allowed_crates_in_the_wrong_place_or_form() {
    for extra in [
        // libc only as a normal dependency.
        "[build-dependencies]\nlibc = \"0.2\"\n",
        // only plain or macOS targets.
        "[target.'cfg(windows)'.dependencies]\nlibc = \"0.2\"\n",
        // libc from crates.io, not a path or git source.
        "[dependencies]\nlibc = { git = \"https://example.com/libc\" }\n",
        "[dependencies]\nlibc = { path = \"../libc\" }\n",
        // fx-bench only as a dev-dependency, and only the workspace path crate.
        "[dependencies]\nfx-bench = { path = \"../fx-bench\" }\n",
        "[dev-dependencies]\nfx-bench = \"0.1\"\n",
        "[dev-dependencies]\nfx-bench = { path = \"../elsewhere\" }\n",
    ] {
        assert!(!violations(extra).is_empty(), "should reject {extra:?}");
    }
}

#[test]
fn crates_without_an_allowlist_entry_may_declare_nothing() {
    let m = format!("{EMPTY}\n[dependencies]\nlibc = \"0.2\"\n");
    for c in ["fx-fs-linux", "fx-ui-win32", "fx-ui-gtk"] {
        assert!(!dependency_violations(c, &m).is_empty(), "{c}");
        assert_eq!(dependency_violations(c, EMPTY), Vec::<String>::new(), "{c}");
    }
    // Neither UI placeholder may pick up its toolkit before its run.
    for (c, extra) in [
        (
            "fx-ui-win32",
            "[target.'cfg(windows)'.dependencies]\nwindows = \"0.58\"\n",
        ),
        (
            "fx-ui-win32",
            "[target.'cfg(windows)'.dependencies]\nwindows-sys = \"0.59\"\n",
        ),
        (
            "fx-ui-gtk",
            "[target.'cfg(target_os = \"linux\")'.dependencies]\ngtk4 = \"0.9\"\n",
        ),
        (
            "fx-ui-gtk",
            "[target.'cfg(target_os = \"macos\")'.dependencies]\nobjc2 = \"0.6\"\n",
        ),
    ] {
        let m = format!("{EMPTY}\n{extra}");
        assert!(
            !dependency_violations(c, &m).is_empty(),
            "{c} should reject {extra:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Self-tests: fx-ui-macos may use objc2, only under the macOS cfg.
// ---------------------------------------------------------------------------

fn ui_macos_violations(extra: &str) -> Vec<String> {
    dependency_violations("fx-ui-macos", &format!("{EMPTY}\n{extra}"))
}

#[test]
fn fx_ui_macos_accepts_objc2_crates_under_the_macos_cfg_and_fx_bench() {
    for extra in [
        "[target.'cfg(target_os = \"macos\")'.dependencies]\nobjc2-app-kit = \"0.3\"\n",
        "[target.'cfg(target_os = \"macos\")'.dependencies]\nobjc2-app-kit = { version = \"0.3\", default-features = false, features = [\"NSTableView\"] }\n",
        "[target.'cfg(target_os = \"macos\")'.dependencies]\nobjc2 = \"0.6\"\nobjc2-foundation = \"0.3\"\nobjc2-app-kit = \"0.3\"\n",
        "[target.'cfg(target_os = \"macos\")'.dependencies.objc2]\nversion = \"0.6\"\n",
        "[dependencies]\nfx-bench = { path = \"../fx-bench\" }\n",
        "[dev-dependencies]\nfx-bench = { path = \"../fx-bench\" }\n",
        "[target.'cfg(target_os = \"macos\")'.dependencies]\nfx-bench = { path = \"../fx-bench\" }\n",
        "[dependencies]\nfx-channel = { path = \"../fx-channel\" }\n",
        "[dependencies]\nfx-bench = { path = \"../fx-bench\" }\n\
         [target.'cfg(target_os = \"macos\")'.dependencies]\nobjc2 = \"0.6\"\nobjc2-foundation = \"0.3\"\nobjc2-app-kit = \"0.3\"\n\
         [dev-dependencies]\nfx-bench = { path = \"../fx-bench\" }\n",
    ] {
        assert_eq!(
            ui_macos_violations(extra),
            Vec::<String>::new(),
            "{extra:?}"
        );
    }
    assert_eq!(ui_macos_violations(""), Vec::<String>::new());
}

#[test]
fn fx_ui_macos_rejects_objc2_outside_the_macos_cfg_or_from_other_sources() {
    for extra in [
        // Not in the plain (non-cfg) table.
        "[dependencies]\nobjc2 = \"0.6\"\n",
        "[dependencies]\nobjc2-app-kit = \"0.3\"\n",
        "[dependencies.objc2-foundation]\nversion = \"0.3\"\n",
        // Not under another target.
        "[target.'cfg(unix)'.dependencies]\nobjc2 = \"0.6\"\n",
        // Not as a dev- or build-dependency.
        "[dev-dependencies]\nobjc2 = \"0.6\"\n",
        "[target.'cfg(target_os = \"macos\")'.dev-dependencies]\nobjc2-app-kit = \"0.3\"\n",
        "[build-dependencies]\nobjc2 = \"0.6\"\n",
        // Only from crates.io.
        "[target.'cfg(target_os = \"macos\")'.dependencies]\nobjc2 = { git = \"https://example.com/objc2\" }\n",
        "[target.'cfg(target_os = \"macos\")'.dependencies]\nobjc2-app-kit = { path = \"../objc2-app-kit\" }\n",
        // Only the listed objc2 crates, and no renames.
        "[target.'cfg(target_os = \"macos\")'.dependencies]\nobjc2-metal = \"0.3\"\n",
        "[target.'cfg(target_os = \"macos\")'.dependencies]\nappkit = { package = \"objc2-app-kit\", version = \"0.3\" }\n",
        // windows and gtk stay forbidden, as do unknown crates.
        "[dependencies]\nwindows-sys = \"0.59\"\n",
        "[dependencies]\ngtk4 = \"0.9\"\n",
        "[target.'cfg(target_os = \"macos\")'.dependencies]\nlibc = \"0.2\"\n",
        // fx-bench only as the workspace path crate.
        "[dependencies]\nfx-bench = \"0.1\"\n",
        "[dev-dependencies]\nfx-bench = { path = \"../elsewhere\" }\n",
        "[build-dependencies]\nfx-bench = { path = \"../fx-bench\" }\n",
    ] {
        assert!(
            !ui_macos_violations(extra).is_empty(),
            "should reject {extra:?}"
        );
    }
}

#[test]
fn objc2_stays_forbidden_for_every_other_platform_crate() {
    let m = format!(
        "{EMPTY}\n[target.'cfg(target_os = \"macos\")'.dependencies]\nobjc2-app-kit = \"0.3\"\n"
    );
    for (c, _) in PLATFORM_CRATES {
        let found = dependency_violations(c, &m);
        if c == "fx-ui-macos" {
            assert_eq!(found, Vec::<String>::new());
        } else {
            assert!(
                found.iter().any(|v| v.contains("`objc2*`")),
                "{c} must reject objc2-app-kit, got {found:?}"
            );
        }
    }
}
