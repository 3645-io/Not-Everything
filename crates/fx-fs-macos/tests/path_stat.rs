//! `fx_fs::PathStat` for macOS (macOS only).
//!
//! Contract pinned here:
//! - `fx_fs_macos::path_stat::MacPathStat::new()` implements
//!   `fx_fs::PathStat` with lstat semantics (symlinks are reported, never
//!   followed) and never materializes cloud files.
//! - A stat'ed entry carries the same key, parent key, name, kind and size
//!   that `MacScanner` reports for the same path.
//! - A missing path, or a path whose parent vanished, is `Ok(None)`.
//! - The name is the one stored on disk, whatever spelling the path used:
//!   on a case-insensitive volume the old spelling of a case-only rename
//!   still finds the entry, and must not bring the old name back. The
//!   root, mount points, firmlinks and hard links keep the path's name.

#![cfg(target_os = "macos")]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fx_fs::{PathStat, VolumeEnumerator};
use fx_fs_macos::enumerator::MacScanner;
use fx_fs_macos::path_stat::MacPathStat;
use fx_fs_macos::scan::ScanOptions;
use fx_model::{Entry, EntryKind};

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(tag: &str) -> TempRoot {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let p = std::env::temp_dir().join(format!("fx-pathstat-{tag}-{nanos}"));
        fs::create_dir_all(&p).expect("create temp root");
        TempRoot(fs::canonicalize(&p).expect("canonicalize"))
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn stat(p: &Path) -> Option<Entry> {
    MacPathStat::new().stat(p).expect("stat")
}

#[test]
fn file_dir_and_symlink_kinds() {
    let root = TempRoot::new("kinds");
    let file = root.0.join("a.txt");
    fs::write(&file, b"hello").expect("write");
    let dir = root.0.join("sub");
    fs::create_dir(&dir).expect("mkdir");
    let link = root.0.join("link");
    std::os::unix::fs::symlink(&dir, &link).expect("symlink");

    let f = stat(&file).expect("file exists");
    assert_eq!(f.kind, EntryKind::File);
    assert_eq!(f.name, "a.txt");
    assert_eq!(f.size, 5);

    let d = stat(&dir).expect("dir exists");
    assert_eq!(d.kind, EntryKind::Dir);
    assert_eq!(d.name, "sub");

    let l = stat(&link).expect("link exists");
    assert_eq!(l.kind, EntryKind::Symlink, "symlink is not followed");
    assert_ne!(l.key, d.key, "link has its own key, not its target's");

    assert_eq!(f.parent, Some(stat(&root.0).expect("root").key));
    assert_eq!(d.parent, f.parent);
}

#[test]
fn dangling_symlink_is_reported() {
    let root = TempRoot::new("dangling");
    let link = root.0.join("dangling");
    std::os::unix::fs::symlink(root.0.join("nowhere"), &link).expect("symlink");
    assert_eq!(stat(&link).expect("lstat sees it").kind, EntryKind::Symlink);
}

#[test]
fn missing_paths_are_none() {
    let root = TempRoot::new("missing");
    assert_eq!(stat(&root.0.join("nope")), None);
    assert_eq!(stat(&root.0.join("gone/child.txt")), None);
}

#[test]
fn matches_what_the_scanner_reports() {
    let root = TempRoot::new("scanner");
    fs::create_dir_all(root.0.join("d/e")).expect("mkdir");
    fs::write(root.0.join("d/e/f.bin"), vec![0u8; 1234]).expect("write");
    fs::write(root.0.join("top.txt"), b"t").expect("write");

    let mut scanned = Vec::new();
    MacScanner::new(ScanOptions::default())
        .enumerate(&root.0, &mut |e| scanned.push(e))
        .expect("enumerate");

    for rel in ["d", "d/e", "d/e/f.bin", "top.txt"] {
        let path = root.0.join(rel);
        let got = stat(&path).unwrap_or_else(|| panic!("{rel} exists"));
        let want = scanned
            .iter()
            .find(|e| e.key == got.key)
            .unwrap_or_else(|| panic!("scanner reported no entry with {rel}'s key"));
        assert_eq!(got.parent, want.parent, "{rel}: parent");
        assert_eq!(got.name, want.name, "{rel}: name");
        assert_eq!(got.kind, want.kind, "{rel}: kind");
        assert_eq!(got.size, want.size, "{rel}: size");
        assert_eq!(got.modified, want.modified, "{rel}: modified");
    }
}

/// Whether the spelling `other` finds the entry at `path` (a case-insensitive
/// volume).
fn folds_case(path: &Path, other: &Path) -> bool {
    fs::symlink_metadata(path).is_ok() && fs::symlink_metadata(other).is_ok()
}

#[test]
fn an_old_spelling_reports_the_name_stored_on_disk() {
    let root = TempRoot::new("case");
    let renames = [
        ("Report.txt", "REPORT.TXT"),
        ("Docs", "docs"),
        ("Link", "LINK"),
        ("\u{e9}t\u{e9}.txt", "\u{c9}T\u{c9}.TXT"),
    ];
    fs::write(root.0.join(renames[0].0), b"r").expect("write");
    fs::create_dir(root.0.join(renames[1].0)).expect("mkdir");
    std::os::unix::fs::symlink("nowhere", root.0.join(renames[2].0)).expect("symlink");
    fs::write(root.0.join(renames[3].0), b"e").expect("write");
    if !folds_case(&root.0.join(renames[0].0), &root.0.join(renames[0].1)) {
        // A case-sensitive volume: the two spellings are two names.
        return;
    }
    for (old, new) in renames {
        fs::rename(root.0.join(old), root.0.join(new)).expect("rename");
    }

    let mut scanned = Vec::new();
    MacScanner::new(ScanOptions::default())
        .enumerate(&root.0, &mut |e| scanned.push(e))
        .expect("enumerate");
    for (old, new) in renames {
        let got = stat(&root.0.join(old)).unwrap_or_else(|| panic!("{old} still finds {new}"));
        assert_eq!(got.name, new, "{old}: the stored name");
        assert_eq!(got, stat(&root.0.join(new)).expect("new spelling"), "{old}");
        let listed = scanned
            .iter()
            .find(|e| e.key == got.key)
            .unwrap_or_else(|| panic!("scanner reported no entry with {new}'s key"));
        assert_eq!(got.name, listed.name, "{old}: the scanner's name");
    }
}

#[test]
fn hard_links_keep_the_name_the_path_gives() {
    let root = TempRoot::new("links");
    fs::create_dir(root.0.join("sub")).expect("mkdir");
    let first = root.0.join("first.txt");
    fs::write(&first, b"x").expect("write");
    fs::hard_link(&first, root.0.join("second.txt")).expect("link");
    fs::hard_link(&first, root.0.join("sub/Third.txt")).expect("link");

    let key = stat(&first).expect("first").key;
    for (rel, name) in [
        ("first.txt", "first.txt"),
        ("second.txt", "second.txt"),
        ("sub/Third.txt", "Third.txt"),
    ] {
        let got = stat(&root.0.join(rel)).unwrap_or_else(|| panic!("{rel} exists"));
        assert_eq!(got.key, key, "{rel}: one file");
        assert_eq!(got.name, name, "{rel}: the link's own name");
    }
}

#[test]
fn the_root_mount_points_and_firmlinks_keep_their_folder_names() {
    // `getattrlist` names a volume's root after the volume ("Macintosh HD"
    // for `/`, "Macintosh HD - Data" for the Data volume): the index needs
    // the folder's name, as its parent lists it.
    assert_eq!(stat(Path::new("/")).expect("/").name, "/");
    for (path, name) in [("/System/Volumes/Data", "Data"), ("/Users", "Users")] {
        if let Some(got) = stat(Path::new(path)) {
            assert_eq!(got.name, name, "{path}");
        }
    }
}
