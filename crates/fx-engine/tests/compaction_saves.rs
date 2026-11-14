//! The compaction saves the performance gate projects into B7 ("Disk writes
//! a day"), checked against the real writer.
//!
//! The gate's background phases (10 or 30 minutes at 20 file operations a
//! second) never reach a compaction of a whole-disk index, so `fx-bench
//! saves` projects the saves from the phase's file activity with the model
//! in `fx_bench::saves`:
//!
//! - a file created adds one new row and a file deleted one removed row; a
//!   file renamed after it was created (since the last compaction) adds
//!   none, because its row is updated in place; a file created and deleted
//!   within one batch adds neither, since it is gone before the writer
//!   looks (the projection counts it anyway, so it errs slightly high);
//! - the writer compacts at the first batch that leaves more than
//!   `fx_bench::saves::compaction_limit(live entries)` new rows or removed
//!   rows, and the limit is `fx_index::DEFAULT_COMPACT_FRACTION` of the
//!   live entries, at least 1,024;
//! - each compaction writes the whole store once, and nothing else writes
//!   it in between.
//!
//! These tests drive an engine with `fx-bench churn`'s own operation stream
//! (`fx_bench::churn::Planner`) on the fake tree, from a compacted index
//! and no saved store, and check each claim: after every batch, the index
//! holds exactly one new row per create and one removed row per delete and
//! nothing has written the store; the compaction comes at the batch the
//! model predicts, once above the 1,024-row floor and once at it; and it
//! writes the store.
//!
//! Index API these tests rely on: `fx_index::Snapshot::id_bound()` counts
//! every entry ID since the last compaction (live and removed), so right
//! after a compaction it is the base's length, `id_bound() - base` is the
//! new rows and `id_bound() - live_count()` the removed rows.

mod common;

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::*;
use fx_bench::churn::{Op, Planner};
use fx_bench::saves;
use fx_model::{Change, StartOutcome};

/// Where the churn's folders go in the fake tree.
const CHURN: &str = "/vol/churn";
/// The churn stream's seed, as the gate runs it.
const SEED: u64 = 1;
/// Operations per batch.
const CHUNK: usize = 50;
/// No file cap, so the mix stays half creates, as in a gate phase, which
/// never reaches churn's 5,000-file cap.
const MAX_FILES: usize = 1_000_000;
/// Batches before the test gives up on a compaction.
const MAX_CHUNKS: usize = 200;

/// A unique scratch directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "fx-engine-compaction-saves-{name}-{}-{nanos}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).expect("create scratch dir");
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The store's size, once a save has written it.
fn store_bytes(path: &Path) -> Option<u64> {
    fs::metadata(path).ok().map(|m| m.len())
}

/// Waits until a save has written the store and returns its size.
fn wait_for_store(path: &Path, what: &str) -> u64 {
    let end = Instant::now() + WAIT;
    loop {
        if let Some(bytes) = store_bytes(path) {
            return bytes;
        }
        assert!(Instant::now() < end, "timed out waiting for: {what}");
        thread::sleep(Duration::from_millis(5));
    }
}

/// Applies one churn operation to the fake tree and returns the paths
/// FSEvents would report for it.
fn apply(tree: &FakeTree, op: &Op) -> Vec<Change> {
    match op {
        Op::Create { path, len } => {
            let p = format!("{CHURN}/{path}");
            tree.touch(&p, *len as u64);
            vec![check(&p)]
        }
        Op::Rename { from, to } => {
            let (from, to) = (format!("{CHURN}/{from}"), format!("{CHURN}/{to}"));
            tree.rename(&from, &to);
            vec![check(&from), check(&to)]
        }
        Op::Delete { path } => {
            let p = format!("{CHURN}/{path}");
            tree.remove(&p);
            vec![check(&p)]
        }
    }
}

/// Drives an engine over `base_files` files with the churn stream until the
/// writer compacts, checking the model on every batch.
fn compaction_follows_the_model(base_files: usize, floor: bool) {
    let scratch = TempDir::new(&format!("{base_files}"));
    let store = scratch.0.join("store").join("index.fxdb");
    let tree = FakeTree::new();
    tree.mkdir("/vol/base");
    tree.add_files("/vol/base", "f", base_files);
    let mut planner = Planner::new(SEED, MAX_FILES);
    tree.mkdir(CHURN);
    for folder in planner.folders() {
        tree.mkdir(&format!("{CHURN}/{folder}"));
    }
    // Compacted, so the churn starts from a whole base, as the app does
    // after its first scan.
    let mut index = tree.build_index();
    index.compact();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = start(&tree, &source, index, config().with_store_path(&store));
    let snap = engine.snapshot();
    let base = snap.index().id_bound();
    assert_eq!(base, snap.index().live_count(), "a compacted base");
    let limit = saves::compaction_limit(base as u64);
    assert_eq!(
        limit == saves::MIN_COMPACT_ROWS,
        floor,
        "{base} entries: limit {limit}"
    );

    // The rows the model counts: one new row per file created and one
    // removed row per file deleted, except that a file created and deleted
    // within one batch is gone before the writer looks at it, so it gets no
    // row at all.
    let (mut new_rows, mut removed_rows) = (0u64, 0u64);
    for chunk in 0..MAX_CHUNKS {
        let mut batch = Vec::new();
        // Files this batch created, by their current path.
        let mut fresh = HashSet::new();
        for _ in 0..CHUNK {
            let op = planner.next_op();
            match &op {
                Op::Create { path, .. } => {
                    new_rows += 1;
                    fresh.insert(path.clone());
                }
                Op::Rename { from, to } => {
                    if fresh.remove(from) {
                        fresh.insert(to.clone());
                    }
                }
                Op::Delete { path } => {
                    if fresh.remove(path) {
                        new_rows -= 1;
                    } else {
                        removed_rows += 1;
                    }
                }
            }
            batch.extend(apply(&tree, &op));
        }
        // Each batch changes the index, so it makes one new generation.
        let generation = engine.snapshot().generation();
        source.push(batch);
        let snap = wait_for(&engine, &format!("churn batch {chunk}"), |s| {
            s.generation() > generation
        });
        assert_eq!(
            snapshot_entries(&snap),
            tree.entries(),
            "batch {chunk}: the index holds the tree"
        );
        let idx = snap.index();
        let live = idx.live_count() as u64;
        let rows = (
            (idx.id_bound() - base) as u64,
            (idx.id_bound() - idx.live_count()) as u64,
        );
        let due = saves::compaction_due(live, new_rows, removed_rows);
        if rows == (new_rows, removed_rows) {
            assert!(
                !due,
                "batch {chunk}: the model predicts a compaction after {new_rows} new and \
                 {removed_rows} removed rows on {live} entries, which the writer didn't make"
            );
            assert_eq!(
                store_bytes(&store),
                None,
                "batch {chunk}: nothing but a compaction writes the store"
            );
            continue;
        }
        // Otherwise the writer compacted: every row is in the base, and the
        // removed ones are gone.
        assert!(
            removed_rows > 0 && idx.id_bound() == idx.live_count(),
            "batch {chunk}: the index holds {rows:?} new and removed rows, the model \
             {:?}",
            (new_rows, removed_rows)
        );
        assert!(
            due,
            "{base_files} files: the writer compacted at batch {chunk} after {new_rows} new \
             and {removed_rows} removed rows on {live} entries, below the model's limit {}",
            saves::compaction_limit(live)
        );
        let bytes = wait_for_store(&store, "the compaction's save");
        assert!(bytes > 0, "the compaction saves the whole store");
        return;
    }
    panic!("{base_files} files: no compaction after {MAX_CHUNKS} batches of {CHUNK} operations");
}

#[test]
fn the_gate_compacts_at_the_index_fraction() {
    assert_eq!(saves::COMPACT_FRACTION, fx_index::DEFAULT_COMPACT_FRACTION);
}

#[test]
fn churn_compacts_and_saves_where_the_model_says() {
    // 12,000 files: a limit of 10% of the entries, above the floor.
    compaction_follows_the_model(12_000, false);
}

#[test]
fn churn_compacts_and_saves_where_the_model_says_at_the_floor() {
    // 3,000 files: 10% is below the 1,024-row floor.
    compaction_follows_the_model(3_000, true);
}
