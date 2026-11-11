//! The checkpoint policy: saving published snapshots with their resume
//! cursor through fx-store, and loading them back at startup.
//!
//! A checkpoint is always written from one published [`EngineSnapshot`], so
//! the saved cursor is exactly the one covering the saved index and orders.
//! Saves happen on a dedicated thread (so the writer keeps applying changes)
//! when the engine has been idle for the configured delay after the last
//! published change, at the latest the configured max delay after the first
//! change not yet saved (a whole volume is rarely idle for long), and as
//! soon as a full rescan has been published. They also happen on request
//! (done at once when the store already holds the latest snapshot) and on
//! an orderly shutdown (the only save that sets the clean-shutdown flag).
//!
//! When the app quits ([`QuitSave`]) nothing is written if the
//! store holds a resume cursor: the next launch loads it and replays the
//! changes made since, as after a crash. Only a store without one (a first
//! scan whose save has not started) is written, and a save already in
//! flight is waited for instead of being followed by another.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use fx_index::Index;
use fx_model::{EntryId, ResumeCursor, VolumeId};
use fx_sort::{Order, SortKey};
use fx_store::{MappedIndex, SaveInput, StoreError, create_private_dir, write_checkpoint};

use crate::snapshot::{ENGINE_SORT_KEYS, EngineSnapshot, settled};

/// Why a checkpoint was not written.
#[derive(Debug)]
pub enum CheckpointError {
    /// The engine's config has no store path.
    NoStorePath,
    /// fx-store refused or failed to write the file; the previous file is
    /// untouched.
    Store(StoreError),
}

impl fmt::Display for CheckpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CheckpointError::NoStorePath => f.write_str("no checkpoint file is configured"),
            CheckpointError::Store(e) => write!(f, "checkpoint failed: {e}"),
        }
    }
}

impl std::error::Error for CheckpointError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CheckpointError::NoStorePath => None,
            CheckpointError::Store(e) => Some(e),
        }
    }
}

impl From<StoreError> for CheckpointError {
    fn from(e: StoreError) -> Self {
        CheckpointError::Store(e)
    }
}

/// The fx-store key code a sort order is saved under.
fn order_code(key: SortKey) -> Option<u8> {
    match key {
        SortKey::Name => Some(1),
        SortKey::Path => Some(2),
        SortKey::Size => Some(3),
        SortKey::Modified => Some(4),
        _ => None,
    }
}

/// The volume of the index's root entry (the entry with no parent).
fn root_volume(idx: &fx_index::Snapshot) -> Option<VolumeId> {
    idx.live_ids()
        .find(|&id| {
            let p = idx.parent(id);
            p.is_none() || p == id
        })
        .map(|id: EntryId| idx.key(id).volume)
}

/// A store accepted at startup.
pub(crate) struct Loaded {
    pub(crate) index: Index,
    /// The saved orders, when every engine order was saved and valid.
    pub(crate) orders: Option<Vec<Order>>,
    /// The cursor saved for the root's volume (or the only one saved).
    pub(crate) cursor: Option<ResumeCursor>,
}

/// Loads and fully verifies the store at `path`. `None` when it is
/// missing or rejected (corrupt, old format, inconsistent index).
pub(crate) fn load(path: &Path) -> Option<Loaded> {
    let _ = fx_store::cleanup_stale_temps(path);
    let mapped = MappedIndex::open(path).ok()?;
    mapped.verify_checksums().ok()?;
    // The base columns stay in the file's map instead of being copied to
    // the heap (run 2.17 step 2).
    let index = mapped.share_index().ok()?;
    let (orders, cursor) = {
        let snap = index.snapshot();
        let cursor = root_volume(&snap)
            .and_then(|v| mapped.cursor(v))
            .or_else(|| match mapped.cursors() {
                [(_, only)] => Some(*only),
                _ => None,
            });
        let orders: Option<Vec<Order>> = ENGINE_SORT_KEYS
            .iter()
            .map(|&key| {
                let order = Order {
                    key,
                    ids: mapped.order(order_code(key)?)?.to_vec(),
                };
                order.validate(&*snap).ok()?;
                Some(order)
            })
            .collect();
        (orders, cursor)
    };
    Some(Loaded {
        index,
        orders,
        cursor,
    })
}

/// What [`Engine::checkpoint_for_quit`](crate::Engine::checkpoint_for_quit)
/// does with the store when the app quits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuitSave {
    /// Writes nothing: the store holds the latest snapshot.
    Skip,
    /// Waits for the save being written now, and writes nothing more; the
    /// next launch replays what came after it.
    WaitForRunning,
    /// Writes the latest snapshot: the store is behind it.
    Write,
}

/// The store's state when the app quits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct QuitState {
    /// A save is being written now.
    pub(crate) save_running: bool,
    /// The store holds the latest published snapshot.
    pub(crate) latest_saved: bool,
}

/// The quit decision: write what the store lacks, since the app saves the
/// store only once a day and the next launch would otherwise replay up to
/// a day of changes; never a second save behind a running one.
pub(crate) fn quit_save(state: QuitState) -> QuitSave {
    if state.latest_saved {
        QuitSave::Skip
    } else if state.save_running {
        QuitSave::WaitForRunning
    } else {
        QuitSave::Write
    }
}

/// When the next save is due, for a snapshot whose last change was
/// published at `last_change` and whose first unsaved change at
/// `first_unsaved` (`None`: nothing unsaved): `idle` after the last
/// change, at the latest `max_delay` after the first unsaved one. `None`
/// when neither applies (no idle delay and nothing unsaved) or a delay is
/// too long to add to an instant.
fn due_at(
    last_change: Instant,
    idle: Option<Duration>,
    first_unsaved: Option<Instant>,
    max_delay: Duration,
) -> Option<Instant> {
    let idle_due = idle.and_then(|d| last_change.checked_add(d));
    let max_due = first_unsaved.and_then(|first| first.checked_add(max_delay));
    match (idle_due, max_due) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// The extended attribute Time Machine reads to leave an item out of
/// backups (what `tmutil addexclusion` sets), and its value: a binary
/// property list holding the string `com.apple.backupd`.
#[cfg(target_os = "macos")]
const BACKUP_EXCLUDE_NAME: &std::ffi::CStr = c"com.apple.metadata:com_apple_backup_excludeItem";
#[cfg(target_os = "macos")]
const BACKUP_EXCLUDE_VALUE: [u8; 61] = [
    0x62, 0x70, 0x6c, 0x69, 0x73, 0x74, 0x30, 0x30, 0x5f, 0x10, 0x11, 0x63, 0x6f, 0x6d, 0x2e, 0x61,
    0x70, 0x70, 0x6c, 0x65, 0x2e, 0x62, 0x61, 0x63, 0x6b, 0x75, 0x70, 0x64, 0x08, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1c,
];

/// Leaves the store at `path` out of Time Machine backups (macOS only).
/// The index is rebuilt from the disk when lost, and without the mark each
/// hourly backup would copy the whole file after every save. The mark lives
/// on the file and every save writes a new one, so every save sets it. A
/// failure is ignored: the store is still valid, only backed up.
pub(crate) fn exclude_from_backups(path: &Path) {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return;
        };
        // SAFETY: both strings are NUL-terminated and live for the call; the
        // value pointer and length describe the constant above.
        unsafe {
            libc::setxattr(
                c_path.as_ptr(),
                BACKUP_EXCLUDE_NAME.as_ptr(),
                BACKUP_EXCLUDE_VALUE.as_ptr().cast(),
                BACKUP_EXCLUDE_VALUE.len(),
                0,
                0,
            );
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = path;
}

/// Writes `snap` to `path` atomically (mode 0600), creating a missing
/// parent directory with mode 0700 (`fx_store::create_private_dir`). An
/// existing directory keeps its mode: the store path can lie in any
/// folder. The file is left out of Time Machine backups.
fn write(snap: &EngineSnapshot, path: &Path, clean: bool) -> Result<(), StoreError> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        create_private_dir(dir).map_err(StoreError::Io)?;
    }
    let idx = snap.index();
    let parts = idx.to_parts();
    let volumes: Vec<(VolumeId, ResumeCursor)> = match (root_volume(idx), snap.cursor()) {
        (Some(volume), Some(cursor)) => vec![(volume, cursor)],
        _ => Vec::new(),
    };
    let orders: Vec<(u8, &[u32])> = snap
        .orders
        .iter()
        .filter_map(|o| order_code(o.key).map(|c| (c, o.ids.as_slice())))
        .collect();
    write_checkpoint(
        path,
        &SaveInput {
            parts: &parts,
            volumes: &volumes,
            orders: &orders,
            clean_shutdown: clean,
        },
    )?;
    exclude_from_backups(path);
    Ok(())
}

struct State {
    /// When the last change was published.
    last_change: Instant,
    /// When the first change not yet saved was published.
    first_unsaved: Option<Instant>,
    /// A save was asked for by [`Checkpoints::request`].
    requested: bool,
    /// Generation of the last snapshot saved (or loaded unchanged).
    saved: Option<u64>,
    stop: bool,
    /// A snapshot to save as it is ([`Checkpoints::save_exact`]).
    exact: Option<Arc<EngineSnapshot>>,
}

/// Checkpoint bookkeeping shared by the engine handle, the writer and the
/// idle-checkpoint thread.
pub(crate) struct Checkpoints {
    path: Option<PathBuf>,
    /// `None`: no save after a quiet spell, only the longest delay.
    idle: Option<Duration>,
    max_delay: Duration,
    state: Mutex<State>,
    wake: Condvar,
    /// Serialises saves, so a later snapshot is never overwritten by an
    /// earlier one still being written.
    save_lock: Mutex<()>,
    /// The index restored from the latest exact save, served from the
    /// file's map, with the snapshot it saved; the writer takes it.
    restored: Mutex<Option<(Arc<EngineSnapshot>, Index)>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Checkpoints {
    /// `saved` is the generation the store already holds, if any.
    pub(crate) fn new(
        path: Option<PathBuf>,
        idle: Option<Duration>,
        max_delay: Duration,
        saved: Option<u64>,
    ) -> Self {
        Checkpoints {
            path,
            idle,
            max_delay,
            state: Mutex::new(State {
                last_change: Instant::now(),
                first_unsaved: None,
                requested: false,
                saved,
                stop: false,
                exact: None,
            }),
            wake: Condvar::new(),
            save_lock: Mutex::new(()),
            restored: Mutex::new(None),
        }
    }

    pub(crate) fn enabled(&self) -> bool {
        self.path.is_some()
    }

    /// Called by the writer after publishing a changed snapshot.
    pub(crate) fn changed(&self) {
        if self.enabled() {
            let now = Instant::now();
            let mut st = lock(&self.state);
            st.last_change = now;
            st.first_unsaved.get_or_insert(now);
            drop(st);
            self.wake.notify_all();
        }
    }

    /// Asks the checkpoint thread to save the latest published snapshot
    /// now, without waiting for the idle delay. Called by the writer after
    /// publishing a full rescan.
    pub(crate) fn request(&self) {
        if self.enabled() {
            lock(&self.state).requested = true;
            self.wake.notify_all();
        }
    }

    /// Saves `snap` as it is, soon, on the checkpoint thread: the writer
    /// asks for this right after a first scan or a compaction, whose
    /// snapshot holds exactly the new base. Once the file is written, the
    /// index restored from its map waits in
    /// [`take_restored`](Checkpoints::take_restored), so the writer can
    /// serve the base from the file instead of the heap.
    pub(crate) fn save_exact(&self, snap: Arc<EngineSnapshot>) {
        if self.enabled() {
            lock(&self.state).exact = Some(snap);
            self.wake.notify_all();
        }
    }

    /// The index restored from the latest exact save and the snapshot it
    /// saved, once.
    pub(crate) fn take_restored(&self) -> Option<(Arc<EngineSnapshot>, Index)> {
        lock(&self.restored).take()
    }

    /// Writes `snap` and maps the file it wrote. Nothing is written once
    /// the engine is stopping or a newer snapshot is saved (an exact save
    /// never replaces a later one); a failed write leaves the usual saves
    /// to catch up, and a file that doesn't map back leaves the base on the
    /// heap.
    fn save_exact_now(&self, current: &ArcSwap<EngineSnapshot>, snap: &Arc<EngineSnapshot>) {
        let Some(path) = self.path.as_deref() else {
            return;
        };
        // Held until the file is mapped, so no other save replaces it first.
        let _guard = lock(&self.save_lock);
        {
            let st = lock(&self.state);
            if st.stop || st.saved.is_some_and(|g| g > snap.generation) {
                return;
            }
        }
        if self.write_snapshot(path, snap, current, false).is_err() {
            return;
        }
        let restored = MappedIndex::open(path)
            .ok()
            .filter(|mapped| mapped.verify_checksums().is_ok())
            .and_then(|mapped| mapped.share_index().ok());
        if let Some(index) = restored {
            *lock(&self.restored) = Some((Arc::clone(snap), index));
        }
    }

    /// When the store was last written (its modification time); `None`
    /// without a store path or a store.
    pub(crate) fn saved_at(&self) -> Option<std::time::SystemTime> {
        let path = self.path.as_deref()?;
        std::fs::metadata(path).ok()?.modified().ok()
    }

    /// Whether the store holds the latest published snapshot.
    fn latest_saved(&self, current: &ArcSwap<EngineSnapshot>) -> bool {
        lock(&self.state).saved == Some(current.load().generation)
    }

    /// Saves the latest published snapshot. Without `clean`, returns at
    /// once when the store already holds it, also after waiting for a save
    /// in flight that wrote it; a clean (shutdown) save always writes, to
    /// set the flag.
    pub(crate) fn save(
        &self,
        current: &ArcSwap<EngineSnapshot>,
        clean: bool,
    ) -> Result<(), CheckpointError> {
        let path = self.path.as_deref().ok_or(CheckpointError::NoStorePath)?;
        if !clean && self.latest_saved(current) {
            return Ok(());
        }
        let _guard = lock(&self.save_lock);
        if !clean && self.latest_saved(current) {
            return Ok(());
        }
        // Never the snapshot of an in-place merge, which has no orders: the
        // store would then have none to load.
        let snap = settled(current);
        self.write_snapshot(path, &snap, current, clean)
    }

    /// The save when the app quits (see [`QuitSave`]). No idle save starts
    /// afterwards.
    pub(crate) fn save_for_quit(
        &self,
        current: &ArcSwap<EngineSnapshot>,
    ) -> Result<QuitSave, CheckpointError> {
        let path = self.path.as_deref().ok_or(CheckpointError::NoStorePath)?;
        // A save in flight holds the lock; holding it here keeps another
        // from starting while this one decides and writes.
        let guard = match self.save_lock.try_lock() {
            Ok(guard) => Some(guard),
            Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        };
        let decision = {
            let mut st = lock(&self.state);
            st.stop = true;
            quit_save(QuitState {
                save_running: guard.is_none(),
                latest_saved: st.saved == Some(current.load().generation),
            })
        };
        self.wake.notify_all();
        match decision {
            QuitSave::Skip => {}
            QuitSave::WaitForRunning => drop(lock(&self.save_lock)),
            QuitSave::Write => {
                let snap = settled(current);
                self.write_snapshot(path, &snap, current, false)?;
            }
        }
        drop(guard);
        Ok(decision)
    }

    /// Writes `snap` and records it. The caller holds `save_lock`.
    fn write_snapshot(
        &self,
        path: &Path,
        snap: &EngineSnapshot,
        current: &ArcSwap<EngineSnapshot>,
        clean: bool,
    ) -> Result<(), CheckpointError> {
        write(snap, path, clean)?;
        let mut st = lock(&self.state);
        st.saved = Some(st.saved.map_or(snap.generation, |g| g.max(snap.generation)));
        if current.load().generation == snap.generation {
            st.requested = false;
            st.first_unsaved = None;
        } else {
            // A change published while this one was written is not saved
            // yet; a pending request still applies to it.
            st.first_unsaved = Some(Instant::now());
        }
        Ok(())
    }

    /// The idle-checkpoint loop; returns after [`stop`](Self::stop).
    pub(crate) fn run_idle(&self, current: &ArcSwap<EngineSnapshot>) {
        let mut st = lock(&self.state);
        loop {
            if st.stop {
                return;
            }
            if let Some(snap) = st.exact.take() {
                drop(st);
                self.save_exact_now(current, &snap);
                st = lock(&self.state);
                continue;
            }
            let generation = current.load().generation;
            if st.saved == Some(generation) {
                st = self
                    .wake
                    .wait(st)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                continue;
            }
            let now = Instant::now();
            let due = if st.requested {
                Some(now)
            } else {
                due_at(st.last_change, self.idle, st.first_unsaved, self.max_delay)
            };
            let Some(due) = due else {
                // Nothing comes due by itself: wait for a change or a request.
                st = self
                    .wake
                    .wait(st)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                continue;
            };
            if now >= due {
                drop(st);
                let result = self.save(current, false);
                st = lock(&self.state);
                if result.is_err() {
                    // Retry later instead of spinning.
                    let now = Instant::now();
                    st.last_change = now;
                    st.first_unsaved = Some(now);
                    st.requested = false;
                }
                continue;
            }
            st = self
                .wake
                .wait_timeout(st, due - now)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }

    /// Ends [`run_idle`](Self::run_idle).
    pub(crate) fn stop(&self) {
        lock(&self.state).stop = true;
        self.wake.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    use fx_model::{Entry, EntryFlags, EntryKind, FileKey};

    use super::*;

    #[test]
    fn the_quit_decision() {
        use QuitSave::{Skip, WaitForRunning, Write};
        // (save running, latest saved) and what quitting does. Unsaved
        // changes are written whether or not the store holds a resume
        // cursor, since the store is saved only once a day.
        let table = [
            ((false, false), Write),
            ((true, false), WaitForRunning),
            ((false, true), Skip),
            ((true, true), Skip),
        ];
        for ((save_running, latest_saved), want) in table {
            let state = QuitState {
                save_running,
                latest_saved,
            };
            assert_eq!(quit_save(state), want, "{state:?}");
        }
    }

    #[test]
    fn no_save_is_due_without_an_idle_delay_until_the_longest_delay() {
        let now = Instant::now();
        let hour = Duration::from_secs(3600);
        // The idle save is off: only the longest delay makes a save due.
        assert_eq!(
            due_at(now, None, Some(now), 48 * hour),
            Some(now + 48 * hour)
        );
        assert_eq!(due_at(now, None, None, 48 * hour), None, "nothing unsaved");
        // With an idle delay, the earlier of the two.
        let idle = Some(Duration::from_secs(30));
        assert_eq!(
            due_at(now, idle, Some(now), 48 * hour),
            Some(now + Duration::from_secs(30))
        );
        // A delay too long to add to an instant never comes due.
        assert_eq!(due_at(now, Some(Duration::MAX), None, Duration::MAX), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_saved_store_is_kept_out_of_time_machine() {
        let dir = Scratch::new("backup-exclusion");
        let path = dir.0.join("index.fxstore");
        std::fs::write(&path, b"store").expect("write a file");
        exclude_from_backups(&path);
        // The temp folder is excluded from backups by path anyway, so
        // `tmutil isexcluded` would pass without the mark: read the mark.
        let out = std::process::Command::new("xattr")
            .arg("-px")
            .arg("com.apple.metadata:com_apple_backup_excludeItem")
            .arg(&path)
            .output()
            .expect("run xattr");
        assert!(out.status.success(), "the file carries no exclusion mark");
        let hex: String = String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .collect();
        // A binary property list holding the string "com.apple.backupd".
        assert!(hex.starts_with("62706C6973743030"), "{hex}");
        assert!(hex.contains("636F6D2E6170706C652E6261636B757064"), "{hex}");
    }

    /// A scratch folder for one test, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let dir = std::env::temp_dir().join(format!(
                "fx-engine-unit-{name}-{}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).expect("create scratch dir");
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A published snapshot of a root folder holding one file, with an
    /// FSEvents cursor.
    fn snapshot(generation: u64) -> EngineSnapshot {
        let key = |id| FileKey::new(VolumeId(1), id);
        let mut index = Index::new();
        for (id, parent, name, kind) in [
            (1, None, "vol", EntryKind::Dir),
            (2, Some(key(1)), "a.txt", EntryKind::File),
        ] {
            index
                .upsert(Entry {
                    key: key(id),
                    parent,
                    name: name.to_string(),
                    kind,
                    flags: EntryFlags::empty(),
                    size: 1,
                    modified: 0,
                    created: 0,
                })
                .expect("upsert into a fresh index");
        }
        let snap = index.snapshot();
        let orders = ENGINE_SORT_KEYS
            .iter()
            .map(|&key| Order::build(&*snap, key))
            .collect();
        EngineSnapshot {
            index: snap,
            orders: Arc::new(orders),
            cursor: Some(ResumeCursor::FsEvents {
                event_id: generation,
                db_uuid: [7; 16],
            }),
            generation,
            priority: None,
            orders_pending: false,
        }
    }

    /// Changes with every write: the file is replaced by a renamed temp
    /// file, with a new modification time (and a new inode on Unix).
    fn file_version(path: &Path) -> (SystemTime, u64) {
        let meta = fs::metadata(path).expect("store exists");
        #[cfg(unix)]
        let inode = std::os::unix::fs::MetadataExt::ino(&meta);
        #[cfg(not(unix))]
        let inode = 0;
        (meta.modified().expect("modification time"), inode)
    }

    #[test]
    fn a_quit_during_a_background_save_waits_for_it_and_writes_nothing_more() {
        let dir = Scratch::new("quit-during-save");
        let path = dir.0.join("index.fxdb");
        let hour = Duration::from_secs(3600);
        let checkpoints = Arc::new(Checkpoints::new(Some(path.clone()), Some(hour), hour, None));
        let current = Arc::new(ArcSwap::from_pointee(snapshot(1)));

        // A background save of generation 1 (the first scan): it holds the
        // save lock as `save` does, and writes once the quit has begun.
        let (started_tx, started_rx) = mpsc::channel();
        let background = {
            let checkpoints = Arc::clone(&checkpoints);
            let current = Arc::clone(&current);
            let path = path.clone();
            thread::spawn(move || {
                let _guard = lock(&checkpoints.save_lock);
                let snap = current.load_full();
                started_tx.send(()).expect("the test waits for the start");
                let end = Instant::now() + Duration::from_secs(10);
                while !lock(&checkpoints.state).stop && Instant::now() < end {
                    thread::sleep(Duration::from_millis(1));
                }
                checkpoints
                    .write_snapshot(&path, &snap, &current, false)
                    .expect("background save");
                file_version(&path)
            })
        };
        started_rx.recv().expect("the background save started");
        // A change published while it is written: not saved by it.
        current.store(Arc::new(snapshot(2)));

        let decision = checkpoints.save_for_quit(&current).expect("quit");
        let after_background = background.join().expect("background save thread");
        assert_eq!(decision, QuitSave::WaitForRunning);
        assert_eq!(
            file_version(&path),
            after_background,
            "the quit wrote the store a second time"
        );
        let mapped = MappedIndex::open(&path).expect("the store loads");
        assert_eq!(
            mapped.cursors().len(),
            1,
            "the background save holds the resume cursor"
        );
    }
}
