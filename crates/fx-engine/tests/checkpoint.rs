//! Checkpoint policy and startup from fx-store, with fakes, on every
//! platform.
//!
//! Contract these tests rely on (in addition to `tests/common/mod.rs`):
//!
//! - `EngineConfig::with_store_path(path)` sets the checkpoint file (the
//!   engine creates its parent directory when saving);
//!   `EngineConfig::with_checkpoint_idle(Duration)` sets how long after the
//!   last published change an idle checkpoint is written;
//!   `EngineConfig::checkpoint_idle()` reads it, default
//!   `fx_engine::DEFAULT_CHECKPOINT_IDLE` (30 s).
//! - `Engine::checkpoint(&self) -> Result<_, E: Debug>` saves on request and
//!   returns once the file holds the latest published snapshot: its index,
//!   its sort orders and the volume's resume cursor, written atomically by
//!   `fx_store::write_checkpoint` with the clean-shutdown flag cleared.
//! - `Engine::shutdown(self)` (orderly shutdown) stops the writer and, when a
//!   store path is configured, saves a final checkpoint with the
//!   clean-shutdown flag set.
//! - `Engine::open(config, source, stat, enumerator) -> io::Result<Engine>`
//!   loads the store at the config's store path. A valid store serves its
//!   index and hands its saved cursor to `ChangeSource::start`; a missing or
//!   rejected (corrupt) store, or a `GapDetected` start, falls back to a full
//!   rescan. Opening never fails because the store file is bad.
//!
//! fx-engine needs fx-store as a dependency for these tests.

mod common;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::*;
use fx_engine::{
    CheckpointError, DEFAULT_CHECKPOINT_IDLE, DEFAULT_CHECKPOINT_MAX_DELAY, Engine, EngineConfig,
    QuitSave,
};
use fx_model::{Entry, EntryId, FileKey, ResumeCursor, StartOutcome};
use fx_store::MappedIndex;

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
            "fx-engine-checkpoint-{name}-{}-{nanos}-{}",
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

/// The saved entries and the only saved cursor of the store at `path`.
fn read_store(path: &Path) -> (HashMap<FileKey, Entry>, ResumeCursor, bool) {
    let mapped = MappedIndex::open(path).expect("store loads");
    mapped.verify_checksums().expect("store checksums");
    let cursors = mapped.cursors();
    assert_eq!(
        cursors.len(),
        1,
        "one volume, one saved cursor: {cursors:?}"
    );
    let cursor = cursors[0].1;
    let index = mapped.to_index().expect("store converts to an index");
    let entries = index_entries(&index.snapshot());
    (entries, cursor, mapped.clean_shutdown())
}

fn sample_tree() -> FakeTree {
    let tree = FakeTree::new();
    tree.mkdir("/vol/a");
    tree.mkdir("/vol/a/inner");
    tree.mkdir("/vol/b");
    tree.touch("/vol/a/x.txt", 10);
    tree.touch("/vol/a/inner/deep.txt", 20);
    tree.touch("/vol/b/y.txt", 30);
    tree.add_files("/vol/b", "bulk", 20);
    tree
}

/// Opens an engine over a missing store, waits for the full scan, applies
/// one live batch, and returns the engine with its published cursor.
fn first_run(tree: &FakeTree, source: &FakeSource, store: &Path) -> (Engine, ResumeCursor) {
    let engine = open(tree, source, store_config(store));
    wait_for_tree(&engine, tree, "first full scan");
    tree.touch("/vol/a/live.txt", 7);
    source.push(vec![check("/vol/a/live.txt")]);
    let snap = wait_for_tree(&engine, tree, "live batch");
    assert_orders_match(&snap);
    let cursor = wait_for(&engine, "cursor of the live batch", |s| {
        s.cursor() == Some(source.current_cursor())
    })
    .cursor()
    .expect("published cursor");
    (engine, cursor)
}

#[test]
fn the_idle_checkpoint_is_off_by_default() {
    // A quiet laptop at night would otherwise save several times a day.
    assert_eq!(DEFAULT_CHECKPOINT_IDLE, None);
    assert_eq!(
        EngineConfig::new(ROOT).checkpoint_idle(),
        DEFAULT_CHECKPOINT_IDLE
    );
    let cfg = EngineConfig::new(ROOT).with_checkpoint_idle(Duration::from_millis(250));
    assert_eq!(cfg.checkpoint_idle(), Some(Duration::from_millis(250)));
}

#[test]
fn checkpoint_then_reload_gives_an_equal_index_and_the_same_cursor() {
    let dir = TempDir::new("reload");
    let store = dir.store();
    let tree = sample_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let (engine, cursor) = first_run(&tree, &source, &store);
    assert_eq!(
        cursor,
        ResumeCursor::FsEvents {
            event_id: 1,
            db_uuid: DB_UUID
        }
    );

    engine.checkpoint().expect("checkpoint on request");
    let live = snapshot_entries(&engine.snapshot());
    let (saved, saved_cursor, clean) = read_store(&store);
    assert_eq!(saved, live, "saved index equals the published index");
    assert_eq!(saved, tree.entries());
    assert_eq!(
        saved_cursor, cursor,
        "cursor saved with the index it covers"
    );
    assert!(
        !clean,
        "a running engine's checkpoint is not a clean shutdown"
    );
    drop(engine);

    // Reload: the saved cursor goes to the new source, the index is served
    // as saved.
    let source2 = FakeSource::new(StartOutcome::Resumed);
    let engine2 = open(&tree, &source2, store_config(&store));
    assert_eq!(source2.started_from(), Some(Some(cursor)));
    let snap = wait_for_tree(&engine2, &tree, "reloaded index");
    assert_orders_match(&snap);
    engine2.shutdown();
}

#[test]
fn orderly_shutdown_saves_with_the_clean_flag() {
    let dir = TempDir::new("shutdown");
    let store = dir.store();
    let tree = sample_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let (engine, cursor) = first_run(&tree, &source, &store);
    let live = snapshot_entries(&engine.snapshot());
    engine.shutdown();
    let (saved, saved_cursor, clean) = read_store(&store);
    assert!(clean, "orderly shutdown sets the clean-shutdown flag");
    assert_eq!(saved, live);
    assert_eq!(saved_cursor, cursor);

    // A later on-request checkpoint of a running engine clears it again
    // (once there is a change to save: an unchanged store is not rewritten).
    let source2 = FakeSource::new(StartOutcome::Resumed);
    let engine2 = open(&tree, &source2, store_config(&store));
    wait_for_tree(&engine2, &tree, "reloaded index");
    tree.touch("/vol/b/after-restart.txt", 4);
    source2.push(vec![check("/vol/b/after-restart.txt")]);
    wait_for_tree(&engine2, &tree, "change after the restart");
    engine2.checkpoint().expect("checkpoint on request");
    let (_, _, clean) = read_store(&store);
    assert!(!clean, "only an orderly shutdown sets the flag");
    engine2.shutdown();
}

#[test]
fn idle_checkpoint_follows_the_last_change() {
    let dir = TempDir::new("idle");
    let store = dir.store();
    let tree = sample_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let cfg = store_config(&store).with_checkpoint_idle(Duration::from_millis(150));
    let engine = open(&tree, &source, cfg);
    wait_for_tree(&engine, &tree, "first full scan");

    tree.touch("/vol/b/idle.txt", 3);
    tree.remove("/vol/a/x.txt");
    source.push(vec![check("/vol/b/idle.txt"), check("/vol/a/x.txt")]);
    let snap = wait_for_tree(&engine, &tree, "batch applied");
    let want_cursor = snap.cursor().expect("published cursor");

    let want = tree.entries();
    let end = Instant::now() + WAIT;
    loop {
        let current = MappedIndex::open(&store).ok().map(|m| {
            let entries = m
                .to_index()
                .map(|i| index_entries(&i.snapshot()))
                .unwrap_or_default();
            (entries, m.cursor_list())
        });
        if let Some((entries, cursors)) = current {
            if entries == want && cursors == vec![want_cursor] {
                break;
            }
        }
        assert!(
            Instant::now() < end,
            "no idle checkpoint of the latest snapshot was written"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let (_, _, clean) = read_store(&store);
    assert!(!clean, "an idle checkpoint is not a clean shutdown");
    engine.shutdown();
}

#[test]
fn checkpoint_max_delay_defaults_to_two_days() {
    // A backstop for a Mac that is never idle; the app saves once a day.
    assert_eq!(DEFAULT_CHECKPOINT_MAX_DELAY, Duration::from_secs(48 * 3600));
    assert_eq!(
        EngineConfig::new(ROOT).checkpoint_max_delay(),
        DEFAULT_CHECKPOINT_MAX_DELAY
    );
    let cfg = EngineConfig::new(ROOT).with_checkpoint_max_delay(Duration::from_millis(400));
    assert_eq!(cfg.checkpoint_max_delay(), Duration::from_millis(400));
}

/// Waits until the store at `path` holds an index for which `done` is true.
fn wait_for_store(path: &Path, what: &str, done: impl Fn(&HashMap<FileKey, Entry>) -> bool) {
    let end = Instant::now() + WAIT;
    loop {
        let entries = MappedIndex::open(path)
            .ok()
            .and_then(|m| m.to_index().ok())
            .map(|i| index_entries(&i.snapshot()));
        if entries.as_ref().is_some_and(&done) {
            return;
        }
        assert!(Instant::now() < end, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn first_full_scan_is_saved_without_waiting_for_idle() {
    // An hour of idle delay: only the save after the first scan can write
    // the store within the test.
    let dir = TempDir::new("first-scan");
    let store = dir.store();
    let tree = sample_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = open(&tree, &source, store_config(&store));
    wait_for_tree(&engine, &tree, "first full scan");

    let want = tree.entries();
    wait_for_store(&store, "the first full scan to be saved", |e| *e == want);
    let (_, cursor, clean) = read_store(&store);
    assert_eq!(Some(cursor), engine.snapshot().cursor());
    assert!(
        !clean,
        "a save after the first scan is not a clean shutdown"
    );
    engine.shutdown();
}

#[test]
fn steady_changes_are_saved_within_the_max_delay() {
    // Changes every 50 ms never leave the engine idle for 300 ms, so only
    // the max delay can save them while they keep coming.
    let dir = TempDir::new("steady");
    let store = dir.store();
    let tree = sample_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let cfg = store_config(&store)
        .with_checkpoint_idle(Duration::from_millis(300))
        .with_checkpoint_max_delay(Duration::from_millis(400));
    let engine = open(&tree, &source, cfg);
    wait_for_tree(&engine, &tree, "first full scan");
    let initial = tree.entries();
    wait_for_store(&store, "the first full scan to be saved", |e| *e == initial);

    let pushing_until = Instant::now() + Duration::from_secs(4);
    let first = tree.touch("/vol/b/steady-0.txt", 1);
    source.push(vec![check("/vol/b/steady-0.txt")]);
    let mut saved_while_changing = false;
    let mut n: u64 = 1;
    while Instant::now() < pushing_until {
        thread::sleep(Duration::from_millis(50));
        let path = format!("/vol/b/steady-{n}.txt");
        tree.touch(&path, n);
        source.push(vec![check(&path)]);
        n += 1;
        let saved = MappedIndex::open(&store)
            .ok()
            .and_then(|m| m.to_index().ok())
            .is_some_and(|i| index_entries(&i.snapshot()).contains_key(&first));
        if saved {
            saved_while_changing = true;
            break;
        }
    }
    assert!(
        saved_while_changing,
        "a change made under a steady stream of changes was not saved within the max delay"
    );
    engine.shutdown();
}

/// Cursors of a mapped store, without volume IDs.
trait CursorList {
    fn cursor_list(&self) -> Vec<ResumeCursor>;
}

impl CursorList for MappedIndex {
    fn cursor_list(&self) -> Vec<ResumeCursor> {
        self.cursors().iter().map(|(_, c)| *c).collect()
    }
}

/// Changes made while the engine is not running: create, delete, a folder
/// reparent with its subtree, a rename and a size change.
fn offline_changes(tree: &FakeTree) -> Vec<fx_model::Change> {
    tree.touch("/vol/b/new.txt", 99);
    tree.remove("/vol/a/x.txt");
    tree.rename("/vol/a/inner", "/vol/b/moved");
    tree.rename("/vol/b/y.txt", "/vol/b/renamed.txt");
    tree.set_size("/vol/b/bulk3.dat", 123_456);
    tree.remove("/vol/b/bulk4.dat");
    vec![
        check("/vol/b/new.txt"),
        check("/vol/a/x.txt"),
        check("/vol/a/inner"),
        check("/vol/b/moved"),
        check("/vol/b/y.txt"),
        check("/vol/b/renamed.txt"),
        check("/vol/b/bulk3.dat"),
        check("/vol/b/bulk4.dat"),
    ]
}

#[test]
fn offline_catch_up_from_the_saved_cursor_equals_a_fresh_scan() {
    let dir = TempDir::new("catchup");
    let store = dir.store();
    let tree = sample_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let (engine, cursor) = first_run(&tree, &source, &store);
    engine.shutdown();

    // Offline: change the tree and script the source's history since the
    // saved cursor.
    let history = offline_changes(&tree);
    let source2 = FakeSource::new(StartOutcome::Resumed);
    source2.push(history);

    let engine2 = open(&tree, &source2, store_config(&store));
    assert_eq!(
        source2.started_from(),
        Some(Some(cursor)),
        "startup hands the saved cursor to ChangeSource::start"
    );
    let snap = wait_for_tree(&engine2, &tree, "offline catch-up");
    assert_eq!(source2.pending(), 0);
    let fresh = tree.build_index();
    assert_eq!(snapshot_entries(&snap), index_entries(&fresh.snapshot()));
    assert_orders_match(&snap);

    // The moved subtree is served under its new path.
    let snap = engine2.snapshot();
    let deep = tree.key_of("/vol/b/moved/deep.txt").expect("moved file");
    assert_eq!(rel_path(&snap, deep).as_deref(), Some("b/moved/deep.txt"));

    // Checkpoint after catch-up, then a third start resumes from the new
    // cursor with nothing to replay.
    engine2.shutdown();
    let (saved, saved_cursor, clean) = read_store(&store);
    assert!(clean);
    assert_eq!(saved, tree.entries());
    assert_eq!(saved_cursor, source2.current_cursor());
    let source3 = FakeSource::new(StartOutcome::Resumed);
    let engine3 = open(&tree, &source3, store_config(&store));
    assert_eq!(source3.started_from(), Some(Some(saved_cursor)));
    wait_for_tree(&engine3, &tree, "third start");
    engine3.shutdown();
}

#[test]
fn gap_detected_on_start_falls_back_to_a_full_rescan() {
    let dir = TempDir::new("gap");
    let store = dir.store();
    let tree = sample_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let (engine, cursor) = first_run(&tree, &source, &store);
    engine.shutdown();

    // Offline changes the source cannot replay.
    let _ = offline_changes(&tree);
    let source2 = FakeSource::new(StartOutcome::GapDetected);
    let engine2 = open(&tree, &source2, store_config(&store));
    assert_eq!(source2.started_from(), Some(Some(cursor)));
    let snap = wait_for_tree(&engine2, &tree, "full rescan after a gap");
    assert_orders_match(&snap);
    engine2.shutdown();
    let (saved, _, _) = read_store(&store);
    assert_eq!(saved, tree.entries());
}

#[test]
fn corrupt_store_triggers_a_rebuild() {
    let dir = TempDir::new("corrupt");
    let store = dir.store();
    let tree = sample_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let (engine, _) = first_run(&tree, &source, &store);
    engine.shutdown();

    // Corrupt the saved file: a torn write that lost its second half, with
    // a damaged byte in the first half too.
    let mut bytes = fs::read(&store).expect("store written");
    let mid = bytes.len() / 2;
    bytes.truncate(mid);
    bytes[mid / 2] ^= 0xA5;
    fs::write(&store, &bytes).expect("corrupt store");
    assert!(
        MappedIndex::open(&store)
            .and_then(|m| m.verify_checksums())
            .is_err(),
        "the store is really corrupt"
    );

    tree.touch("/vol/b/after.txt", 1);
    let source2 = FakeSource::new(StartOutcome::Resumed);
    let engine2 = open(&tree, &source2, store_config(&store));
    let snap = wait_for_tree(&engine2, &tree, "rebuild after a corrupt store");
    assert_orders_match(&snap);
    engine2.checkpoint().expect("checkpoint after rebuild");
    let (saved, _, _) = read_store(&store);
    assert_eq!(
        saved,
        tree.entries(),
        "the rebuilt store replaces the corrupt one"
    );
    engine2.shutdown();
}

/// Rewrites the store at `path` with its saved orders changed by `damage`
/// (given each order's key code); the columns, the cursor and the flags
/// stay as they are. The new file's checksums cover the changed orders,
/// as a build that sorts by other rules would have written them.
fn rewrite_orders(path: &Path, damage: impl Fn(u8, &mut Vec<u32>)) {
    let mapped = MappedIndex::open(path).expect("store loads");
    mapped.verify_checksums().expect("store checksums");
    let parts = mapped.to_parts();
    let volumes = mapped.cursors().to_vec();
    let clean = mapped.clean_shutdown();
    let orders: Vec<(u8, Vec<u32>)> = mapped
        .orders()
        .into_iter()
        .map(|(code, ids)| {
            let mut ids = ids.to_vec();
            damage(code, &mut ids);
            (code, ids)
        })
        .collect();
    drop(mapped);
    let orders: Vec<(u8, &[u32])> = orders.iter().map(|(c, ids)| (*c, ids.as_slice())).collect();
    fx_store::write_checkpoint(
        path,
        &fx_store::SaveInput {
            parts: &parts,
            volumes: &volumes,
            orders: &orders,
            clean_shutdown: clean,
        },
    )
    .expect("rewrite the store");
}

#[test]
fn saved_orders_out_of_key_order_are_rebuilt_at_open() {
    // Each case: what is done to the saved orders, and to which one (by
    // key code: 1 Name, 2 Path, 3 Size, 4 Modified; `None` for all four).
    // Every damaged order is still a checksummed permutation of the live
    // entries, so only the check of the key order can reject it; the engine
    // merges changes into its orders, so it must not keep a wrong one.
    type Damage = fn(&mut Vec<u32>);
    let reverse: Damage = |ids| ids.reverse();
    let swap_ends: Damage = |ids| {
        let last = ids.len() - 1;
        ids.swap(0, last);
    };
    let rotate: Damage = |ids| ids.rotate_left(1);
    let swap_middle: Damage = |ids| {
        let mid = ids.len() / 2;
        ids.swap(mid - 1, mid);
    };
    let cases: [(&str, Option<u8>, Damage); 8] = [
        ("reversed", None, reverse),
        ("ends swapped", None, swap_ends),
        ("rotated", None, rotate),
        ("neighbours swapped", None, swap_middle),
        ("Name reversed", Some(1), reverse),
        ("Path ends swapped", Some(2), swap_ends),
        ("Size rotated", Some(3), rotate),
        ("Modified neighbours swapped", Some(4), swap_middle),
    ];
    for (what, target, damage) in cases {
        let dir = TempDir::new("unsorted-orders");
        let store = dir.store();
        let tree = sample_tree();
        let source = FakeSource::new(StartOutcome::Resumed);
        let (engine, _) = first_run(&tree, &source, &store);
        engine.shutdown();
        rewrite_orders(&store, |code, ids| {
            if target.is_none_or(|t| t == code) {
                damage(ids);
            }
        });

        let source2 = FakeSource::new(StartOutcome::Resumed);
        let engine2 = open(&tree, &source2, store_config(&store));
        // The first snapshot already sorts right: the store's orders were
        // rebuilt, not served.
        let first = engine2.snapshot();
        assert_eq!(snapshot_entries(&first), tree.entries(), "{what}");
        assert_orders_match(&first);

        // Changes merged into the orders keep them right.
        let changes = offline_changes(&tree);
        source2.push(changes);
        let snap = wait_for_tree(&engine2, &tree, what);
        assert_orders_match(&snap);
        engine2.shutdown();
    }
}

#[test]
fn garbage_store_file_triggers_a_rebuild() {
    let dir = TempDir::new("garbage");
    let store = dir.store();
    fs::create_dir_all(store.parent().unwrap()).unwrap();
    fs::write(&store, b"this is not a checkpoint file at all").unwrap();
    let tree = sample_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = open(&tree, &source, store_config(&store));
    let snap = wait_for_tree(&engine, &tree, "rebuild from garbage");
    assert_orders_match(&snap);
    engine.shutdown();
    let (saved, _, clean) = read_store(&store);
    assert!(clean);
    assert_eq!(saved, tree.entries());
}

// ---------------------------------------------------------------- quitting

/// One written version of the store file. Every write renames a new temp
/// file over the store (`fx_store::write_checkpoint`), which changes the
/// inode on Unix and the modification time everywhere, even when the bytes
/// come out the same.
#[derive(Debug, PartialEq, Eq)]
struct Written {
    /// A hash of the file's bytes.
    content: u64,
    modified: SystemTime,
    #[cfg(unix)]
    inode: u64,
}

fn written(path: &Path) -> Written {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let meta = fs::metadata(path).expect("store exists");
    let mut hasher = DefaultHasher::new();
    fs::read(path).expect("store readable").hash(&mut hasher);
    Written {
        content: hasher.finish(),
        modified: meta.modified().expect("modification time"),
        #[cfg(unix)]
        inode: std::os::unix::fs::MetadataExt::ino(&meta),
    }
}

/// The names in the store's folder (the store and any temp file).
fn store_dir_names(store: &Path) -> Vec<String> {
    let folder = store.parent().expect("store folder");
    let mut names: Vec<String> = fs::read_dir(folder)
        .expect("store folder readable")
        .map(|e| e.expect("folder entry").file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn checkpoint_returns_at_once_when_the_latest_snapshot_is_saved() {
    let dir = TempDir::new("unchanged");
    let store = dir.store();
    let tree = sample_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let (engine, _) = first_run(&tree, &source, &store);
    engine.shutdown();

    // The reloaded snapshot is the saved one: nothing to write.
    let source2 = FakeSource::new(StartOutcome::Resumed);
    let engine2 = open(&tree, &source2, store_config(&store));
    wait_for_tree(&engine2, &tree, "reloaded index");
    let before = written(&store);
    engine2.checkpoint().expect("checkpoint on request");
    assert_eq!(
        written(&store),
        before,
        "an already saved snapshot is not written again"
    );

    // A change is written once, then not again.
    tree.touch("/vol/a/changed.txt", 5);
    source2.push(vec![check("/vol/a/changed.txt")]);
    wait_for_tree(&engine2, &tree, "change applied");
    engine2.checkpoint().expect("checkpoint of the change");
    let after = written(&store);
    assert_ne!(after, before, "the change is written");
    let (saved, _, _) = read_store(&store);
    assert_eq!(saved, tree.entries());
    engine2.checkpoint().expect("checkpoint again");
    assert_eq!(written(&store), after, "the change is written only once");
    engine2.shutdown();
}

#[test]
fn quit_with_unsaved_changes_writes_them_and_the_next_launch_resumes() {
    let dir = TempDir::new("quit-resumable");
    let store = dir.store();
    let tree = sample_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let (engine, _) = first_run(&tree, &source, &store);
    engine.shutdown();

    // Reopened from a store with a resume cursor, then changed: the store is
    // older than the latest snapshot. The store is saved only once a day,
    // so quitting writes the change rather than leave a day to replay.
    let source2 = FakeSource::new(StartOutcome::Resumed);
    let engine2 = open(&tree, &source2, store_config(&store));
    wait_for_tree(&engine2, &tree, "reloaded index");
    tree.touch("/vol/b/unsaved.txt", 8);
    source2.push(vec![check("/vol/b/unsaved.txt")]);
    wait_for_tree(&engine2, &tree, "change not saved yet");
    let before = written(&store);
    let names = store_dir_names(&store);
    assert_eq!(
        engine2.checkpoint_for_quit().expect("quit"),
        QuitSave::Write
    );
    let after = written(&store);
    assert_ne!(after, before, "quitting saved the change");
    assert_eq!(store_dir_names(&store), names, "quitting left a temp file");
    let (saved, quit_cursor, clean) = read_store(&store);
    assert_eq!(saved, tree.entries(), "the store holds the change");
    assert!(!clean, "a quit is not an orderly shutdown");
    // The app exits without dropping the engine (no orderly shutdown).
    drop(engine2);
    assert_eq!(written(&store), after, "dropping saves nothing more");

    // The next launch resumes from the cursor the quit saved.
    let source3 = FakeSource::new(StartOutcome::Resumed);
    let engine3 = open(&tree, &source3, store_config(&store));
    assert_eq!(source3.started_from(), Some(Some(quit_cursor)));
    let snap = wait_for_tree(&engine3, &tree, "the launch after the quit");
    assert_orders_match(&snap);
    engine3.shutdown();
}

#[test]
fn quit_writes_only_what_the_saved_store_lacks() {
    let dir = TempDir::new("quit-after-scan");
    let store = dir.store();
    let tree = sample_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = open(&tree, &source, store_config(&store));
    wait_for_tree(&engine, &tree, "first full scan");
    let scanned = tree.entries();
    wait_for_store(&store, "the first full scan to be saved", |e| *e == scanned);
    // Returns once that background save is recorded, writing nothing more.
    let before = written(&store);
    engine.checkpoint().expect("checkpoint of a saved snapshot");
    assert_eq!(
        written(&store),
        before,
        "the saved scan is not written again"
    );

    // Nothing changed since that save: quitting writes nothing.
    assert_eq!(engine.checkpoint_for_quit().expect("quit"), QuitSave::Skip);
    assert_eq!(written(&store), before, "quitting rewrote a current store");

    tree.touch("/vol/a/later.txt", 2);
    source.push(vec![check("/vol/a/later.txt")]);
    wait_for_tree(&engine, &tree, "change after the scan");
    assert_eq!(engine.checkpoint_for_quit().expect("quit"), QuitSave::Write);
    let (saved, _, _) = read_store(&store);
    assert_ne!(saved, scanned, "the change is not in the saved scan");
    assert_eq!(saved, tree.entries(), "the quit saved the change");
    engine.shutdown();
}

#[test]
fn quit_with_no_store_writes_one() {
    let dir = TempDir::new("quit-no-store");
    let store = dir.store();
    let tree = sample_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    // A started index that needs no scan, and no background save due for an
    // hour: the store does not exist when the app quits.
    let cfg = store_config(&store)
        .with_checkpoint_max_delay(Duration::from_secs(3600))
        .with_resume_from(source.current_cursor());
    let engine = start(&tree, &source, tree.build_index(), cfg);
    wait_for_tree(&engine, &tree, "started index");
    assert!(!store.exists(), "no store before the quit");

    assert_eq!(engine.checkpoint_for_quit().expect("quit"), QuitSave::Write);
    let (saved, cursor, clean) = read_store(&store);
    assert_eq!(saved, tree.entries(), "the quit saved the index");
    assert_eq!(cursor, source.current_cursor(), "with its resume cursor");
    assert!(!clean, "a quit is not an orderly shutdown");

    // The store is resumable now: quitting again writes nothing.
    let before = written(&store);
    assert_eq!(engine.checkpoint_for_quit().expect("quit"), QuitSave::Skip);
    assert_eq!(written(&store), before);
    drop(engine);
}

#[test]
fn quit_without_a_store_path_is_an_error() {
    let tree = sample_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = start(&tree, &source, tree.build_index(), config());
    assert!(matches!(
        engine.checkpoint_for_quit(),
        Err(CheckpointError::NoStorePath)
    ));
    engine.shutdown();
}

// ------------------------------------------------------ full scan on grant

/// Enumerates a [`FakeTree`] without one folder and everything below it,
/// as the app's limited (non-FDA) scan skips the protected folders.
#[derive(Clone)]
struct Skipping {
    tree: FakeTree,
    skip: FileKey,
}

impl Skipping {
    /// What a full scan through this enumerator finds.
    fn entries(&self) -> HashMap<FileKey, Entry> {
        let mut out = HashMap::new();
        fx_fs::VolumeEnumerator::enumerate(self, Path::new(ROOT), &mut |e| {
            out.insert(e.key, e);
        })
        .expect("enumerates");
        out
    }
}

impl fx_fs::VolumeEnumerator for Skipping {
    fn enumerate(&self, root: &Path, sink: &mut dyn FnMut(Entry)) -> std::io::Result<()> {
        // Parents come before their children, so a skipped folder is known
        // before anything inside it.
        let mut skipped = std::collections::HashSet::from([self.skip]);
        self.tree.enumerate(root, &mut |e| {
            if skipped.contains(&e.key) || e.parent.is_some_and(|p| skipped.contains(&p)) {
                skipped.insert(e.key);
            } else {
                sink(e);
            }
        })
    }
}

/// Every file under `dir`, recursively.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).expect("read dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(files_under(&path));
        } else {
            out.push(path);
        }
    }
    out.sort();
    out
}

/// When Full Disk Access is granted after a limited launch, the app
/// opens a full-access engine over the same root beside the running limited
/// one, each with its own store, and shuts the limited one down once it has
/// switched. Neither may write the other's store.
#[test]
fn two_engines_over_one_root_with_different_stores_do_not_share_files() {
    let dir = TempDir::new("two-stores");
    let limited_store = dir.0.join("stores-limited").join("root.fxstore");
    let full_store = dir.0.join("stores").join("root.fxstore");
    let tree = sample_tree();
    let protected = tree.mkdir("/vol/protected");
    tree.touch("/vol/protected/secret.txt", 5);
    let skipping = Skipping {
        tree: tree.clone(),
        skip: protected,
    };
    let limited_want = skipping.entries();
    assert!(!limited_want.contains_key(&protected));

    let limited_source = FakeSource::new(StartOutcome::Resumed);
    let limited = Engine::open(
        store_config(&limited_store),
        limited_source.clone(),
        tree.clone(),
        skipping,
    )
    .expect("the limited engine opens");
    wait_for(&limited, "the limited first scan", |s| {
        snapshot_entries(s) == limited_want
    });

    // Access granted: the full engine opens over the same root while the
    // limited one keeps running.
    let full_source = FakeSource::new(StartOutcome::Resumed);
    let full = open(&tree, &full_source, store_config(&full_store));
    wait_for_tree(&full, &tree, "the full first scan");
    let want = tree.entries();
    wait_for_store(&full_store, "the full scan to be saved", |e| *e == want);
    wait_for_store(&limited_store, "the limited scan to be saved", |e| {
        *e == limited_want
    });

    // A change inside the protected folder reaches the full index only.
    tree.touch("/vol/protected/new.txt", 1);
    full_source.push(vec![check("/vol/protected/new.txt")]);
    wait_for_tree(&full, &tree, "the live change in the full index");
    assert_eq!(snapshot_entries(&limited.snapshot()), limited_want);
    full.checkpoint().expect("the full index saves");
    let full_bytes = fs::read(&full_store).expect("the full store");

    // The switch done, the limited engine shuts down (saving its store)
    // without touching the full one.
    limited.shutdown();
    assert_eq!(
        fs::read(&full_store).expect("the full store"),
        full_bytes,
        "shutting the limited engine down wrote the full store"
    );
    let (saved_limited, _, clean) = read_store(&limited_store);
    assert_eq!(
        saved_limited, limited_want,
        "the limited store kept its scope"
    );
    assert!(clean, "the limited engine shut down in order");
    let (saved_full, _, _) = read_store(&full_store);
    assert_eq!(saved_full, tree.entries());
    assert!(saved_full.contains_key(&protected));

    full.shutdown();
    assert_eq!(
        files_under(&dir.0),
        [full_store, limited_store],
        "each engine wrote its own store and nothing else"
    );
}

/// The permission bits of `path`.
#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).expect("metadata").permissions().mode() & 0o777
}

/// The store holds the names of every indexed file, so every save writes
/// its file 0600 and creates a missing store folder 0700. A folder that
/// already exists keeps its mode: the store path may lie in any folder.
#[cfg(unix)]
#[test]
fn the_store_folder_and_file_are_private() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new("private");
    let store = dir.store();
    let folder = store.parent().expect("store folder").to_path_buf();
    let tree = sample_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let (engine, _) = first_run(&tree, &source, &store);
    engine.checkpoint().expect("checkpoint on request");
    assert_eq!(mode(&folder), 0o700, "a new store folder is private");
    assert_eq!(mode(&store), 0o600, "the store file is private");

    // A file an older version left readable is tightened by the next save;
    // its folder, which the save did not make, keeps its mode.
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).expect("chmod folder");
    fs::set_permissions(&store, fs::Permissions::from_mode(0o644)).expect("chmod store");
    tree.touch("/vol/a/later.txt", 3);
    source.push(vec![check("/vol/a/later.txt")]);
    wait_for_tree(&engine, &tree, "a later batch");
    engine.checkpoint().expect("second checkpoint");
    assert_eq!(
        mode(&folder),
        0o755,
        "an existing store folder keeps its mode"
    );
    assert_eq!(mode(&store), 0o600, "an existing store file is tightened");
    engine.shutdown();
    assert_eq!(mode(&folder), 0o755, "the last save keeps it too");
}

/// A file at `path` removed on drop.
#[cfg(unix)]
struct RemoveOnDrop(PathBuf);

#[cfg(unix)]
impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// A folder this user may write in but not change the mode of: the shared
/// `/tmp`, which belongs to root. `None`, and the test that asks is
/// skipped, when the tests run as root (who may change any mode), when
/// `/tmp` belongs to this user, or when it can't be written here. `own` is
/// a folder this test made, so its owner is this user.
#[cfg(unix)]
fn shared_folder(own: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let me = fs::metadata(own).ok()?.uid();
    let dir = fs::canonicalize("/tmp").ok()?;
    let owner = fs::metadata(&dir).ok()?.uid();
    if me == 0 || owner == me {
        return None;
    }
    let probe = dir.join(format!(".fx-engine-probe-{}", own_suffix(own)));
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .ok()?;
    let _ = fs::remove_file(&probe);
    Some(dir)
}

/// The unique last part of the scratch folder `own`'s name, to name files
/// in a shared folder after it.
#[cfg(unix)]
fn own_suffix(own: &Path) -> String {
    own.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A store in a folder that belongs to someone else: its mode can't be
/// changed, and the saves must not try.
#[cfg(unix)]
#[test]
fn saves_into_a_folder_the_engine_may_not_change_succeed() {
    let dir = TempDir::new("shared");
    let Some(shared) = shared_folder(&dir.0) else {
        eprintln!("skipped: no folder here that this user may write in but not change");
        return;
    };
    let before = mode(&shared);
    let store = shared.join(format!("{}.fxdb", own_suffix(&dir.0)));
    let _cleanup = RemoveOnDrop(store.clone());
    let tree = sample_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let (engine, _) = first_run(&tree, &source, &store);
    engine
        .checkpoint()
        .expect("a save into a folder whose mode can't be changed");
    assert_eq!(mode(&shared), before, "the shared folder keeps its mode");
    assert_eq!(mode(&store), 0o600, "the store file is private");
    engine.shutdown();
    let (saved, _, clean) = read_store(&store);
    assert_eq!(saved, tree.entries());
    assert!(clean, "the shutdown save was written too");
}

// ---------------------------------------------------------------------------
// Run 2.17 step 2: the base is served from the store's memory map.
// ---------------------------------------------------------------------------

/// Bytes every entry has in the mapped per-entry columns, names aside:
/// name offset, parent, volume, file ID, meta, two times and extension.
const MAPPED_PER_ENTRY: usize = 4 + 4 + 4 + 8 + 8 + 8 + 8 + 2;

/// Whether every entry of `snap` sits in a base served from the store.
fn whole_base_mapped(snap: &fx_engine::EngineSnapshot) -> bool {
    let idx = snap.index();
    idx.mapped_bytes() >= idx.id_bound() * MAPPED_PER_ENTRY
}

#[test]
fn after_the_first_scan_is_saved_its_base_is_served_from_the_store() {
    let dir = TempDir::new("mapped-first-scan");
    let store = dir.store();
    let tree = sample_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = open(&tree, &source, store_config(&store));
    wait_for_tree(&engine, &tree, "first full scan");
    let snap = wait_for(&engine, "the base served from the store", whole_base_mapped);
    assert_eq!(index_entries(snap.index()), tree.entries());
    assert_orders_match(&snap);

    // Changes go on applying on top of the mapped base.
    tree.touch("/vol/a/after.txt", 3);
    tree.remove("/vol/b/y.txt");
    source.push(vec![check("/vol/a/after.txt"), check("/vol/b/y.txt")]);
    let snap = wait_for_tree(&engine, &tree, "changes after the swap");
    assert!(snap.index().mapped_bytes() > 0, "the base stays mapped");
    assert_orders_match(&snap);
    engine.shutdown();
}

#[test]
fn a_loaded_store_is_served_from_its_map() {
    let dir = TempDir::new("mapped-load");
    let store = dir.store();
    let tree = sample_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let (engine, _) = first_run(&tree, &source, &store);
    engine.shutdown();

    let engine = open(&tree, &source, store_config(&store));
    let snap = wait_for_tree(&engine, &tree, "the loaded index");
    assert!(
        snap.index().mapped_bytes() > 0,
        "a loaded base is served from the store's map"
    );
    assert_orders_match(&snap);
    engine.shutdown();
}

#[test]
fn a_compaction_moves_the_new_base_to_the_store_again() {
    let dir = TempDir::new("mapped-compaction");
    let store = dir.store();
    let tree = sample_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = open(&tree, &source, store_config(&store));
    wait_for_tree(&engine, &tree, "first full scan");
    wait_for(
        &engine,
        "the first base served from the store",
        whole_base_mapped,
    );

    // More new entries than the compaction limit (1,024 rows at this size).
    tree.add_files("/vol/b", "many", 1_500);
    let changes = (0..1_500)
        .map(|n| check(&format!("/vol/b/many{n}.dat")))
        .collect();
    source.push(changes);
    wait_for_tree(&engine, &tree, "the compacted index");
    let snap = wait_for(&engine, "the compacted base served from the store", |s| {
        s.index().live_count() == s.index().id_bound() && whole_base_mapped(s)
    });
    assert_eq!(index_entries(snap.index()), tree.entries());
    assert_orders_match(&snap);
    engine.shutdown();
}
