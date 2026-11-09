//! The writer thread: collects change batches, checks every path on disk,
//! applies the result to the index, merges the sort orders and publishes a
//! new snapshot.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use fx_fs::{ChangeSource, PathStat, VolumeEnumerator, diff_subtree, reconcile_path};
use fx_index::{DEFAULT_COMPACT_FRACTION, Index, IndexError};
use fx_model::{Change, Entry, EntryId, FileKey, ResumeCursor, StartOutcome};
use fx_sort::{Batch, MatchSet, Order};

use crate::Shared;
use crate::config::MAX_RESTART_BACKOFF;
use crate::health::EngineHealth;
use crate::priority::PriorityFolders;
use crate::qos;
use crate::snapshot::EngineSnapshot;
use crate::tree::Tree;

/// How deep `check_path` may recurse into missing parents.
const MAX_DEPTH: u32 = 64;

/// Compaction never runs below this many delta or removed rows.
const MIN_COMPACT_ROWS: usize = 1024;

/// How long a paused writer sleeps between checks (it is also unparked by
/// `resume` and `shutdown`).
const PAUSE_POLL: Duration = Duration::from_millis(20);

/// A removal postponed to the end of the batch, so that an entry that only
/// moved (reported at its old path first) is found at its new path before
/// anything is removed.
struct Deferred {
    key: FileKey,
    parent: Option<FileKey>,
    name: String,
    path: PathBuf,
}

/// State owned by the writer thread.
pub(crate) struct Writer<C, P, V> {
    pub(crate) index: Index,
    pub(crate) tree: Tree,
    pub(crate) orders: Arc<Vec<Order>>,
    pub(crate) root: PathBuf,
    pub(crate) window: Duration,
    pub(crate) source: C,
    pub(crate) stat: P,
    pub(crate) enumerator: V,
    pub(crate) shared: Arc<Shared>,
    pub(crate) generation: u64,
    /// Changes applied to the index since the last publish, by entry ID.
    pub(crate) batch: Batch,
    /// A full rescan is due (start outcome, `RescanVolume`, or a change the
    /// index rejected).
    pub(crate) full_rescan_pending: bool,
    /// Set while the full rescan runs, so its own failures do not request
    /// another one.
    pub(crate) in_full_rescan: bool,
    /// Source error counting and the restart / rescan retry schedule.
    pub(crate) recovery: Recovery,
    /// The priority folders ("Your files first").
    pub(crate) priority: PriorityFolders,
    /// The priority group of the index as last published.
    pub(crate) group: Option<Arc<MatchSet>>,
    /// The index changed since `group` was built: rebuild it at the next
    /// publish.
    pub(crate) group_stale: bool,
}

/// What the writer must do before it reads the change source again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// The source is delivering; read it.
    Idle,
    /// The source stopped or failed too often: restart it from the last
    /// published cursor.
    Restart,
    /// History was lost (a gap, no cursor support) or a full rescan failed:
    /// rescan the volume.
    Rescan,
}

/// Source error counting and the retry schedule of the writer thread.
pub(crate) struct Recovery {
    /// Consecutive `next_batch` errors that trigger a restart.
    threshold: u32,
    min_backoff: Duration,
    max_backoff: Duration,
    /// Consecutive `next_batch` errors so far.
    errors: u32,
    phase: Phase,
    /// Failed restart (or rescan) attempts in a row.
    failed_attempts: u32,
    /// The wait before the next attempt after another failure.
    backoff: Duration,
    next_attempt: Instant,
}

impl Recovery {
    pub(crate) fn new(threshold: u32, min_backoff: Duration) -> Recovery {
        Recovery {
            threshold: threshold.max(1),
            min_backoff,
            max_backoff: MAX_RESTART_BACKOFF.max(min_backoff),
            errors: 0,
            phase: Phase::Idle,
            failed_attempts: 0,
            backoff: min_backoff,
            next_attempt: Instant::now(),
        }
    }

    /// Enters `phase` with its first attempt due now.
    fn begin(&mut self, phase: Phase) {
        self.phase = phase;
        self.next_attempt = Instant::now();
    }

    /// Schedules the next attempt after a failed one, doubling the wait.
    fn failed(&mut self) {
        self.failed_attempts = self.failed_attempts.saturating_add(1);
        self.next_attempt = Instant::now() + self.backoff;
        self.backoff = (self.backoff * 2).min(self.max_backoff);
    }

    /// Back to normal delivery.
    fn reset(&mut self) {
        self.phase = Phase::Idle;
        self.errors = 0;
        self.failed_attempts = 0;
        self.backoff = self.min_backoff;
    }
}

impl<C, P, V> Writer<C, P, V>
where
    C: ChangeSource,
    P: PathStat,
    V: VolumeEnumerator,
{
    /// The writer thread's main loop; returns on shutdown.
    pub(crate) fn run(mut self) {
        if self.full_rescan_pending {
            self.try_rescan();
        }
        while !self.stopping() {
            if self.shared.paused.load(Ordering::Acquire) {
                thread::park_timeout(PAUSE_POLL);
                continue;
            }
            if self.recovery.phase != Phase::Idle {
                self.recover_step();
                continue;
            }
            let Some(changes) = self.collect() else {
                continue;
            };
            // The cursor covers every batch collected so far; it is published
            // with (never before) the snapshot that applies them.
            let cursor = self.source.cursor();
            if self.process(changes) {
                self.commit(Some(cursor));
            } else {
                // A full rescan failed and will be retried: the published
                // index does not cover the new cursor yet.
                let old = self.published_cursor();
                self.commit(old);
            }
        }
    }

    fn stopping(&self) -> bool {
        self.shared.stop.load(Ordering::Acquire)
    }

    /// Waits up to one window for a first change, then keeps collecting
    /// for one more window. `None` if nothing arrived.
    fn collect(&mut self) -> Option<Vec<Change>> {
        let mut changes = match self.source.next_batch(Instant::now() + self.window) {
            Ok(batch) => {
                self.recovery.errors = 0;
                if batch.is_empty() {
                    return None;
                }
                batch
            }
            Err(_) => {
                if !self.source_error() {
                    // A failing source: back off instead of spinning.
                    thread::park_timeout(self.window);
                }
                return None;
            }
        };
        let deadline = Instant::now() + self.window;
        while Instant::now() < deadline && !self.stopping() {
            match self.source.next_batch(deadline) {
                Ok(batch) => {
                    self.recovery.errors = 0;
                    changes.extend(batch);
                }
                Err(_) => {
                    self.source_error();
                    break;
                }
            }
        }
        Some(changes)
    }

    /// Counts one `next_batch` error; at the threshold, schedules a restart
    /// of the source and reports `Recovering`. Returns whether it did.
    fn source_error(&mut self) -> bool {
        self.recovery.errors = self.recovery.errors.saturating_add(1);
        if self.recovery.errors < self.recovery.threshold {
            return false;
        }
        self.recovery.begin(Phase::Restart);
        self.shared.health.set(EngineHealth::Recovering);
        true
    }

    /// Runs the due restart or rescan attempt, or waits for it (woken early
    /// by `shutdown`).
    fn recover_step(&mut self) {
        let now = Instant::now();
        if now < self.recovery.next_attempt {
            thread::park_timeout((self.recovery.next_attempt - now).min(PAUSE_POLL));
            return;
        }
        match self.recovery.phase {
            Phase::Restart => self.try_restart(),
            Phase::Rescan => self.try_rescan(),
            Phase::Idle => {}
        }
    }

    /// Restarts the source from the last published cursor. A resumed start
    /// is `Live` again; a start that lost history moves on to a full rescan
    /// (the previous snapshot keeps serving meanwhile); a failed start is
    /// retried after a growing wait and, after `threshold` failures in a
    /// row, reported as `Failed`.
    fn try_restart(&mut self) {
        let cursor = self
            .published_cursor()
            .unwrap_or_else(|| self.source.cursor());
        match self.source.start(Some(&cursor)) {
            Ok(StartOutcome::Resumed) => {
                self.recovery.reset();
                self.shared.health.set(EngineHealth::Live);
            }
            Ok(StartOutcome::GapDetected | StartOutcome::NoCursorSupport) => {
                self.recovery.reset();
                self.try_rescan();
            }
            Err(e) => {
                self.recovery.failed();
                self.shared.health.set(
                    if self.recovery.failed_attempts >= self.recovery.threshold {
                        EngineHealth::Failed(e.to_string())
                    } else {
                        EngineHealth::Recovering
                    },
                );
            }
        }
    }

    /// Runs a full rescan and publishes it. On success the engine is `Live`;
    /// a failed rescan is retried after a growing wait.
    fn try_rescan(&mut self) {
        self.shared.health.set(EngineHealth::Recovering);
        // Changes the source buffers meanwhile are applied afterwards.
        let cursor = self.source.cursor();
        // Someone waits for a full scan (the first index, or a rescan after
        // a gap): at the writer's usual utility class it ran about a quarter
        // slower (21.8 s against 17.6 s for 2.9M entries). Threads the scan
        // starts inherit the class.
        qos::set_current_thread_class(qos::ThreadClass::UserInitiated);
        let ok = self.full_rescan();
        qos::set_current_thread_class(qos::ThreadClass::Utility);
        if ok {
            self.commit(Some(cursor));
        } else {
            // Keep the cursor the published index really covers.
            let old = self.published_cursor();
            self.commit(old);
        }
        if ok {
            self.recovery.reset();
            self.shared.health.set(EngineHealth::Live);
            // Save a full rescan at once: it is the costliest state to
            // rebuild, and a busy volume may not go idle for a long time.
            self.shared.checkpoints.request();
        } else {
            self.schedule_rescan_retry();
        }
    }

    /// The cursor of the latest published snapshot.
    fn published_cursor(&self) -> Option<ResumeCursor> {
        self.shared.current.load().cursor()
    }

    /// A full rescan failed: retry it after a growing wait.
    fn schedule_rescan_retry(&mut self) {
        if self.recovery.phase != Phase::Rescan {
            self.recovery.begin(Phase::Rescan);
        }
        self.recovery.failed();
        self.shared.health.set(EngineHealth::Recovering);
    }

    /// Applies one collected batch of changes to the index and the tree.
    /// Returns `false` when a full rescan it needed failed (a retry is then
    /// scheduled).
    fn process(&mut self, changes: Vec<Change>) -> bool {
        if changes.iter().any(|c| matches!(c, Change::RescanVolume)) {
            // The full rescan supersedes every other change in the batch.
            self.full_rescan_pending = true;
        } else {
            let mut deferred = Vec::new();
            for change in changes {
                match change {
                    Change::CheckPath(path) => self.check_path(&path, &mut deferred, 0),
                    Change::RescanSubtree(path) => self.rescan_subtree(&path, &mut deferred, 0),
                    Change::Upsert(entry) => {
                        if let Err(e) = self.upsert(entry) {
                            self.rejected(e);
                        }
                    }
                    Change::Remove { key } => self.remove(key),
                    Change::RescanVolume => {}
                }
            }
            self.finish_deferred(deferred);
        }
        if self.full_rescan_pending && !self.full_rescan() {
            self.schedule_rescan_retry();
            return false;
        }
        true
    }

    /// Stats `path` and reconciles it with the index (`fx_fs::reconcile_path`).
    fn check_path(&mut self, path: &Path, deferred: &mut Vec<Deferred>, depth: u32) {
        let Ok(on_disk) = self.stat_at(path) else {
            // Unreadable (e.g. permission denied): leave the index as is.
            return;
        };
        let indexed = self
            .tree
            .resolve(&self.root, path)
            .and_then(|k| self.tree.get(k))
            .cloned();
        // A folder found here under another key (new, or moved in from
        // another path) is re-enumerated. A moved folder's children move
        // with it, but changes made inside it are reported under its old
        // path (FSEvents history in particular), which no longer resolves
        // once the move is applied; the subtree diff picks them up.
        let result = reconcile_path(on_disk, indexed.as_ref());
        for change in result.changes {
            match change {
                Change::Remove { key } => {
                    if let Some(old) = indexed.as_ref().filter(|i| i.key == key) {
                        deferred.push(Deferred {
                            key,
                            parent: old.parent,
                            name: old.name.clone(),
                            path: path.to_path_buf(),
                        });
                    }
                }
                Change::Upsert(entry) => self.upsert_at(entry, path, deferred, depth),
                _ => {}
            }
        }
        if result.rescan_subtree && depth < MAX_DEPTH {
            self.rescan_subtree(path, deferred, depth + 1);
        }
    }

    /// Upserts an entry found at `path`; if its parent is not indexed yet,
    /// checks the parent path first and retries.
    fn upsert_at(&mut self, entry: Entry, path: &Path, deferred: &mut Vec<Deferred>, depth: u32) {
        match self.upsert(entry.clone()) {
            Ok(()) => {}
            Err(IndexError::UnknownParent { .. }) if depth < MAX_DEPTH => match path.parent() {
                Some(parent) if parent.starts_with(&self.root) && parent != path => {
                    self.check_path(parent, deferred, depth + 1);
                    if let Err(e) = self.upsert(entry) {
                        self.rejected(e);
                    }
                }
                _ => self.rejected(IndexError::NeedsFileSystem),
            },
            Err(e) => self.rejected(e),
        }
    }

    /// Re-enumerates the folder at `path` and applies `fx_fs::diff_subtree`.
    fn rescan_subtree(&mut self, path: &Path, deferred: &mut Vec<Deferred>, depth: u32) {
        let Some(top) = self.tree.resolve(&self.root, path) else {
            // Not indexed: checking the path upserts it and, for a folder,
            // rescans it from there.
            if depth < MAX_DEPTH {
                self.check_path(path, deferred, depth + 1);
            }
            return;
        };
        let fresh = match self.walk(path) {
            Ok(fresh) => fresh,
            Err(e) if e.kind() == io::ErrorKind::NotFound && depth < MAX_DEPTH => {
                // Gone: checking the path removes it.
                self.check_path(path, deferred, depth + 1);
                return;
            }
            // A partial walk would remove everything it missed.
            Err(_) => return,
        };
        // The indexed side covers the folder's subtree plus the subtrees of
        // folders that moved in from elsewhere: whatever they held that is
        // not listed now is gone (or is found again at its new path).
        let mut keys = self.tree.subtree(top);
        let mut seen: HashSet<FileKey> = keys.iter().copied().collect();
        for e in &fresh {
            let moved_in = self
                .tree
                .get(e.key)
                .is_some_and(|old| old.parent != e.parent && !seen.contains(&e.key));
            if moved_in {
                for k in self.tree.subtree(e.key) {
                    if seen.insert(k) {
                        keys.push(k);
                    }
                }
            }
        }
        let indexed: HashMap<FileKey, Entry> = keys
            .into_iter()
            .filter_map(|k| self.tree.get(k).map(|e| (k, e.clone())))
            .collect();
        self.apply_changes(diff_subtree(&indexed, fresh).changes);
    }

    /// Re-enumerates the whole volume and applies the difference. Readers
    /// keep the previous snapshot until the result is published. Returns
    /// whether the volume could be enumerated.
    fn full_rescan(&mut self) -> bool {
        self.full_rescan_pending = false;
        self.in_full_rescan = true;
        let root = self.root.clone();
        let ok = match self.walk(&root) {
            Ok(fresh) => {
                let diff = diff_subtree(self.tree.entries(), fresh);
                self.apply_changes(diff.changes);
                true
            }
            Err(_) => false,
        };
        self.in_full_rescan = false;
        ok
    }

    /// Enumerates the folder at `path`, parent before child, with the
    /// folder's own entry first. Enumerators that list only what is below
    /// the folder (the macOS scanner does not emit its root) get the
    /// folder's entry from `PathStat` prepended, so `diff_subtree` keeps it.
    fn walk(&self, path: &Path) -> io::Result<Vec<Entry>> {
        let mut fresh = Vec::new();
        self.enumerator.enumerate(path, &mut |e| fresh.push(e))?;
        if let Ok(Some(top)) = self.stat_at(path) {
            if !fresh.iter().any(|e| e.key == top.key) {
                fresh.insert(0, top);
            }
        }
        Ok(fresh)
    }

    /// `PathStat::stat`, with the watched root reported as the index's root
    /// entry (no parent): its parent folder lies outside the index.
    fn stat_at(&self, path: &Path) -> io::Result<Option<Entry>> {
        let mut entry = self.stat.stat(path)?;
        if path == self.root {
            if let Some(e) = entry.as_mut() {
                if e.parent.is_some_and(|p| p != e.key) {
                    e.parent = None;
                }
            }
        }
        Ok(entry)
    }

    fn apply_changes(&mut self, changes: Vec<Change>) {
        for change in changes {
            match change {
                Change::Upsert(entry) => {
                    if let Err(e) = self.upsert(entry) {
                        self.rejected(e);
                    }
                }
                Change::Remove { key } => self.remove(key),
                _ => {}
            }
        }
    }

    /// Applies the postponed removals whose entry is still where it was and
    /// is no longer on disk there.
    fn finish_deferred(&mut self, deferred: Vec<Deferred>) {
        for d in deferred {
            let unmoved = self
                .tree
                .get(d.key)
                .is_some_and(|e| e.parent == d.parent && e.name == d.name);
            if !unmoved {
                continue;
            }
            match self.stat_at(&d.path) {
                Ok(Some(now)) if now.key == d.key => {}
                Ok(_) => self.remove(d.key),
                Err(_) => {}
            }
        }
    }

    /// The index rejected a change (unknown parent, cycle, bad name): the
    /// batch's view of the disk is inconsistent, so rescan the volume.
    fn rejected(&mut self, _error: IndexError) {
        if !self.in_full_rescan {
            self.full_rescan_pending = true;
        }
    }

    /// Upserts one entry into the index and the tree and records it in the
    /// order batch: a changed name or parent through `Batch::moved`, anything
    /// else through `Batch::upsert`.
    fn upsert(&mut self, entry: Entry) -> Result<(), IndexError> {
        let old = self.tree.get(entry.key);
        if old == Some(&entry) {
            return Ok(());
        }
        let moved = old.is_some_and(|o| o.name != entry.name || o.parent != entry.parent);
        let id = self.index.upsert(entry.clone())?;
        if moved {
            self.batch.moved(id);
        } else {
            self.batch.upsert(id);
        }
        self.tree.insert(entry);
        Ok(())
    }

    /// Removes an entry and its descendants, recording every removed ID.
    fn remove(&mut self, key: FileKey) {
        if self.tree.get(key).is_none() {
            return;
        }
        for k in self.tree.remove_subtree(key) {
            if let Some(id) = self.index.lookup(k) {
                self.batch.remove(id);
            }
        }
        self.index.remove(key);
    }

    /// Merges the batch into the orders, compacts the index if due and
    /// publishes the new snapshot with `cursor`.
    fn commit(&mut self, cursor: Option<ResumeCursor>) {
        if !self.batch.is_empty() {
            let snap = self.index.snapshot();
            let rebuild = self.batch.len() > self.tree.len() / 2;
            let mut orders: Vec<Order> = if rebuild {
                self.orders
                    .iter()
                    .map(|o| Order::build(&*snap, o.key))
                    .collect()
            } else {
                self.orders
                    .iter()
                    .map(|o| {
                        let mut o = o.clone();
                        o.apply(&*snap, &self.batch);
                        o
                    })
                    .collect()
            };
            self.batch.clear();
            if self.compaction_due() {
                // Compaction keeps live entries in ID order, so remapping an
                // order keeps it sorted.
                let remap = self.index.compact();
                for order in &mut orders {
                    for id in &mut order.ids {
                        *id = remap[*id as usize].0;
                    }
                    debug_assert!(order.ids.iter().all(|&id| id != EntryId::NONE.0));
                }
            }
            self.orders = Arc::new(orders);
            self.group_stale = true;
        }
        self.generation += 1;
        self.publish(cursor);
    }

    fn compaction_due(&self) -> bool {
        let limit = (self.index.live_count() as f64 * DEFAULT_COMPACT_FRACTION) as usize;
        let limit = limit.max(MIN_COMPACT_ROWS);
        self.index.delta_len() > limit || self.index.deleted_len() > limit
    }

    /// Swaps in a snapshot of the current state and tells subscribers. The
    /// priority group is rebuilt first when the index changed.
    fn publish(&mut self, cursor: Option<ResumeCursor>) {
        let index = self.index.snapshot();
        if self.group_stale {
            self.group = self.priority.group(&self.tree, &index, &self.orders);
            self.group_stale = false;
        }
        let snap = EngineSnapshot {
            index,
            orders: Arc::clone(&self.orders),
            cursor,
            generation: self.generation,
            priority: self.group.clone(),
        };
        self.shared.current.store(Arc::new(snap));
        self.shared.notify(self.generation);
    }
}
