//! Tests for the counting half of the `cloud_check` example
//! (`examples/cloud_check/count.rs`), which checks the Phase 2 line
//! "iCloud and File Provider folders: indexing triggers 0 downloads".
//!
//! Dataless items can't be made in a test: only a file provider makes them.
//! So the census runs on a plain folder with a stand-in for the
//! `SF_DATALESS` test (a name starting with `cloud-`), and checks that:
//!
//! - it finds every item the test picks out, files and folders, below the
//!   root, and never lists a dataless folder or follows a symlink;
//! - a download is an item dataless before and still there but not after;
//!   an item that went away is not one;
//! - the report is the five `key=value` lines and no path, and exits 1 only
//!   on a download;
//! - with the real flag test (`lstat`'s `st_flags`), a plain folder has no
//!   dataless item.

#![cfg(unix)]

#[path = "../examples/cloud_check/count.rs"]
mod count;

use std::collections::BTreeSet;
use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use count::{Census, Report, census, downloads};

/// A fresh folder under the system temp folder, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "fx-cloud-check-{tag}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        TempDir(dir.canonicalize().unwrap())
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The stand-in for `SF_DATALESS`: a name starting with `cloud-`.
fn named_cloud(path: &Path, _: &Metadata) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("cloud-"))
}

/// root/
///   a.txt
///   cloud-file.txt          dataless file
///   docs/b.txt
///   docs/cloud-doc.pdf      dataless file in a folder
///   cloud-folder/           dataless folder, never listed
///     cloud-inside.txt
///   link -> ../outside      a symlink, never followed
/// outside/cloud-outside.txt
fn tree(tag: &str) -> (TempDir, PathBuf) {
    let tmp = TempDir::new(tag);
    let root = tmp.0.join("root");
    for dir in ["root/docs", "root/cloud-folder", "outside"] {
        fs::create_dir_all(tmp.0.join(dir)).unwrap();
    }
    for file in [
        "root/a.txt",
        "root/cloud-file.txt",
        "root/docs/b.txt",
        "root/docs/cloud-doc.pdf",
        "root/cloud-folder/cloud-inside.txt",
        "outside/cloud-outside.txt",
    ] {
        fs::write(tmp.0.join(file), "x").unwrap();
    }
    std::os::unix::fs::symlink("../outside", root.join("link")).unwrap();
    (tmp, root)
}

fn set(root: &Path, rels: &[&str]) -> BTreeSet<PathBuf> {
    rels.iter().map(|r| root.join(r)).collect()
}

#[test]
fn census_finds_dataless_items_without_entering_them_or_symlinks() {
    let (_tmp, root) = tree("census");
    let seen = Mutex::new(Vec::new());
    let test = |path: &Path, meta: &Metadata| {
        seen.lock().unwrap().push(path.to_path_buf());
        // lstat metadata: the symlink is a symlink, not its target folder.
        if path.ends_with("link") {
            assert!(meta.file_type().is_symlink(), "lstat, not stat");
        }
        named_cloud(path, meta)
    };
    let found = census(&root, &test).unwrap();
    assert_eq!(
        found.dataless,
        set(
            &root,
            &["cloud-file.txt", "docs/cloud-doc.pdf", "cloud-folder"]
        )
    );
    assert_eq!(found.unreadable, 0);
    let seen = seen.into_inner().unwrap();
    for never in ["cloud-folder/cloud-inside.txt", "link/cloud-outside.txt"] {
        assert!(
            !seen.contains(&root.join(never)),
            "{never} is below a dataless folder or a symlink: never listed"
        );
    }
    assert_eq!(seen.len(), 7, "every item above the cut, once: {seen:?}");
}

#[test]
fn census_refuses_a_root_that_is_no_folder() {
    let (tmp, root) = tree("census-root");
    assert!(census(&root.join("a.txt"), &named_cloud).is_err());
    assert!(census(&root.join("link"), &named_cloud).is_err());
    assert!(census(&tmp.0.join("missing"), &named_cloud).is_err());
}

#[test]
fn a_download_is_an_item_that_stopped_being_dataless() {
    let root = Path::new("/r");
    let before = Census {
        dataless: set(root, &["still.txt", "came-down.txt", "gone.txt", "folder"]),
        unreadable: 0,
    };
    let now = |path: &Path| match path.file_name().and_then(|n| n.to_str()) {
        Some("still.txt") => Some(true),
        Some("came-down.txt" | "folder") => Some(false),
        _ => None,
    };
    assert_eq!(downloads(&before, &now), 2);
    assert_eq!(downloads(&Census::default(), &now), 0);
    assert_eq!(downloads(&before, &|_| Some(true)), 0);
    assert_eq!(downloads(&before, &|_| None), 0, "gone is not downloaded");
}

#[test]
fn the_report_is_five_counts_and_no_path() {
    let report = Report {
        dataless_before: 120,
        dataless_after: 120,
        skipped_dataless: 7,
        edeadlk: 2,
        downloads: 0,
    };
    assert_eq!(
        report.lines(),
        "cloud_dataless_before=120\ncloud_dataless_after=120\nskipped_dataless=7\n\
         edeadlk=2\ncloud_downloads=0\n"
    );
    assert_eq!(report.exit_code(), 0);
    let downloaded = Report {
        downloads: 1,
        ..report
    };
    assert_eq!(downloaded.exit_code(), 1);
    for line in downloaded.lines().lines() {
        let (key, value) = line.split_once('=').expect("key=value");
        assert!(key.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'));
        assert!(value.bytes().all(|b| b.is_ascii_digit()), "{line}");
    }
}

/// With the real test, `SF_DATALESS` in `lstat`'s flags, a folder the test
/// made has no dataless item: nothing there is in the cloud.
#[cfg(target_os = "macos")]
#[test]
fn a_plain_folder_has_no_dataless_items() {
    use std::os::macos::fs::MetadataExt;
    let (_tmp, root) = tree("census-real");
    let real = |_: &Path, meta: &Metadata| fx_fs_macos::cloud::is_dataless(meta.st_flags());
    let found = census(&root, &real).unwrap();
    assert_eq!(found, Census::default());
}
