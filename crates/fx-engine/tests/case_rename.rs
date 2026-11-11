//! Case-only renames on a case-insensitive volume (macOS only).
//!
//! Found by the soak (run 2.2): a file renamed from `759-Release
//! receipt.rs` to `759-RELEASE RECEIPT.RS` while the engine was down kept
//! its old letter case in the index after the reopen replayed the FSEvents
//! history, while a fresh scan showed the new one.
//!
//! The cause: `fx_fs_macos::path_stat::MacPathStat` took an entry's name
//! from the path it was given, and on a case-insensitive volume the old
//! path still finds the renamed file. So when the old path was checked last
//! (FSEvents does not promise the order of a rename's two paths, and its
//! history may keep only one spelling of a path), the writer saw the file
//! under its old name and kept it. The index must hold the name as stored
//! on disk, whatever spelling an event used: `MacPathStat` now reads it
//! (`getattrlist` with `ATTR_CMN_NAME`).
//!
//! The tests run the engine with the real `MacPathStat` and `MacScanner`
//! over a private temporary folder, with a scripted change source, so the
//! events and their order are fixed. They do nothing on a case-sensitive
//! volume.

#![cfg(target_os = "macos")]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::{FakeSource, WAIT};
use fx_engine::{Engine, EngineConfig, EngineSnapshot};
use fx_fs_macos::enumerator::MacScanner;
use fx_fs_macos::path_stat::MacPathStat;
use fx_fs_macos::scan::ScanOptions;
use fx_index::Index;
use fx_model::{Change, EntryId, StartOutcome};

/// A private scratch folder, canonical, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!(
            "fx-engine-case-rename-{tag}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        Scratch(fs::canonicalize(&dir).unwrap())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Starts the engine over `root` with the real stat and scanner and waits
/// for its first index.
fn start(root: &Path) -> (Engine, FakeSource) {
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = Engine::start(
        Index::new(),
        EngineConfig::new(root).with_batch_window(Duration::from_millis(50)),
        source.clone(),
        MacPathStat::new(),
        MacScanner::new(ScanOptions::default()),
    )
    .expect("engine starts");
    sync(&engine, &source);
    (engine, source)
}

/// Waits until the writer applied every pushed batch.
fn sync(engine: &Engine, source: &FakeSource) -> std::sync::Arc<EngineSnapshot> {
    let end = Instant::now() + WAIT;
    loop {
        if source.pending() == 0 {
            let snap = engine.snapshot();
            if snap.cursor() == Some(source.current_cursor()) {
                return snap;
            }
        }
        assert!(Instant::now() < end, "the writer never applied the batch");
        thread::sleep(Duration::from_millis(2));
    }
}

/// The sorted names of the live entries whose parent is `parent`.
fn names_under(snap: &EngineSnapshot, parent: impl Fn(EntryId) -> bool) -> Vec<String> {
    let idx = snap.index();
    let mut out: Vec<String> = idx
        .live_ids()
        .filter(|&id| parent(idx.parent(id)))
        .map(|id| idx.name(id).to_string())
        .collect();
    out.sort();
    out
}

/// The names of the entries directly in the root.
fn names(snap: &EngineSnapshot) -> Vec<String> {
    let idx = snap.index();
    names_under(snap, |p| !p.is_none() && idx.parent(p).is_none())
}

/// The names of the entries in the root's folder named `dir`.
fn names_in(snap: &EngineSnapshot, dir: &str) -> Vec<String> {
    let idx = snap.index();
    names_under(snap, |p| {
        !p.is_none() && idx.name(p) == dir && {
            let gp = idx.parent(p);
            !gp.is_none() && idx.parent(gp).is_none()
        }
    })
}

/// Whether `spelling` finds `stored` on this volume (case-insensitive).
fn folds_case(stored: &Path, spelling: &Path) -> bool {
    stored.exists() && spelling.exists()
}

#[test]
fn a_case_only_rename_keeps_the_stored_name_whatever_order_the_events_come_in() {
    let scratch = Scratch::new("order");
    let root = &scratch.0;
    let old = root.join("7-report.txt");
    let new = root.join("7-REPORT.TXT");
    fs::write(&old, b"x").unwrap();
    if !folds_case(&old, &new) {
        // A case-sensitive volume: the two spellings are two names.
        return;
    }
    let (engine, source) = start(root);
    assert_eq!(names(&engine.snapshot()), vec!["7-report.txt".to_string()]);

    fs::rename(&old, &new).unwrap();
    source.push(vec![
        Change::CheckPath(new.clone()),
        Change::CheckPath(old.clone()),
    ]);
    let snap = sync(&engine, &source);
    assert_eq!(
        names(&snap),
        vec!["7-REPORT.TXT".to_string()],
        "the index must hold the name stored on disk"
    );

    // And back again, with the events in the other order.
    fs::rename(&new, &old).unwrap();
    source.push(vec![Change::CheckPath(new), Change::CheckPath(old)]);
    let snap = sync(&engine, &source);
    assert_eq!(names(&snap), vec!["7-report.txt".to_string()]);
    engine.shutdown();
}

#[test]
fn an_event_for_the_old_spelling_alone_renames_the_entry() {
    // FSEvents history may keep only one spelling of a path: here, the old
    // one, which the index already holds.
    let scratch = Scratch::new("alone");
    let root = &scratch.0;
    let old = root.join("notes.md");
    let new = root.join("Notes.MD");
    fs::write(&old, b"x").unwrap();
    if !folds_case(&old, &new) {
        return;
    }
    let (engine, source) = start(root);
    assert_eq!(names(&engine.snapshot()), vec!["notes.md".to_string()]);

    fs::rename(&old, &new).unwrap();
    source.push(vec![Change::CheckPath(old)]);
    let snap = sync(&engine, &source);
    assert_eq!(names(&snap), vec!["Notes.MD".to_string()]);
    engine.shutdown();
}

#[test]
fn a_folder_rescanned_under_its_old_spelling_takes_the_stored_name() {
    let scratch = Scratch::new("folder");
    let root = &scratch.0;
    let old = root.join("docs");
    let new = root.join("DOCS");
    fs::create_dir(&old).unwrap();
    fs::write(old.join("a.txt"), b"a").unwrap();
    if !folds_case(&old, &new) {
        return;
    }
    let (engine, source) = start(root);
    assert_eq!(names(&engine.snapshot()), vec!["docs".to_string()]);

    fs::rename(&old, &new).unwrap();
    fs::write(new.join("b.txt"), b"b").unwrap();
    source.push(vec![Change::RescanSubtree(old)]);
    let snap = sync(&engine, &source);
    assert_eq!(names(&snap), vec!["DOCS".to_string()]);
    assert_eq!(
        names_in(&snap, "DOCS"),
        vec!["a.txt".to_string(), "b.txt".to_string()],
        "the folder keeps its children and gains the new one"
    );
    engine.shutdown();
}
