//! Phase 1 gate metrics at CI scale.
//!
//! The release benchmarks (`examples/startup_bench.rs`,
//! `examples/catchup_bench.rs`, `examples/first_run_bench.rs`) measure the
//! full-scale numbers on real machines. These tests check the same
//! paths at a small scale:
//!
//! - Memory: `EngineSnapshot::memory_report()` reports the heap bytes of
//!   the default columns and stays at or under 100 B per live entry for
//!   100k synthetic entries (fx-bench's tree generator).
//! - Catch-up: 500 changes made while the engine is stopped are replayed
//!   from the saved cursor, search works on the reopened store, and the
//!   result equals a fresh scan.
//! - Startup: a store written by the engine is reopened (read-only through
//!   `StoreReader`, and by `Engine::open`) and answers a first query.
//!
//! Memory report API these tests rely on:
//!
//! - `EngineSnapshot::memory_report()` returns a report with public fields
//!   `live_entries: usize` (live entry count), `total_bytes: usize` (sum of
//!   the columns) and `columns`, a `Vec<(&'static str, usize)>` of
//!   `(label, heap bytes)` pairs with one distinct,
//!   non-empty label per default column (names, name offsets, parents,
//!   packed size/type/flags, times, extension IDs, the folded search copy
//!   and the pre-sorted orders). The label of the name column contains
//!   `name` and that of the orders contains `order`.
//! - `total_bytes` is real heap accounting: at least
//!   `fx_index::Snapshot::heap_bytes()` (the same accounting as
//!   `Index::heap_bytes()`, without the writer-only child counts) plus the
//!   allocated capacity of the pre-sorted orders. Uncompacted delta
//!   overlays (updates and inserts after the build) are counted.
//! - `bytes_per_entry()` is `total_bytes / live_entries` as an `f64`.
//!
//! Gate benches: each writes the scale it ran at into its gate metrics
//! (`startup_entries`, `catchup_changes`, `first_run_entries`,
//! `freshness_ops`), and the crate README documents those keys.
//! - Its `Display` output is the benchmark format documented in the crate
//!   README: one `metric=value` per line, including `live_entries`,
//!   `total_bytes` and `bytes_per_entry`.

mod common;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::*;
use fx_engine::{Engine, EngineConfig, ReadOptions, StoreReader};
use fx_index::Index;
use fx_model::{Change, Entry, EntryFlags, EntryId, EntryKind, FileKey, StartOutcome, VolumeId};
use fx_sort::{Direction, SortKey};

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
            "fx-engine-gate-{name}-{}-{nanos}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).expect("create scratch dir");
        TempDir(dir)
    }

    fn store(&self) -> PathBuf {
        self.0.join("store").join("index.fxdb")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn store_config(path: &Path) -> EngineConfig {
    config()
        .with_store_path(path)
        .with_checkpoint_idle(Duration::from_secs(3600))
}

fn open(tree: &FakeTree, source: &FakeSource, config: EngineConfig) -> Engine {
    Engine::open(config, source.clone(), tree.clone(), tree.clone()).expect("engine opens")
}

/// Every live entry of an index snapshot, by key.
fn index_entries(idx: &fx_index::Snapshot) -> HashMap<FileKey, Entry> {
    let ids: Vec<EntryId> = idx.live_ids().collect();
    ids.into_iter()
        .map(|id| {
            let p = idx.parent(id);
            let parent = if p.is_none() { None } else { Some(idx.key(p)) };
            let e = Entry {
                key: idx.key(id),
                parent,
                name: idx.name(id).to_string(),
                kind: idx.kind(id),
                flags: idx.flags(id),
                size: idx.size(id),
                modified: idx.modified(id),
                created: idx.created(id),
            };
            (e.key, e)
        })
        .collect()
}

/// Ten folders of 100 files each under `/vol`.
fn gate_tree() -> FakeTree {
    let tree = FakeTree::new();
    for d in 0..10 {
        tree.mkdir(&format!("/vol/d{d}"));
    }
    for d in 0..10 {
        tree.add_files(&format!("/vol/d{d}"), "bulk", 100);
    }
    tree
}

/// Opens an engine over a missing store, waits for the full scan, applies
/// one live batch so a cursor is published, and shuts down (saving the
/// store). Returns the saved cursor.
fn build_store(tree: &FakeTree, store: &Path) -> fx_model::ResumeCursor {
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = open(tree, &source, store_config(store));
    wait_for_tree(&engine, tree, "first full scan");
    tree.touch("/vol/d0/live.txt", 7);
    source.push(vec![check("/vol/d0/live.txt")]);
    wait_for_tree(&engine, tree, "live batch");
    let cursor = wait_for(&engine, "cursor of the live batch", |s| {
        s.cursor() == Some(source.current_cursor())
    })
    .cursor()
    .expect("published cursor");
    engine.close().expect("orderly shutdown saves the store");
    assert!(store.exists(), "the store file is written");
    cursor
}

/// 500 scripted changes made while the engine is stopped: 150 creates,
/// 100 deletes, 100 renames across folders, 100 size changes and 50 new
/// folders. Returns the change history a source would report, one check
/// per touched path.
fn offline_changes(tree: &FakeTree) -> (usize, Vec<Change>) {
    let mut ops = 0;
    let mut history = Vec::new();
    for i in 0..150 {
        let p = format!("/vol/d{}/new{i}.txt", i % 10);
        tree.touch(&p, i as u64 + 1);
        history.push(check(&p));
        ops += 1;
    }
    for i in 0..100 {
        let p = format!("/vol/d{}/bulk{}.dat", i % 10, i / 10);
        tree.remove(&p);
        history.push(check(&p));
        ops += 1;
    }
    for i in 0..100 {
        let from = format!("/vol/d{}/bulk{}.dat", i % 10, 10 + i / 10);
        let to = format!("/vol/d{}/ren{i}.dat", (i + 1) % 10);
        tree.rename(&from, &to);
        history.push(check(&from));
        history.push(check(&to));
        ops += 1;
    }
    for i in 0..100 {
        let p = format!("/vol/d{}/bulk{}.dat", i % 10, 20 + i / 10);
        tree.set_size(&p, 1_000_000 + i as u64);
        history.push(check(&p));
        ops += 1;
    }
    for i in 0..50 {
        let p = format!("/vol/d{}/sub{i}", i % 10);
        tree.mkdir(&p);
        history.push(check(&p));
        ops += 1;
    }
    (ops, history)
}

fn total(engine: &Engine, query: &str) -> usize {
    engine
        .search(
            query,
            fx_engine::QueryOptions::default(),
            SortKey::Name,
            Direction::Ascending,
            0,
            10,
        )
        .expect("search is not cancelled")
        .total
}

/// `n` synthetic entries from fx-bench's tree generator, parents first.
fn generated_entries(n: usize) -> Vec<Entry> {
    let generated = fx_bench::generate_tree(n, 0x6761_7465, fx_bench::TreeShape::default());
    assert_eq!(generated.len(), n);
    let key = |i: usize| FileKey::new(VolumeId(1), i as u64 + 1);
    (0..n)
        .map(|i| {
            let parent = generated.parent[i];
            let t = 1_700_000_000 + (i as i64 * 37) % 50_000_000;
            Entry {
                key: key(i),
                parent: (parent != u32::MAX).then(|| key(parent as usize)),
                name: generated.names.get(i).to_string(),
                kind: if generated.is_dir[i] {
                    EntryKind::Dir
                } else {
                    EntryKind::File
                },
                flags: EntryFlags::empty(),
                size: if generated.is_dir[i] {
                    0
                } else {
                    (i as u64 * 7919) % 10_000_000
                },
                modified: t,
                created: t - 1_000,
            }
        })
        .collect()
}

fn index_of(entries: &[Entry]) -> Index {
    let mut index = Index::new();
    for e in entries {
        index.upsert(e.clone()).expect("upsert into a fresh index");
    }
    index
}

/// A compacted index of `entries`, then size updates of every
/// `every`-th file left uncompacted in the delta (same IDs, same names).
fn index_with_updates(entries: &[Entry], every: usize) -> Index {
    let mut index = index_of(entries);
    index.compact();
    index.take_auto_remap();
    index.set_compact_fraction(f64::INFINITY);
    for e in entries
        .iter()
        .filter(|e| e.kind == EntryKind::File)
        .step_by(every)
    {
        let mut e = e.clone();
        e.size += 1_000_003;
        index.upsert(e).expect("update in place");
    }
    index
}

/// Allocated bytes of the engine's pre-sorted orders.
fn order_capacity_bytes(snap: &fx_engine::EngineSnapshot) -> usize {
    fx_engine::ENGINE_SORT_KEYS
        .iter()
        .map(|&k| {
            snap.order(k)
                .unwrap_or_else(|| panic!("engine keeps the {k:?} order"))
                .ids
                .capacity()
                * std::mem::size_of::<u32>()
        })
        .sum()
}

/// `memory_report()` is built on fx-index's own accounting: it covers at
/// least `fx_index::Snapshot::heap_bytes()` plus the allocated pre-sorted
/// orders.
fn assert_report_covers_real_heap(snap: &fx_engine::EngineSnapshot, what: &str) {
    let report = snap.memory_report();
    let index_heap = snap.index().heap_bytes();
    let orders = order_capacity_bytes(snap);
    assert!(
        report.total_bytes >= index_heap + orders,
        "{what}: report total {} < index heap {index_heap} + order capacities {orders}\n{report}",
        report.total_bytes
    );
}

#[test]
fn memory_report_total_covers_index_heap_bytes_minus_writer_only_parts() {
    const N: usize = 20_000;
    let entries = generated_entries(N);
    // 30% of the files updated after the build, not compacted.
    let mut index = index_with_updates(&entries, 3);
    assert!(index.delta_len() > 0, "updates stay in the delta");
    index.take_auto_remap();
    let index_heap = index.heap_bytes();
    let id_bound = index.snapshot().id_bound();

    let tree = FakeTree::new();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = start(&tree, &source, index, config());
    let snap = engine.snapshot();
    assert_eq!(snap.index().id_bound(), id_bound, "no compaction at start");
    let report = snap.memory_report();
    assert_eq!(report.live_entries, N);

    // Writer-only parts of `Index::heap_bytes`: the live-children counts,
    // one u32 per entry ID (allowing for a doubled capacity).
    let writer_only = 2 * std::mem::size_of::<u32>() * id_bound + 64;
    assert!(
        report.total_bytes + writer_only >= index_heap,
        "report total {} + writer-only {writer_only} < Index::heap_bytes {index_heap}\n{report}",
        report.total_bytes
    );
    assert_report_covers_real_heap(&snap, "with an uncompacted delta");
    let sum: usize = report.columns.iter().map(|&(_, b)| b).sum();
    assert_eq!(report.total_bytes, sum, "total is the sum of the columns");
    engine.shutdown();
}

#[test]
fn memory_report_counts_uncompacted_delta_overlays() {
    const N: usize = 10_000;
    let entries = generated_entries(N);

    let report_of = |index: Index| {
        let tree = FakeTree::new();
        let source = FakeSource::new(StartOutcome::Resumed);
        let engine = start(&tree, &source, index, config());
        let snap = engine.snapshot();
        assert_report_covers_real_heap(&snap, "index-backed engine");
        let report = snap.memory_report();
        engine.shutdown();
        (snap.index().id_bound(), report)
    };

    let mut compacted = index_of(&entries);
    compacted.compact();
    let (bound_base, base) = report_of(compacted);

    // Same entries, then updates after the build that stay in the delta:
    // the ID bound and the names are unchanged, only the overlay is new.
    let (bound_updated, updated) = report_of(index_with_updates(&entries, 5));
    assert_eq!(bound_updated, bound_base, "updates keep the ID bound");
    assert_eq!(updated.live_entries, base.live_entries);
    assert!(
        updated.total_bytes > base.total_bytes,
        "delta overrides are counted: {} > {}\nbase:\n{base}\nupdated:\n{updated}",
        updated.total_bytes,
        base.total_bytes
    );

    // Live inserts through the engine, below the writer's compaction
    // threshold: the published snapshot's report grows.
    let tree = gate_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = start(&tree, &source, tree.build_index(), config());
    let before = engine.snapshot();
    assert_report_covers_real_heap(&before, "before live inserts");
    let before_total = before.memory_report().total_bytes;
    let mut batch = Vec::new();
    for i in 0..20 {
        let p = format!("/vol/d{}/inserted{i}.txt", i % 10);
        tree.touch(&p, i as u64 + 1);
        batch.push(check(&p));
    }
    source.push(batch);
    let after = wait_for_tree(&engine, &tree, "20 live inserts");
    assert_report_covers_real_heap(&after, "after live inserts");
    let after_report = after.memory_report();
    assert_eq!(after_report.live_entries, before.index().live_count() + 20);
    assert!(
        after_report.total_bytes > before_total,
        "inserts after the build are counted: {} > {before_total}",
        after_report.total_bytes
    );
    engine.shutdown();
}

/// Every gate bench writes the scale it ran at into its gate metrics, and
/// the crate README documents those keys and which startup number is cold.
#[test]
fn gate_benches_emit_and_document_their_scale_keys() {
    let readme = include_str!("../README.md");
    let benches = [
        (
            "startup_entries",
            include_str!("../examples/startup_bench.rs"),
        ),
        (
            "catchup_changes",
            include_str!("../examples/catchup_bench.rs"),
        ),
        (
            "first_run_entries",
            include_str!("../examples/first_run_bench.rs"),
        ),
        (
            "freshness_ops",
            include_str!("../examples/freshness_bench.rs"),
        ),
    ];
    for (key, source) in benches {
        assert!(
            readme.contains(&format!("{key}=")) || readme.contains(&format!("`{key}`")),
            "README documents the `{key}` gate metric"
        );
        let shared = include_str!("../examples/bench_common/mod.rs");
        assert!(
            source.contains(key) || shared.contains(key),
            "the bench writes `{key}` into its gate metrics"
        );
    }
    let lower = readme.to_lowercase();
    assert!(
        lower.contains("cold") && lower.contains("first result"),
        "README states which store-open-to-first-result number is the cold one"
    );
}

#[test]
fn memory_report_stays_within_100_bytes_per_entry_at_100k() {
    const N: usize = 100_000;
    let entries = generated_entries(N);
    let name_bytes: usize = entries.iter().map(|e| e.name.len()).sum();
    let index = index_of(&entries);

    let tree = FakeTree::new();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = start(&tree, &source, index, config());
    let snap = engine.snapshot();
    let report = snap.memory_report();

    let live: usize = report.live_entries;
    assert_eq!(live, N, "live entry count");
    let total_bytes: usize = report.total_bytes;

    let columns: Vec<(String, usize)> = report
        .columns
        .iter()
        .map(|(label, bytes)| (label.to_string(), *bytes))
        .collect();
    let labels: HashSet<&str> = columns.iter().map(|(l, _)| l.as_str()).collect();
    assert_eq!(
        labels.len(),
        columns.len(),
        "column labels are distinct: {columns:?}"
    );
    assert!(
        columns.iter().all(|(l, _)| !l.is_empty()),
        "every column has a label: {columns:?}"
    );
    assert!(
        columns.len() >= 6,
        "the report lists every default column separately: {columns:?}"
    );
    let sum: usize = columns.iter().map(|(_, b)| b).sum();
    assert_eq!(total_bytes, sum, "total is the sum of the columns");

    let name_col: usize = columns
        .iter()
        .filter(|(l, _)| l.contains("name") && !l.contains("offset"))
        .map(|(_, b)| b)
        .sum();
    assert!(
        name_col >= name_bytes,
        "the name column holds at least the name bytes ({name_bytes}): {columns:?}"
    );
    let order_col: usize = columns
        .iter()
        .filter(|(l, _)| l.contains("order"))
        .map(|(_, b)| b)
        .sum();
    assert!(
        order_col >= fx_engine::ENGINE_SORT_KEYS.len() * N * 4,
        "the pre-sorted orders (4 B per entry each) are counted: {columns:?}"
    );

    let per_entry: f64 = report.bytes_per_entry();
    let expected = total_bytes as f64 / N as f64;
    assert!(
        (per_entry - expected).abs() < 1e-6,
        "bytes_per_entry {per_entry} == total / live {expected}"
    );
    assert!(
        per_entry <= 100.0,
        "memory budget: {per_entry:.1} B per entry > 100 ({columns:?})"
    );

    // The benchmark format: one `metric=value` per line.
    let text = report.to_string();
    let mut metrics = HashMap::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let (k, v) = line
            .split_once('=')
            .unwrap_or_else(|| panic!("not a metric=value line: {line:?}"));
        assert!(!k.trim().is_empty(), "metric name in {line:?}");
        metrics.insert(k.trim().to_string(), v.trim().to_string());
    }
    assert_eq!(
        metrics.get("live_entries").map(String::as_str),
        Some(N.to_string().as_str()),
        "{text}"
    );
    assert_eq!(
        metrics.get("total_bytes").map(String::as_str),
        Some(total_bytes.to_string().as_str()),
        "{text}"
    );
    let printed: f64 = metrics
        .get("bytes_per_entry")
        .unwrap_or_else(|| panic!("bytes_per_entry in {text}"))
        .parse()
        .expect("bytes_per_entry is a number");
    assert!((printed - per_entry).abs() < 0.1, "{text}");

    engine.shutdown();
}

#[test]
fn small_scale_offline_catch_up_equals_a_fresh_scan() {
    let dir = TempDir::new("catchup");
    let store = dir.store();
    let tree = gate_tree();
    let cursor = build_store(&tree, &store);

    // Engine stopped: 500 changes on the "disk", scripted as the source's
    // history since the saved cursor.
    let (ops, history) = offline_changes(&tree);
    assert_eq!(ops, 500, "500 scripted changes");
    let source = FakeSource::new(StartOutcome::Resumed);
    for batch in history.chunks(100) {
        source.push(batch.to_vec());
    }

    let engine = open(&tree, &source, store_config(&store));
    assert_eq!(
        source.started_from(),
        Some(Some(cursor)),
        "restart resumes from the saved cursor"
    );
    // Search is usable at once on the reopened store, before or during the
    // catch-up (the untouched bulk files are in both).
    assert!(total(&engine, "bulk") > 0, "search usable after restart");

    let snap = wait_for_tree(&engine, &tree, "offline catch-up of 500 changes");
    assert_eq!(source.pending(), 0, "every scripted batch was replayed");
    let fresh = tree.build_index();
    assert_eq!(
        snapshot_entries(&snap),
        index_entries(&fresh.snapshot()),
        "caught-up index equals a fresh scan"
    );
    assert_orders_match(&snap);

    // The caught-up index answers queries for the offline changes.
    assert_eq!(total(&engine, "new149.txt"), 1);
    assert_eq!(total(&engine, "ren0.dat"), 1);
    assert_eq!(total(&engine, "bulk0.dat"), 0, "deleted in every folder");
    assert_eq!(total(&engine, "sub49"), 1);
    engine.shutdown();
}

#[test]
fn reopened_store_gives_a_first_result() {
    let dir = TempDir::new("startup");
    let store = dir.store();
    let tree = gate_tree();
    let cursor = build_store(&tree, &store);
    let want = tree.entries();

    // Process-independent open: read-only, no change source.
    let reader = StoreReader::open(&store).expect("store opens read-only");
    assert_eq!(reader.live_count(), want.len(), "every saved entry loads");
    let page = reader
        .search(
            "bulk7.dat",
            &ReadOptions::default(),
            SortKey::Name,
            Direction::Ascending,
            0,
            1,
        )
        .expect("query runs");
    assert_eq!(page.total, 10, "one bulk7.dat per folder");
    let first = *page.ids.first().expect("a first result");
    assert_eq!(reader.name(first).as_deref(), Some("bulk7.dat"));
    drop(reader);

    // The engine: open the store, snapshot published, first query result.
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = open(&tree, &source, store_config(&store));
    assert_eq!(source.started_from(), Some(Some(cursor)));
    let snap = engine.snapshot();
    assert_eq!(
        snapshot_entries(&snap),
        want,
        "the first published snapshot serves the saved index"
    );
    let page = engine
        .search(
            "bulk7.dat",
            fx_engine::QueryOptions::default(),
            SortKey::Name,
            Direction::Ascending,
            0,
            1,
        )
        .expect("search is not cancelled");
    assert_eq!(page.total, 10);
    assert_eq!(page.ids.len(), 1, "a first result");
    engine.shutdown();
}
