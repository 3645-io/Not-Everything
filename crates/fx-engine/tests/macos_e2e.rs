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

use fx_engine::{ENGINE_SORT_KEYS, Engine, EngineConfig, EngineHealth, EngineSnapshot};
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
    /// Changes the real source handed out that ask for the whole root:
    /// `RescanVolume`, or `RescanSubtree` of the root or a folder above it.
    /// FSEvents sends those when it coalesces or drops events (a busy
    /// machine), and a full enumeration then is right.
    root_rescans_asked: AtomicUsize,
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
    /// The watched root, to recognise the rescans that cover it.
    root: PathBuf,
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
        if let Ok(changes) = &batch {
            let whole_root = changes
                .iter()
                .filter(|c| match c {
                    Change::RescanVolume => true,
                    Change::RescanSubtree(p) => self.root.starts_with(p),
                    _ => false,
                })
                .count();
            self.probe
                .root_rescans_asked
                .fetch_add(whole_root, Ordering::SeqCst);
        }
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
        root: root.to_path_buf(),
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
    // changed folders are fine), unless FSEvents itself asked for it. On a
    // busy machine it coalesces events into MustScanSubDirs on a folder
    // above the root, or drops them, and since run 2.1 the source passes
    // those on instead of losing them.
    let asked = probe.root_rescans_asked.load(Ordering::SeqCst);
    if asked == 0 {
        assert_eq!(
            probe.root_enumerations.load(Ordering::SeqCst),
            0,
            "the resumed run enumerated the root in full ({} subtree rescans)",
            probe.subtree_enumerations.load(Ordering::SeqCst)
        );
    } else {
        eprintln!(
            "offline catch-up: FSEvents asked for {asked} rescan(s) of the whole root; \
             {} full enumeration(s)",
            probe.root_enumerations.load(Ordering::SeqCst)
        );
    }
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

// ------------------------------------- run 2.1: gaps at start, end to end

/// The real `FsEventsSource`, except that `cursor()` reports a tampered
/// copy of the real cursor, so the engine's checkpoint stores it: the
/// next run then starts from a stored cursor the volume cannot resume.
struct TamperedCursorSource {
    inner: FsEventsSource,
    tamper: fn(ResumeCursor) -> ResumeCursor,
}

impl ChangeSource for TamperedCursorSource {
    fn start(&mut self, from: Option<&ResumeCursor>) -> fx_fs::Result<StartOutcome> {
        self.inner.start(from)
    }

    fn next_batch(&mut self, deadline: Instant) -> fx_fs::Result<Vec<Change>> {
        self.inner.next_batch(deadline)
    }

    fn cursor(&self) -> ResumeCursor {
        (self.tamper)(self.inner.cursor())
    }
}

/// First run with a tampered cursor checkpointed, offline changes, then a
/// second, honest run: the stored cursor is a gap, so the second run
/// rescans the root in full and ends equal to a fresh scan (0 lost
/// changes).
fn stored_cursor_gap_rescans_and_catches_up(name: &str, tamper: fn(ResumeCursor) -> ResumeCursor) {
    let tmp = TempDir::new(name);
    let store_dir = TempDir::new(&format!("{name}-store"));
    let root = &tmp.path;
    let store = store_dir.path.join("index.fxdb");
    initial_tree(root);

    let cfg = config(root)
        .with_store_path(&store)
        .with_checkpoint_idle(Duration::from_secs(3600));
    let first = Engine::open(
        cfg,
        TamperedCursorSource {
            inner: source(root),
            tamper,
        },
        MacPathStat::new(),
        scanner(),
    )
    .expect("first engine opens");
    wait_for_fresh_scan(&first, root, "first run initial scan");
    first.checkpoint().expect("checkpoint saves");
    let stored = first.snapshot().cursor();
    assert!(
        matches!(stored, Some(ResumeCursor::FsEvents { .. })),
        "the checkpointed snapshot carries the tampered FSEvents cursor, got {stored:?}"
    );
    first.shutdown();

    let ops = scripted_changes(root);
    assert!(ops >= 180, "about 200 scripted changes, ran {ops}");
    let want = fresh_scan(root);

    let probe = Arc::new(Probe::default());
    let engine = open(root, &store, &probe);
    assert_eq!(
        probe.outcomes(),
        vec![StartOutcome::GapDetected],
        "{name}: the stored cursor cannot resume"
    );
    let after = wait_for_fresh_scan(&engine, root, name);
    assert_eq!(indexed(&after, root), want);
    assert_orders_match(&after);
    assert!(
        probe.root_enumerations.load(Ordering::SeqCst) >= 1,
        "{name}: a gap rescans the root in full"
    );
    assert!(
        matches!(after.cursor(), Some(ResumeCursor::FsEvents { .. })),
        "{name}: the rescanned snapshot carries an FSEvents cursor, got {:?}",
        after.cursor()
    );
    assert_ne!(
        after.cursor(),
        stored,
        "{name}: the rescanned index carries a fresh cursor, not the stored one"
    );

    // The engine keeps watching live after the rescan.
    touch(&root.join("fresh/after_gap.txt"), 4);
    wait_for_search(&engine, root, "after_gap", &["fresh/after_gap.txt"], SETUP);
    engine.shutdown();
}

#[test]
fn stored_event_id_ahead_of_the_volume_rescans_and_catches_up() {
    stored_cursor_gap_rescans_and_catches_up("id-too-high", |c| match c {
        ResumeCursor::FsEvents { event_id, db_uuid } => ResumeCursor::FsEvents {
            // Far above any ID the volume can have reached (a store restored
            // from another machine, or an FSEvents database that was reset).
            event_id: event_id.saturating_add(1 << 50),
            db_uuid,
        },
        other => other,
    });
}

#[test]
fn stored_cursor_of_another_fsevents_database_rescans_and_catches_up() {
    stored_cursor_gap_rescans_and_catches_up("uuid-changed", |c| match c {
        ResumeCursor::FsEvents {
            event_id,
            mut db_uuid,
        } => {
            // The volume's FSEvents database was recreated: same IDs, new UUID.
            db_uuid[0] ^= 0xFF;
            db_uuid[15] ^= 0x5A;
            ResumeCursor::FsEvents { event_id, db_uuid }
        }
        other => other,
    });
}

// ----------------------------------- run 2.1: every rescan flag, end to end
//
// FSEvents cannot be made to send MustScanSubDirs, a dropped-events flag,
// EventIdsWrapped, RootChanged or Mount on demand. These tests mute the
// real stream while they change the tree, check that the index really
// missed the changes, then inject the one raw event FSEvents would send,
// routed exactly as `FsEventsSource` routes it (`map_event` with the
// stream's scope root, then `coalesce`). The index must then equal a fresh
// scan: the flag's change alone recovers every muted change.

use fx_fs_macos::fsevents::change::{coalesce, map_event};
use fx_fs_macos::fsevents::flags::EventFlags;

/// How long the muted stream must stay quiet before its reports of the
/// changes count as all discarded.
const QUIET: Duration = Duration::from_millis(700);

/// Shared between a [`FlagSource`] and its test.
#[derive(Default)]
struct FlagControl {
    /// While set, every change the real stream reports is discarded.
    muted: AtomicBool,
    /// When the muted stream last reported (and lost) a change.
    last_discard: Mutex<Option<Instant>>,
    /// Raw events to route and hand out with the next batch.
    inject: Mutex<Vec<(PathBuf, EventFlags)>>,
}

impl FlagControl {
    fn inject(&self, events: Vec<(PathBuf, EventFlags)>) {
        self.inject
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .extend(events);
    }

    /// Waits until the muted stream reported the changes and then stayed
    /// quiet for [`QUIET`].
    fn wait_until_discarded(&self, what: &str) {
        let end = Instant::now() + SETUP;
        loop {
            let last = *self
                .last_discard
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if last.is_some_and(|t| t.elapsed() >= QUIET) {
                return;
            }
            assert!(
                Instant::now() < end,
                "{what}: the muted stream never reported the changes"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}

/// The real `FsEventsSource` with mute and inject controls.
struct FlagSource {
    inner: FsEventsSource,
    /// The stream's scope root (its path filter), as `FsEventsSource`
    /// passes it to `map_event`.
    scope: PathBuf,
    ctl: Arc<FlagControl>,
}

impl FlagSource {
    fn injected(&self) -> Vec<Change> {
        let raw: Vec<(PathBuf, EventFlags)> = std::mem::take(
            &mut *self
                .ctl
                .inject
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
        let changes: Vec<Change> = raw
            .iter()
            .filter_map(|(path, flags)| map_event(path, *flags, false, &self.scope))
            .collect();
        coalesce(changes)
    }
}

impl ChangeSource for FlagSource {
    fn start(&mut self, from: Option<&ResumeCursor>) -> fx_fs::Result<StartOutcome> {
        self.inner.start(from)
    }

    fn next_batch(&mut self, deadline: Instant) -> fx_fs::Result<Vec<Change>> {
        let injected = self.injected();
        if !injected.is_empty() {
            return Ok(injected);
        }
        let mut batch = self.inner.next_batch(deadline)?;
        if self.ctl.muted.load(Ordering::SeqCst) && !batch.is_empty() {
            batch.clear();
            *self
                .ctl
                .last_discard
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Instant::now());
        }
        Ok(batch)
    }

    fn cursor(&self) -> ResumeCursor {
        self.inner.cursor()
    }
}

/// What enumeration the injected event must cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// The root in full (`RescanVolume`, or `RescanSubtree` of the root).
    Root,
    /// Folders below the root only (`RescanSubtree` of a folder).
    Subtree,
    /// No enumeration at all (`CheckPath` only).
    Check,
}

/// Starts an engine over `initial_tree`, mutes the stream, runs `change`,
/// asserts the index missed it, injects `inject(root)` and waits until the
/// index equals a fresh scan again, then checks the enumeration `expect`s.
fn muted_changes_are_recovered_by(
    name: &str,
    change: fn(&Path),
    inject: fn(&Path) -> Vec<(PathBuf, EventFlags)>,
    expect: Expect,
) {
    let tmp = TempDir::new(name);
    let root = &tmp.path;
    initial_tree(root);
    let probe = Arc::new(Probe::default());
    let ctl = Arc::new(FlagControl::default());
    let engine = Engine::start(
        Index::new(),
        config(root),
        FlagSource {
            inner: source(root),
            scope: root.clone(),
            ctl: Arc::clone(&ctl),
        },
        MacPathStat::new(),
        ProbedScanner {
            inner: scanner(),
            root: root.clone(),
            probe: Arc::clone(&probe),
        },
    )
    .expect("engine starts");
    wait_for_fresh_scan(&engine, root, &format!("{name}: initial scan"));

    ctl.muted.store(true, Ordering::SeqCst);
    change(root);
    ctl.wait_until_discarded(name);
    let want = fresh_scan(root);
    assert_ne!(
        indexed(&engine.snapshot(), root),
        want,
        "{name}: the muted changes must be missing from the index before the injected event"
    );

    let roots = probe.root_enumerations.load(Ordering::SeqCst);
    let subtrees = probe.subtree_enumerations.load(Ordering::SeqCst);
    // The stream stays muted: only the injected event can repair the index.
    ctl.inject(inject(root));
    let after = wait_for_fresh_scan(&engine, root, name);
    assert_orders_match(&after);

    let roots = probe.root_enumerations.load(Ordering::SeqCst) - roots;
    let subtrees = probe.subtree_enumerations.load(Ordering::SeqCst) - subtrees;
    match expect {
        Expect::Root => assert!(roots >= 1, "{name}: the root was not rescanned"),
        Expect::Subtree => {
            assert_eq!(roots, 0, "{name}: the root was rescanned in full");
            assert!(subtrees >= 1, "{name}: no folder was rescanned");
        }
        Expect::Check => assert_eq!(
            (roots, subtrees),
            (0, 0),
            "{name}: a path check enumerated folders"
        ),
    }
    engine.shutdown();
}

/// Creates, renames, writes and deletes inside `d3`, including a new
/// nested folder.
fn change_inside_d3(root: &Path) {
    let d = root.join("d3");
    touch(&d.join("new_a.txt"), 5);
    touch(&d.join("new_b.txt"), 6);
    fs::rename(d.join("f0.txt"), d.join("f0_renamed.txt")).unwrap();
    fs::remove_file(d.join("f1.txt")).unwrap();
    fs::create_dir_all(d.join("nested/deeper")).unwrap();
    touch(&d.join("nested/deeper/leaf.txt"), 7);
}

/// Changes in `d3` and in `d7`, so a rescan of only one of them is not
/// enough.
fn change_in_two_folders(root: &Path) {
    change_inside_d3(root);
    let d = root.join("d7");
    touch(&d.join("other_new.txt"), 9);
    fs::remove_file(d.join("f4.txt")).unwrap();
    fs::rename(d.join("f5.txt"), root.join("d2/f5_moved.txt")).unwrap();
}

fn at(root: &Path, rel: &str, flags: EventFlags) -> Vec<(PathBuf, EventFlags)> {
    vec![(root.join(rel), flags)]
}

#[test]
fn must_scan_sub_dirs_rescans_the_folder() {
    muted_changes_are_recovered_by(
        "flag-must-scan",
        change_inside_d3,
        |root| at(root, "d3", EventFlags::MUST_SCAN_SUB_DIRS),
        Expect::Subtree,
    );
}

#[test]
fn must_scan_sub_dirs_above_the_root_rescans_the_root() {
    // Hierarchical coalescing reported on a folder above the indexed root:
    // the stream's filter used to drop it.
    muted_changes_are_recovered_by(
        "flag-must-scan-above",
        change_in_two_folders,
        |root| {
            vec![(
                root.parent().expect("root has a parent").to_path_buf(),
                EventFlags::MUST_SCAN_SUB_DIRS,
            )]
        },
        Expect::Root,
    );
}

#[test]
fn user_dropped_rescans_everything_watched() {
    // Reported on d3 only, but the changes in d7 must come back too.
    muted_changes_are_recovered_by(
        "flag-user-dropped",
        change_in_two_folders,
        |root| {
            at(
                root,
                "d3",
                EventFlags::MUST_SCAN_SUB_DIRS | EventFlags::USER_DROPPED,
            )
        },
        Expect::Root,
    );
}

#[test]
fn kernel_dropped_on_the_volume_root_rescans_everything_watched() {
    muted_changes_are_recovered_by(
        "flag-kernel-dropped",
        change_in_two_folders,
        |_| {
            vec![(
                PathBuf::from("/"),
                EventFlags::MUST_SCAN_SUB_DIRS | EventFlags::KERNEL_DROPPED,
            )]
        },
        Expect::Root,
    );
}

#[test]
fn event_ids_wrapped_rescans_the_volume() {
    muted_changes_are_recovered_by(
        "flag-ids-wrapped",
        change_in_two_folders,
        |_| vec![(PathBuf::from("/"), EventFlags::EVENT_IDS_WRAPPED)],
        Expect::Root,
    );
}

#[test]
fn root_changed_rescans_the_volume() {
    muted_changes_are_recovered_by(
        "flag-root-changed",
        change_in_two_folders,
        |root| vec![(root.to_path_buf(), EventFlags::ROOT_CHANGED)],
        Expect::Root,
    );
}

#[test]
fn unmount_of_the_root_rescans_the_volume() {
    muted_changes_are_recovered_by(
        "flag-unmount-root",
        change_in_two_folders,
        |root| {
            vec![(
                root.to_path_buf(),
                EventFlags::UNMOUNT | EventFlags::ITEM_IS_DIR,
            )]
        },
        Expect::Root,
    );
}

#[test]
fn mount_on_the_root_rescans_the_root() {
    muted_changes_are_recovered_by(
        "flag-mount-root",
        change_in_two_folders,
        |root| {
            vec![(
                root.to_path_buf(),
                EventFlags::MOUNT | EventFlags::ITEM_IS_DIR,
            )]
        },
        Expect::Root,
    );
}

#[test]
fn flag_word_none_rescans_the_folder() {
    // kFSEventStreamEventFlagNone: "some change in the directory".
    muted_changes_are_recovered_by(
        "flag-none",
        change_inside_d3,
        |root| at(root, "d3", EventFlags::NONE),
        Expect::Subtree,
    );
}

#[test]
#[ignore = "engine follow-up (run 2.1): Writer::rescan_subtree on a path that is a file \
            does nothing, because the scanner refuses to enumerate a non-folder and the \
            writer drops any walk error other than NotFound; the source only asks for \
            subtree rescans of folders, so this is not reached today"]
fn subtree_rescan_of_a_file_path_updates_the_file() {
    muted_changes_are_recovered_by(
        "flag-none-on-file",
        |root| {
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(root.join("d1/f5.txt"))
                .unwrap();
            f.write_all(b"grown").unwrap();
        },
        // A flag word with no item bit names a folder in FSEvents.h; on a
        // file path the subtree rescan it maps to must still update it.
        |root| at(root, "d1/f5.txt", EventFlags::NONE),
        Expect::Subtree,
    );
}

#[test]
fn folder_rename_pair_rescans_both_names() {
    muted_changes_are_recovered_by(
        "flag-folder-rename",
        |root| {
            fs::rename(root.join("d3"), root.join("d3_renamed")).unwrap();
            touch(&root.join("d3_renamed/inside_after.txt"), 3);
            fs::remove_file(root.join("d3_renamed/f2.txt")).unwrap();
        },
        |root| {
            let renamed = EventFlags::ITEM_RENAMED | EventFlags::ITEM_IS_DIR;
            vec![
                (root.join("d3"), renamed),
                (root.join("d3_renamed"), renamed),
            ]
        },
        Expect::Subtree,
    );
}

#[test]
fn file_rename_pairs_check_each_side() {
    muted_changes_are_recovered_by(
        "flag-file-rename",
        |root| {
            fs::rename(root.join("d4/f2.txt"), root.join("d4/f2_renamed.txt")).unwrap();
            fs::rename(root.join("d4/f3.txt"), root.join("d5/f3_moved.txt")).unwrap();
        },
        |root| {
            let renamed = EventFlags::ITEM_RENAMED | EventFlags::ITEM_IS_FILE;
            [
                "d4/f2.txt",
                "d4/f2_renamed.txt",
                "d4/f3.txt",
                "d5/f3_moved.txt",
            ]
            .iter()
            .map(|rel| (root.join(rel), renamed))
            .collect()
        },
        Expect::Check,
    );
}

#[test]
fn coalesced_create_and_remove_checks_the_path() {
    muted_changes_are_recovered_by(
        "flag-create-remove",
        |root| {
            // Gone again within the latency window: nothing to index.
            touch(&root.join("d2/transient.txt"), 1);
            fs::remove_file(root.join("d2/transient.txt")).unwrap();
            // Removed and created again: a new file at an old path.
            fs::remove_file(root.join("d2/f6.txt")).unwrap();
            touch(&root.join("d2/f6.txt"), 66);
            // Created, removed, created: present at the end.
            touch(&root.join("d2/kept.txt"), 2);
            fs::remove_file(root.join("d2/kept.txt")).unwrap();
            touch(&root.join("d2/kept.txt"), 3);
        },
        |root| {
            let both =
                EventFlags::ITEM_CREATED | EventFlags::ITEM_REMOVED | EventFlags::ITEM_IS_FILE;
            ["d2/transient.txt", "d2/f6.txt", "d2/kept.txt"]
                .iter()
                .map(|rel| (root.join(rel), both))
                .collect()
        },
        Expect::Check,
    );
}

#[test]
fn own_event_and_modified_check_the_path() {
    muted_changes_are_recovered_by(
        "flag-own-event",
        |root| {
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(root.join("d1/f5.txt"))
                .unwrap();
            f.write_all(b"grown by this process").unwrap();
        },
        |root| {
            at(
                root,
                "d1/f5.txt",
                EventFlags::OWN_EVENT | EventFlags::ITEM_MODIFIED | EventFlags::ITEM_IS_FILE,
            )
        },
        Expect::Check,
    );
}

#[test]
fn cloned_file_checks_the_path() {
    muted_changes_are_recovered_by(
        "flag-cloned",
        |root| {
            // `fs::copy` clones on APFS (`fclonefileat`).
            fs::copy(root.join("d1/f1.txt"), root.join("d1/f1_clone.txt")).unwrap();
        },
        |root| {
            at(
                root,
                "d1/f1_clone.txt",
                EventFlags::ITEM_CREATED | EventFlags::ITEM_CLONED | EventFlags::ITEM_IS_FILE,
            )
        },
        Expect::Check,
    );
}

// ------------------- run 2.1: a folder above the root moves away and back

/// The live entries of the latest snapshot.
fn live_count(engine: &Engine) -> usize {
    engine.snapshot().index().live_ids().count()
}

#[test]
fn renaming_a_folder_above_the_root_empties_then_restores_the_index() {
    // The indexed root is outer/inner. Renaming outer moves the root away:
    // FSEvents reports only `outer` (above the stream's filter), which the
    // filter used to drop, so the index kept listing paths that no longer
    // existed. No muting here: real FSEvents end to end.
    let tmp = TempDir::new("above-root");
    let outer = tmp.path.join("outer");
    let root = outer.join("inner");
    fs::create_dir_all(&root).unwrap();
    initial_tree(&root);
    let engine = Engine::start(
        Index::new(),
        config(&root),
        source(&root),
        MacPathStat::new(),
        scanner(),
    )
    .expect("engine starts");
    wait_for_fresh_scan(&engine, &root, "initial scan");
    assert!(live_count(&engine) > 100);

    let moved = tmp.path.join("outer_moved");
    fs::rename(&outer, &moved).unwrap();
    // The move empties the index. If FSEvents also dropped events on a busy
    // machine, the volume rescan that follows cannot enumerate a missing
    // root: the engine then keeps the last index and reports `Recovering`
    // until the root is back, which also shows the move was seen.
    let end = Instant::now() + SETUP;
    while live_count(&engine) != 0 && engine.health() == EngineHealth::Live {
        assert!(
            Instant::now() < end,
            "the index still lists {} entries under a root that moved away",
            live_count(&engine)
        );
        thread::sleep(Duration::from_millis(20));
    }

    fs::rename(&moved, &outer).unwrap();
    touch(&root.join("d0/back_again.txt"), 8);
    let after = wait_for_fresh_scan(&engine, &root, "root moved back");
    assert_orders_match(&after);
    engine.shutdown();
}
