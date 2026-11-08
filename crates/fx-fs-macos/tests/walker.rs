//! Walker correctness tests for `fx_fs_macos::scan` (macOS only).
//!
//! Each test materializes a synthetic tree from `fx-bench` (plus a few
//! hand-made extras) into its own fresh directory under
//! `std::env::temp_dir()`, scans it, and compares the result against a
//! recursive `std::fs::symlink_metadata` / `read_dir` ground truth.
//!
//! Contract pinned here:
//! - The scan root itself is not an entry. `stats` and the collected
//!   entries cover everything strictly below the root; entries directly in
//!   the root have `parent == u32::MAX`.
//! - `relative_path(entries, i)` rebuilds entry `i`'s path relative to the
//!   scan root (without the root's own name).
//! - `ScannedEntry { kind: attrbuf::EntryKind, size: u64 (file data length),
//!   file_id: u64 (== st_ino), flags: u32 (== st_flags), parent: u32, .. }`.
//! - `ScanOptions: Default`; `threads`, `collect` and `buf_size` are set
//!   with struct-update syntax.
//!
//! Tests only ever create and remove their own temporary directory.

#![cfg(target_os = "macos")]

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::fs;
use std::io::Write;
use std::os::macos::fs::MetadataExt as MacMetadataExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fx_bench::{TreeShape, generate_tree, materialize};
use fx_fs_macos::attrbuf::EntryKind;
use fx_fs_macos::scan::{ScanOptions, ScanResult, ScannedEntry, relative_path, scan};

// ---------------------------------------------------------------- helpers

/// Converts any count type (`usize`, `u64`, ...) to `u64`.
fn n<T: TryInto<u64>>(v: T) -> u64
where
    T::Error: Debug,
{
    v.try_into().expect("count fits in u64")
}

/// A fresh, unique temporary directory removed (with permissions restored)
/// on drop.
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
            "fx-walker-{tag}-{}-{nanos}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("create unique temp dir");
        // Canonicalize so /var -> /private/var symlinks don't matter.
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

/// Makes every directory under `dir` accessible again so it can be removed.
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

/// Restores a directory's mode when dropped.
struct ModeGuard {
    path: PathBuf,
    mode: u32,
}

impl Drop for ModeGuard {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(self.mode));
    }
}

fn kind_char(kind: EntryKind) -> char {
    match kind {
        EntryKind::Dir => 'd',
        EntryKind::File => 'f',
        EntryKind::Symlink => 'l',
        EntryKind::Other => 'o',
    }
}

/// What both sides agree on for one entry: kind, size (files only), file
/// ID and BSD flags.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Facts {
    kind: char,
    size: Option<u64>,
    file_id: u64,
    flags: u32,
}

/// Recursive std::fs ground truth: relative path -> facts. Never follows
/// symlinks; unreadable directories are listed but not entered.
fn ground_truth(root: &Path) -> BTreeMap<PathBuf, Facts> {
    fn walk(root: &Path, rel: &Path, out: &mut BTreeMap<PathBuf, Facts>) {
        let Ok(rd) = fs::read_dir(root.join(rel)) else {
            return;
        };
        for e in rd {
            let e = e.expect("read_dir entry");
            let child_rel = rel.join(e.file_name());
            let md = fs::symlink_metadata(root.join(&child_rel)).expect("symlink_metadata");
            let ft = md.file_type();
            let kind = if ft.is_symlink() {
                'l'
            } else if ft.is_dir() {
                'd'
            } else if ft.is_file() {
                'f'
            } else {
                'o'
            };
            out.insert(
                child_rel.clone(),
                Facts {
                    kind,
                    size: (kind == 'f').then_some(md.len()),
                    file_id: md.ino(),
                    flags: md.st_flags(),
                },
            );
            if kind == 'd' {
                walk(root, &child_rel, out);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, Path::new(""), &mut out);
    out
}

/// Scanned entries as relative path -> facts, after checking the parent
/// links are well formed and no path appears twice.
fn scanned_map(entries: &[ScannedEntry]) -> BTreeMap<PathBuf, Facts> {
    let len = entries.len();
    for (i, e) in entries.iter().enumerate() {
        // Parent links point at directories and terminate at the root.
        let mut cur = e.parent;
        let mut steps = 0usize;
        while cur != u32::MAX {
            let p = cur as usize;
            assert!(p < len, "entry {i} has out-of-range parent {p}");
            assert_eq!(
                entries[p].kind,
                EntryKind::Dir,
                "entry {i}'s ancestor {p} is not a directory"
            );
            steps += 1;
            assert!(steps <= len, "entry {i} has a parent cycle");
            cur = entries[p].parent;
        }
    }
    let mut out = BTreeMap::new();
    for (i, e) in entries.iter().enumerate() {
        let path = relative_path(entries, i);
        assert!(
            !path.as_os_str().is_empty(),
            "entry {i} has an empty relative path (the root must not be an entry)"
        );
        let facts = Facts {
            kind: kind_char(e.kind),
            size: (e.kind == EntryKind::File).then_some(e.size),
            file_id: e.file_id,
            flags: e.flags,
        };
        let prev = out.insert(path.clone(), facts);
        assert!(prev.is_none(), "path {} scanned twice", path.display());
    }
    out
}

/// All stats except `elapsed`, as comparable numbers.
fn stat_tuple(r: &ScanResult) -> [u64; 8] {
    let s = &r.stats;
    [
        n(s.entries),
        n(s.dirs),
        n(s.files),
        n(s.symlinks),
        n(s.errors),
        n(s.skipped_mount_points),
        n(s.skipped_dataless),
        n(s.name_bytes),
    ]
}

fn opts(threads: usize, collect: bool) -> ScanOptions {
    ScanOptions {
        threads,
        collect,
        ..ScanOptions::default()
    }
}

fn run(root: &Path, o: &ScanOptions) -> ScanResult {
    scan(root, o).unwrap_or_else(|e| panic!("scan of {} failed: {e}", root.display()))
}

/// Builds the standard fixture: a ~3,000-entry fx-bench tree plus a
/// directory of files with known sizes and one wide directory. Returns the
/// scan root.
fn build_fixture(tmp: &TempDir) -> PathBuf {
    let tree = generate_tree(3_000, 0x5ca9_0003, TreeShape::default());
    let stats = materialize(&tree, &tmp.path, None).expect("materialize tree");
    assert_eq!(stats.entries, 3_000);
    let root = tmp.path.join("root");

    let sized = root.join("zz-sized");
    fs::create_dir(&sized).unwrap();
    for (i, len) in [0usize, 1, 100, 4_096, 5_000, 70_001]
        .into_iter()
        .enumerate()
    {
        let mut f = fs::File::create(sized.join(format!("blob-{i}.bin"))).unwrap();
        f.write_all(&vec![b'x'; len]).unwrap();
    }
    let nested = sized.join("nested").join("deeper");
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("leaf.txt"), b"hello walker").unwrap();

    // A wide directory so one directory needs several getattrlistbulk calls
    // with a small buffer.
    let wide = root.join("zz-wide");
    fs::create_dir(&wide).unwrap();
    for i in 0..1_500 {
        fs::File::create(wide.join(format!("wide-entry-with-a-longish-name-{i:05}.dat"))).unwrap();
    }
    root
}

fn assert_matches_truth(result: &ScanResult, truth: &BTreeMap<PathBuf, Facts>, label: &str) {
    let entries = result
        .entries
        .as_ref()
        .unwrap_or_else(|| panic!("{label}: collect=true must return entries"));
    let got = scanned_map(entries);

    let missing: Vec<_> = truth
        .keys()
        .filter(|k| !got.contains_key(*k))
        .take(5)
        .collect();
    let extra: Vec<_> = got
        .keys()
        .filter(|k| !truth.contains_key(*k))
        .take(5)
        .collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "{label}: path sets differ; missing (first 5) {missing:?}, extra (first 5) {extra:?}"
    );
    for (path, want) in truth {
        assert_eq!(
            &got[path],
            want,
            "{label}: facts differ for {}",
            path.display()
        );
    }

    let count = |k: char| n(truth.values().filter(|f| f.kind == k).count());
    let s = &result.stats;
    assert_eq!(n(s.entries), n(truth.len()), "{label}: stats.entries");
    assert_eq!(n(entries.len()), n(s.entries), "{label}: collected len");
    assert_eq!(n(s.dirs), count('d'), "{label}: stats.dirs");
    assert_eq!(n(s.files), count('f'), "{label}: stats.files");
    assert_eq!(n(s.symlinks), count('l'), "{label}: stats.symlinks");
    let name_bytes: usize = truth
        .keys()
        .map(|p| p.file_name().unwrap().as_encoded_bytes().len())
        .sum();
    assert_eq!(n(s.name_bytes), n(name_bytes), "{label}: stats.name_bytes");
}

// ---------------------------------------------------------------- tests

#[test]
fn scan_matches_std_fs_ground_truth_for_all_thread_counts() {
    let tmp = TempDir::new("truth");
    let root = build_fixture(&tmp);
    let truth = ground_truth(&root);
    assert!(truth.len() > 4_000, "fixture unexpectedly small");

    let mut first: Option<(BTreeMap<PathBuf, Facts>, [u64; 8])> = None;
    for threads in [1usize, 2, 4, 8] {
        let label = format!("threads={threads}");
        let result = run(&root, &opts(threads, true));
        assert_matches_truth(&result, &truth, &label);
        assert_eq!(n(result.stats.errors), 0, "{label}: no errors expected");
        assert_eq!(n(result.stats.skipped_mount_points), 0, "{label}");
        assert_eq!(n(result.stats.skipped_dataless), 0, "{label}");

        let snapshot = (
            scanned_map(result.entries.as_ref().unwrap()),
            stat_tuple(&result),
        );
        match &first {
            None => first = Some(snapshot),
            Some(f) => assert_eq!(f, &snapshot, "{label}: differs from threads=1"),
        }
    }
}

#[test]
fn small_buffer_gives_same_result() {
    let tmp = TempDir::new("smallbuf");
    let root = build_fixture(&tmp);
    let truth = ground_truth(&root);
    for threads in [1usize, 4] {
        let o = ScanOptions {
            threads,
            collect: true,
            buf_size: 4096,
            ..ScanOptions::default()
        };
        let result = run(&root, &o);
        assert_matches_truth(&result, &truth, &format!("buf_size=4096 threads={threads}"));
    }
}

#[test]
fn collect_false_gives_same_stats_as_collect_true() {
    let tmp = TempDir::new("nocollect");
    let root = build_fixture(&tmp);
    for threads in [1usize, 4] {
        let with = run(&root, &opts(threads, true));
        let without = run(&root, &opts(threads, false));
        assert!(with.entries.is_some());
        assert!(
            without.entries.is_none(),
            "collect=false must not return entries"
        );
        assert_eq!(
            stat_tuple(&with),
            stat_tuple(&without),
            "threads={threads}: stats differ between collect=true and collect=false"
        );
        assert!(n(without.stats.entries) > 4_000);
    }
}

#[test]
fn symlinks_are_reported_and_never_followed() {
    let tmp = TempDir::new("symlink");
    let root = tmp.path.join("scanroot");
    fs::create_dir(&root).unwrap();
    let target = root.join("target");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("inside.txt"), b"data").unwrap();
    fs::create_dir(target.join("sub")).unwrap();
    fs::write(target.join("sub").join("deep.txt"), b"x").unwrap();

    // A symlink to a directory inside the root, one to a directory outside
    // it, a dangling one and a self-referential loop.
    std::os::unix::fs::symlink(&target, root.join("link-to-target")).unwrap();
    let outside = tmp.path.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("must-not-appear.txt"), b"x").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("link-outside")).unwrap();
    std::os::unix::fs::symlink(root.join("nowhere"), root.join("dangling")).unwrap();
    std::os::unix::fs::symlink(root.join("loop"), root.join("loop")).unwrap();

    let truth = ground_truth(&root);
    for threads in [1usize, 2, 4, 8] {
        let label = format!("threads={threads}");
        let result = run(&root, &opts(threads, true));
        assert_matches_truth(&result, &truth, &label);
        let got = scanned_map(result.entries.as_ref().unwrap());
        for link in ["link-to-target", "link-outside", "dangling", "loop"] {
            assert_eq!(
                got.get(Path::new(link)).map(|f| f.kind),
                Some('l'),
                "{label}: {link} must be reported as a symlink"
            );
        }
        assert!(
            got.keys().all(|p| {
                !(p.starts_with("link-to-target") && p != Path::new("link-to-target"))
                    && !(p.starts_with("link-outside") && p != Path::new("link-outside"))
            }),
            "{label}: a symlink was followed"
        );
        assert!(
            got.keys()
                .all(|p| p.file_name().unwrap() != "must-not-appear.txt"),
            "{label}: walked outside the root through a symlink"
        );
        assert_eq!(n(result.stats.symlinks), 4, "{label}");
        assert_eq!(n(result.stats.entries), 8, "{label}");
        assert_eq!(n(result.stats.errors), 0, "{label}");
    }
}

#[test]
fn unreadable_subdirectory_counts_an_error_and_scan_continues() {
    let tmp = TempDir::new("eacces");
    let root = build_fixture(&tmp);
    let locked = root.join("zz-locked");
    fs::create_dir(&locked).unwrap();
    fs::write(locked.join("secret.txt"), b"x").unwrap();
    fs::create_dir(locked.join("hidden-dir")).unwrap();

    // Ground truth before locking, minus the locked directory's contents.
    let mut truth = ground_truth(&root);
    truth.retain(|p, _| !(p.starts_with("zz-locked") && p != Path::new("zz-locked")));
    assert!(truth.contains_key(Path::new("zz-locked")));

    let _guard = ModeGuard {
        path: locked.clone(),
        mode: 0o755,
    };
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read_dir(&locked).is_ok() {
        eprintln!("skipping: running with privileges that ignore chmod 000");
        return;
    }

    for threads in [1usize, 2, 4, 8] {
        let label = format!("threads={threads} with unreadable dir");
        let result = run(&root, &opts(threads, true));
        assert!(
            n(result.stats.errors) >= 1,
            "{label}: the unreadable directory must be counted as an error"
        );
        assert_matches_truth(&result, &truth, &label);

        let without = run(&root, &opts(threads, false));
        assert_eq!(stat_tuple(&result), stat_tuple(&without), "{label}");
    }
}

#[test]
fn nonexistent_root_is_an_error() {
    let tmp = TempDir::new("noroot");
    let missing = tmp.path.join("does-not-exist");
    for threads in [1usize, 4] {
        assert!(
            scan(&missing, &opts(threads, true)).is_err(),
            "threads={threads}: scanning a nonexistent root must return Err"
        );
        assert!(scan(&missing, &opts(threads, false)).is_err());
    }
}

#[test]
fn empty_root_scans_to_nothing() {
    let tmp = TempDir::new("empty");
    for threads in [1usize, 8] {
        let result = run(&tmp.path, &opts(threads, true));
        assert_eq!(n(result.stats.entries), 0);
        assert_eq!(n(result.stats.errors), 0);
        assert_eq!(result.entries.as_ref().map(Vec::len), Some(0));
    }
}

#[test]
fn scan_never_modifies_the_tree() {
    let tmp = TempDir::new("readonly");
    let root = build_fixture(&tmp);
    let before = ground_truth(&root);
    for threads in [1usize, 4] {
        run(&root, &opts(threads, true));
    }
    assert_eq!(before, ground_truth(&root), "the scan changed the tree");
}

#[test]
fn mount_points_below_the_root_are_not_descended() {
    // On APFS volume-group systems /System/Volumes/Data is a mount point;
    // scanning /System/Volumes must list it but not walk into it.
    let volumes = Path::new("/System/Volumes");
    let data = volumes.join("Data");
    // Behavioural check: the Data volume can report the same device ID as
    // its parent, so the walker must not rely on st_dev alone. Only skip
    // when the volume-group layout is absent altogether.
    if !volumes.is_dir() {
        eprintln!("skipping: no /System/Volumes on this host");
        return;
    }
    assert!(
        fs::symlink_metadata(&data).is_ok_and(|m| m.is_dir()),
        "/System/Volumes exists but /System/Volumes/Data is not a directory"
    );
    let result = run(volumes, &opts(4, true));
    assert!(
        n(result.stats.skipped_mount_points) >= 1,
        "the Data volume mount point must be counted as skipped"
    );
    let got = scanned_map(result.entries.as_ref().unwrap());
    assert_eq!(
        got.get(Path::new("Data")).map(|f| f.kind),
        Some('d'),
        "the mount point itself is still reported as a directory"
    );
    assert!(
        got.keys()
            .all(|p| !(p.starts_with("Data") && p != Path::new("Data"))),
        "the walker descended into the /System/Volumes/Data mount point"
    );
}

// ------------------------------------------------------- policy + enumerator

/// `UF_HIDDEN` from `sys/stat.h`.
const UF_HIDDEN: u32 = 0x0000_8000;

/// Paths (relative to `root`) of every entry, via `relative_path`.
fn rel_paths(result: &ScanResult) -> std::collections::BTreeSet<PathBuf> {
    let entries = result.entries.as_ref().expect("collect = true");
    (0..entries.len())
        .map(|i| relative_path(entries, i))
        .collect()
}

/// Adds hidden, node_modules and dot entries to a fresh fixture.
fn build_policy_fixture(tmp: &TempDir) -> PathBuf {
    let root = build_fixture(tmp);
    for p in [
        ".hidden_dir/inner/file.rs",
        "uf_dir/child.rs",
        "src/node_modules/dep/x.js",
        "src/main.rs",
        "a/b/c/deep.rs",
        "a/other.rs",
    ] {
        let path = root.join(p);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"x").unwrap();
    }
    let c =
        std::ffi::CString::new(root.join("uf_dir").into_os_string().into_encoded_bytes()).unwrap();
    // SAFETY: valid NUL-terminated path inside the test's own temp dir.
    assert_eq!(unsafe { libc::chflags(c.as_ptr(), UF_HIDDEN) }, 0);
    root
}

fn with_exclusions(threads: usize, cfg: fx_fs::ExclusionConfig) -> ScanOptions {
    ScanOptions {
        threads,
        collect: true,
        exclusions: fx_fs::Exclusions::new(cfg).expect("valid config"),
        ..ScanOptions::default()
    }
}

#[test]
fn exclusions_prune_subtrees_and_match_ground_truth() {
    let tmp = TempDir::new("excl");
    let root = build_policy_fixture(&tmp);
    let truth = ground_truth(&root);
    let pruned = |rel: &Path| {
        let mut prefix = PathBuf::new();
        rel.iter().any(|c| {
            prefix.push(c);
            let flags = fs::symlink_metadata(root.join(&prefix)).unwrap().st_flags();
            let name = c.to_string_lossy();
            name.starts_with('.') || flags & UF_HIDDEN != 0 || name == "node_modules"
        })
    };
    let want: std::collections::BTreeSet<PathBuf> =
        truth.keys().filter(|p| !pruned(p)).cloned().collect();
    assert!(want.len() < truth.len());
    let cfg = fx_fs::ExclusionConfig {
        exclude_hidden: true,
        wildcard_patterns: vec!["**/node_modules".to_owned()],
        ..fx_fs::ExclusionConfig::default()
    };
    for threads in [1, 4] {
        let result = run(&root, &with_exclusions(threads, cfg.clone()));
        assert_eq!(rel_paths(&result), want, "threads={threads}");
        assert_eq!(n(result.stats.entries), n(want.len()));
        assert_eq!(n(result.stats.errors), 0);
        assert!(result.stats.materialization_disabled);
    }
}

#[test]
fn excluded_unreadable_folder_is_never_opened() {
    let tmp = TempDir::new("exclfolder");
    let root = build_policy_fixture(&tmp);
    let locked = root.join("a");
    let _guard = ModeGuard {
        path: locked.clone(),
        mode: 0o755,
    };
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read_dir(&locked).is_ok() {
        // As root the folder opens anyway, so an error count of zero would
        // not show that it was never opened.
        eprintln!("skipping: running with privileges that ignore chmod 000");
        return;
    }
    let cfg = fx_fs::ExclusionConfig {
        excluded_folders: vec![locked],
        ..fx_fs::ExclusionConfig::default()
    };
    let result = run(&root, &with_exclusions(2, cfg));
    assert_eq!(n(result.stats.errors), 0, "the excluded folder was opened");
    assert_eq!(n(result.stats.skipped_permission_denied), 0);
    assert!(!rel_paths(&result).iter().any(|p| p.starts_with("a")));
}

#[test]
fn mac_scanner_emits_parent_first_with_metadata_and_include_only() {
    use fx_fs::VolumeEnumerator;
    use fx_fs_macos::enumerator::MacScanner;
    use fx_model::{EntryFlags, FileKey};
    use std::collections::HashSet;

    let tmp = TempDir::new("enum");
    let root = build_policy_fixture(&tmp);
    let truth = ground_truth(&root);
    let root_ino = fs::metadata(&root).unwrap().ino();
    for threads in [1, 4] {
        let scanner = MacScanner::new(with_exclusions(threads, Default::default()));
        let mut seen: HashSet<FileKey> = HashSet::new();
        let mut count = 0usize;
        scanner
            .enumerate(&root, &mut |e| {
                let p = e.parent.expect("every entry has a parent key");
                assert!(seen.contains(&p) || p.file_id == root_ino, "order: {e:?}");
                assert!(seen.insert(e.key), "duplicate {e:?}");
                count += 1;
                if e.name == "uf_dir" || e.name.starts_with('.') {
                    assert!(e.flags.contains(EntryFlags::HIDDEN), "{e:?}");
                }
                if e.name == "deep.rs" {
                    let md = fs::metadata(root.join("a/b/c/deep.rs")).unwrap();
                    assert_eq!(e.created, md.st_birthtime());
                    assert_eq!(e.modified, md.mtime());
                    assert_eq!(e.size, 1);
                }
            })
            .unwrap();
        assert_eq!(count, truth.len(), "threads={threads}");

        let cfg = fx_fs::ExclusionConfig {
            include_only: vec![root.join("a/b")],
            ..fx_fs::ExclusionConfig::default()
        };
        let mut names = Vec::new();
        MacScanner::new(with_exclusions(threads, cfg))
            .enumerate(&root, &mut |e| names.push(e.name))
            .unwrap();
        names.sort();
        assert_eq!(names, ["b", "c", "deep.rs"], "threads={threads}");
    }
}
