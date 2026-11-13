//! `day_replay_bench`'s comparison of the index with a fresh scan
//! (`examples/day_replay_compare/mod.rs`), on a temporary folder with the
//! real engine, scanner and FSEvents source (macOS only).
//!
//! On the whole disk it first reported about 250,000 of 3.08M entries as
//! changed: files with several hard links, which the index holds at one
//! link and the scanner lists at every one. These tests check that such
//! files compare equal, and that real differences are still found.

#![cfg(target_os = "macos")]

#[path = "../examples/day_replay_compare/mod.rs"]
mod compare;

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fx_engine::{Engine, EngineConfig, EngineHealth};
use fx_fs_macos::enumerator::MacScanner;
use fx_fs_macos::fsevents::source::FsEventsSource;
use fx_fs_macos::fsevents::stream::StreamConfig;
use fx_fs_macos::path_stat::MacPathStat;
use fx_fs_macos::scan::ScanOptions;
use fx_index::Index;

/// Files with a second hard link in another folder.
const LINKED: usize = 30;

/// A canonical scratch folder, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!(
            "fx-engine-fresh-compare-{name}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create scratch folder");
        Scratch(fs::canonicalize(&dir).expect("canonical scratch folder"))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Folders `a`, `b` and `c/d`: every file in `a` has a second link in `b`,
/// and `c/d` holds plain files.
fn make_tree(root: &Path) {
    for dir in ["a", "b", "c/d"] {
        fs::create_dir_all(root.join(dir)).expect("create folder");
    }
    for n in 0..LINKED {
        let file = root.join("a").join(format!("linked-{n}.o"));
        fs::write(&file, vec![b'x'; n + 1]).expect("write file");
        fs::hard_link(&file, root.join("b").join(format!("other-{n}.o"))).expect("link file");
    }
    for n in 0..10 {
        fs::write(root.join("c/d").join(format!("plain-{n}.txt")), b"plain").expect("write file");
    }
}

/// An engine over `root` with the app's scanner, stat and FSEvents source,
/// once its first scan is published.
fn indexed(root: &Path) -> Engine {
    let mut stream = StreamConfig::new(root.to_path_buf());
    stream.path_filter = Some(root.to_path_buf());
    let engine = Engine::start(
        Index::new(),
        EngineConfig::new(root),
        FsEventsSource::new(stream),
        MacPathStat::new(),
        MacScanner::new(ScanOptions::default()),
    )
    .expect("engine starts");
    let end = Instant::now() + Duration::from_secs(30);
    while !(engine.health() == EngineHealth::Live && engine.snapshot().index().live_count() > 1) {
        assert!(Instant::now() < end, "the first scan never finished");
        thread::sleep(Duration::from_millis(10));
    }
    engine
}

#[test]
fn a_file_with_several_hard_links_compares_equal_at_any_of_them() {
    let dir = Scratch::new("links");
    make_tree(&dir.0);
    let engine = indexed(&dir.0);
    let fresh = compare::scan(&dir.0).expect("fresh scan");
    assert_eq!(fresh.other_links.len(), LINKED, "the scan lists both links");

    let differences = compare::compare(&compare::engine_listing(&engine.snapshot()), &fresh);
    assert_eq!(
        differences.count(),
        0,
        "{} of {} hard-linked files compared as changed: {differences:?}",
        differences.changed.len(),
        LINKED
    );
    engine.shutdown();
}

#[test]
fn real_differences_are_still_found() {
    let dir = Scratch::new("differences");
    make_tree(&dir.0);
    let engine = indexed(&dir.0);
    // A published snapshot never changes: the disk moves on without it.
    let snap = engine.snapshot();
    engine.shutdown();
    fs::write(dir.0.join("c/d/new.txt"), b"new").expect("create a file");
    fs::remove_file(dir.0.join("c/d/plain-0.txt")).expect("remove a file");
    fs::write(dir.0.join("c/d/plain-1.txt"), b"plain, and longer").expect("grow a file");
    // A link of a hard-linked file at a new name: the old name is gone.
    fs::rename(dir.0.join("b/other-0.o"), dir.0.join("b/renamed-0.o")).expect("rename a link");
    fs::remove_file(dir.0.join("a/linked-0.o")).expect("remove the other link");

    let fresh = compare::scan(&dir.0).expect("fresh scan");
    let differences = compare::compare(&compare::engine_listing(&snap), &fresh);
    assert_eq!(
        differences.missing.len(),
        1,
        "the new file: {differences:?}"
    );
    assert_eq!(
        differences.extra.len(),
        1,
        "the removed file: {differences:?}"
    );
    // The grown file, and the linked file now at a name the index never
    // had (whichever link the index held).
    assert_eq!(differences.changed.len(), 2, "{differences:?}");
    assert_eq!(differences.changed_kind_or_size, 1, "the grown file");
}

/// Gives a folder its permissions back on drop, so the scratch folder can
/// be removed whatever the test's outcome.
struct Restore(PathBuf);

impl Drop for Restore {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
    }
}

#[test]
fn differences_the_scan_cannot_see_are_told_apart() {
    // SAFETY: a plain query of the process's effective user ID.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipped: root lists every folder");
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let dir = Scratch::new("unseen");
    make_tree(&dir.0);
    let hidden = dir.0.join("e");
    fs::create_dir_all(&hidden).expect("create folder");
    for n in 0..3 {
        fs::write(hidden.join(format!("kept-{n}.txt")), b"kept").expect("write file");
    }
    let engine = indexed(&dir.0);
    let snap = engine.snapshot();
    engine.shutdown();

    // A folder that refuses to be listed: the index still has its files.
    let _restore = Restore(hidden.clone());
    fs::set_permissions(&hidden, fs::Permissions::from_mode(0o000)).expect("lock the folder");
    // A file written after `since`, which the index doesn't know of yet.
    let since = SystemTime::now();
    thread::sleep(Duration::from_millis(20));
    fs::write(dir.0.join("c/d/plain-2.txt"), b"plain, written later").expect("write a file");

    let fresh = compare::scan(&dir.0).expect("fresh scan");
    let indexed = compare::engine_listing(&snap);
    let differences = compare::compare(&indexed, &fresh);
    assert!(differences.missing.is_empty(), "{differences:?}");
    assert_eq!(
        differences.extra.len(),
        3,
        "the locked folder's files: {differences:?}"
    );
    for &key in &differences.extra {
        assert!(compare::in_unlistable_folder(&indexed, &dir.0, key));
    }
    assert_eq!(differences.changed.len(), 1, "{differences:?}");
    let key = differences.changed[0];
    assert!(fresh.only_size_differs(key, &indexed[&key]));
    assert!(compare::written_since(&fresh.items, &dir.0, key, since));
    // Nothing else is told apart: the plain files were written before.
    let plain = fresh
        .items
        .iter()
        .find(|(_, item)| item.name == "plain-3.txt")
        .map(|(key, _)| *key)
        .expect("a plain file");
    assert!(!compare::written_since(&fresh.items, &dir.0, plain, since));
    assert!(!compare::in_unlistable_folder(&indexed, &dir.0, plain));
}
