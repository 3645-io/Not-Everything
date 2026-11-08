//! `fx_fs::PathStat` for macOS (macOS only).
//!
//! Contract pinned here:
//! - `fx_fs_macos::path_stat::MacPathStat::new()` implements
//!   `fx_fs::PathStat` with lstat semantics (symlinks are reported, never
//!   followed) and never materializes cloud files.
//! - A stat'ed entry carries the same key, parent key, name, kind and size
//!   that `MacScanner` reports for the same path.
//! - A missing path, or a path whose parent vanished, is `Ok(None)`.

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
