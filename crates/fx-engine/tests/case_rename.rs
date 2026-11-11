//! A case-only rename on a case-insensitive volume (macOS only).
//!
//! Found by the soak (run 2.2): a file renamed from `759-Release
//! receipt.rs` to `759-RELEASE RECEIPT.RS` while the engine was down kept
//! its old letter case in the index after the reopen replayed the FSEvents
//! history, while a fresh scan showed the new one.
//!
//! The cause: `fx_fs_macos::path_stat::MacPathStat` takes an entry's name
//! from the path it is given, and on a case-insensitive volume the old
//! path still finds the renamed file. So when the old path is checked last
//! (FSEvents does not promise the order of a rename's two paths, and its
//! history may keep only one spelling of a path), the writer sees the file
//! under its old name and keeps it. The index must hold the name as stored
//! on disk, whatever spelling an event used. The test is ignored until
//! `MacPathStat` (or the writer) reads the stored name.
//!
//! It runs the engine with the real `MacPathStat` and `MacScanner` over a
//! private temporary folder, with a scripted change source, so the order of
//! the two events is fixed. It does nothing on a case-sensitive volume.

#![cfg(target_os = "macos")]

mod common;

use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::{FakeSource, WAIT};
use fx_engine::{Engine, EngineConfig, EngineSnapshot};
use fx_fs_macos::enumerator::MacScanner;
use fx_fs_macos::path_stat::MacPathStat;
use fx_fs_macos::scan::ScanOptions;
use fx_index::Index;
use fx_model::{Change, StartOutcome};

/// A private scratch folder, canonical, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!(
            "fx-engine-case-rename-{}-{nanos}",
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

/// The names of the entries directly in the root.
fn names(snap: &EngineSnapshot) -> Vec<String> {
    let idx = snap.index();
    let mut out: Vec<String> = idx
        .live_ids()
        .filter(|&id| {
            let p = idx.parent(id);
            !p.is_none() && idx.parent(p).is_none()
        })
        .map(|id| idx.name(id).to_string())
        .collect();
    out.sort();
    out
}

#[test]
#[ignore = "fx-fs-macos bug (run 2.2 follow-up): MacPathStat names an entry after the path \
            it is given, so on a case-insensitive volume an event for the old spelling of a \
            case-only rename puts the old name back"]
fn a_case_only_rename_keeps_the_stored_name_whatever_order_the_events_come_in() {
    let scratch = Scratch::new();
    let root = &scratch.0;
    let old = root.join("7-report.txt");
    let new = root.join("7-REPORT.TXT");
    fs::write(&old, b"x").unwrap();
    if !new.exists() {
        // A case-sensitive volume: the two spellings are two names.
        return;
    }
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = Engine::start(
        Index::new(),
        EngineConfig::new(root).with_batch_window(Duration::from_millis(50)),
        source.clone(),
        MacPathStat::new(),
        MacScanner::new(ScanOptions::default()),
    )
    .expect("engine starts");
    let snap = sync(&engine, &source);
    assert_eq!(names(&snap), vec!["7-report.txt".to_string()]);

    fs::rename(&old, &new).unwrap();
    source.push(vec![Change::CheckPath(new), Change::CheckPath(old)]);
    let snap = sync(&engine, &source);
    assert_eq!(
        names(&snap),
        vec!["7-REPORT.TXT".to_string()],
        "the index must hold the name stored on disk"
    );
    engine.shutdown();
}
