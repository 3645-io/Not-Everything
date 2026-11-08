//! A scan of `/` enters firmlinked folders (macOS only).
//!
//! `/Users`, `/Applications`, `/Library`, `/private` and the other
//! firmlinks in `/usr/share/firmlinks` lead from the system volume into the
//! Data volume. Listing `/` reports a firmlink's own file ID, while opening
//! it gives the Data volume directory's inode, and both volumes can report
//! the same `st_dev`. The walker's "directory was replaced between listing
//! and opening" check must therefore not apply to firmlinks, or a scan of
//! `/` indexes nothing below them (a release test once found exactly that:
//! a scan of `/` emitted about 514k entries, all under `/System` and
//! `/usr`).
//!
//! Tests only create and remove their own temporary directories.

#![cfg(target_os = "macos")]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fx_fs::{ExclusionConfig, Exclusions};
use fx_fs_macos::scan::{ScanOptions, relative_path, reopened_dir_matches, scan};

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
            "fx-firmlink-{tag}-{}-{nanos}-{}",
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

/// Scans `/` but walks only down to `include`, and returns the emitted
/// paths. The ancestors of `include` are traversed, not emitted, so the
/// paths start with `include`'s own name.
fn scan_root_towards(include: &Path, threads: usize) -> Vec<PathBuf> {
    let exclusions = Exclusions::new(ExclusionConfig {
        include_only: vec![include.to_path_buf()],
        ..ExclusionConfig::default()
    })
    .expect("valid include-only config");
    let opts = ScanOptions {
        threads,
        collect: true,
        exclusions,
        ..ScanOptions::default()
    };
    let result = scan(Path::new("/"), &opts).expect("scan of / succeeds");
    let entries = result.entries.expect("collected entries");
    (0..entries.len())
        .map(|i| relative_path(&entries, i))
        .collect()
}

#[test]
fn scan_of_root_enters_the_private_firmlink() {
    let tmp = TempDir::new("private");
    // The default temp dir is under /private/var/folders, reached from /
    // through the /private firmlink. A custom TMPDIR elsewhere tests no
    // firmlink, so the test has nothing to check there.
    if !tmp.path.starts_with("/private/") {
        eprintln!(
            "skipped: the temp dir {} is not under /private (custom TMPDIR?)",
            tmp.path.display()
        );
        return;
    }
    fs::create_dir(tmp.path.join("sub")).unwrap();
    fs::write(tmp.path.join("sub/found-me.txt"), b"x").unwrap();

    let top = PathBuf::from(tmp.path.file_name().unwrap());
    for threads in [1, 4] {
        let paths = scan_root_towards(&tmp.path, threads);
        for want in [top.clone(), top.join("sub"), top.join("sub/found-me.txt")] {
            assert!(
                paths.contains(&want),
                "threads={threads}: a scan of / did not reach {} ({} paths emitted: {:?})",
                want.display(),
                paths.len(),
                paths.iter().take(10).collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn scan_of_root_enters_every_firmlinked_folder_that_has_entries() {
    // /Applications always holds at least one entry on macOS (Safari.app
    // lives in /Applications on every supported release).
    let apps = Path::new("/Applications");
    let Ok(mut listing) = fs::read_dir(apps) else {
        return;
    };
    let Some(Ok(first)) = listing.next() else {
        return;
    };
    let child = first.path();
    let paths = scan_root_towards(&child, 2);
    let want = PathBuf::from(child.file_name().unwrap());
    assert!(
        paths.contains(&want),
        "a scan of / did not reach {} through the /Applications firmlink",
        child.display()
    );
}

#[test]
fn reopen_check_accepts_firmlinks_and_volume_crossings_only() {
    // Listed ID 0: nothing to compare (the root).
    assert!(reopened_dir_matches(0, 1, 1, 42, false));
    // Same device, same inode: the directory listed.
    assert!(reopened_dir_matches(42, 1, 1, 42, false));
    // Same device, another inode, not a firmlink: replaced after listing.
    assert!(!reopened_dir_matches(42, 1, 1, 43, false));
    // A firmlink: the listing's ID is the firmlink's, the open gives the
    // Data volume inode, and the device may be the same.
    assert!(reopened_dir_matches(
        1_152_921_500_312_607_302,
        16_777_233,
        16_777_233,
        3717,
        true
    ));
    // Another device: IDs from different volumes can't be compared.
    assert!(reopened_dir_matches(42, 1, 2, 43, false));
}
