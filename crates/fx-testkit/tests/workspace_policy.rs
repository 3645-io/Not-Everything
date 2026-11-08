//! Workspace policy tests.
//!
//! These tests enforce the project rules from `.agents/rules/` on every crate in
//! the workspace, now and as crates are added later:
//!
//! - the root manifest declares `members = ["crates/*"]`,
//! - every package name starts with `fx-`,
//! - no crate name contains "everything" in any letter case,
//! - the license is inherited from the workspace (`license.workspace = true`,
//!   which is MIT) or, for designated subcomponents, is exactly
//!   `MIT OR Apache-2.0` or `Apache-2.0`,
//! - every crate whose license is not inherited from the workspace is listed
//!   by crate name in README.md's "Designated Apache-2.0 subcomponents" table,
//! - `publish` is `false` or inherited from the workspace (which is `false`),
//! - `rust-toolchain.toml` pins the stable channel.
//!
//! Only `std` is used; manifests are parsed with simple string handling.

mod common;

use common::{
    Item, get, inherits_from_workspace, items, package_name, read, repo_root, section_headers,
    section_lines, squash, strip_comment, table_header, table_keys, unquote,
};
use std::fs;

/// License expressions a designated subcomponent may declare instead of
/// inheriting MIT from the workspace.
const SUBCOMPONENT_LICENSES: [&str; 2] = ["MIT OR Apache-2.0", "Apache-2.0"];

/// Heading of the README table that lists designated subcomponents.
const DESIGNATED_HEADING: &str = "Designated Apache-2.0 subcomponents";

// ---------------------------------------------------------------------------
// Policy checks, returning human-readable violations.
// ---------------------------------------------------------------------------

/// The crate's license expression, unquoted, if it sets one explicitly.
fn explicit_license(manifest: &str) -> Option<String> {
    get(manifest, "package", "license")
        .as_deref()
        .and_then(unquote)
        .map(str::to_owned)
}

/// Checks one crate manifest against the policy. `dir_name` is the crate's
/// directory under `crates/`. Every violation message names the crate.
fn crate_violations(dir_name: &str, manifest: &str) -> Vec<String> {
    let mut v = Vec::new();

    let name = package_name(manifest);
    let label = name.clone().unwrap_or_else(|| dir_name.to_owned());

    match &name {
        None => v.push(format!(
            "crate `{dir_name}`: crates/{dir_name}/Cargo.toml has no [package] name"
        )),
        Some(n) => {
            if !n.starts_with("fx-") {
                v.push(format!(
                    "crate `{n}` (crates/{dir_name}): package name must start with `fx-`"
                ));
            }
            if n.to_lowercase().contains("everything") {
                v.push(format!(
                    "crate `{n}` (crates/{dir_name}): crate names must not contain \"everything\""
                ));
            }
        }
    }
    if dir_name.to_lowercase().contains("everything") {
        v.push(format!(
            "crate `{label}`: directory crates/{dir_name} must not contain \"everything\""
        ));
    }

    let license_ok = inherits_from_workspace(manifest, "license")
        || explicit_license(manifest).is_some_and(|l| SUBCOMPONENT_LICENSES.contains(&l.as_str()));
    if !license_ok {
        v.push(format!(
            "crate `{label}` (crates/{dir_name}): license must be `license.workspace = true` \
             (MIT), or exactly \"MIT OR Apache-2.0\" or \"Apache-2.0\" for designated \
             subcomponents; found {:?}",
            get(manifest, "package", "license")
        ));
    }
    if get(manifest, "package", "license-file").is_some() {
        v.push(format!(
            "crate `{label}` (crates/{dir_name}): use the `license` field, not `license-file`"
        ));
    }

    let publish_ok = inherits_from_workspace(manifest, "publish")
        || get(manifest, "package", "publish").as_deref() == Some("false");
    if !publish_ok {
        v.push(format!(
            "crate `{label}` (crates/{dir_name}): publish must be `false` or \
             `publish.workspace = true`; found {:?}",
            get(manifest, "package", "publish")
        ));
    }

    v
}

/// Returns the entries of the first column of README's "Designated
/// Apache-2.0 subcomponents" table, with backticks removed and placeholder
/// rows such as `_none yet_` skipped. Returns `None` if the heading is
/// missing.
fn designated_subcomponents(readme: &str) -> Option<Vec<String>> {
    let mut lines = readme.lines().skip_while(|l| {
        !(l.starts_with('#') && l.trim_start_matches('#').trim() == DESIGNATED_HEADING)
    });
    lines.next()?;
    let rows = lines
        .take_while(|l| !l.starts_with('#'))
        .map(str::trim)
        .filter(|l| l.starts_with('|'))
        .skip(1); // header row
    let mut names = Vec::new();
    for row in rows {
        let first = row.trim_matches('|').split('|').next().unwrap_or("").trim();
        let is_separator = !first.is_empty() && first.chars().all(|c| matches!(c, '-' | ':'));
        let name = first.trim_matches('`').trim();
        if is_separator || name.is_empty() || name.starts_with('_') {
            continue;
        }
        names.push(name.to_owned());
    }
    Some(names)
}

/// A crate whose license is not inherited from the workspace must be listed
/// by crate name among the `designated` subcomponents.
fn designation_violations(dir_name: &str, manifest: &str, designated: &[String]) -> Vec<String> {
    if inherits_from_workspace(manifest, "license") {
        return Vec::new();
    }
    let name = package_name(manifest).unwrap_or_else(|| dir_name.to_owned());
    if designated.contains(&name) {
        return Vec::new();
    }
    vec![format!(
        "crate `{name}` (crates/{dir_name}): its license is not inherited from the workspace \
         (found {:?}), so it must be listed by crate name in README.md's \
         \"{DESIGNATED_HEADING}\" table",
        get(manifest, "package", "license")
    )]
}

// ---------------------------------------------------------------------------
// Repository access.
// ---------------------------------------------------------------------------

/// Returns `(dir_name, manifest_text)` for every `crates/*/Cargo.toml`.
fn crate_manifests() -> Vec<(String, String)> {
    let crates_dir = repo_root().join("crates");
    let mut out = Vec::new();
    for entry in fs::read_dir(&crates_dir)
        .unwrap_or_else(|e| panic!("cannot list {}: {e}", crates_dir.display()))
    {
        let entry = entry.expect("readable directory entry");
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let manifest = path.join("Cargo.toml");
        if !manifest.is_file() {
            continue;
        }
        let dir_name = entry.file_name().to_string_lossy().into_owned();
        out.push((dir_name, read(&manifest)));
    }
    out.sort();
    out
}

// ---------------------------------------------------------------------------
// Tests against the real repository.
// ---------------------------------------------------------------------------

#[test]
fn root_manifest_is_virtual_workspace_over_crates_glob() {
    let root = read(&repo_root().join("Cargo.toml"));
    assert!(
        section_lines(&root, "package").is_empty(),
        "root Cargo.toml must be a virtual workspace (no [package] section)"
    );
    assert_eq!(
        get(&root, "workspace", "members")
            .map(|m| m.replace(",]", "]"))
            .as_deref(),
        Some("[\"crates/*\"]"),
        "root Cargo.toml must declare `members = [\"crates/*\"]` under [workspace]"
    );
}

#[test]
fn workspace_package_defaults_are_mit_and_unpublished() {
    let root = read(&repo_root().join("Cargo.toml"));
    assert_eq!(
        get(&root, "workspace.package", "license").as_deref(),
        Some("\"MIT\""),
        "[workspace.package] must set license = \"MIT\" so inheriting crates are MIT"
    );
    assert_eq!(
        get(&root, "workspace.package", "publish").as_deref(),
        Some("false"),
        "[workspace.package] must set publish = false so inheriting crates are unpublished"
    );
}

#[test]
fn workspace_package_names_the_public_repository() {
    let root = read(&repo_root().join("Cargo.toml"));
    for key in ["repository", "homepage"] {
        assert_eq!(
            get(&root, "workspace.package", key).as_deref(),
            Some("\"https://github.com/3645-io/Not-Everything\""),
            "[workspace.package] must set {key} to the public repository"
        );
    }
}

/// Every `unsafe` block says why it is sound in a `// SAFETY:` comment.
#[test]
fn workspace_lints_ask_for_safety_comments() {
    let root = read(&repo_root().join("Cargo.toml"));
    assert_eq!(
        get(
            &root,
            "workspace.lints.clippy",
            "undocumented_unsafe_blocks"
        )
        .as_deref(),
        Some("\"warn\""),
        "[workspace.lints.clippy] must set undocumented_unsafe_blocks = \"warn\""
    );
}

#[test]
fn workspace_has_crates() {
    let manifests = crate_manifests();
    assert!(
        manifests.iter().any(|(d, _)| d == "fx-testkit"),
        "expected crates/fx-testkit/Cargo.toml among workspace crates; found {:?}",
        manifests.iter().map(|(d, _)| d).collect::<Vec<_>>()
    );
}

#[test]
fn every_crate_follows_naming_license_and_publish_policy() {
    let mut all = Vec::new();
    for (dir, manifest) in crate_manifests() {
        all.extend(crate_violations(&dir, &manifest));
    }
    assert!(
        all.is_empty(),
        "workspace policy violations:\n  {}",
        all.join("\n  ")
    );
}

#[test]
fn every_non_mit_crate_is_a_designated_subcomponent_in_readme() {
    let readme = read(&repo_root().join("README.md"));
    let designated = designated_subcomponents(&readme).unwrap_or_else(|| {
        panic!("README.md must have a \"### {DESIGNATED_HEADING}\" section with a table")
    });
    let mut all = Vec::new();
    for (dir, manifest) in crate_manifests() {
        all.extend(designation_violations(&dir, &manifest, &designated));
    }
    assert!(
        all.is_empty(),
        "undesignated subcomponents:\n  {}",
        all.join("\n  ")
    );
}

#[test]
fn every_crate_name_has_fx_prefix() {
    for (dir, manifest) in crate_manifests() {
        let name = package_name(&manifest).unwrap_or_default();
        assert!(
            name.starts_with("fx-"),
            "crate in crates/{dir} has package name {name:?}; it must start with `fx-`"
        );
    }
}

#[test]
fn no_crate_name_contains_everything() {
    for (dir, manifest) in crate_manifests() {
        let name = package_name(&manifest).unwrap_or_default();
        assert!(
            !name.to_lowercase().contains("everything"),
            "crate `{name}` (crates/{dir}) contains \"everything\" in its name"
        );
        assert!(
            !dir.to_lowercase().contains("everything"),
            "crate `{name}` lives in crates/{dir}, whose name contains \"everything\""
        );
    }
}

#[test]
fn rust_toolchain_pins_stable_channel() {
    let path = repo_root().join("rust-toolchain.toml");
    let toolchain = read(&path);
    assert_eq!(
        get(&toolchain, "toolchain", "channel").as_deref(),
        Some("\"stable\""),
        "rust-toolchain.toml must set `channel = \"stable\"` under [toolchain]"
    );
}

// ---------------------------------------------------------------------------
// Self-tests: the checker must actually reject bad manifests.
// ---------------------------------------------------------------------------

const GOOD: &str = r#"
[package]
name = "fx-good"   # trailing comment
version.workspace = true
edition.workspace = true
license.workspace = true
publish.workspace = true

[lints]
workspace = true
"#;

#[test]
fn checker_accepts_workspace_inheritance() {
    assert_eq!(crate_violations("fx-good", GOOD), Vec::<String>::new());
}

#[test]
fn checker_accepts_inline_table_inheritance_and_apache_subcomponents() {
    for license in ["\"MIT OR Apache-2.0\"", "\"Apache-2.0\"", "'Apache-2.0'"] {
        let m = format!("[package]\nname = \"fx-sub\"\nlicense = {license}\npublish = false\n");
        assert_eq!(
            crate_violations("fx-sub", &m),
            Vec::<String>::new(),
            "{license}"
        );
    }
    let m = r#"
[package]
name = "fx-sub"
license = { workspace = true }
publish = { workspace = true }
"#;
    assert_eq!(crate_violations("fx-sub", m), Vec::<String>::new());
}

#[test]
fn checker_rejects_missing_fx_prefix() {
    let m = GOOD.replace("fx-good", "good");
    let v = crate_violations("good", &m);
    assert!(
        v.iter().any(|s| s.contains("`good`") && s.contains("fx-")),
        "{v:?}"
    );
}

#[test]
fn checker_rejects_everything_in_any_case() {
    let m = GOOD.replace("fx-good", "fx-EveryThing-ui");
    let v = crate_violations("fx-EveryThing-ui", &m);
    assert!(
        v.iter()
            .any(|s| s.contains("fx-EveryThing-ui") && s.contains("everything")),
        "{v:?}"
    );
}

#[test]
fn checker_rejects_other_licenses() {
    for bad in [
        "license = \"MIT\"\n",
        "license = \"GPL-3.0\"\n",
        "license = \"GPL-3.0-or-later\"\n",
        "license = \"MIT OR GPL-3.0\"\n",
        "license = \"Apache-2.0 OR MIT\"\n",
        "license = \"Apache-2.0 AND GPL-2.0\"\n",
        "license = \"BSD-3-Clause\"\n",
        "license.workspace = false\n",
    ] {
        let m = GOOD.replace("license.workspace = true\n", bad);
        let v = crate_violations("fx-good", &m);
        assert!(
            v.iter()
                .any(|s| s.contains("fx-good") && s.contains("license")),
            "expected license violation for {bad:?}, got {v:?}"
        );
    }
    let m = GOOD.replace("license.workspace = true\n", "");
    let v = crate_violations("fx-good", &m);
    assert!(
        v.iter().any(|s| s.contains("license")),
        "missing license: {v:?}"
    );
}

#[test]
fn checker_rejects_publishable_crates() {
    for bad in ["publish = true\n", "publish = [\"crates-io\"]\n", ""] {
        let m = GOOD.replace("publish.workspace = true\n", bad);
        let v = crate_violations("fx-good", &m);
        assert!(
            v.iter()
                .any(|s| s.contains("fx-good") && s.contains("publish")),
            "expected publish violation for {bad:?}, got {v:?}"
        );
    }
}

#[test]
fn checker_ignores_keys_outside_package_section() {
    let m = r#"
[package]
name = "fx-x"

[dependencies]
license = "MIT OR Apache-2.0"
publish = false
"#;
    let v = crate_violations("fx-x", m);
    assert!(v.iter().any(|s| s.contains("license")), "{v:?}");
    assert!(v.iter().any(|s| s.contains("publish")), "{v:?}");
}

const README_NONE: &str = "\
# Title

### Designated Apache-2.0 subcomponents

| Crate or path | License | Origin | NOTICE file |
| --- | --- | --- | --- |
| _none yet_ | | | |

**Dependencies.** fx-sub is not designated here.
";

const README_SOME: &str = "\
## Licensing

### Designated Apache-2.0 subcomponents

| Crate or path | License | Origin | NOTICE file |
| :--- | --- | --- | --- |
| `fx-sub` | Apache-2.0 | Example project | crates/fx-sub/NOTICE |
| fx-dual | MIT OR Apache-2.0 | Example project | none |

## Other

| fx-later | Apache-2.0 | | |
";

#[test]
fn designated_table_parser_reads_first_column() {
    assert_eq!(
        designated_subcomponents(README_NONE),
        Some(Vec::<String>::new())
    );
    assert_eq!(
        designated_subcomponents(README_SOME),
        Some(vec!["fx-sub".to_owned(), "fx-dual".to_owned()])
    );
    assert_eq!(designated_subcomponents("# Title\n\nNo table.\n"), None);
}

#[test]
fn designation_check_requires_readme_listing_for_non_mit_crates() {
    let apache = "[package]\nname = \"fx-sub\"\nlicense = \"Apache-2.0\"\npublish = false\n";
    let dual = "[package]\nname = \"fx-dual\"\nlicense = \"MIT OR Apache-2.0\"\n";
    let later = "[package]\nname = \"fx-later\"\nlicense = \"Apache-2.0\"\n";

    let none = designated_subcomponents(README_NONE).expect("table present");
    assert_eq!(
        designation_violations("fx-good", GOOD, &none),
        Vec::<String>::new()
    );
    let v = designation_violations("fx-sub", apache, &none);
    assert!(
        v.iter()
            .any(|s| s.contains("`fx-sub`") && s.contains("README")),
        "{v:?}"
    );

    let some = designated_subcomponents(README_SOME).expect("table present");
    assert_eq!(
        designation_violations("fx-sub", apache, &some),
        Vec::<String>::new()
    );
    assert_eq!(
        designation_violations("fx-dual", dual, &some),
        Vec::<String>::new()
    );
    assert_eq!(
        designation_violations("fx-good", GOOD, &some),
        Vec::<String>::new()
    );
    // Listed outside the designated table, so not designated.
    let v = designation_violations("fx-later", later, &some);
    assert!(v.iter().any(|s| s.contains("`fx-later`")), "{v:?}");
}

#[test]
fn members_parser_handles_multiline_arrays() {
    let root = "[workspace]\nresolver = \"3\"\nmembers = [\n    \"crates/*\",\n]\n";
    // The parser squashes whitespace only; the root-manifest test tolerates
    // the trailing comma separately.
    assert_eq!(
        get(root, "workspace", "members").as_deref(),
        Some("[\"crates/*\",]")
    );
    let root = "[workspace]\nmembers = [ \"crates/*\" ]  # all crates\n";
    assert_eq!(
        get(root, "workspace", "members").as_deref(),
        Some("[\"crates/*\"]")
    );
}

// ---------------------------------------------------------------------------
// Self-tests of the shared TOML helpers in `common`.
// ---------------------------------------------------------------------------

#[test]
fn helpers_handle_comments_quotes_and_headers() {
    assert_eq!(strip_comment("a = \"x # y\" # z"), "a = \"x # y\" ");
    assert_eq!(strip_comment("a = 'x # y' # z"), "a = 'x # y' ");
    assert_eq!(
        strip_comment("a = \"x \\\" # y\" # z"),
        "a = \"x \\\" # y\" "
    );
    assert_eq!(squash(" a = \"b c\" "), "a=\"b c\"");
    assert_eq!(table_header("[ package ] # c").as_deref(), Some("package"));
    assert_eq!(table_header("[[bin]]").as_deref(), Some("[bin]"));
    assert_eq!(
        table_header("[target.'cfg(target_os = \"macos\")'.dependencies]").as_deref(),
        Some("target.'cfg(target_os = \"macos\")'.dependencies")
    );
    assert_eq!(table_header("name = \"x\""), None);
    assert_eq!(
        table_keys("target.'cfg(unix)'.dependencies"),
        ["target", "cfg(unix)", "dependencies"]
    );
    assert_eq!(unquote("\"MIT\""), Some("MIT"));
    assert_eq!(unquote("'MIT'"), Some("MIT"));
    assert_eq!(unquote("\""), None);
    assert_eq!(unquote("MIT"), None);
}

#[test]
fn items_join_multiline_arrays_and_ignore_brackets_in_strings() {
    let m = "[a]\nx = [\n  \"]\",\n  \"y\", # c\n]\nz = 1\n[[b]]\nname = \"n\"\n";
    assert_eq!(
        items(m),
        [
            Item::Header("a".into()),
            Item::Entry("x=[\"]\",\"y\",]".into()),
            Item::Entry("z=1".into()),
            Item::Header("[b]".into()),
            Item::Entry("name=\"n\"".into()),
        ]
    );
    assert_eq!(get(m, "a", "z").as_deref(), Some("1"));
    assert_eq!(get(m, "[b]", "name").as_deref(), Some("\"n\""));
    assert_eq!(section_headers(m), ["a", "[b]"]);
}
