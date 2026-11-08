//! `fx_fs_macos::enumerator::MacScanner` as an `fx_fs::VolumeEnumerator`
//! (macOS only).
//!
//! Contract pinned here:
//! - `MacScanner::new(ScanOptions)` builds a scanner; it implements
//!   `fx_fs::VolumeEnumerator` and can be used as `&dyn VolumeEnumerator`.
//! - Every entry strictly below the root is passed to the sink exactly once
//!   as an `fx_model::Entry`: `key.file_id == st_ino`, `name` is the last
//!   path component, `kind` matches `lstat`, `size == st_size` for files,
//!   `modified == st_mtime` and `created == st_birthtime` (creation time, as
//!   `fx_model::Entry::created` documents).
//! - Flags: `HIDDEN` iff the name starts with '.' or `UF_HIDDEN` is set;
//!   ordinary temp entries are neither `SYSTEM` nor `DATALESS`.
//! - Parent before child: every `parent` key was already emitted, except
//!   for the top of the emitted tree. Entries directly in the root may have
//!   `parent == None` or the root's own key; the root itself may be emitted
//!   first (with `parent == None`) or not at all.
//! - Exclusions from `ScanOptions::exclusions` apply; include-only roots
//!   emit only the root folder and its contents.
//!
//! Tests only create and remove their own temporary directories.

#![cfg(target_os = "macos")]

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ffi::CString;
use std::fs;
use std::os::macos::fs::MetadataExt as MacMetadataExt;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fx_bench::{TreeShape, generate_tree, materialize};
use fx_fs::{ExclusionConfig, Exclusions, VolumeEnumerator};
use fx_fs_macos::enumerator::MacScanner;
use fx_fs_macos::scan::ScanOptions;
use fx_model::{Entry, EntryFlags, EntryKind, FileKey};

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
            "fx-enum-{tag}-{}-{nanos}-{}",
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
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn write(path: &Path) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent dirs");
    }
    fs::write(path, b"hello").expect("write file");
}

fn build_fixture(tmp: &TempDir) -> PathBuf {
    let root = tmp.path.join("root");
    fs::create_dir(&root).expect("create root");
    let tree = generate_tree(2_500, 0x5ca9_0117, TreeShape::default());
    fs::create_dir(root.join("gen")).expect("create gen");
    materialize(&tree, &root.join("gen"), None).expect("materialize tree");
    write(&root.join(".dotdir/inner.rs"));
    write(&root.join("plain/uf_hidden.rs"));
    let c = CString::new(root.join("plain/uf_hidden.rs").as_os_str().as_bytes()).expect("path");
    // SAFETY: valid NUL-terminated path inside the test's own temp dir.
    let rc = unsafe { libc::chflags(c.as_ptr(), UF_HIDDEN) };
    assert_eq!(rc, 0, "chflags(UF_HIDDEN)");
    write(&root.join("a/b/c/deep.rs"));
    write(&root.join("a/b/sibling.rs"));
    write(&root.join("a/other/outside.rs"));
    write(&root.join("node_modules/pkg/index.js"));
    std::os::unix::fs::symlink("a/b", root.join("link_to_b")).expect("symlink");
    root
}

/// What `lstat` says about one entry.
#[derive(Debug, Clone)]
struct Truth {
    rel: PathBuf,
    name: String,
    kind: EntryKind,
    size: u64,
    mtime: i64,
    birthtime: i64,
    flags: u32,
    parent_ino: u64,
}

/// Inode -> facts for everything strictly below `root`.
fn ground_truth(root: &Path) -> BTreeMap<u64, Truth> {
    fn walk(root: &Path, rel: &Path, parent_ino: u64, out: &mut BTreeMap<u64, Truth>) {
        for e in fs::read_dir(root.join(rel)).expect("read_dir").flatten() {
            let child = rel.join(e.file_name());
            let md = fs::symlink_metadata(root.join(&child)).expect("lstat");
            let ft = md.file_type();
            let kind = if ft.is_dir() {
                EntryKind::Dir
            } else if ft.is_symlink() {
                EntryKind::Symlink
            } else {
                EntryKind::File
            };
            let prev = out.insert(
                md.ino(),
                Truth {
                    rel: child.clone(),
                    name: e.file_name().to_string_lossy().into_owned(),
                    kind,
                    size: md.size(),
                    mtime: md.mtime(),
                    birthtime: md.st_birthtime(),
                    flags: md.st_flags(),
                    parent_ino,
                },
            );
            assert!(prev.is_none(), "fixture has no hard links");
            if ft.is_dir() {
                walk(root, &child, md.ino(), out);
            }
        }
    }
    let root_ino = fs::metadata(root).expect("stat root").ino();
    let mut out = BTreeMap::new();
    walk(root, Path::new(""), root_ino, &mut out);
    out
}

fn scanner(threads: usize, cfg: ExclusionConfig) -> MacScanner {
    MacScanner::new(ScanOptions {
        threads,
        exclusions: Exclusions::new(cfg).expect("valid exclusion config"),
        ..ScanOptions::default()
    })
}

fn enumerate(e: &dyn VolumeEnumerator, root: &Path) -> Vec<Entry> {
    let mut out = Vec::new();
    e.enumerate(root, &mut |entry| out.push(entry))
        .expect("enumerate root");
    out
}

/// Checks uniqueness and parent-before-child order, dropping an emitted
/// root entry (if any). Returns the entries strictly below the root.
fn check_order(entries: Vec<Entry>, root: &Path, label: &str) -> Vec<Entry> {
    let root_ino = fs::metadata(root).expect("stat root").ino();
    let mut seen: HashSet<FileKey> = HashSet::new();
    let mut below = Vec::new();
    for (i, e) in entries.into_iter().enumerate() {
        assert!(seen.insert(e.key), "{label}: key emitted twice: {e:?}");
        if e.key.file_id == root_ino {
            assert_eq!(i, 0, "{label}: the root, if emitted, comes first");
            assert_eq!(e.parent, None, "{label}: the root has no parent");
            continue;
        }
        if let Some(p) = e.parent {
            assert!(
                seen.contains(&p) || p.file_id == root_ino,
                "{label}: parent of {e:?} was not emitted before it"
            );
            assert_ne!(p, e.key, "{label}: entry is its own parent");
        }
        below.push(e);
    }
    below
}

// ------------------------------------------------------------------ tests

#[test]
fn emits_every_entry_once_with_its_metadata() {
    let tmp = TempDir::new("all");
    let root = build_fixture(&tmp);
    let truth = ground_truth(&root);
    let root_ino = fs::metadata(&root).expect("stat root").ino();

    for threads in [1, 4] {
        let label = format!("threads={threads}");
        let s = scanner(threads, ExclusionConfig::default());
        let below = check_order(enumerate(&s, &root), &root, &label);
        assert_eq!(below.len(), truth.len(), "{label}: entry count");

        let volume = below[0].key.volume;
        for e in &below {
            let t = truth
                .get(&e.key.file_id)
                .unwrap_or_else(|| panic!("{label}: unknown inode {e:?}"));
            let rel = t.rel.display();
            assert_eq!(e.key.volume, volume, "{label} {rel}: one volume id");
            assert_eq!(e.name, t.name, "{label} {rel}: name");
            assert_eq!(e.kind, t.kind, "{label} {rel}: kind");
            if t.kind == EntryKind::File {
                assert_eq!(e.size, t.size, "{label} {rel}: size");
            }
            assert_eq!(e.modified, t.mtime, "{label} {rel}: modified");
            assert_eq!(e.created, t.birthtime, "{label} {rel}: created");
            match e.parent {
                Some(p) => assert_eq!(p.file_id, t.parent_ino, "{label} {rel}: parent"),
                None => assert_eq!(
                    t.parent_ino, root_ino,
                    "{label} {rel}: only root children may lack a parent"
                ),
            }
            let hidden = t.name.starts_with('.') || t.flags & UF_HIDDEN != 0;
            assert_eq!(
                e.flags.contains(EntryFlags::HIDDEN),
                hidden,
                "{label} {rel}: HIDDEN flag"
            );
            assert!(
                !e.flags.contains(EntryFlags::SYSTEM),
                "{label} {rel}: SYSTEM"
            );
            assert!(
                !e.flags.contains(EntryFlags::DATALESS),
                "{label} {rel}: DATALESS"
            );
        }
        let link = below
            .iter()
            .find(|e| e.name == "link_to_b")
            .expect("symlink emitted");
        assert_eq!(link.kind, EntryKind::Symlink, "{label}: symlink kind");
    }
}

#[test]
fn applies_exclusions_before_descending() {
    let tmp = TempDir::new("excl");
    let root = build_fixture(&tmp);
    let truth = ground_truth(&root);
    let cfg = ExclusionConfig {
        exclude_hidden: true,
        wildcard_patterns: vec!["**/node_modules".to_owned()],
        ..ExclusionConfig::default()
    };
    let want: BTreeSet<u64> = truth
        .iter()
        .filter(|(_, t)| {
            let mut prefix = PathBuf::new();
            t.rel.iter().all(|comp| {
                prefix.push(comp);
                let name = comp.to_string_lossy();
                let flags = fs::symlink_metadata(root.join(&prefix))
                    .expect("lstat")
                    .st_flags();
                !(name.starts_with('.')
                    || flags & UF_HIDDEN != 0
                    || name.eq_ignore_ascii_case("node_modules"))
            })
        })
        .map(|(ino, _)| *ino)
        .collect();
    assert!(want.len() < truth.len(), "the fixture has excluded entries");

    for threads in [1, 4] {
        let label = format!("threads={threads}");
        let below = check_order(
            enumerate(&scanner(threads, cfg.clone()), &root),
            &root,
            &label,
        );
        let got: BTreeSet<u64> = below.iter().map(|e| e.key.file_id).collect();
        assert_eq!(got, want, "{label}: emitted inodes");
    }
}

#[test]
fn include_only_emits_the_root_folder_and_its_contents() {
    let tmp = TempDir::new("incl");
    let root = build_fixture(&tmp);
    let truth = ground_truth(&root);
    let want: BTreeSet<u64> = truth
        .iter()
        .filter(|(_, t)| t.rel.starts_with("a/b"))
        .map(|(ino, _)| *ino)
        .collect();
    assert_eq!(want.len(), 4, "a/b, a/b/c, a/b/c/deep.rs, a/b/sibling.rs");
    let cfg = ExclusionConfig {
        include_only: vec![root.join("a/b")],
        ..ExclusionConfig::default()
    };
    for threads in [1, 4] {
        let mut got = BTreeSet::new();
        let mut seen = HashSet::new();
        scanner(threads, cfg.clone())
            .enumerate(&root, &mut |e| {
                assert!(seen.insert(e.key), "key emitted twice: {e:?}");
                got.insert(e.key.file_id);
            })
            .expect("enumerate");
        assert_eq!(got, want, "threads={threads}: include-only entries");
    }
}

#[test]
fn missing_root_is_an_error() {
    let tmp = TempDir::new("missing");
    let s = scanner(1, ExclusionConfig::default());
    let mut count = 0usize;
    let result = s.enumerate(&tmp.path.join("does-not-exist"), &mut |_| count += 1);
    assert!(result.is_err(), "a missing root must be an error");
    assert_eq!(count, 0);
}

#[test]
fn enumerating_never_modifies_the_tree() {
    let tmp = TempDir::new("readonly");
    let root = build_fixture(&tmp);
    let before = ground_truth(&root);
    let s = scanner(4, ExclusionConfig::default());
    let _ = enumerate(&s, &root);
    let after = ground_truth(&root);
    let key = |m: &BTreeMap<u64, Truth>| -> Vec<(u64, PathBuf, u64, i64, u32)> {
        m.iter()
            .map(|(ino, t)| (*ino, t.rel.clone(), t.size, t.mtime, t.flags))
            .collect()
    };
    assert_eq!(key(&before), key(&after));
}
