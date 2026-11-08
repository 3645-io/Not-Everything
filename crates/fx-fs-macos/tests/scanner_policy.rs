//! Scanner policy tests for `fx_fs_macos::scan` (macOS only).
//!
//! Contract pinned here (on top of `tests/walker.rs`):
//! - `ScanOptions::exclusions: fx_fs::Exclusions`, defaulting to "exclude
//!   nothing". The walker asks it about every entry *before* descending:
//!   excluded directories are neither emitted nor opened, excluded files are
//!   not emitted. The path given to the engine is the absolute path (scan
//!   root joined with the entry's relative path).
//! - Hidden = name starts with '.' or `UF_HIDDEN` is set. System =
//!   `SF_RESTRICTED` or anything under `/System`.
//! - `ScanStats::materialization_disabled: bool` records the read-back of
//!   the dataless materialization policy after the scan switched it off.
//! - `ScanStats::skipped_firmlink_aliases: u64` counts firmlink aliases
//!   (such as `/System/Volumes/Data`) that were listed but not entered.
//! - Gate (ignored, run by the owner): scanning `/` emits nothing under
//!   `/System/Volumes/Data` and no (device, inode) pair twice.
//!
//! Tests only create and remove their own temporary directories.

#![cfg(target_os = "macos")]

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ffi::CString;
use std::fs;
use std::os::macos::fs::MetadataExt as MacMetadataExt;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use fx_bench::{TreeShape, generate_tree, materialize};
use fx_fs::{ExclusionConfig, Exclusions};
use fx_fs_macos::scan::{ScanOptions, ScanResult, relative_path, scan};

/// `UF_HIDDEN` from `sys/stat.h`.
const UF_HIDDEN: u32 = 0x0000_8000;

// ---------------------------------------------------------------- helpers

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> TempDir {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "fx-policy-{tag}-{}-{nanos}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("create unique temp dir");
        let path = fs::canonicalize(&path).expect("canonicalize temp dir");
        TempDir { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        restore_perms(&self.path);
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn restore_perms(dir: &Path) {
    let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o755));
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_type().is_ok_and(|ft| ft.is_dir()) {
                restore_perms(&e.path());
            }
        }
    }
}

fn write(path: &Path) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent dirs");
    }
    fs::write(path, b"x").expect("write file");
}

fn set_uf_hidden(path: &Path) {
    let c = CString::new(path.as_os_str().as_bytes()).expect("no NUL in path");
    // SAFETY: `c` is a valid NUL-terminated path; chflags only changes the
    // flags of a file inside the test's own temp dir.
    let rc = unsafe { libc::chflags(c.as_ptr(), UF_HIDDEN) };
    assert_eq!(rc, 0, "chflags(UF_HIDDEN) on {}", path.display());
}

/// fx-bench tree under `gen/` plus hand-made entries exercising every rule.
fn build_fixture(tmp: &TempDir) -> PathBuf {
    let root = tmp.path.join("root");
    fs::create_dir(&root).expect("create root");
    let tree = generate_tree(2_000, 0x5ca9_0107, TreeShape::default());
    fs::create_dir(root.join("gen")).expect("create gen");
    materialize(&tree, &root.join("gen"), None).expect("materialize tree");

    write(&root.join("keep/a.txt"));
    write(&root.join("keep/b.rs"));
    write(&root.join(".hidden_dir/inner/file.rs"));
    write(&root.join(".dotfile"));
    write(&root.join("uf_hidden_file"));
    set_uf_hidden(&root.join("uf_hidden_file"));
    write(&root.join("uf_hidden_dir/child.rs"));
    set_uf_hidden(&root.join("uf_hidden_dir"));
    write(&root.join("node_modules/pkg/index.js"));
    write(&root.join("src/node_modules/dep/x.js"));
    write(&root.join("src/main.rs"));
    write(&root.join("cache123/blob"));
    write(&root.join("cache/keep.rs"));
    root
}

/// Relative path -> BSD flags, for everything strictly below `root`.
fn ground_truth(root: &Path) -> BTreeMap<PathBuf, u32> {
    fn walk(root: &Path, rel: &Path, out: &mut BTreeMap<PathBuf, u32>) {
        let Ok(rd) = fs::read_dir(root.join(rel)) else {
            return;
        };
        for e in rd.flatten() {
            let child = rel.join(e.file_name());
            let md = fs::symlink_metadata(root.join(&child)).expect("lstat");
            out.insert(child.clone(), md.st_flags());
            if md.file_type().is_dir() {
                walk(root, &child, out);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, Path::new(""), &mut out);
    out
}

/// Truth entries that survive when every path whose prefix (itself
/// included) satisfies `pruned` is removed.
fn expected(
    truth: &BTreeMap<PathBuf, u32>,
    pruned: impl Fn(&Path, &str, u32) -> bool,
) -> BTreeSet<PathBuf> {
    truth
        .keys()
        .filter(|rel| {
            let mut prefix = PathBuf::new();
            for comp in rel.iter() {
                prefix.push(comp);
                let flags = truth[&prefix];
                if pruned(&prefix, &comp.to_string_lossy(), flags) {
                    return false;
                }
            }
            true
        })
        .cloned()
        .collect()
}

fn excl(cfg: ExclusionConfig) -> Exclusions {
    Exclusions::new(cfg).expect("valid exclusion config")
}

fn opts(threads: usize, exclusions: Exclusions) -> ScanOptions {
    ScanOptions {
        threads,
        collect: true,
        exclusions,
        ..ScanOptions::default()
    }
}

fn paths(result: &ScanResult) -> BTreeSet<PathBuf> {
    let entries = result.entries.as_ref().expect("collect = true");
    let set: BTreeSet<PathBuf> = (0..entries.len())
        .map(|i| relative_path(entries, i))
        .collect();
    assert_eq!(set.len(), entries.len(), "no path may be emitted twice");
    set
}

/// Scans `root` with `cfg` for 1 and 4 threads and checks the emitted
/// paths against `want`, with no errors and matching entry counts.
fn assert_scan(root: &Path, cfg: &ExclusionConfig, want: &BTreeSet<PathBuf>, label: &str) {
    for threads in [1, 4] {
        let result = scan(root, &opts(threads, excl(cfg.clone()))).expect("scan root");
        let got = paths(&result);
        let missing: Vec<_> = want.difference(&got).take(5).collect();
        let extra: Vec<_> = got.difference(want).take(5).collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "{label} threads={threads}: missing {missing:?}, unexpected {extra:?}"
        );
        assert_eq!(result.stats.entries, want.len() as u64, "{label}: entries");
        assert_eq!(result.stats.errors, 0, "{label}: errors");
    }
}

// ------------------------------------------------------------------ tests

#[test]
fn default_options_exclude_nothing() {
    let tmp = TempDir::new("default");
    let root = build_fixture(&tmp);
    let truth = ground_truth(&root);
    let want: BTreeSet<PathBuf> = truth.keys().cloned().collect();
    for threads in [1, 4] {
        let o = ScanOptions {
            threads,
            collect: true,
            ..ScanOptions::default()
        };
        let result = scan(&root, &o).expect("scan");
        assert_eq!(paths(&result), want, "threads={threads}");
    }
    assert_scan(&root, &ExclusionConfig::default(), &want, "empty config");
}

#[test]
fn hidden_exclusion_prunes_dot_names_and_uf_hidden() {
    let tmp = TempDir::new("hidden");
    let root = build_fixture(&tmp);
    let truth = ground_truth(&root);
    let want = expected(&truth, |_, name, flags| {
        name.starts_with('.') || flags & UF_HIDDEN != 0
    });
    assert!(!want.contains(Path::new(".hidden_dir")));
    assert!(!want.contains(Path::new("uf_hidden_dir/child.rs")));
    assert!(want.contains(Path::new("keep/a.txt")));
    let cfg = ExclusionConfig {
        exclude_hidden: true,
        ..ExclusionConfig::default()
    };
    assert_scan(&root, &cfg, &want, "exclude_hidden");
}

#[test]
fn system_exclusion_keeps_ordinary_entries() {
    let tmp = TempDir::new("system-temp");
    let root = build_fixture(&tmp);
    let truth = ground_truth(&root);
    let want: BTreeSet<PathBuf> = truth.keys().cloned().collect();
    let cfg = ExclusionConfig {
        exclude_system: true,
        ..ExclusionConfig::default()
    };
    assert_scan(&root, &cfg, &want, "exclude_system on a temp tree");
}

#[test]
fn system_exclusion_prunes_everything_under_system() {
    let root = Path::new("/System/Library/Fonts");
    if !root.is_dir() {
        return;
    }
    let all = scan(root, &opts(2, excl(ExclusionConfig::default()))).expect("scan fonts");
    assert!(all.stats.entries > 0, "/System/Library/Fonts is not empty");
    let cfg = ExclusionConfig {
        exclude_system: true,
        ..ExclusionConfig::default()
    };
    let none = scan(root, &opts(2, excl(cfg))).expect("scan fonts");
    assert_eq!(none.stats.entries, 0, "everything under /System is system");
    assert_eq!(paths(&none).len(), 0);
}

#[test]
fn excluded_folder_is_pruned_by_component_and_never_opened() {
    let tmp = TempDir::new("folder");
    let root = build_fixture(&tmp);
    write(&root.join("locked/secret.rs"));
    write(&root.join("lockedness/visible.rs"));
    let truth = ground_truth(&root);
    let want = expected(&truth, |rel, _, _| rel == Path::new("locked"));
    assert!(want.contains(Path::new("lockedness/visible.rs")));

    // An unreadable excluded folder: opening it would count an error.
    fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o000)).expect("chmod 000");
    if fs::read_dir(root.join("locked")).is_ok() {
        // As root the folder opens anyway, so an error count of zero would
        // not show that it was never opened.
        eprintln!("skipping: running with privileges that ignore chmod 000");
        return;
    }
    let cfg = ExclusionConfig {
        excluded_folders: vec![root.join("locked")],
        ..ExclusionConfig::default()
    };
    assert_scan(&root, &cfg, &want, "excluded_folders");
}

#[test]
fn double_star_wildcard_prunes_node_modules_at_any_depth() {
    let tmp = TempDir::new("doublestar");
    let root = build_fixture(&tmp);
    let truth = ground_truth(&root);
    let want = expected(&truth, |_, name, _| {
        name.eq_ignore_ascii_case("node_modules")
    });
    assert!(!want.contains(Path::new("src/node_modules")));
    assert!(want.contains(Path::new("src/main.rs")));
    let cfg = ExclusionConfig {
        wildcard_patterns: vec!["**/node_modules".to_owned()],
        ..ExclusionConfig::default()
    };
    assert_scan(&root, &cfg, &want, "**/node_modules");
}

#[test]
fn name_wildcard_skips_matching_entries_case_insensitively() {
    let tmp = TempDir::new("namewc");
    let root = build_fixture(&tmp);
    write(&root.join("keep/UPPER.TXT"));
    let truth = ground_truth(&root);
    let want = expected(&truth, |_, name, _| name.to_lowercase().ends_with(".txt"));
    assert!(!want.contains(Path::new("keep/UPPER.TXT")));
    assert!(want.contains(Path::new("keep/b.rs")));
    let cfg = ExclusionConfig {
        wildcard_patterns: vec!["*.txt".to_owned()],
        ..ExclusionConfig::default()
    };
    assert_scan(&root, &cfg, &want, "*.txt");
}

#[test]
fn regex_exclusion_prunes_matching_directories() {
    let tmp = TempDir::new("regex");
    let root = build_fixture(&tmp);
    let truth = ground_truth(&root);
    let want = expected(&truth, |_, name, _| {
        let lower = name.to_lowercase();
        lower
            .strip_prefix("cache")
            .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
    });
    assert!(!want.contains(Path::new("cache123/blob")));
    assert!(want.contains(Path::new("cache/keep.rs")));
    let cfg = ExclusionConfig {
        regex_patterns: vec![r"^cache\d+$".to_owned()],
        ..ExclusionConfig::default()
    };
    assert_scan(&root, &cfg, &want, "regex");
}

#[test]
fn scan_switches_off_materialization_and_records_it() {
    let tmp = TempDir::new("cloud");
    let root = build_fixture(&tmp);
    let result = scan(&root, &opts(2, excl(ExclusionConfig::default()))).expect("scan");
    assert!(
        result.stats.materialization_disabled,
        "stats must record that dataless materialization is off"
    );
    assert!(
        fx_fs_macos::cloud::materialization_disabled().expect("getiopolicy_np"),
        "the policy reads back as off after a scan"
    );
}

/// Gate: "No duplicates from /System/Volumes/Data; 0 cloud
/// downloads". Also reachable as
/// `cargo run --release -p fx-fs-macos --example scan_bench -- / --check-duplicates`.
#[test]
#[ignore = "scans the whole boot volume; run by the owner"]
fn root_scan_has_no_data_volume_paths_and_no_duplicate_inodes() {
    let start = Instant::now();
    let result = scan(Path::new("/"), &opts(0, excl(ExclusionConfig::default()))).expect("scan /");
    let entries = result.entries.as_ref().expect("collect = true");
    assert!(result.stats.materialization_disabled, "0 cloud downloads");
    assert!(
        result.stats.skipped_mount_points + result.stats.skipped_firmlink_aliases >= 1,
        "/System/Volumes/Data must be listed but not entered"
    );

    let data = Path::new("/System/Volumes/Data");
    let mut seen = HashSet::with_capacity(entries.len());
    let mut duplicates = Vec::new();
    for i in 0..entries.len() {
        let path = Path::new("/").join(relative_path(entries, i));
        // The mount point itself may be listed; nothing below it.
        assert!(
            !path.starts_with(data) || path == data,
            "emitted a Data-volume alias: {}",
            path.display()
        );
        // Entries may vanish during the scan; only live ones are compared.
        // Files with several hard links legitimately share an inode, so a
        // repeat counts only for directories and single-link entries.
        if let Ok(md) = fs::symlink_metadata(&path) {
            let single = md.is_dir() || md.nlink() == 1;
            if single && !seen.insert((md.dev(), md.ino())) && duplicates.len() < 20 {
                duplicates.push(path);
            }
        }
    }
    assert!(
        duplicates.is_empty(),
        "duplicate (device, inode): {duplicates:?}"
    );
    eprintln!(
        "scanned / : {} entries, {} errors, {:?} scan, {:?} total",
        entries.len(),
        result.stats.errors,
        result.stats.elapsed,
        start.elapsed()
    );
}
