//! Contract tests for the exclusion engine in `fx-fs`.
//!
//! Written from docs/PLAN.md ("Exclusions (hidden, system, folders,
//! wildcards, regex) and include-only folders", and the plan's task for
//! the exclusion engine) only.
//!
//! `Exclusions::check(path, name, is_dir, hidden, system)` receives the full
//! path of the entry (including its own name) and returns a `Verdict`:
//! - `Include`: emit the entry (and descend into it if it is a directory);
//! - `SkipEntry`: do not emit this file;
//! - `SkipSubtree`: do not emit this directory and do not descend into it;
//! - `TraverseOnly`: a directory that is an ancestor of an include-only root:
//!   descend into it but do not emit it.

use std::path::{Path, PathBuf};

use fx_fs::{ExclusionConfig, ExclusionError, Exclusions, Verdict};

fn build(cfg: ExclusionConfig) -> Exclusions {
    Exclusions::new(cfg).expect("config should compile")
}

fn dir(ex: &Exclusions, path: &str) -> Verdict {
    let p = Path::new(path);
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
    ex.check(p, name, true, false, false)
}

fn file(ex: &Exclusions, path: &str) -> Verdict {
    let p = Path::new(path);
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
    ex.check(p, name, false, false, false)
}

fn folders(paths: &[&str]) -> Vec<PathBuf> {
    paths.iter().map(PathBuf::from).collect()
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

// ---------------------------------------------------------------- defaults

#[test]
fn default_config_excludes_nothing() {
    let cfg = ExclusionConfig::default();
    assert!(!cfg.exclude_hidden);
    assert!(!cfg.exclude_system);
    assert!(cfg.excluded_folders.is_empty());
    assert!(cfg.include_only.is_empty());
    assert!(cfg.wildcard_patterns.is_empty());
    assert!(cfg.regex_patterns.is_empty());
    assert!(!cfg.case_sensitive, "APFS is case-insensitive by default");

    let ex = build(cfg);
    for (path, is_dir) in [
        ("/", true),
        ("/Users", true),
        ("/Users/me/.ssh", true),
        ("/Users/me/.zshrc", false),
        ("/System/Library/Kernels/kernel", false),
        ("/private/tmp/x.tmp", false),
    ] {
        let p = Path::new(path);
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        for (hidden, system) in [(false, false), (true, false), (false, true), (true, true)] {
            assert_eq!(
                ex.check(p, name, is_dir, hidden, system),
                Verdict::Include,
                "{path} (dir={is_dir}, hidden={hidden}, system={system})"
            );
        }
    }
}

#[test]
fn exclusions_are_shareable_across_threads() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Exclusions>();
    let cfg = ExclusionConfig::default();
    let _copy = cfg.clone();
}

// ---------------------------------------------------------- hidden / system

#[test]
fn hidden_entries_are_excluded_when_enabled() {
    let ex = build(ExclusionConfig {
        exclude_hidden: true,
        ..Default::default()
    });
    let p = Path::new("/Users/me/.cache");
    assert_eq!(
        ex.check(p, ".cache", true, true, false),
        Verdict::SkipSubtree
    );
    let p = Path::new("/Users/me/.zshrc");
    assert_eq!(
        ex.check(p, ".zshrc", false, true, false),
        Verdict::SkipEntry
    );
    // Not hidden: included.
    let p = Path::new("/Users/me/Documents");
    assert_eq!(
        ex.check(p, "Documents", true, false, false),
        Verdict::Include
    );
    let p = Path::new("/Users/me/a.txt");
    assert_eq!(ex.check(p, "a.txt", false, false, false), Verdict::Include);
    // System-only entries are untouched by the hidden switch.
    let p = Path::new("/System/thing");
    assert_eq!(ex.check(p, "thing", false, false, true), Verdict::Include);
}

#[test]
fn system_entries_are_excluded_when_enabled() {
    let ex = build(ExclusionConfig {
        exclude_system: true,
        ..Default::default()
    });
    let p = Path::new("/System/Library");
    assert_eq!(
        ex.check(p, "Library", true, false, true),
        Verdict::SkipSubtree
    );
    let p = Path::new("/System/kernel");
    assert_eq!(
        ex.check(p, "kernel", false, false, true),
        Verdict::SkipEntry
    );
    let p = Path::new("/Users/me/.zshrc");
    assert_eq!(ex.check(p, ".zshrc", false, true, false), Verdict::Include);
    let p = Path::new("/Users/me/a.txt");
    assert_eq!(ex.check(p, "a.txt", false, false, false), Verdict::Include);
}

// ------------------------------------------------------- excluded folders

#[test]
fn excluded_folder_skips_its_subtree() {
    let ex = build(ExclusionConfig {
        excluded_folders: folders(&["/a/b"]),
        ..Default::default()
    });
    assert_eq!(dir(&ex, "/a/b"), Verdict::SkipSubtree);
    assert_eq!(dir(&ex, "/a/b/c"), Verdict::SkipSubtree);
    assert_eq!(dir(&ex, "/a/b/c/d"), Verdict::SkipSubtree);
    assert_eq!(file(&ex, "/a/b/c.txt"), Verdict::SkipEntry);
    assert_eq!(file(&ex, "/a/b/c/d/e.txt"), Verdict::SkipEntry);
}

#[test]
fn excluded_folder_matches_by_path_component() {
    let ex = build(ExclusionConfig {
        excluded_folders: folders(&["/a/b"]),
        ..Default::default()
    });
    // Siblings sharing a prefix are not excluded.
    assert_eq!(dir(&ex, "/a/bc"), Verdict::Include);
    assert_eq!(dir(&ex, "/a/bc/d"), Verdict::Include);
    assert_eq!(file(&ex, "/a/b.txt"), Verdict::Include);
    assert_eq!(file(&ex, "/a/bc/x"), Verdict::Include);
    // The ancestors of an excluded folder are still indexed.
    assert_eq!(dir(&ex, "/a"), Verdict::Include);
    assert_eq!(dir(&ex, "/"), Verdict::Include);
    // Unrelated folders are included.
    assert_eq!(dir(&ex, "/x/a/b"), Verdict::Include);
}

#[test]
fn excluded_folder_with_trailing_slash_and_several_entries() {
    let ex = build(ExclusionConfig {
        excluded_folders: folders(&["/a/b/", "/opt/cache"]),
        ..Default::default()
    });
    assert_eq!(dir(&ex, "/a/b"), Verdict::SkipSubtree);
    assert_eq!(dir(&ex, "/opt/cache"), Verdict::SkipSubtree);
    assert_eq!(file(&ex, "/opt/cache/blob"), Verdict::SkipEntry);
    assert_eq!(dir(&ex, "/opt/caches"), Verdict::Include);
    assert_eq!(dir(&ex, "/opt"), Verdict::Include);
}

// --------------------------------------------------------------- wildcards

#[test]
fn wildcard_star_matches_the_whole_name() {
    let ex = build(ExclusionConfig {
        wildcard_patterns: strings(&["*.tmp"]),
        ..Default::default()
    });
    assert_eq!(file(&ex, "/x/a.tmp"), Verdict::SkipEntry);
    assert_eq!(file(&ex, "/x/y/z/.tmp"), Verdict::SkipEntry);
    assert_eq!(dir(&ex, "/x/build.tmp"), Verdict::SkipSubtree);
    // Whole-name match: no partial matches.
    assert_eq!(file(&ex, "/x/a.tmpx"), Verdict::Include);
    assert_eq!(file(&ex, "/x/a.tmp.bak"), Verdict::Include);
    assert_eq!(file(&ex, "/x/a.txt"), Verdict::Include);
}

#[test]
fn wildcard_name_without_metacharacters_matches_exactly() {
    let ex = build(ExclusionConfig {
        wildcard_patterns: strings(&["node_modules"]),
        ..Default::default()
    });
    assert_eq!(dir(&ex, "/src/app/node_modules"), Verdict::SkipSubtree);
    assert_eq!(dir(&ex, "/src/app/node_modules_old"), Verdict::Include);
    assert_eq!(dir(&ex, "/src/app/my_node_modules"), Verdict::Include);
    assert_eq!(dir(&ex, "/src/app"), Verdict::Include);
}

#[test]
fn wildcard_question_mark_matches_exactly_one_character() {
    let ex = build(ExclusionConfig {
        wildcard_patterns: strings(&["a?c"]),
        ..Default::default()
    });
    assert_eq!(file(&ex, "/d/abc"), Verdict::SkipEntry);
    assert_eq!(file(&ex, "/d/a.c"), Verdict::SkipEntry);
    assert_eq!(file(&ex, "/d/ac"), Verdict::Include);
    assert_eq!(file(&ex, "/d/abbc"), Verdict::Include);
}

#[test]
fn wildcard_names_are_case_insensitive_by_default() {
    let ex = build(ExclusionConfig {
        wildcard_patterns: strings(&["*.TMP", "Thumbs.db"]),
        ..Default::default()
    });
    assert_eq!(file(&ex, "/x/a.tmp"), Verdict::SkipEntry);
    assert_eq!(file(&ex, "/x/A.Tmp"), Verdict::SkipEntry);
    assert_eq!(file(&ex, "/x/thumbs.DB"), Verdict::SkipEntry);
}

#[test]
fn wildcard_names_respect_case_sensitive_option() {
    let ex = build(ExclusionConfig {
        wildcard_patterns: strings(&["*.TMP"]),
        case_sensitive: true,
        ..Default::default()
    });
    assert_eq!(file(&ex, "/x/a.TMP"), Verdict::SkipEntry);
    assert_eq!(file(&ex, "/x/a.tmp"), Verdict::Include);
}

#[test]
fn wildcard_with_slash_matches_the_full_path() {
    let ex = build(ExclusionConfig {
        wildcard_patterns: strings(&["/Users/*/Library"]),
        ..Default::default()
    });
    assert_eq!(dir(&ex, "/Users/me/Library"), Verdict::SkipSubtree);
    assert_eq!(dir(&ex, "/Users/you/Library"), Verdict::SkipSubtree);
    // A single `*` does not cross `/`.
    assert_eq!(dir(&ex, "/Users/me/x/Library"), Verdict::Include);
    // The name alone is not enough when the pattern contains `/`.
    assert_eq!(dir(&ex, "/Library"), Verdict::Include);
    assert_eq!(dir(&ex, "/opt/Library"), Verdict::Include);
    assert_eq!(dir(&ex, "/Users/me"), Verdict::Include);
}

#[test]
fn wildcard_double_star_crosses_slashes() {
    let ex = build(ExclusionConfig {
        wildcard_patterns: strings(&["**/node_modules", "/data/**/cache", "/scratch/**"]),
        ..Default::default()
    });
    assert_eq!(dir(&ex, "/a/node_modules"), Verdict::SkipSubtree);
    assert_eq!(dir(&ex, "/a/b/c/d/node_modules"), Verdict::SkipSubtree);
    assert_eq!(dir(&ex, "/a/b/node_modules2"), Verdict::Include);

    assert_eq!(dir(&ex, "/data/x/cache"), Verdict::SkipSubtree);
    assert_eq!(dir(&ex, "/data/x/y/z/cache"), Verdict::SkipSubtree);
    assert_eq!(dir(&ex, "/other/x/cache"), Verdict::Include);
    assert_eq!(dir(&ex, "/data/x/caches"), Verdict::Include);

    assert_eq!(file(&ex, "/scratch/a/b/c.txt"), Verdict::SkipEntry);
    assert_eq!(dir(&ex, "/scratch/a"), Verdict::SkipSubtree);
    assert_eq!(dir(&ex, "/scratchpad/a"), Verdict::Include);
}

// ------------------------------------------------------------------- regex

#[test]
fn regex_excludes_matching_names() {
    let ex = build(ExclusionConfig {
        regex_patterns: strings(&[r"\.bak$", r"node_modules$"]),
        ..Default::default()
    });
    assert_eq!(file(&ex, "/x/report.bak"), Verdict::SkipEntry);
    assert_eq!(file(&ex, "/x/report.bak2"), Verdict::Include);
    assert_eq!(file(&ex, "/x/report.txt"), Verdict::Include);
    assert_eq!(dir(&ex, "/src/node_modules"), Verdict::SkipSubtree);
    assert_eq!(dir(&ex, "/src/node_modules_x"), Verdict::Include);
}

#[test]
fn regex_is_case_insensitive_by_default() {
    let ex = build(ExclusionConfig {
        regex_patterns: strings(&[r"\.BAK$"]),
        ..Default::default()
    });
    assert_eq!(file(&ex, "/x/report.bak"), Verdict::SkipEntry);
    assert_eq!(file(&ex, "/x/report.Bak"), Verdict::SkipEntry);
}

#[test]
fn regex_respects_case_sensitive_option() {
    let ex = build(ExclusionConfig {
        regex_patterns: strings(&[r"\.BAK$"]),
        case_sensitive: true,
        ..Default::default()
    });
    assert_eq!(file(&ex, "/x/report.BAK"), Verdict::SkipEntry);
    assert_eq!(file(&ex, "/x/report.bak"), Verdict::Include);
}

// --------------------------------------------------------- invalid configs

#[test]
fn invalid_regex_is_an_error_not_a_panic() {
    for bad in ["(", "[z-a]", "a{2,1}", "(?P<", "*abc"] {
        let result = Exclusions::new(ExclusionConfig {
            regex_patterns: strings(&[r"\.ok$", bad]),
            ..Default::default()
        });
        let err: ExclusionError = match result {
            Ok(_) => panic!("regex {bad:?} should be rejected"),
            Err(e) => e,
        };
        assert!(!err.to_string().is_empty());
        // Usable as a standard error.
        let boxed: Box<dyn std::error::Error + Send + Sync> = Box::new(err);
        assert!(!format!("{boxed:?}").is_empty());
    }
}

#[test]
fn valid_patterns_compile() {
    let result = Exclusions::new(ExclusionConfig {
        exclude_hidden: true,
        exclude_system: true,
        excluded_folders: folders(&["/a/b"]),
        include_only: folders(&["/a"]),
        wildcard_patterns: strings(&["*.tmp", "a?c", "**/node_modules", "/x/*/y"]),
        regex_patterns: strings(&[r"^\d+$", r"\.bak$"]),
        ..Default::default()
    });
    assert!(result.is_ok());
}

// ------------------------------------------------------------ include-only

#[test]
fn include_only_root_and_its_contents_are_included() {
    let ex = build(ExclusionConfig {
        include_only: folders(&["/Users/me/Documents"]),
        ..Default::default()
    });
    assert_eq!(dir(&ex, "/Users/me/Documents"), Verdict::Include);
    assert_eq!(dir(&ex, "/Users/me/Documents/work"), Verdict::Include);
    assert_eq!(file(&ex, "/Users/me/Documents/a.txt"), Verdict::Include);
    assert_eq!(
        file(&ex, "/Users/me/Documents/work/deep/b.txt"),
        Verdict::Include
    );
}

#[test]
fn include_only_ancestors_are_traverse_only() {
    let ex = build(ExclusionConfig {
        include_only: folders(&["/Users/me/Documents"]),
        ..Default::default()
    });
    assert_eq!(dir(&ex, "/Users"), Verdict::TraverseOnly);
    assert_eq!(dir(&ex, "/Users/me"), Verdict::TraverseOnly);
}

#[test]
fn outside_include_only_roots_is_skipped() {
    let ex = build(ExclusionConfig {
        include_only: folders(&["/Users/me/Documents"]),
        ..Default::default()
    });
    assert_eq!(dir(&ex, "/Applications"), Verdict::SkipSubtree);
    assert_eq!(dir(&ex, "/Users/other"), Verdict::SkipSubtree);
    assert_eq!(dir(&ex, "/Users/me/Downloads"), Verdict::SkipSubtree);
    // Path-component match: a prefix sibling is outside the root.
    assert_eq!(dir(&ex, "/Users/me/Documents2"), Verdict::SkipSubtree);
    // Files sitting in traverse-only ancestors are not emitted.
    assert_eq!(file(&ex, "/Users/me/notes.txt"), Verdict::SkipEntry);
    assert_eq!(file(&ex, "/Users/me/Documents.txt"), Verdict::SkipEntry);
    assert_eq!(file(&ex, "/top.txt"), Verdict::SkipEntry);
}

#[test]
fn several_include_only_roots() {
    let ex = build(ExclusionConfig {
        include_only: folders(&["/a/x", "/b"]),
        ..Default::default()
    });
    assert_eq!(dir(&ex, "/a"), Verdict::TraverseOnly);
    assert_eq!(dir(&ex, "/a/x"), Verdict::Include);
    assert_eq!(dir(&ex, "/a/y"), Verdict::SkipSubtree);
    assert_eq!(dir(&ex, "/b"), Verdict::Include);
    assert_eq!(file(&ex, "/b/f"), Verdict::Include);
    assert_eq!(dir(&ex, "/c"), Verdict::SkipSubtree);
}

#[test]
fn exclusions_apply_inside_include_only_roots() {
    let ex = build(ExclusionConfig {
        include_only: folders(&["/Users/me/Documents"]),
        excluded_folders: folders(&["/Users/me/Documents/private"]),
        wildcard_patterns: strings(&["*.tmp"]),
        exclude_hidden: true,
        ..Default::default()
    });
    assert_eq!(
        dir(&ex, "/Users/me/Documents/private"),
        Verdict::SkipSubtree
    );
    assert_eq!(
        file(&ex, "/Users/me/Documents/private/x"),
        Verdict::SkipEntry
    );
    assert_eq!(file(&ex, "/Users/me/Documents/a.tmp"), Verdict::SkipEntry);
    let p = Path::new("/Users/me/Documents/.git");
    assert_eq!(ex.check(p, ".git", true, true, false), Verdict::SkipSubtree);
    assert_eq!(file(&ex, "/Users/me/Documents/a.txt"), Verdict::Include);
}

#[test]
fn excluded_folder_wins_over_traverse_only_ancestor() {
    let ex = build(ExclusionConfig {
        include_only: folders(&["/Users/me/Documents"]),
        excluded_folders: folders(&["/Users"]),
        ..Default::default()
    });
    assert_eq!(dir(&ex, "/Users"), Verdict::SkipSubtree);
}
