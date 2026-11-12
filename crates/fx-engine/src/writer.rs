//! The writer thread: collects change batches, checks every path on disk,
//! applies the result to the index, merges the sort orders and publishes a
//! new snapshot.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use fx_fs::{ChangeSource, PathStat, VolumeEnumerator, reconcile_path};
use fx_index::{DEFAULT_COMPACT_FRACTION, Index, IndexBuilder, IndexError, Upserted};
use fx_model::{Change, Entry, EntryId, EntryKind, FileKey, ResumeCursor, StartOutcome, VolumeId};
use fx_sort::{Batch, MatchSet, Order};

use crate::Shared;
use crate::config::MAX_RESTART_BACKOFF;
use crate::health::EngineHealth;
use crate::paths;
use crate::priority::PriorityFolders;
use crate::qos;
use crate::snapshot::EngineSnapshot;

/// How deep `check_path` may recurse into missing parents.
const MAX_DEPTH: u32 = 64;

/// Compaction never runs below this many delta or removed rows.
const MIN_COMPACT_ROWS: usize = 1024;

/// How long a paused writer sleeps between checks (it is also unparked by
/// `resume` and `shutdown`).
const PAUSE_POLL: Duration = Duration::from_millis(20);

/// While collecting a background batch, the writer waits on the source in
/// slices this long, so a window that shows ends the batch within one
/// slice. Only a batch in progress is sliced: the wait for its first change
/// is not, since nothing is pending then.
const BACKGROUND_SLICE: Duration = Duration::from_millis(250);

/// The longest the writer expects readers to wait for a merge in place
/// (`Writer::take_orders`): half of `snapshot::SETTLE_LIMIT`, the owner's
/// 100 ms bound, so a merge slower than expected still ends inside it.
pub(crate) const IN_PLACE_BUDGET: Duration = Duration::from_millis(50);

/// How many order entries one batch entry counts for in `merge_units`.
/// Measured on the M4 Pro (11 Nov, whole disk, 3.73M entries, batches of
/// 74,000 to 126,000 changes): a merge in place took 1.1 to 1.65 s, about
/// 10 µs per batch entry, some 800 units at [`FIRST_NS_PER_UNIT`].
const BATCH_WEIGHT: f64 = 1024.0;

/// The expected cost of a merge in place per unit, in nanoseconds, until
/// the first one is timed: a cautious guess, so the first is tried only on
/// an index of up to about 4M entries with a small batch. Every later
/// merge goes by the time the last one took.
const FIRST_NS_PER_UNIT: f64 = 12.0;

/// What a refused merge in place takes off the cost estimate. Slow on
/// purpose: hidden commits come every few seconds under steady changes,
/// and a merge timed past the budget should be tried again only minutes
/// later (about 80 refusals from 1.5 times the budget), each retry being
/// a chance of a wait past the bound.
const COST_DECAY: f64 = 0.995;

/// A removal postponed to the end of the batch, so that an entry that only
/// moved (reported at its old path first) is found at its new path before
/// anything is removed.
struct Deferred {
    key: FileKey,
}

/// State owned by the writer thread.
pub(crate) struct Writer<C, P, V> {
    pub(crate) index: Index,
    pub(crate) orders: Arc<Vec<Order>>,
    pub(crate) root: PathBuf,
    pub(crate) window: Duration,
    /// The batch window while no window shows the results.
    pub(crate) background_window: Duration,
    pub(crate) source: C,
    pub(crate) stat: P,
    pub(crate) enumerator: V,
    pub(crate) shared: Arc<Shared>,
    pub(crate) generation: u64,
    /// Changes applied to the index since the last publish, by entry ID.
    pub(crate) batch: Batch,
    /// Every order must be rebuilt at the next commit (after a first scan,
    /// which records no batch).
    pub(crate) orders_stale: bool,
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
    /// The base was just built on the heap (a first scan or a compaction):
    /// the next publish asks for that exact snapshot to be saved, so the
    /// base can then be served from the saved file (run 2.17 step 2).
    pub(crate) fresh_base: bool,
    /// The folder volumes are mounted in
    /// ([`EngineConfig::with_volumes_folder`](crate::EngineConfig::with_volumes_folder)).
    pub(crate) volumes_folder: Option<PathBuf>,
    /// The next offline volume number to hand out; found from the index
    /// on first use.
    pub(crate) next_offline: Option<u32>,
    /// What the last merge in place took per unit of `merge_units`, in
    /// nanoseconds, from publishing the snapshot without orders to
    /// publishing the result: what readers could wait. `None` before the
    /// first.
    pub(crate) in_place_cost: Option<f64>,
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
            self.adopt_saved_base();
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

    /// Whether a window shows the results.
    fn foreground(&self) -> bool {
        self.shared.foreground.load(Ordering::Acquire)
    }

    /// Waits up to one window for a first change, then keeps collecting
    /// for one more window. `None` if nothing arrived. The window is the
    /// background one while no window shows the results; such a batch ends
    /// early once a window shows.
    fn collect(&mut self) -> Option<Vec<Change>> {
        let window = if self.foreground() {
            self.window
        } else {
            self.background_window
        };
        let mut changes = match self.source.next_batch(Instant::now() + window) {
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
        let background = !self.foreground();
        let window = if background {
            self.background_window
        } else {
            self.window
        };
        let deadline = Instant::now() + window;
        while Instant::now() < deadline && !self.stopping() {
            let until = if background {
                deadline.min(Instant::now() + BACKGROUND_SLICE)
            } else {
                deadline
            };
            match self.source.next_batch(until) {
                Ok(batch) => {
                    self.recovery.errors = 0;
                    changes.extend(batch);
                }
                Err(_) => {
                    self.source_error();
                    break;
                }
            }
            if background && self.foreground() {
                // A window shows the results now: publish what is pending.
                break;
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

    /// Applies one collected batch of changes to the index.
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
            self.drop_replaced_offline();
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
        // A replaced entry still waiting for its removal shares its name:
        // the one the disk has now is the one to compare.
        let prefer = on_disk.as_ref().map(|e| e.key);
        let indexed = paths::resolve(&self.index, &self.root, path, prefer)
            .and_then(|id| self.index.get(self.index.key(id)));
        // A folder found here under another key (new, or moved in from
        // another path) is re-enumerated. A moved folder's children move
        // with it, but changes made inside it are reported under its old
        // path (FSEvents history in particular), which no longer resolves
        // once the move is applied; the subtree diff picks them up.
        let result = reconcile_path(on_disk, indexed.as_ref());
        for change in result.changes {
            match change {
                Change::Remove { key } => {
                    if indexed.as_ref().is_some_and(|i| i.key == key) {
                        deferred.push(Deferred { key });
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

    /// Re-enumerates the folder at `path`: the folder the disk has there now
    /// is upserted with every entry the walk lists (each recorded in the
    /// batch only when it changed), then whatever that folder holds in the
    /// index and the walk didn't list is removed. A failed walk removes
    /// nothing.
    ///
    /// The folder walked is the one the disk has at `path`, by key, not the
    /// entry the index has there: after a swap of names (`a` to `t`, `b` to
    /// `a`, `t` to `b`) the index still has the other entry at `path` until
    /// that entry's own new path is checked. Comparing the walk with that
    /// entry's subtree kept what the folder had lost meanwhile and removed
    /// the other entry instead (found by the soak with restarts, whose
    /// history replay reports `a` before `b`). The entry the index had at
    /// `path`, when it is another one, is removed at the end of the batch
    /// unless it is found at a new path by then, as `check_path` does.
    fn rescan_subtree(&mut self, path: &Path, deferred: &mut Vec<Deferred>, depth: u32) {
        // The folder's own entry: enumerators that list only what is below
        // it (the macOS scanner) don't emit it.
        let top_entry = match self.stat_at(path) {
            Ok(Some(entry)) if entry.kind == EntryKind::Dir => entry,
            Ok(_) => {
                // Gone, or not a folder (replaced by a file, or a swap put a
                // file here): there is nothing to walk, and checking the
                // path reconciles what it holds now.
                if depth < MAX_DEPTH {
                    self.check_path(path, deferred, depth + 1);
                }
                return;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound && depth < MAX_DEPTH => {
                self.check_path(path, deferred, depth + 1);
                return;
            }
            Err(_) => return,
        };
        let top = top_entry.key;
        if self.index.lookup(top).is_none() {
            // A folder new to the index: checking the path upserts it (and
            // its parents if need be) and rescans it from there.
            if depth < MAX_DEPTH {
                self.check_path(path, deferred, depth + 1);
            }
            return;
        }
        // Another entry indexed at this path (replaced, or swapped away):
        // removed at the end of the batch unless it is found elsewhere.
        if let Some(old) = paths::resolve(&self.index, &self.root, path, Some(top))
            .map(|id| self.index.key(id))
            .filter(|&old| old != top)
        {
            deferred.push(Deferred { key: old });
        }
        // The folder itself, where the disk has it: a folder that moved here
        // may need its new parent checked first.
        self.upsert_at(top_entry, path, deferred, depth);
        let Some(top_id) = self.index.lookup(top) else {
            return;
        };
        // The entries stream into the index, as in a full rescan (run 2.17):
        // no list of every entry and no map of the old ones, which took more
        // than 300 B per entry for a busy folder macOS asked to rescan.
        let Writer {
            index,
            batch,
            enumerator,
            ..
        } = self;
        // A bit per entry: a byte per entry was 3.3 MB at 3.2M entries.
        let mut seen = MatchSet::with_bound(index.id_bound());
        seen.insert(top_id);
        let mut take = |e: Entry| {
            // Several hard links: keep the first listing, as a full rescan does.
            if index.lookup(e.key).is_some_and(|id| seen.contains(id)) {
                return;
            }
            take_one(index, batch, &mut seen, e);
        };
        match enumerator.enumerate(path, &mut take) {
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) && depth < MAX_DEPTH =>
            {
                // Gone or replaced since the stat: reconcile the path.
                self.check_path(path, deferred, depth + 1);
                return;
            }
            // A partial walk would remove everything it missed; what it
            // listed is applied.
            Err(_) => return,
        }
        // Whatever the folder holds now (folders that moved in included,
        // with what they held) and the walk didn't list is gone, or is found
        // again at its new path.
        let unseen = match self.index.lookup(top) {
            Some(id) => self.unseen_below(id, &seen),
            None => Vec::new(),
        };
        self.remove_unseen(unseen);
    }

    /// The keys of `top` and every live entry below it that are not in
    /// `seen`, each after its parent. The subtree is walked depth first,
    /// holding only the folders still to visit: a list of the whole subtree
    /// (`Index::subtree`) took 4 B per entry, 13 MB for a rescan of a
    /// 3.2M-entry disk, just before the commit.
    fn unseen_below(&self, top: EntryId, seen: &MatchSet) -> Vec<FileKey> {
        let mut unseen = Vec::new();
        if !self.index.is_live(top) {
            return unseen;
        }
        let mut stack = vec![top];
        while let Some(id) = stack.pop() {
            if !seen.contains(id) {
                unseen.push(self.index.key(id));
            }
            stack.extend(self.index.children(id));
        }
        unseen
    }

    /// Re-enumerates the whole volume and applies the difference. Readers
    /// keep the previous snapshot until the result is published. Returns
    /// whether the volume could be enumerated.
    ///
    /// The entries stream from the enumerator into the index, with no list
    /// of every entry and no map of the old ones (run 2.17): an empty index
    /// is built straight into its base columns ([`IndexBuilder`]); a loaded
    /// one takes each entry as an upsert, and whatever the scan did not
    /// list is removed at the end.
    fn full_rescan(&mut self) -> bool {
        self.full_rescan_pending = false;
        self.in_full_rescan = true;
        let root = self.root.clone();
        let ok = if self.index.live_count() == 0 {
            self.first_scan(&root)
        } else {
            self.rescan_loaded(&root)
        };
        self.drop_replaced_offline();
        self.in_full_rescan = false;
        ok
    }

    /// The first scan of an empty index: built in the base columns, then
    /// published with every order rebuilt by one shared Name sort.
    fn first_scan(&mut self, root: &Path) -> bool {
        let mut builder = IndexBuilder::new();
        // The enumerator may not list the root itself (the macOS scanner
        // does not); the root goes first so every entry finds its parent.
        if let Ok(Some(top)) = self.stat_at(root) {
            let _ = builder.push(top);
        }
        // An entry whose parent is unknown is dropped, as a rejected change
        // during a full rescan is; a repeated key keeps its first listing.
        let listed = self.enumerator.enumerate(root, &mut |e| {
            let _ = builder.push(e);
        });
        if listed.is_err() {
            return false;
        }
        self.index = builder.finish();
        self.fresh_base = true;
        // The writer compacts between batches itself.
        self.index.set_compact_fraction(f64::INFINITY);
        self.batch.clear();
        self.orders_stale = true;
        true
    }

    /// A full rescan of a loaded index: every listed entry is upserted (and
    /// recorded in the batch only when it changed), then every live entry
    /// the scan did not list is removed. A failed scan removes nothing.
    fn rescan_loaded(&mut self, root: &Path) -> bool {
        let top = self.stat_at(root);
        let Writer {
            index,
            batch,
            enumerator,
            ..
        } = self;
        let mut seen = MatchSet::with_bound(index.id_bound());
        let mut take = |e: Entry| {
            // A file with several hard links is listed once per link: keep
            // the first listing, as a first scan does, so rescans don't move
            // it back and forth.
            if index.lookup(e.key).is_some_and(|id| seen.contains(id)) {
                return;
            }
            take_one(index, batch, &mut seen, e);
        };
        if let Ok(Some(top)) = top {
            take(top);
        }
        let listed = enumerator.enumerate(root, &mut take);
        if listed.is_err() {
            return false;
        }
        let unseen: Vec<FileKey> = (0..self.index.id_bound())
            .map(|i| EntryId(i as u32))
            .filter(|&id| self.index.is_live(id) && !seen.contains(id))
            .map(|id| self.index.key(id))
            .collect();
        self.remove_unseen(unseen);
        true
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

    /// Applies the postponed removals of entries that are not on disk where
    /// the index has them now.
    fn finish_deferred(&mut self, deferred: Vec<Deferred>) {
        if deferred.is_empty() {
            return;
        }
        let roots = self.volume_roots();
        // Volume roots first: an unmounted volume goes offline whole,
        // before a change reported inside it removes any of its entries.
        let (mut deferred, rest): (Vec<Deferred>, Vec<Deferred>) =
            deferred.into_iter().partition(|d| {
                self.index
                    .lookup(d.key)
                    .is_some_and(|id| roots.contains(&id))
            });
        deferred.extend(rest);
        for d in deferred {
            // Where the index has the entry now: an entry that moved in this
            // batch, itself or with a folder above it, has a new path by
            // now. Checking only the path it was reported at removed a moved
            // folder's subtree when a change inside it was reported under
            // the folder's old path (found by the differential test).
            let Some(path) = self.path_of(d.key) else {
                continue;
            };
            match self.stat_at(&path) {
                Ok(Some(now)) if now.key == d.key => {}
                Ok(_) => self.remove_in(d.key, &roots),
                Err(_) => {}
            }
        }
    }

    /// The absolute path the index has for `key` now: the watched root,
    /// then the names below the root entry. `None` if `key` is not live.
    fn path_of(&self, key: FileKey) -> Option<PathBuf> {
        let mut names = Vec::new();
        let mut cur = self.index.get(key)?;
        while let Some(parent) = cur.parent {
            names.push(cur.name);
            // The index keeps no cycles; the cap guards a corrupt one.
            if names.len() > self.index.live_count() {
                return None;
            }
            cur = self.index.get(parent)?;
        }
        let mut path = self.root.clone();
        path.extend(names.iter().rev());
        Some(path)
    }

    /// The index rejected a change (unknown parent, cycle, bad name): the
    /// batch's view of the disk is inconsistent, so rescan the volume.
    fn rejected(&mut self, _error: IndexError) {
        if !self.in_full_rescan {
            self.full_rescan_pending = true;
        }
    }

    /// Upserts one entry into the index and records it in the order batch:
    /// a changed name or parent through `Batch::moved`, anything else
    /// through `Batch::upsert`, nothing when it was already stored so.
    fn upsert(&mut self, entry: Entry) -> Result<(), IndexError> {
        let (id, what) = self.index.upsert_reporting(entry)?;
        match what {
            Upserted::Unchanged => {}
            Upserted::Moved => self.batch.moved(id),
            Upserted::Updated | Upserted::Inserted => self.batch.upsert(id),
        }
        Ok(())
    }

    /// Removes an entry and its descendants, recording every removed ID.
    ///
    /// External volumes (run 2.4): a volume root goes offline instead
    /// ([`take_offline`](Self::take_offline)), and an offline entry stays:
    /// its path is gone while its volume is unmounted. Offline entries
    /// leave only when a volume mounted in their place replaces them
    /// ([`drop_replaced_offline`](Self::drop_replaced_offline)).
    fn remove(&mut self, key: FileKey) {
        let roots = self.volume_roots();
        self.remove_in(key, &roots);
    }

    /// [`remove`](Self::remove), with the volume roots found beforehand.
    /// A root taken offline since stays in `roots`; its ID is no longer
    /// live, so no key finds it.
    fn remove_in(&mut self, key: FileKey, roots: &[EntryId]) {
        let Some(id) = self.index.lookup(key) else {
            return;
        };
        if key.volume.is_offline() {
            return;
        }
        if roots.contains(&id) {
            self.take_offline(id);
            return;
        }
        self.remove_now(id);
    }

    /// Removes live entry `id` and its descendants, recording every
    /// removed ID.
    fn remove_now(&mut self, id: EntryId) {
        let key = self.index.key(id);
        for d in self.index.subtree(id) {
            self.batch.remove(d);
        }
        self.index.remove(key);
    }

    /// Removes the entries a rescan did not list. Volume roots among them
    /// go offline first, with their whole subtree, so none of their
    /// entries is removed before its volume is taken offline.
    fn remove_unseen(&mut self, unseen: Vec<FileKey>) {
        let roots = self.volume_roots();
        let (gone_volumes, rest): (Vec<FileKey>, Vec<FileKey>) = unseen
            .into_iter()
            .partition(|&key| self.index.lookup(key).is_some_and(|id| roots.contains(&id)));
        for key in gone_volumes.into_iter().chain(rest) {
            self.remove_in(key, &roots);
        }
    }

    /// The live volume roots: the entries directly in the volumes folder
    /// on another volume than the folder's own, offline ones left out.
    /// Empty without a volumes folder.
    fn volume_roots(&self) -> Vec<EntryId> {
        let Some(folder) = self.volumes_folder.as_deref() else {
            return Vec::new();
        };
        let Some(folder_id) = paths::resolve(&self.index, &self.root, folder, None) else {
            return Vec::new();
        };
        let home = self.index.key(folder_id).volume;
        self.index
            .children(folder_id)
            .into_iter()
            .filter(|&c| {
                let volume = self.index.key(c).volume;
                volume != home && !volume.is_offline()
            })
            .collect()
    }

    /// Takes the volume rooted at live entry `id` offline: its entries are
    /// re-keyed onto a fresh offline volume ID, keeping their names,
    /// folders and metadata, so the device number the system gives the
    /// next volume never meets them. Entries of another volume inside it (a
    /// mount point listed on the volume) are dropped. A volume with nothing
    /// indexed below its root (a mount the scanner listed but did not
    /// enter, such as a network share or a hidden system volume) is
    /// removed: there is nothing to keep. So is every volume once the
    /// offline IDs run out.
    fn take_offline(&mut self, id: EntryId) {
        let top = self.index.key(id);
        // Parent first (`subtree`'s order), so every parent is back before
        // its children.
        let ids = self.index.subtree(id);
        if ids.len() <= 1 {
            self.remove_now(id);
            return;
        }
        let entries: Vec<Entry> = ids
            .into_iter()
            .filter_map(|d| self.index.get(self.index.key(d)))
            .collect();
        let offline = self.next_offline_volume();
        self.remove_now(id);
        let Some(offline) = offline else {
            return;
        };
        let remap = |key: FileKey| {
            if key.volume == top.volume {
                FileKey::new(offline, key.file_id)
            } else {
                key
            }
        };
        for mut e in entries {
            if e.key.volume != top.volume {
                continue;
            }
            e.key = remap(e.key);
            e.parent = e.parent.map(remap);
            // An entry whose parent was dropped is dropped too; nothing on
            // disk to rescan for.
            let _ = self.upsert(e);
        }
    }

    /// A fresh offline volume ID: one past the highest in the index (found
    /// once), then counting up. `None` once the range is used up.
    fn next_offline_volume(&mut self) -> Option<VolumeId> {
        let next = match self.next_offline {
            Some(n) => n,
            None => (0..self.index.id_bound())
                .map(|i| EntryId(i as u32))
                .filter(|&id| self.index.is_live(id))
                .map(|id| self.index.key(id).volume)
                .filter(|v| v.is_offline())
                .map(|v| v.0 - VolumeId::OFFLINE_FIRST + 1)
                .max()
                .unwrap_or(0),
        };
        let volume = VolumeId::offline(next)?;
        self.next_offline = Some(next + 1);
        Some(volume)
    }

    /// Where a live volume root is in the volumes folder, removes every
    /// other entry there with its name that is not a volume root: an
    /// offline volume of that name (mounted again, or another volume with
    /// its name: the live entries replace the offline ones) or the plain
    /// folder the volume is mounted on (covered by the volume now). A plain
    /// folder alone, such as a mount point folder left behind after an
    /// unmount, replaces no offline volume.
    fn drop_replaced_offline(&mut self) {
        let Some(folder) = self.volumes_folder.as_deref() else {
            return;
        };
        let Some(folder_id) = paths::resolve(&self.index, &self.root, folder, None) else {
            return;
        };
        let home = self.index.key(folder_id).volume;
        let is_root = |v: VolumeId| v != home && !v.is_offline();
        let children = self.index.children(folder_id);
        if !children
            .iter()
            .any(|&c| self.index.key(c).volume.is_offline())
        {
            // Nothing offline: a covered folder can only be met at a mount,
            // which the writer checks there and re-keys.
            return;
        }
        let replaced: Vec<EntryId> = children
            .into_iter()
            .filter(|&c| !is_root(self.index.key(c).volume))
            .filter(|&c| {
                self.index.get(self.index.key(c)).is_some_and(|e| {
                    self.index
                        .children_named(folder_id, &e.name)
                        .into_iter()
                        .any(|n| is_root(self.index.key(n).volume))
                })
            })
            .collect();
        for id in replaced {
            self.remove_now(id);
        }
    }

    /// Merges the batch into the orders, compacts the index if due and
    /// publishes the new snapshot with `cursor`. A batch that changed
    /// nothing in the index (changes reported for entries that are already
    /// as the disk has them, or for excluded paths) makes no new
    /// generation: the newer cursor is published silently, so no window
    /// re-runs its search and no idle save is started for it.
    fn commit(&mut self, cursor: Option<ResumeCursor>) {
        if self.batch.is_empty() && !self.orders_stale {
            self.publish_cursor(cursor);
            return;
        }
        let snap = self.index.snapshot();
        let rebuild = self.orders_stale || self.batch.len() > self.index.live_count() / 2;
        self.orders_stale = false;
        let units = self.merge_units();
        // `pending` is held until the new snapshot is published.
        let (mut orders, pending): (Vec<Order>, Option<Pending>) = if rebuild {
            // One Name sort shared by every order (a first scan lands here).
            let keys: Vec<_> = self.orders.iter().map(|o| o.key).collect();
            (Order::build_keys(&*snap, &keys), None)
        } else if let Some((mut orders, pending)) = self.take_orders() {
            // Nothing else holds the orders: the batch goes into them in
            // place, and none is copied.
            for order in &mut orders {
                order.apply(&*snap, &self.batch);
            }
            (orders, Some(pending))
        } else {
            // The published snapshot shares `self.orders`, so each order is
            // read, not changed: `applied` copies it once into the new one.
            let orders = self
                .orders
                .iter()
                .map(|o| o.applied(&*snap, &self.batch))
                .collect();
            (orders, None)
        };
        self.batch.clear();
        if self.compaction_due() {
            // Compaction keeps live entries in ID order, so remapping an
            // order keeps it sorted.
            let remap = self.index.compact();
            self.fresh_base = true;
            for order in &mut orders {
                for id in &mut order.ids {
                    *id = remap[*id as usize].0;
                }
                debug_assert!(order.ids.iter().all(|&id| id != EntryId::NONE.0));
            }
        }
        self.orders = Arc::new(orders);
        self.group_stale = true;
        self.generation += 1;
        self.publish(cursor);
        if let Some(pending) = pending {
            // How long readers could have waited: the next estimate.
            self.in_place_cost = Some(pending.since.elapsed().as_nanos() as f64 / units);
            drop(pending);
        }
        if std::mem::take(&mut self.fresh_base) {
            // The snapshot just published holds exactly the new base: its
            // delta is empty and nothing is removed yet.
            self.shared
                .checkpoints
                .save_exact(self.shared.current.load_full());
        }
    }

    /// Serves the base from the store once the checkpoint thread has saved
    /// exactly this base and mapped the file (run 2.17 step 2): the heap
    /// copy goes when the last snapshot holding it does. The swap is
    /// published at once, under the same generation, so no window re-runs
    /// its search for it; it doesn't happen when the base was compacted
    /// again meanwhile.
    fn adopt_saved_base(&mut self) {
        let Some((saved, restored)) = self.shared.checkpoints.take_restored() else {
            return;
        };
        if !self.index.adopt_base(saved.index(), restored) {
            return;
        }
        let current = self.shared.current.load_full();
        let snap = EngineSnapshot {
            index: self.index.snapshot(),
            orders: Arc::clone(&self.orders),
            cursor: current.cursor,
            generation: self.generation,
            hide_offline: current.hide_offline,
            priority: self.group.clone(),
            orders_pending: false,
        };
        self.shared.current.store(Arc::new(snap));
    }

    /// Publishes `cursor` with the snapshot already published, without a
    /// new generation and without telling subscribers. A save keeps the
    /// cursor of the generation it saved, so a relaunch may replay a few
    /// changes that changed nothing: harmless, since applying them again
    /// changes nothing either.
    fn publish_cursor(&mut self, cursor: Option<ResumeCursor>) {
        let current = self.shared.current.load_full();
        if current.cursor == cursor {
            return;
        }
        let snap = EngineSnapshot {
            index: current.index.clone(),
            orders: Arc::clone(&current.orders),
            cursor,
            generation: current.generation,
            hide_offline: current.hide_offline,
            priority: current.priority.clone(),
            orders_pending: false,
        };
        self.shared.current.store(Arc::new(snap));
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
            self.group = self.priority.group(&self.index, &index, &self.orders);
            self.group_stale = false;
        }
        // A look at the volumes folder's children: offline entries are
        // hidden from searches only when there are some.
        let hide_offline =
            paths::has_offline(&self.index, &self.root, self.volumes_folder.as_deref());
        let snap = EngineSnapshot {
            index,
            orders: Arc::clone(&self.orders),
            cursor,
            generation: self.generation,
            hide_offline,
            priority: self.group.clone(),
            orders_pending: false,
        };
        self.shared.current.store(Arc::new(snap));
        self.shared.notify(self.generation);
    }

    /// The work of merging the batch into the orders, in the units of
    /// `in_place_cost`: each order is read once or twice whole, and each
    /// entry of the batch is sorted and placed by halving in every order
    /// (some 25 name comparisons each), counted as [`BATCH_WEIGHT`] order
    /// entries.
    fn merge_units(&self) -> f64 {
        self.index.live_count() as f64 + BATCH_WEIGHT * self.batch.len() as f64
    }

    /// Whether a merge in place is expected to end within
    /// [`IN_PLACE_BUDGET`], from the time the last one took per unit (or
    /// [`FIRST_NS_PER_UNIT`] before any). A refusal lowers the estimate a
    /// little, so a merge timed during a busy moment doesn't rule the
    /// in-place path out for good: it is tried, and timed, again later.
    fn in_place_fits(&mut self) -> bool {
        let per_unit = self.in_place_cost.unwrap_or(FIRST_NS_PER_UNIT);
        let expected = per_unit * self.merge_units();
        if expected <= IN_PLACE_BUDGET.as_nanos() as f64 {
            return true;
        }
        if let Some(cost) = &mut self.in_place_cost {
            *cost *= COST_DECAY;
        }
        false
    }

    /// Takes the published orders over, so the batch can be merged into
    /// them in place: only while no window shows, when no compaction is
    /// due, when the merge is expected to end well inside the wait readers
    /// accept ([`in_place_fits`](Self::in_place_fits)), and only when
    /// nothing but the writer and the published snapshot holds them.
    ///
    /// The writer always merged into a copy of each order, since the
    /// published snapshot still held the old one: 16 B per entry at every
    /// commit, about 51 MB at 3.2M entries after a rescan that changed
    /// anything. Instead it publishes the current snapshot without its
    /// orders, marked `orders_pending` (readers wait for the next one, see
    /// `snapshot::settled`), and takes the orders when that leaves them to
    /// it alone. A reader may hold the old snapshot (a window keeps the one
    /// it shows; a save may be writing it): then it is put back as it was
    /// and `None` returned, and the batch is merged into copies as before.
    /// While a window shows, searches never wait for a merge: the orders
    /// are copied, as before.
    fn take_orders(&mut self) -> Option<(Vec<Order>, Pending)> {
        // A compaction would run while readers wait; a merge expected to
        // take long would keep them waiting past `snapshot::SETTLE_LIMIT`.
        if self.foreground() || self.compaction_due() || !self.in_place_fits() {
            return None;
        }
        let current = self.shared.current.load_full();
        // Between commits the published orders are the writer's own.
        if !Arc::ptr_eq(&current.orders, &self.orders) {
            return None;
        }
        let interim = Arc::new(EngineSnapshot {
            orders: Arc::new(Vec::new()),
            orders_pending: true,
            ..(*current).clone()
        });
        drop(current);
        // The swap settles the loans of readers that are still reading the
        // old snapshot (arc-swap's `load`), so the counts below are exact.
        let since = Instant::now();
        let old = self.shared.current.swap(Arc::clone(&interim));
        let pending = Pending {
            shared: Arc::clone(&self.shared),
            interim,
            since,
        };
        let mut old = match Arc::try_unwrap(old) {
            Ok(old) => old,
            Err(held) => {
                self.shared.current.store(held);
                return None;
            }
        };
        let orders = std::mem::replace(&mut old.orders, Arc::new(Vec::new()));
        drop(std::mem::replace(&mut self.orders, Arc::new(Vec::new())));
        match Arc::try_unwrap(orders) {
            Ok(orders) => Some((orders, pending)),
            Err(held) => {
                // A reader kept a copy of the snapshot's fields.
                self.orders = Arc::clone(&held);
                old.orders = held;
                self.shared.current.store(Arc::new(old));
                None
            }
        }
    }
}

/// The snapshot the writer publishes while it merges the orders in place
/// ([`Writer::take_orders`]), held until the merge's result is published.
/// If the merge unwinds before that, dropping it publishes the same
/// snapshot without the pending mark, so readers don't wait for a snapshot
/// that won't come: an order they need is then built on the spot.
struct Pending {
    shared: Arc<Shared>,
    interim: Arc<EngineSnapshot>,
    /// When it was published: readers may wait from then on.
    since: Instant,
}

impl Drop for Pending {
    fn drop(&mut self) {
        let still_published = Arc::ptr_eq(&*self.shared.current.load(), &self.interim);
        if still_published {
            let mut snap = (*self.interim).clone();
            snap.orders_pending = false;
            self.shared.current.store(Arc::new(snap));
        }
    }
}

/// One entry of a full rescan of a loaded index: upserted, recorded in the
/// order batch when it changed, and marked seen. A rejected entry is
/// dropped, as during any full rescan: the rescan is already the fallback
/// for an inconsistent batch.
fn take_one(index: &mut Index, batch: &mut Batch, seen: &mut MatchSet, e: Entry) {
    let Ok((id, what)) = index.upsert_reporting(e) else {
        return;
    };
    match what {
        Upserted::Unchanged => {}
        Upserted::Moved => batch.moved(id),
        Upserted::Updated | Upserted::Inserted => batch.upsert(id),
    }
    // A new entry's ID may be past the bound the set was made for: it grows.
    seen.insert(id);
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicBool;

    use arc_swap::ArcSwap;

    use super::*;
    use crate::checkpoint::Checkpoints;
    use crate::health::HealthState;

    fn snapshot(generation: u64, orders_pending: bool) -> EngineSnapshot {
        EngineSnapshot {
            index: Index::new().snapshot(),
            orders: Arc::new(Vec::new()),
            cursor: None,
            generation,
            hide_offline: false,
            priority: None,
            orders_pending,
        }
    }

    fn shared(current: Arc<EngineSnapshot>) -> Arc<Shared> {
        Arc::new(Shared {
            current: ArcSwap::new(current),
            subscribers: Mutex::new(Vec::new()),
            health: HealthState::new(EngineHealth::Live),
            paused: AtomicBool::new(false),
            foreground: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            checkpoints: Checkpoints::new(None, None, Duration::from_secs(60), None),
        })
    }

    #[test]
    fn a_merge_that_unwinds_leaves_no_snapshot_pending() {
        let interim = Arc::new(snapshot(3, true));
        let shared = shared(Arc::clone(&interim));
        let pending = Pending {
            shared: Arc::clone(&shared),
            interim,
            since: Instant::now(),
        };
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _pending = pending;
            panic!("the merge failed");
        }));
        assert!(unwound.is_err());
        let now = shared.current.load_full();
        assert!(!now.orders_pending, "readers would wait for nothing");
        assert_eq!(now.generation, 3, "the same snapshot, without its mark");
    }

    #[test]
    fn a_published_merge_is_left_alone() {
        let interim = Arc::new(snapshot(3, true));
        let shared = shared(Arc::clone(&interim));
        let pending = Pending {
            shared: Arc::clone(&shared),
            interim,
            since: Instant::now(),
        };
        let published = Arc::new(snapshot(4, false));
        shared.current.store(Arc::clone(&published));
        drop(pending);
        assert!(Arc::ptr_eq(&shared.current.load_full(), &published));
    }

    #[test]
    fn the_writer_waits_less_than_the_readers_accept() {
        // The owner's bound for a search waiting on a hidden merge (11 Nov),
        // and the writer's own budget well inside it.
        assert!(crate::snapshot::SETTLE_LIMIT <= Duration::from_millis(100));
        assert!(IN_PLACE_BUDGET * 2 <= crate::snapshot::SETTLE_LIMIT);
    }

    #[test]
    fn a_merge_expected_past_the_budget_is_not_done_in_place() {
        // The first estimate: a small batch over a few million entries
        // fits; the whole-disk rescans measured on 11 Nov (74,000 changes
        // and more, 1.1 s and more in place) don't, nor does a far smaller
        // batch.
        let budget = IN_PLACE_BUDGET.as_nanos() as f64;
        let entries = 3_500_000.0;
        assert!(FIRST_NS_PER_UNIT * (entries + BATCH_WEIGHT * 100.0) <= budget);
        for batch in [1_000.0, 74_000.0] {
            assert!(FIRST_NS_PER_UNIT * (entries + BATCH_WEIGHT * batch) > budget);
        }
        // A refusal lowers a timed estimate, so the path is tried again,
        // but only after many commits (minutes of steady changes).
        let slow = budget / entries * 1.5;
        let mut cost = slow;
        let mut refusals = 0;
        while cost * entries > budget {
            cost *= COST_DECAY;
            refusals += 1;
        }
        assert!((50..=120).contains(&refusals), "{refusals} refusals");
    }
}
