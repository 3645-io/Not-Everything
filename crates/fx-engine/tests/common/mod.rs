//! Scripted fakes shared by the fx-engine integration tests.
//!
//! - [`FakeTree`]: an in-memory file tree that implements `fx_fs::PathStat`
//!   and `fx_fs::VolumeEnumerator`. Clones share one tree, so a test mutates
//!   the "disk" while the engine's writer thread reads it.
//! - [`FakeSource`]: an `fx_fs::ChangeSource` fed batch by batch from a test.
//!   Its cursor is `ResumeCursor::FsEvents` whose `event_id` counts the
//!   non-empty batches handed out (from 0, or from the event ID given to
//!   `FakeSource::starting_at`).
//!
//! Engine API these tests rely on:
//!
//! - `EngineConfig::new(root)`, `.with_batch_window(Duration)`,
//!   `.with_resume_from(ResumeCursor)`, `.batch_window()` (clamped value).
//! - `Engine::start(index, config, source, stat, enumerator) -> io::Result<Engine>`
//!   with `Send + 'static` platform implementations.
//! - `Engine::snapshot() -> Arc<EngineSnapshot>`, `subscribe()` (a receiver
//!   with `recv_timeout` yielding `u64` generations), `pause(&self)`,
//!   `resume(&self)`, `shutdown(self)` (returns `()` once the writer thread
//!   is joined). `Engine` is `Sync` so readers on many threads share it.
//! - Generations increase by batch: a published batch bumps it; the first
//!   snapshot is available as soon as `start` returns.
//! - `EngineSnapshot::index() -> &fx_index::Snapshot` (or a smart pointer to
//!   it), `order(SortKey) -> Option<&Order>`, `cursor() -> Option<ResumeCursor>`,
//!   `generation() -> u64`.
//!
//! Paths: the engine resolves an absolute path by stripping the configured
//! root and walking child names down from the root entry (the entry with no
//! parent). The fake root is `/vol`, its entry is named `vol`.

#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::{Component, Path};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use fx_engine::{Engine, EngineConfig, EngineSnapshot};
use fx_fs::{ChangeSource, PathStat, VolumeEnumerator};
use fx_index::Index;
use fx_model::{
    Change, Entry, EntryFlags, EntryId, EntryKind, FileKey, ResumeCursor, StartOutcome, VolumeId,
};
use fx_sort::{Order, SortKey};

/// Absolute path of the fake volume root.
pub const ROOT: &str = "/vol";

/// FSEvents database UUID reported by [`FakeSource`].
pub const DB_UUID: [u8; 16] = [7; 16];

/// The sort orders the engine must keep merged.
pub const ENGINE_KEYS: [SortKey; 4] = [
    SortKey::Name,
    SortKey::Path,
    SortKey::Size,
    SortKey::Modified,
];

/// How long a test waits for the engine before failing.
pub const WAIT: Duration = Duration::from_secs(20);

fn key(id: u64) -> FileKey {
    FileKey::new(VolumeId(1), id)
}

struct TreeState {
    entries: HashMap<FileKey, Entry>,
    next_id: u64,
    clock: i64,
}

impl TreeState {
    fn root_key(&self) -> FileKey {
        key(1)
    }

    fn child(&self, parent: FileKey, name: &str) -> Option<FileKey> {
        self.entries
            .values()
            .find(|e| e.parent == Some(parent) && e.key != parent && e.name == name)
            .map(|e| e.key)
    }

    fn resolve(&self, path: &Path) -> Option<FileKey> {
        let rel = path.strip_prefix(ROOT).ok()?;
        let mut cur = self.root_key();
        for comp in rel.components() {
            match comp {
                Component::Normal(name) => cur = self.child(cur, name.to_str()?)?,
                Component::CurDir => {}
                _ => return None,
            }
        }
        Some(cur)
    }

    fn tick(&mut self) -> i64 {
        self.clock += 1;
        self.clock
    }

    fn insert(&mut self, parent: FileKey, name: &str, kind: EntryKind, size: u64) -> FileKey {
        // An entry is on its folder's volume (a mounted volume's root
        // starts a new one: `FakeTree::mount`).
        let k = FileKey::new(parent.volume, self.next_id);
        self.next_id += 1;
        let t = self.tick();
        self.entries.insert(
            k,
            Entry {
                key: k,
                parent: Some(parent),
                name: name.to_string(),
                kind,
                flags: EntryFlags::empty(),
                size,
                modified: t,
                created: t,
            },
        );
        k
    }

    fn create(&mut self, path: &str, kind: EntryKind, size: u64) -> FileKey {
        let path = Path::new(path);
        let parent = path.parent().expect("path has a parent");
        let parent = self
            .resolve(parent)
            .unwrap_or_else(|| panic!("parent of {path:?} exists"));
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(
            self.child(parent, name).is_none(),
            "{path:?} already exists"
        );
        self.insert(parent, name, kind, size)
    }
}

/// In-memory file tree implementing `PathStat` and `VolumeEnumerator`.
#[derive(Clone)]
pub struct FakeTree {
    inner: Arc<Mutex<TreeState>>,
}

impl Default for FakeTree {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeTree {
    /// A tree holding only the root folder `/vol`.
    pub fn new() -> FakeTree {
        let root = Entry {
            key: key(1),
            parent: None,
            name: "vol".to_string(),
            kind: EntryKind::Dir,
            flags: EntryFlags::empty(),
            size: 0,
            modified: 0,
            created: 0,
        };
        let mut entries = HashMap::new();
        entries.insert(root.key, root);
        FakeTree {
            inner: Arc::new(Mutex::new(TreeState {
                entries,
                next_id: 2,
                clock: 1_000,
            })),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, TreeState> {
        self.inner.lock().unwrap()
    }

    /// Creates a folder; its parent must exist.
    pub fn mkdir(&self, path: &str) -> FileKey {
        self.lock().create(path, EntryKind::Dir, 0)
    }

    /// Mounts a volume at `path`: a folder keyed on volume `volume` (like
    /// a mount point, keyed by the volume's root), whose entries are on
    /// that volume too. Its parent must exist. Unmount with
    /// [`remove`](Self::remove).
    pub fn mount(&self, path: &str, volume: u32) -> FileKey {
        let mut st = self.lock();
        let p = Path::new(path);
        let parent = st.resolve(p.parent().unwrap()).expect("parent exists");
        let name = p.file_name().unwrap().to_str().unwrap();
        assert!(st.child(parent, name).is_none(), "{path:?} already exists");
        let k = st.insert(parent, name, EntryKind::Dir, 0);
        let mut e = st.entries.remove(&k).unwrap();
        e.key = FileKey::new(VolumeId(volume), k.file_id);
        let k = e.key;
        st.entries.insert(k, e);
        k
    }

    /// Creates a file of `size` bytes; its parent must exist.
    pub fn touch(&self, path: &str, size: u64) -> FileKey {
        self.lock().create(path, EntryKind::File, size)
    }

    /// Creates `count` files named `{prefix}{n}.dat` in an existing folder.
    pub fn add_files(&self, dir: &str, prefix: &str, count: usize) {
        let mut st = self.lock();
        let parent = st.resolve(Path::new(dir)).expect("folder exists");
        for n in 0..count {
            let size = (n as u64 * 7919) % 100_000;
            st.insert(parent, &format!("{prefix}{n}.dat"), EntryKind::File, size);
        }
    }

    /// Renames and/or reparents the entry at `from` to `to` (key unchanged).
    pub fn rename(&self, from: &str, to: &str) {
        let mut st = self.lock();
        let k = st.resolve(Path::new(from)).expect("source exists");
        let to = Path::new(to);
        let parent = st
            .resolve(to.parent().unwrap())
            .expect("target parent exists");
        let name = to.file_name().unwrap().to_str().unwrap().to_string();
        assert!(st.child(parent, &name).is_none(), "{to:?} already exists");
        let e = st.entries.get_mut(&k).unwrap();
        e.parent = Some(parent);
        e.name = name;
    }

    /// Removes the entry at `path` and everything under it.
    pub fn remove(&self, path: &str) {
        let mut st = self.lock();
        let k = st.resolve(Path::new(path)).expect("entry exists");
        let mut doomed = vec![k];
        let mut i = 0;
        while i < doomed.len() {
            let p = doomed[i];
            let kids: Vec<FileKey> = st
                .entries
                .values()
                .filter(|e| e.parent == Some(p) && e.key != p)
                .map(|e| e.key)
                .collect();
            doomed.extend(kids);
            i += 1;
        }
        for d in doomed {
            st.entries.remove(&d);
        }
    }

    /// Changes a file's size (and bumps its modification time).
    pub fn set_size(&self, path: &str, size: u64) {
        let mut st = self.lock();
        let k = st.resolve(Path::new(path)).expect("entry exists");
        let t = st.tick();
        let e = st.entries.get_mut(&k).unwrap();
        e.size = size;
        e.modified = t;
    }

    /// Key of the entry at `path`, if it exists.
    pub fn key_of(&self, path: &str) -> Option<FileKey> {
        self.lock().resolve(Path::new(path))
    }

    /// A fresh full enumeration of the tree, by key.
    pub fn entries(&self) -> HashMap<FileKey, Entry> {
        let mut out = HashMap::new();
        self.enumerate(Path::new(ROOT), &mut |e| {
            out.insert(e.key, e);
        })
        .unwrap();
        out
    }

    /// An index holding the current tree, as a full scan would build it.
    pub fn build_index(&self) -> Index {
        let mut index = Index::new();
        self.enumerate(Path::new(ROOT), &mut |e| {
            index.upsert(e).expect("upsert into a fresh index");
        })
        .unwrap();
        index
    }
}

impl VolumeEnumerator for FakeTree {
    fn enumerate(&self, root: &Path, sink: &mut dyn FnMut(Entry)) -> io::Result<()> {
        let out = {
            let st = self.lock();
            let start = st
                .resolve(root)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such folder"))?;
            let mut children: HashMap<FileKey, Vec<&Entry>> = HashMap::new();
            for e in st.entries.values() {
                match e.parent {
                    Some(p) if p != e.key => children.entry(p).or_default().push(e),
                    _ => {}
                }
            }
            for kids in children.values_mut() {
                kids.sort_by(|a, b| a.name.cmp(&b.name));
            }
            let mut stack: Vec<&Entry> = vec![&st.entries[&start]];
            let mut out = Vec::new();
            while let Some(e) = stack.pop() {
                out.push(e.clone());
                if let Some(kids) = children.get(&e.key) {
                    stack.extend(kids.iter().rev().copied());
                }
            }
            out
        };
        for e in out {
            sink(e);
        }
        Ok(())
    }
}

impl PathStat for FakeTree {
    fn stat(&self, path: &Path) -> io::Result<Option<Entry>> {
        let st = self.lock();
        Ok(st.resolve(path).map(|k| st.entries[&k].clone()))
    }
}

struct SourceState {
    queue: VecDeque<Vec<Change>>,
    outcome: StartOutcome,
    started_from: Option<Option<ResumeCursor>>,
    event_id: u64,
    polls: u64,
}

/// A `ChangeSource` whose batches are pushed by the test.
#[derive(Clone)]
pub struct FakeSource {
    inner: Arc<Mutex<SourceState>>,
}

impl FakeSource {
    /// A source whose `start` returns `outcome`.
    pub fn new(outcome: StartOutcome) -> FakeSource {
        FakeSource::starting_at(outcome, 0)
    }

    /// A source whose `start` returns `outcome` and whose cursor starts at
    /// `event_id`, like a source resumed from a cursor an earlier process
    /// saved: its next non-empty batch is event `event_id + 1`.
    pub fn starting_at(outcome: StartOutcome, event_id: u64) -> FakeSource {
        FakeSource {
            inner: Arc::new(Mutex::new(SourceState {
                queue: VecDeque::new(),
                outcome,
                started_from: None,
                event_id,
                polls: 0,
            })),
        }
    }

    /// Queues one batch for the next `next_batch` call.
    pub fn push(&self, batch: Vec<Change>) {
        self.inner.lock().unwrap().queue.push_back(batch);
    }

    /// Number of `next_batch` calls so far.
    pub fn polls(&self) -> u64 {
        self.inner.lock().unwrap().polls
    }

    /// Batches still waiting to be handed out.
    pub fn pending(&self) -> usize {
        self.inner.lock().unwrap().queue.len()
    }

    /// The cursor passed to `start`, or `None` if `start` was not called.
    pub fn started_from(&self) -> Option<Option<ResumeCursor>> {
        self.inner.lock().unwrap().started_from
    }

    /// The cursor right now (after the last batch handed out).
    pub fn current_cursor(&self) -> ResumeCursor {
        ResumeCursor::FsEvents {
            event_id: self.inner.lock().unwrap().event_id,
            db_uuid: DB_UUID,
        }
    }
}

impl ChangeSource for FakeSource {
    fn start(&mut self, from: Option<&ResumeCursor>) -> io::Result<StartOutcome> {
        let mut st = self.inner.lock().unwrap();
        st.started_from = Some(from.copied());
        Ok(st.outcome)
    }

    fn next_batch(&mut self, deadline: Instant) -> io::Result<Vec<Change>> {
        self.inner.lock().unwrap().polls += 1;
        loop {
            {
                let mut st = self.inner.lock().unwrap();
                if let Some(batch) = st.queue.pop_front() {
                    if !batch.is_empty() {
                        st.event_id += 1;
                    }
                    return Ok(batch);
                }
            }
            if Instant::now() >= deadline {
                return Ok(Vec::new());
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn cursor(&self) -> ResumeCursor {
        self.current_cursor()
    }
}

/// `CheckPath` for an absolute path string.
pub fn check(path: &str) -> Change {
    Change::CheckPath(path.into())
}

/// The test config: root `/vol`, a 50 ms batch window.
pub fn config() -> EngineConfig {
    EngineConfig::new(ROOT).with_batch_window(Duration::from_millis(50))
}

/// Starts an engine over `index` with the fakes.
pub fn start(tree: &FakeTree, source: &FakeSource, index: Index, config: EngineConfig) -> Engine {
    Engine::start(index, config, source.clone(), tree.clone(), tree.clone()).expect("engine starts")
}

/// Rebuilds every live entry of a published snapshot as an `Entry`, by key.
pub fn snapshot_entries(snap: &EngineSnapshot) -> HashMap<FileKey, Entry> {
    index_entries(snap.index())
}

/// Rebuilds every live entry of an index snapshot (published, or loaded
/// from a store) as an `Entry`, by key.
pub fn index_entries(idx: &fx_index::Snapshot) -> HashMap<FileKey, Entry> {
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

/// Asserts every merged order equals a fresh `Order::build` on the same
/// published index snapshot and passes `Order::validate`.
pub fn assert_orders_match(snap: &EngineSnapshot) {
    let idx: &fx_index::Snapshot = snap.index();
    for k in ENGINE_KEYS {
        let order = snap
            .order(k)
            .unwrap_or_else(|| panic!("engine keeps a {k:?} order"));
        assert_eq!(order.key, k, "order stored under {k:?} sorts by {k:?}");
        order
            .validate(idx)
            .unwrap_or_else(|e| panic!("{k:?} order invalid: {e:?}"));
        let fresh = Order::build(idx, k);
        assert_eq!(order.ids, fresh.ids, "merged {k:?} order != Order::build");
    }
}

/// Polls `engine.snapshot()` until `cond` holds, failing after [`WAIT`].
pub fn wait_for(
    engine: &Engine,
    what: &str,
    mut cond: impl FnMut(&EngineSnapshot) -> bool,
) -> Arc<EngineSnapshot> {
    let end = Instant::now() + WAIT;
    loop {
        let snap = engine.snapshot();
        if cond(&snap) {
            return snap;
        }
        assert!(Instant::now() < end, "timed out waiting for: {what}");
        thread::sleep(Duration::from_millis(5));
    }
}

/// Waits until the published entries equal the tree's fresh enumeration.
pub fn wait_for_tree(engine: &Engine, tree: &FakeTree, what: &str) -> Arc<EngineSnapshot> {
    let want = tree.entries();
    let end = Instant::now() + WAIT;
    loop {
        let snap = engine.snapshot();
        let got = snapshot_entries(&snap);
        if got == want {
            return snap;
        }
        if Instant::now() >= end {
            let missing: Vec<_> = want.keys().filter(|k| !got.contains_key(*k)).collect();
            let extra: Vec<_> = got.keys().filter(|k| !want.contains_key(*k)).collect();
            let differ: Vec<_> = want
                .iter()
                .filter(|(k, e)| got.get(*k).is_some_and(|g| g != *e))
                .map(|(k, _)| k)
                .collect();
            panic!(
                "timed out waiting for: {what}; missing {missing:?}, extra {extra:?}, differ {differ:?}"
            );
        }
        thread::sleep(Duration::from_millis(5));
    }
}

/// Path of `key` relative to the root, from a published snapshot.
pub fn rel_path(snap: &EngineSnapshot, key: FileKey) -> Option<String> {
    let idx: &fx_index::Snapshot = snap.index();
    let mut id = idx.lookup(key)?;
    if !idx.is_live(id) {
        return None;
    }
    let mut parts = Vec::new();
    loop {
        let p = idx.parent(id);
        if p.is_none() {
            break;
        }
        parts.push(idx.name(id).to_string());
        id = p;
    }
    parts.reverse();
    Some(parts.join("/"))
}
