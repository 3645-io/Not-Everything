//! End-to-end tests of the engine on real macOS file systems.
//!
//! The engine runs with the real `FsEventsSource`, `MacPathStat` and
//! `MacScanner` over a private temporary directory (canonicalized, so it
//! lives under `/private/var/...` like the paths FSEvents reports).
//!
//! Contract these tests rely on:
//!
//! - fx-engine has fx-fs-macos as a macOS-only dev-dependency.
//! - `fx_fs_macos::fsevents::source::FsEventsSource::new(StreamConfig)`
//!   (`fx_fs::ChangeSource`), `fx_fs_macos::path_stat::MacPathStat::new()`
//!   (`fx_fs::PathStat`) and `fx_fs_macos::enumerator::MacScanner::new(
//!   ScanOptions)` (`fx_fs::VolumeEnumerator`) plug straight into
//!   `Engine::start` / `Engine::open` with `EngineConfig::new(root)` where
//!   `root` is the watched directory.
//! - A create, a file rename, a folder move (reparent, carrying its
//!   contents) and a delete each show up in `Engine::search`. These tests
//!   allow a generous [`CHANGE_LIMIT`] (10 s), so a busy machine or a debug
//!   build does not fail them; the 1 s freshness gate is measured by the
//!   `freshness_bench` example in a release build.
//! - Gate: offline catch-up. Start with a store path, checkpoint, shut
//!   down, make about 200 scripted changes while nothing runs, then
//!   `Engine::open` again: the store is served at once and resumed from its
//!   saved `ResumeCursor::FsEvents`, and the index ends up equal to a fresh
//!   `MacScanner` scan of the directory (same paths, file keys, kinds and
//!   file sizes), with every merged sort order equal to `Order::build`.
//!
//! Tests only create and remove their own temporary directories.

#![cfg(target_os = "macos")]

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fx_engine::{ENGINE_SORT_KEYS, Engine, EngineConfig, EngineSnapshot};
use fx_fs::{ChangeSource, VolumeEnumerator};
use fx_fs_macos::enumerator::MacScanner;
use fx_fs_macos::fsevents::source::FsEventsSource;
use fx_fs_macos::fsevents::stream::StreamConfig;
use fx_fs_macos::path_stat::MacPathStat;
use fx_fs_macos::scan::ScanOptions;
use fx_index::Index;
use fx_model::{Change, Entry, EntryId, EntryKind, FileKey, ResumeCursor, StartOutcome};
use fx_sort::{Direction, Order, SortKey};

/// How long one change may take to reach search in these tests: far above
/// the 1 s freshness gate, which `freshness_bench` measures, so a loaded
/// machine or a debug build does not fail them.
const CHANGE_LIMIT: Duration = Duration::from_secs(10);
/// How long setup steps (initial scans, catch-up) may take in debug builds.
const SETUP: Duration = Duration::from_secs(20);

// ------------------------------------------------------------- helpers

/// A unique scratch directory, removed on drop. `path` is canonical.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(name: &str) -> TempDir {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "fx-engine-e2e-{name}-{}-{nanos}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).expect("create scratch dir");
        let path = fs::canonicalize(&dir).expect("canonicalize scratch dir");
        TempDir { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn source(root: &Path) -> FsEventsSource {
    let mut cfg = StreamConfig::new(root.to_path_buf());
    cfg.path_filter = Some(root.to_path_buf());
    cfg.no_defer = true;
    FsEventsSource::new(cfg)
}

fn scanner() -> MacScanner {
    MacScanner::new(ScanOptions::default())
}

fn config(root: &Path) -> EngineConfig {
    EngineConfig::new(root).with_batch_window(Duration::from_millis(50))
}

fn start(root: &Path) -> Engine {
    Engine::start(
        Index::new(),
        config(root),
        source(root),
        MacPathStat::new(),
        scanner(),
    )
    .expect("engine starts")
}

/// What one engine run did with its change source and enumerator, recorded
/// by [`ProbedSource`] and [`ProbedScanner`].
#[derive(Default)]
struct Probe {
    /// Every `StartOutcome` the real source returned, in order.
    outcomes: Mutex<Vec<StartOutcome>>,
    /// The real source's `history_done()` after the latest `next_batch`.
    history_done: AtomicBool,
    /// Enumerations of the watched root itself (full scans).
    root_enumerations: AtomicUsize,
    /// Enumerations of folders below the root (subtree rescans).
    subtree_enumerations: AtomicUsize,
}

impl Probe {
    fn outcomes(&self) -> Vec<StartOutcome> {
        self.outcomes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

/// The real `FsEventsSource`, forwarding every call and recording what it
/// returned in a shared [`Probe`].
struct ProbedSource {
    inner: FsEventsSource,
    probe: Arc<Probe>,
}

impl ChangeSource for ProbedSource {
    fn start(&mut self, from: Option<&ResumeCursor>) -> fx_fs::Result<StartOutcome> {
        let outcome = self.inner.start(from)?;
        self.probe
            .outcomes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(outcome);
        self.probe
            .history_done
            .store(self.inner.history_done(), Ordering::SeqCst);
        Ok(outcome)
    }

    fn next_batch(&mut self, deadline: Instant) -> fx_fs::Result<Vec<Change>> {
        let batch = self.inner.next_batch(deadline);
        self.probe
            .history_done
            .store(self.inner.history_done(), Ordering::SeqCst);
        batch
    }

    fn cursor(&self) -> ResumeCursor {
        self.inner.cursor()
    }
}

/// The real `MacScanner`, forwarding every call and counting how often the
/// watched root is enumerated in full versus a folder below it.
struct ProbedScanner {
    inner: MacScanner,
    root: PathBuf,
    probe: Arc<Probe>,
}

impl VolumeEnumerator for ProbedScanner {
    fn enumerate(&self, root: &Path, sink: &mut dyn FnMut(Entry)) -> fx_fs::Result<()> {
        if root == self.root {
            self.probe.root_enumerations.fetch_add(1, Ordering::SeqCst);
        } else {
            self.probe
                .subtree_enumerations
                .fetch_add(1, Ordering::SeqCst);
        }
        self.inner.enumerate(root, sink)
    }
}

/// Opens an engine with a store, with the real source and scanner wrapped
/// so `probe` records what they did.
fn open(root: &Path, store: &Path, probe: &Arc<Probe>) -> Engine {
    let cfg = config(root)
        .with_store_path(store)
        .with_checkpoint_idle(Duration::from_secs(3600));
    let source = ProbedSource {
        inner: source(root),
        probe: Arc::clone(probe),
    };
    let enumerator = ProbedScanner {
        inner: scanner(),
        root: root.to_path_buf(),
        probe: Arc::clone(probe),
    };
    Engine::open(cfg, source, MacPathStat::new(), enumerator).expect("engine opens")
}

fn root_ino(root: &Path) -> u64 {
    fs::symlink_metadata(root).expect("stat root").ino()
}

fn touch(path: &Path, bytes: usize) {
    let mut f = fs::File::create(path).expect("create file");
    f.write_all(&vec![b'x'; bytes]).expect("write file");
}

/// What the comparison with a fresh scan looks at, per relative path.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Item {
    key: FileKey,
    kind: EntryKind,
    /// File size; 0 for folders and symlinks.
    size: u64,
}

/// Relative paths of `entries` (by key), skipping the root itself. An
/// entry whose parent is missing, `None` or the root sits directly in the
/// root.
fn by_path(entries: &HashMap<FileKey, Entry>, root_ino: u64) -> BTreeMap<String, Item> {
    let mut out = BTreeMap::new();
    for e in entries.values() {
        if e.key.file_id == root_ino {
            continue;
        }
        let mut parts = vec![e.name.clone()];
        let mut parent = e.parent;
        let mut guard = 0;
        while let Some(pk) = parent {
            if pk.file_id == root_ino {
                break;
            }
            let Some(p) = entries.get(&pk) else { break };
            parts.push(p.name.clone());
            parent = p.parent;
            guard += 1;
            assert!(guard < 10_000, "parent cycle at {:?}", e.key);
        }
        parts.reverse();
        let size = if e.kind == EntryKind::File { e.size } else { 0 };
        let item = Item {
            key: e.key,
            kind: e.kind,
            size,
        };
        let path = parts.join("/");
        assert!(
            out.insert(path.clone(), item).is_none(),
            "two entries at {path}"
        );
    }
    out
}

/// Every live entry of a published snapshot, by key.
fn snapshot_entries(snap: &EngineSnapshot) -> HashMap<FileKey, Entry> {
    let idx: &fx_index::Snapshot = snap.index();
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

/// A fresh `MacScanner` scan of `root`, by relative path.
fn fresh_scan(root: &Path) -> BTreeMap<String, Item> {
    let mut entries = HashMap::new();
    scanner()
        .enumerate(root, &mut |e: Entry| {
            entries.insert(e.key, e);
        })
        .expect("fresh scan");
    by_path(&entries, root_ino(root))
}

fn indexed(snap: &EngineSnapshot, root: &Path) -> BTreeMap<String, Item> {
    by_path(&snapshot_entries(snap), root_ino(root))
}

/// Polls until the published index equals a fresh scan of `root`.
fn wait_for_fresh_scan(engine: &Engine, root: &Path, what: &str) -> Arc<EngineSnapshot> {
    let end = Instant::now() + SETUP;
    loop {
        let want = fresh_scan(root);
        let snap = engine.snapshot();
        let got = indexed(&snap, root);
        if got == want {
            return snap;
        }
        if Instant::now() >= end {
            let missing: Vec<_> = want.keys().filter(|k| !got.contains_key(*k)).collect();
            let extra: Vec<_> = got.keys().filter(|k| !want.contains_key(*k)).collect();
            let differ: Vec<_> = want
                .iter()
                .filter(|(k, v)| got.get(*k).is_some_and(|g| g != *v))
                .map(|(k, _)| k)
                .collect();
            panic!(
                "timed out waiting for: {what}; missing {missing:?}, extra {extra:?}, differ {differ:?}"
            );
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Relative paths of the search hits for `query`, resolved in the snapshot
/// that was searched; `None` when a newer snapshot was published meanwhile.
fn search_paths(engine: &Engine, root: &Path, query: &str) -> Option<Vec<String>> {
    let snap = engine.snapshot();
    let page = engine
        .search(
            query,
            fx_engine::QueryOptions::default(),
            SortKey::Name,
            Direction::Ascending,
            0,
            10_000,
        )
        .expect("search succeeds");
    if page.generation.get() != snap.generation() {
        return None;
    }
    let entries = snapshot_entries(&snap);
    let paths = by_path(&entries, root_ino(root));
    let idx: &fx_index::Snapshot = snap.index();
    let mut out: Vec<String> = page
        .ids
        .iter()
        .map(|&id| {
            let key = idx.key(EntryId(id));
            paths
                .iter()
                .find(|(_, item)| item.key == key)
                .map(|(p, _)| p.clone())
                .unwrap_or_else(|| panic!("hit {id} has no path"))
        })
        .collect();
    out.sort();
    assert_eq!(page.total, out.len(), "{query}: total counts every hit");
    Some(out)
}

/// Polls the search for `query` until its sorted hit paths equal `want`,
/// and returns how long that took. Fails after `limit`.
fn wait_for_search(
    engine: &Engine,
    root: &Path,
    query: &str,
    want: &[&str],
    limit: Duration,
) -> Duration {
    let mut want: Vec<String> = want.iter().map(|s| s.to_string()).collect();
    want.sort();
    let t0 = Instant::now();
    let mut last = None;
    loop {
        if let Some(got) = search_paths(engine, root, query) {
            if got == want {
                return t0.elapsed();
            }
            last = Some(got);
        }
        assert!(
            t0.elapsed() < limit,
            "search {query:?} did not reach {want:?} within {limit:?}; last {last:?}"
        );
        thread::sleep(Duration::from_millis(2));
    }
}

/// Like `wait_for_search` with [`CHANGE_LIMIT`].
fn fresh_within_limit(engine: &Engine, root: &Path, what: &str, query: &str, want: &[&str]) {
    let took = wait_for_search(engine, root, query, want, CHANGE_LIMIT);
    assert!(
        took <= CHANGE_LIMIT,
        "{what} took {took:?}, limit is {CHANGE_LIMIT:?}"
    );
}

/// Starts an engine over `root` and waits until its initial scan is in,
/// and until FSEvents delivers a warm-up change.
fn started(root: &Path) -> Engine {
    let engine = start(root);
    wait_for_fresh_scan(&engine, root, "initial scan");
    touch(&root.join("warmup_probe.txt"), 1);
    wait_for_search(&engine, root, "warmup_probe", &["warmup_probe.txt"], SETUP);
    engine
}

// ------------------------------------------------- changes reach search

#[test]
fn created_file_appears_in_search() {
    let tmp = TempDir::new("create");
    let root = &tmp.path;
    fs::create_dir(root.join("docs")).unwrap();
    touch(&root.join("docs/existing.txt"), 3);
    let engine = started(root);
    wait_for_search(&engine, root, "existing", &["docs/existing.txt"], SETUP);

    touch(&root.join("docs/created_e2e.txt"), 5);
    fresh_within_limit(
        &engine,
        root,
        "create",
        "created_e2e",
        &["docs/created_e2e.txt"],
    );

    fs::create_dir(root.join("newfolder_e2e")).unwrap();
    fresh_within_limit(
        &engine,
        root,
        "folder create",
        "newfolder_e2e",
        &["newfolder_e2e"],
    );
    engine.shutdown();
}

#[test]
fn renamed_file_appears_in_search() {
    let tmp = TempDir::new("rename");
    let root = &tmp.path;
    fs::create_dir(root.join("docs")).unwrap();
    touch(&root.join("docs/before_name.txt"), 7);
    let engine = started(root);
    wait_for_search(
        &engine,
        root,
        "before_name",
        &["docs/before_name.txt"],
        SETUP,
    );

    fs::rename(
        root.join("docs/before_name.txt"),
        root.join("docs/after_name.txt"),
    )
    .unwrap();
    fresh_within_limit(
        &engine,
        root,
        "rename (new name)",
        "after_name",
        &["docs/after_name.txt"],
    );
    fresh_within_limit(&engine, root, "rename (old name)", "before_name", &[]);
    engine.shutdown();
}

#[test]
fn moved_folder_carries_its_contents_in_search() {
    let tmp = TempDir::new("move");
    let root = &tmp.path;
    fs::create_dir_all(root.join("a/moving_dir/nested")).unwrap();
    fs::create_dir(root.join("b")).unwrap();
    touch(&root.join("a/moving_dir/inner_file.txt"), 11);
    touch(&root.join("a/moving_dir/nested/deep_file.txt"), 13);
    let engine = started(root);
    wait_for_search(
        &engine,
        root,
        "deep_file",
        &["a/moving_dir/nested/deep_file.txt"],
        SETUP,
    );

    // Reparent: a/moving_dir -> b/moving_dir.
    fs::rename(root.join("a/moving_dir"), root.join("b/moving_dir")).unwrap();
    fresh_within_limit(
        &engine,
        root,
        "folder move",
        "deep_file",
        &["b/moving_dir/nested/deep_file.txt"],
    );
    fresh_within_limit(
        &engine,
        root,
        "folder move (sibling file)",
        "inner_file",
        &["b/moving_dir/inner_file.txt"],
    );

    // Rename in place: b/moving_dir -> b/renamed_dir.
    fs::rename(root.join("b/moving_dir"), root.join("b/renamed_dir")).unwrap();
    fresh_within_limit(
        &engine,
        root,
        "folder rename",
        "deep_file",
        &["b/renamed_dir/nested/deep_file.txt"],
    );
    fresh_within_limit(&engine, root, "folder rename (old name)", "moving_dir", &[]);

    let snap = engine.snapshot();
    assert_orders_match(&snap);
    engine.shutdown();
}

#[test]
fn deleted_file_and_folder_leave_search() {
    let tmp = TempDir::new("delete");
    let root = &tmp.path;
    fs::create_dir_all(root.join("doomed_dir/sub")).unwrap();
    touch(&root.join("doomed_file.txt"), 2);
    touch(&root.join("doomed_dir/sub/doomed_child.txt"), 2);
    touch(&root.join("survivor.txt"), 2);
    let engine = started(root);
    wait_for_search(&engine, root, "doomed_file", &["doomed_file.txt"], SETUP);
    wait_for_search(
        &engine,
        root,
        "doomed_child",
        &["doomed_dir/sub/doomed_child.txt"],
        SETUP,
    );

    fs::remove_file(root.join("doomed_file.txt")).unwrap();
    fresh_within_limit(&engine, root, "file delete", "doomed_file", &[]);

    fs::remove_dir_all(root.join("doomed_dir")).unwrap();
    fresh_within_limit(&engine, root, "folder delete", "doomed", &[]);
    wait_for_search(&engine, root, "survivor", &["survivor.txt"], CHANGE_LIMIT);
    engine.shutdown();
}

// ------------------------------------------------------ offline catch-up

/// The starting tree: d0..d9, each holding f0.txt..f9.txt.
fn initial_tree(root: &Path) {
    for k in 0..10 {
        let d = root.join(format!("d{k}"));
        fs::create_dir(&d).unwrap();
        for j in 0..10 {
            touch(&d.join(format!("f{j}.txt")), k * 10 + j + 1);
        }
    }
}

/// About 200 scripted changes: creates, renames, writes, deletes,
/// transient files, a new nested tree, folder moves and renames, a moved
/// file and a deleted folder. Returns how many operations ran.
fn scripted_changes(root: &Path) -> usize {
    let mut ops = 0;
    for k in 0..10 {
        let d = root.join(format!("d{k}"));
        for j in 0..6 {
            touch(&d.join(format!("new_{k}_{j}.txt")), 100 + j);
            ops += 1;
        }
        for j in 0..4 {
            fs::rename(
                d.join(format!("f{j}.txt")),
                d.join(format!("ren_{k}_{j}.txt")),
            )
            .unwrap();
            ops += 1;
        }
        fs::remove_file(d.join("f9.txt")).unwrap();
        ops += 1;
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(d.join("f8.txt"))
            .unwrap();
        f.write_all(b"grown").unwrap();
        ops += 1;
    }
    for i in 0..20 {
        let p = root.join(format!("transient_{i}.tmp"));
        touch(&p, 1);
        fs::remove_file(&p).unwrap();
        ops += 2;
    }
    let fresh = root.join("fresh");
    fs::create_dir(&fresh).unwrap();
    ops += 1;
    for s in 0..4 {
        let sub = fresh.join(format!("sub{s}"));
        fs::create_dir(&sub).unwrap();
        ops += 1;
        for l in 0..4 {
            touch(&sub.join(format!("leaf{l}.txt")), s * 4 + l);
            ops += 1;
        }
    }
    // Folder reparents, carrying their contents.
    for k in 0..5 {
        fs::rename(
            root.join(format!("d{k}")),
            root.join(format!("d{}/moved_{k}", k + 5)),
        )
        .unwrap();
        ops += 1;
    }
    // Folder rename in place (d9 now holds moved_4).
    fs::rename(root.join("d9"), root.join("d9_renamed")).unwrap();
    ops += 1;
    // Folder delete with contents (d6 holds moved_1).
    fs::remove_dir_all(root.join("d6")).unwrap();
    ops += 1;
    // A folder and a file reparented across the tree.
    fs::rename(root.join("fresh/sub0"), root.join("d7/sub0_moved")).unwrap();
    fs::rename(
        root.join("d8/ren_8_0.txt"),
        root.join("fresh/ren_moved.txt"),
    )
    .unwrap();
    ops += 2;
    ops
}

#[test]
fn offline_catch_up_from_saved_cursor_equals_a_fresh_scan() {
    let tmp = TempDir::new("catchup");
    let store_dir = TempDir::new("catchup-store");
    let root = &tmp.path;
    let store = store_dir.path.join("index.fxdb");
    initial_tree(root);

    // First run: full scan, checkpoint, orderly shutdown.
    let first_probe = Arc::new(Probe::default());
    let engine = open(root, &store, &first_probe);
    let before = wait_for_fresh_scan(&engine, root, "first run initial scan");
    assert!(
        first_probe.root_enumerations.load(Ordering::SeqCst) >= 1,
        "the probe sees the first run's full scan of the root"
    );
    assert!(
        matches!(before.cursor(), Some(ResumeCursor::FsEvents { .. })),
        "a published snapshot carries an FSEvents cursor, got {:?}",
        before.cursor()
    );
    engine.checkpoint().expect("checkpoint saves");
    let saved = engine.snapshot().cursor();
    let Some(ResumeCursor::FsEvents {
        event_id: saved_id,
        db_uuid: saved_uuid,
    }) = saved
    else {
        panic!("saved cursor is an FSEvents cursor, got {saved:?}");
    };
    let indexed_before = indexed(&before, root);
    engine.shutdown();

    // Offline: nothing watches the directory.
    let ops = scripted_changes(root);
    assert!(ops >= 180, "about 200 scripted changes, ran {ops}");
    let want = fresh_scan(root);
    assert_ne!(want, indexed_before, "the script changed the tree");

    // Second run: served from the store at once, then caught up.
    let probe = Arc::new(Probe::default());
    let engine = open(root, &store, &probe);
    assert_eq!(
        probe.outcomes(),
        vec![StartOutcome::Resumed],
        "the restart resumes FSEvents from the saved cursor"
    );
    let first = engine.snapshot();
    assert!(
        first.index().live_ids().next().is_some(),
        "the reopened engine serves the stored index at once"
    );
    if first.generation() == 0 {
        assert_eq!(
            indexed(&first, root),
            indexed_before,
            "the first snapshot is the checkpointed index"
        );
    }
    let after = wait_for_fresh_scan(&engine, root, "offline catch-up");
    assert_eq!(indexed(&after, root), want);
    assert_orders_match(&after);
    match after.cursor() {
        Some(ResumeCursor::FsEvents { event_id, db_uuid }) => {
            assert_eq!(db_uuid, saved_uuid, "same FSEvents database");
            assert!(
                event_id >= saved_id,
                "cursor moved forward from {saved_id}, got {event_id}"
            );
        }
        other => panic!("caught-up snapshot carries an FSEvents cursor, got {other:?}"),
    }

    // The replayed history was handed out up to its HistoryDone marker
    // (the marker may trail the last replayed change by a batch).
    let end = Instant::now() + SETUP;
    while !probe.history_done.load(Ordering::SeqCst) {
        assert!(
            Instant::now() < end,
            "the resumed source never finished replaying its history"
        );
        thread::sleep(Duration::from_millis(20));
    }

    // The caught-up engine keeps watching live.
    touch(&root.join("fresh/after_catch_up.txt"), 4);
    wait_for_search(
        &engine,
        root,
        "after_catch_up",
        &["fresh/after_catch_up.txt"],
        SETUP,
    );

    // The catch-up came from the replayed history, not from a full rescan:
    // the second run never enumerated the whole root (subtree rescans of
    // changed folders are fine).
    assert_eq!(
        probe.root_enumerations.load(Ordering::SeqCst),
        0,
        "the resumed run enumerated the root in full ({} subtree rescans)",
        probe.subtree_enumerations.load(Ordering::SeqCst)
    );
    assert_eq!(
        probe.outcomes(),
        vec![StartOutcome::Resumed],
        "the source was started exactly once"
    );
    engine.shutdown();
}

/// Every merged order equals a fresh `Order::build` on the same index.
fn assert_orders_match(snap: &EngineSnapshot) {
    let idx: &fx_index::Snapshot = snap.index();
    for k in ENGINE_SORT_KEYS {
        let order = snap
            .order(k)
            .unwrap_or_else(|| panic!("engine keeps a {k:?} order"));
        order
            .validate(idx)
            .unwrap_or_else(|e| panic!("{k:?} order invalid: {e:?}"));
        assert_eq!(
            order.ids,
            Order::build(idx, k).ids,
            "merged {k:?} order != Order::build"
        );
    }
}
