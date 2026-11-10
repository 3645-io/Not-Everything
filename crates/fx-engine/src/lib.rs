//! The search engine that owns the index.
//!
//! An [`Engine`] owns one volume's [`fx_index::Index`] and its pre-sorted
//! [`fx_sort::Order`]s (Name, Path, Size, Modified). A dedicated writer
//! thread is the only code that changes them:
//!
//! 1. It collects changes from an [`fx_fs::ChangeSource`] for one batch
//!    window (default 100 ms, clamped to 50–200 ms).
//! 2. It checks every reported path on disk through [`fx_fs::PathStat`] and
//!    [`fx_fs::reconcile_path`]; `RescanSubtree` re-enumerates the folder
//!    with the [`fx_fs::VolumeEnumerator`] and applies
//!    [`fx_fs::diff_subtree`]; `RescanVolume` (and a `GapDetected` or
//!    `NoCursorSupport` start, or an empty index) runs a full rescan while
//!    readers keep the previous snapshot. A folder found at a path under a
//!    new key, including one moved in from elsewhere, is re-enumerated too,
//!    because changes inside a moved folder may have been reported under
//!    its old path. Enumerators that do not emit the folder they list (the
//!    macOS scanner) get its entry from `PathStat`, and the watched root
//!    is always indexed as the parentless root entry.
//! 3. It applies the result to the index and merges it into every order
//!    with [`Order::apply`](fx_sort::Order::apply): a changed name or parent
//!    goes through `Batch::moved`, so a moved folder carries its subtree in
//!    the Path order; attribute-only changes go through `Batch::upsert`.
//! 4. It publishes an immutable [`EngineSnapshot`] (index snapshot, orders,
//!    cursor, generation and, with
//!    [`EngineConfig::with_priority_folders`], the priority group of "Your
//!    files first", rebuilt from the Path order when the index changed)
//!    through `arc-swap`, then sends the new generation to every
//!    subscriber. The cursor is only ever published together with the
//!    batch it covers.
//!
//! Recovery: after [`EngineConfig::restart_threshold`] consecutive
//! `next_batch` errors (a stopped stream fails every call), the writer
//! calls [`ChangeSource::start`] again with the last published cursor,
//! waiting [`EngineConfig::restart_backoff`] after a failed attempt and
//! doubling the wait up to [`MAX_RESTART_BACKOFF`]. A restart that reports
//! `GapDetected` or `NoCursorSupport` runs a full rescan while the previous
//! snapshot keeps serving. [`Engine::health`] reports [`EngineHealth`]
//! (`Live`, `Recovering`, or `Failed` after repeated failed restarts) and
//! [`Engine::subscribe_health`] is told about every change.
//!
//! Readers call [`Engine::snapshot`], which is lock-free and never waits
//! for the writer. [`Engine::search`] runs a query over the latest snapshot
//! (an [`EngineSnapshot`] is an [`fx_query::SearchView`]) through fx-query's
//! refinement cache and cancellation, then reads one window of the matches
//! in a merged sort order with [`fx_sort::emit`]. Every search takes the
//! search field's [`QueryOptions`] (match case, match path, and the whole
//! query as one regular expression) and reports an invalid regular
//! expression as [`SearchError::InvalidRegex`]. The snapshot keeps no
//! paths and no folded copy of the names: the matcher folds names as it
//! scans, and a path term is decided per folder and propagated down the
//! tree (fx-query's folder memo, one byte per entry for the duration of a
//! search) instead of building every entry's full path. [`Engine::search_grouped`]
//! can list the priority group first instead ([`fx_sort::emit_tiered`]).
//! [`Engine::positions`] answers the reverse question for a few known
//! entries: where, by path, they sit in those results, with one pass over
//! them; [`Engine::positions_grouped`] does so in the grouped results.
//!
//! Checkpoints (see [`EngineConfig::with_store_path`]): the latest snapshot
//! is saved with its resume cursor through `fx_store::write_checkpoint`, and
//! left out of Time Machine backups, when the engine has been idle for
//! [`EngineConfig::checkpoint_idle`] after the last change (off by default),
//! at the latest [`EngineConfig::checkpoint_max_delay`] after the first
//! unsaved change, right after a full rescan is published, on request
//! ([`Engine::checkpoint`], which returns at once when the store already
//! holds the latest snapshot, or [`Engine::request_checkpoint`], which
//! leaves the save to the checkpoint thread) and on an orderly
//! [`Engine::shutdown`], which alone sets the clean-shutdown flag. An app
//! that quits calls [`Engine::checkpoint_for_quit`], which writes what the
//! store lacks. The writer thread and the checkpoint thread run at the
//! utility quality-of-service class ([`qos`]).
//! [`Engine::open`] loads the store and resumes the change source from the
//! saved cursor, falling back to a full rescan when the store is missing or
//! rejected, or the source reports a gap.
//!
//! Read-only access (for the command line): [`StoreReader::open`] loads a
//! checkpoint without a change source, threads or any write to the store,
//! and [`StoreReader::search`] answers queries over it with the same query
//! and sort path as [`Engine::search`], plus the search toggles.
//!
//! Measurement: [`EngineSnapshot::memory_report`] returns a
//! [`MemoryReport`] with the heap bytes of the default columns and the
//! bytes per live entry (the Phase 1 memory gate).

mod checkpoint;
mod config;
mod health;
mod memory;
mod paths;
mod priority;
pub mod qos;
mod query;
mod reader;
mod snapshot;
mod writer;

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use arc_swap::ArcSwap;
use fx_fs::{ChangeSource, PathStat, VolumeEnumerator};
use fx_index::Index;
use fx_model::{EntryId, StartOutcome};
use fx_query::{CancelToken, Searcher};
use fx_sort::{Batch, Direction, MatchSet, Order, SortKey};

pub use checkpoint::{CheckpointError, QuitSave};
pub use config::{
    DEFAULT_BACKGROUND_BATCH_WINDOW, DEFAULT_BATCH_WINDOW, DEFAULT_CHECKPOINT_IDLE,
    DEFAULT_CHECKPOINT_MAX_DELAY, DEFAULT_RESTART_BACKOFF, DEFAULT_RESTART_THRESHOLD, EngineConfig,
    MAX_BACKGROUND_BATCH_WINDOW, MAX_BATCH_WINDOW, MAX_RESTART_BACKOFF, MIN_BATCH_WINDOW,
};
pub use fx_query::QueryOptions;
pub use health::EngineHealth;
pub use memory::MemoryReport;
pub use query::{Page, Positions};
pub use reader::{ReadError, ReadOptions, ReadPage, SearchError, StoreReader};
pub use snapshot::{ENGINE_SORT_KEYS, EngineSnapshot, Generation};

use checkpoint::Checkpoints;
use health::HealthState;
use priority::PriorityFolders;
use writer::{Recovery, Writer};

/// State shared between the [`Engine`] handle and its threads.
pub(crate) struct Shared {
    current: ArcSwap<EngineSnapshot>,
    subscribers: Mutex<Vec<Sender<Generation>>>,
    health: HealthState,
    paused: AtomicBool,
    /// A window shows the results ([`Engine::set_foreground`]).
    foreground: AtomicBool,
    stop: AtomicBool,
    checkpoints: Checkpoints,
}

impl Shared {
    /// Sends `generation` to every subscriber, dropping disconnected ones,
    /// and restarts the idle-checkpoint delay.
    fn notify(&self, generation: u64) {
        {
            let mut subs = self
                .subscribers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            subs.retain(|tx| tx.send(Generation(generation)).is_ok());
        }
        self.checkpoints.changed();
    }
}

/// The owner of one volume's live index: a writer thread plus lock-free
/// snapshots for readers. See the crate documentation.
///
/// Dropping the engine stops and joins its threads like
/// [`shutdown`](Engine::shutdown) but saves no checkpoint: only an orderly
/// shutdown does.
pub struct Engine {
    shared: Arc<Shared>,
    writer: Option<JoinHandle<()>>,
    checkpointer: Option<JoinHandle<()>>,
    searcher: Mutex<Searcher>,
    /// The window hid while a search held the cache: drop it after that
    /// search ([`Engine::set_foreground`]).
    drop_cache: AtomicBool,
    query_generation: fx_query::Generation,
    utc_offset: Option<i32>,
}

impl Engine {
    /// Loads the store at the config's store path and starts the engine
    /// over it.
    ///
    /// A valid store serves its index (and saved orders) at once and hands
    /// its saved cursor to [`ChangeSource::start`]. A missing or rejected
    /// (corrupt) store starts from an empty index with no cursor, which the
    /// writer fills with a full rescan; so does a store without a cursor,
    /// and a `GapDetected` or `NoCursorSupport` start. A bad store file
    /// never makes opening fail; the next checkpoint replaces it.
    ///
    /// Errors: the source's `start` error, or a failure to spawn a thread.
    pub fn open<C, P, V>(
        mut config: EngineConfig,
        source: C,
        stat: P,
        enumerator: V,
    ) -> io::Result<Engine>
    where
        C: ChangeSource + Send + 'static,
        P: PathStat + Send + 'static,
        V: VolumeEnumerator + Send + 'static,
    {
        match config.store_path().and_then(checkpoint::load) {
            Some(loaded) => {
                let force_rescan = loaded.cursor.is_none();
                config.set_resume_from(loaded.cursor);
                Self::launch(
                    loaded.index,
                    loaded.orders,
                    config,
                    force_rescan,
                    Some(0),
                    source,
                    stat,
                    enumerator,
                )
            }
            None => {
                config.set_resume_from(None);
                Self::launch(
                    Index::new(),
                    None,
                    config,
                    true,
                    None,
                    source,
                    stat,
                    enumerator,
                )
            }
        }
    }

    /// Starts the change source from the config's resume cursor and spawns
    /// the writer thread over `index` (empty, or loaded from fx-store).
    ///
    /// The first snapshot, serving `index` as given with freshly built
    /// orders, is available as soon as this returns. A full rescan is
    /// scheduled when the source reports a gap or has no cursor support, or
    /// when `index` is empty.
    ///
    /// With a store path in the config, checkpoints are written as
    /// described in the crate documentation (use [`Engine::open`] to also
    /// load it).
    ///
    /// Errors: the source's `start` error, or a failure to spawn a thread.
    pub fn start<C, P, V>(
        index: Index,
        config: EngineConfig,
        source: C,
        stat: P,
        enumerator: V,
    ) -> io::Result<Engine>
    where
        C: ChangeSource + Send + 'static,
        P: PathStat + Send + 'static,
        V: VolumeEnumerator + Send + 'static,
    {
        Self::launch(index, None, config, false, None, source, stat, enumerator)
    }

    /// Starts the source and spawns the threads. `orders` are reused when
    /// given (already validated against `index`); `saved` is the generation
    /// already held by the store, if any.
    #[allow(clippy::too_many_arguments)]
    fn launch<C, P, V>(
        mut index: Index,
        orders: Option<Vec<Order>>,
        config: EngineConfig,
        force_rescan: bool,
        saved: Option<u64>,
        mut source: C,
        stat: P,
        enumerator: V,
    ) -> io::Result<Engine>
    where
        C: ChangeSource + Send + 'static,
        P: PathStat + Send + 'static,
        V: VolumeEnumerator + Send + 'static,
    {
        let outcome = source.start(config.resume_from().as_ref())?;
        // The writer compacts between batches itself, so entry IDs stay
        // stable while a batch is recorded.
        index.set_compact_fraction(f64::INFINITY);
        let snap = index.snapshot();
        let orders: Vec<Order> =
            orders.unwrap_or_else(|| Order::build_keys(&*snap, &ENGINE_SORT_KEYS));
        let orders = Arc::new(orders);
        let (include, exclude) = config.priority_folders();
        let priority = PriorityFolders::new(config.root(), include, exclude);
        let group = priority.group(&index, &snap, &orders);
        let full_rescan =
            force_rescan || outcome != StartOutcome::Resumed || index.live_count() == 0;
        let shared = Arc::new(Shared {
            current: ArcSwap::from_pointee(EngineSnapshot {
                index: snap,
                orders: Arc::clone(&orders),
                cursor: config.resume_from(),
                generation: 0,
                priority: group.clone(),
            }),
            subscribers: Mutex::new(Vec::new()),
            // Catching up with a startup rescan counts as recovering.
            health: HealthState::new(if full_rescan {
                EngineHealth::Recovering
            } else {
                EngineHealth::Live
            }),
            paused: AtomicBool::new(false),
            foreground: AtomicBool::new(true),
            stop: AtomicBool::new(false),
            checkpoints: Checkpoints::new(
                config.store_path().map(Into::into),
                config.checkpoint_idle(),
                config.checkpoint_max_delay(),
                saved,
            ),
        });
        let writer = Writer {
            index,
            orders,
            root: config.root().to_path_buf(),
            window: config.batch_window(),
            background_window: config.background_batch_window(),
            source,
            stat,
            enumerator,
            shared: Arc::clone(&shared),
            generation: 0,
            batch: Batch::new(),
            orders_stale: false,
            full_rescan_pending: full_rescan,
            in_full_rescan: false,
            recovery: Recovery::new(config.restart_threshold(), config.restart_backoff()),
            priority,
            group,
            group_stale: false,
            fresh_base: false,
        };
        let handle = thread::Builder::new()
            .name("fx-engine-writer".into())
            .spawn(move || {
                qos::set_current_thread_class(qos::ThreadClass::Utility);
                writer.run();
            })?;
        let mut engine = Engine {
            shared,
            writer: Some(handle),
            checkpointer: None,
            searcher: Mutex::new(Searcher::new()),
            drop_cache: AtomicBool::new(false),
            query_generation: fx_query::Generation::new(),
            utc_offset: config.utc_offset(),
        };
        if engine.shared.checkpoints.enabled() {
            let shared = Arc::clone(&engine.shared);
            // On error, dropping `engine` stops the writer again.
            let handle = thread::Builder::new()
                .name("fx-engine-checkpoint".into())
                .spawn(move || {
                    qos::set_current_thread_class(qos::ThreadClass::Utility);
                    shared.checkpoints.run_idle(&shared.current);
                })?;
            engine.checkpointer = Some(handle);
        }
        Ok(engine)
    }

    /// Searches the latest snapshot for `query` (fx-query syntax, or with
    /// `options.regex` one regular expression) under the search field's
    /// `options`, and returns the matches at positions
    /// `offset .. offset + limit` in `sort` order read in `direction`, with
    /// the total match count and the generation of the snapshot searched.
    /// [`QueryOptions::default`] is the plain search.
    ///
    /// Repeated and narrowing queries on the same generation (and with the
    /// same options) are answered from fx-query's refinement cache; a new
    /// generation clears it. Date functions use the current time and the
    /// local UTC offset (or the config's
    /// [`with_utc_offset`](EngineConfig::with_utc_offset)). Safe to call
    /// from many threads at once.
    ///
    /// Errors: [`SearchError::InvalidRegex`] when a regular expression does
    /// not compile (the whole query with `options.regex`, its pattern then
    /// being `query` as given, or a `regex:` term), checked before
    /// searching; [`SearchError::Cancelled`] when
    /// [`cancel_searches`](Self::cancel_searches) was called while this
    /// search ran.
    pub fn search(
        &self,
        query: &str,
        options: QueryOptions,
        sort: SortKey,
        direction: Direction,
        offset: usize,
        limit: usize,
    ) -> Result<Page, SearchError> {
        self.search_grouped(query, options, sort, direction, false, offset, limit)
    }

    /// Like [`search`](Self::search), with "Your files first": when
    /// `priority_first` is set, the matches in the snapshot's priority group
    /// (see [`EngineConfig::with_priority_folders`]) come first, in `sort`
    /// order read in `direction`, then the other matches in the same order
    /// ([`fx_sort::emit_tiered`]). The total is unchanged. Without
    /// `priority_first`, or without priority folders, this is
    /// [`search`](Self::search).
    ///
    /// Errors: as for [`search`](Self::search).
    #[allow(clippy::too_many_arguments)]
    pub fn search_grouped(
        &self,
        query: &str,
        options: QueryOptions,
        sort: SortKey,
        direction: Direction,
        priority_first: bool,
        offset: usize,
        limit: usize,
    ) -> Result<Page, SearchError> {
        let snap = self.snapshot();
        let cancel = self.query_generation.token();
        let matches = self.matches(&snap, query, options, &cancel)?;
        let order = snap.order_or_build(sort);
        if cancel.is_cancelled() {
            return Err(SearchError::Cancelled);
        }
        let page = match snap.priority_group().filter(|_| priority_first) {
            Some(first) => fx_sort::emit_tiered(&order, &matches, first, direction, offset, limit),
            None => fx_sort::emit(&order, &matches, direction, offset, limit),
        };
        Ok(Page {
            generation: Generation(snap.generation()),
            total: page.total,
            ids: page.ids,
        })
    }

    /// Finds where the entries at `paths` sit in the results of `query`
    /// under `options` in `sort` order read in `direction`, in the latest
    /// snapshot: for each path, the offset at which [`search`](Self::search)
    /// returns it, or `None` when no live entry has that path or it does not
    /// match.
    ///
    /// Paths are index paths, as [`fx_index::Snapshot::path`] gives them
    /// (names from the root entry down, joined with `/`); see
    /// [`EngineSnapshot::find_path`]. The query runs as for `search` (from
    /// the refinement cache when it just ran), each path is found with a
    /// binary search of the Path order, and the results are then walked
    /// once, however many paths are asked about, stopping at the last one
    /// found. A window uses this to keep its selection on the same files
    /// when new results arrive.
    ///
    /// Errors: as for [`search`](Self::search), except that with no
    /// `paths` nothing is searched and nothing can fail.
    pub fn positions(
        &self,
        query: &str,
        options: QueryOptions,
        sort: SortKey,
        direction: Direction,
        paths: &[&str],
    ) -> Result<Positions, SearchError> {
        self.positions_grouped(query, options, sort, direction, false, paths)
    }

    /// Like [`positions`](Self::positions), in the results of
    /// [`search_grouped`](Self::search_grouped) with the same
    /// `priority_first`: with it set (and priority folders configured), an
    /// entry of the priority group is at its offset among the group's
    /// matches, and any other match after every match of the group. Then
    /// the walk goes on past the last entry found when one of them is not in
    /// the group, since the group's size comes first; that is the cost of
    /// the grouped search's first pass.
    ///
    /// Errors: as for [`positions`](Self::positions).
    pub fn positions_grouped(
        &self,
        query: &str,
        options: QueryOptions,
        sort: SortKey,
        direction: Direction,
        priority_first: bool,
        paths: &[&str],
    ) -> Result<Positions, SearchError> {
        let snap = self.snapshot();
        let generation = Generation(snap.generation());
        if paths.is_empty() {
            return Ok(Positions {
                generation,
                rows: Vec::new(),
            });
        }
        let cancel = self.query_generation.token();
        let matches = self.matches(&snap, query, options, &cancel)?;
        let targets: Vec<Option<u32>> = paths
            .iter()
            .map(|path| {
                snap.find_path(path)
                    .filter(|&id| matches.contains(EntryId(id)))
            })
            .collect();
        let order = snap.order_or_build(sort);
        if cancel.is_cancelled() {
            return Err(SearchError::Cancelled);
        }
        let first = snap.priority_group().filter(|_| priority_first);
        Ok(Positions {
            generation,
            rows: query::positions_in(&order, &matches, first, direction, &targets),
        })
    }

    /// The entries of `snap` matching `query` under `options`, through the
    /// refinement cache when no other search holds it. A regular expression
    /// that does not compile is reported before anything is searched.
    fn matches(
        &self,
        snap: &EngineSnapshot,
        query: &str,
        options: QueryOptions,
        cancel: &CancelToken<'_>,
    ) -> Result<MatchSet, SearchError> {
        let text = options.query_text(query);
        let opts = options.search_options();
        if let Some(e) = reader::first_regex_error(&fx_query::parse(&text, &opts).query) {
            let mut e = SearchError::from(e);
            if let SearchError::InvalidRegex { pattern, .. } = &mut e
                && options.regex
            {
                // The whole query is the pattern; report it as typed.
                query.clone_into(pattern);
            }
            return Err(e);
        }
        let ctx = query::eval_context(self.utc_offset);
        let hits = match self.searcher.try_lock() {
            Ok(mut searcher) => {
                searcher.set_context(ctx);
                searcher.set_epoch(snap.generation());
                let hits = searcher.search(&text, &opts, snap, cancel);
                if self.drop_cache.swap(false, Ordering::AcqRel) && !self.is_foreground() {
                    searcher.invalidate();
                }
                hits?
            }
            // Another thread holds the cache: search without it rather than
            // wait.
            Err(_) => {
                let mut searcher = Searcher::with_capacity(0);
                searcher.set_context(ctx);
                searcher.search(&text, &opts, snap, cancel)?
            }
        };
        Ok(snap.match_set(&hits))
    }

    /// Cancels every search running now: they return
    /// [`SearchError::Cancelled`].
    /// Searches started afterwards run normally.
    pub fn cancel_searches(&self) {
        self.query_generation.bump();
    }

    /// Saves the latest published snapshot (index, sort orders and resume
    /// cursor) to the store atomically, with the clean-shutdown flag
    /// cleared, and returns once the file holds it: at once when it already
    /// does (the snapshot was saved, or loaded unchanged), else after any
    /// save in flight.
    ///
    /// Errors: no store path is configured, or fx-store failed (the
    /// previous file is then untouched).
    pub fn checkpoint(&self) -> Result<(), CheckpointError> {
        self.shared.checkpoints.save(&self.shared.current, false)
    }

    /// Asks the checkpoint thread to save the latest published snapshot, as
    /// [`checkpoint`](Self::checkpoint) would, and returns at once: for a
    /// caller that must not wait for the write, such as the app's daily
    /// save on the main thread. Nothing happens without a store path, when
    /// the store already holds the latest snapshot, or after
    /// [`checkpoint_for_quit`](Self::checkpoint_for_quit).
    pub fn request_checkpoint(&self) {
        self.shared.checkpoints.request();
    }

    /// When the store was last written; `None` without a store path or
    /// before the first save.
    pub fn saved_at(&self) -> Option<std::time::SystemTime> {
        self.shared.checkpoints.saved_at()
    }

    /// The save for an app that is quitting, and so exits without dropping
    /// the engine. The app saves the store only once a day, so a quit
    /// writes what the store lacks rather than leave the next launch up to
    /// a day of changes to replay:
    ///
    /// - [`QuitSave::Skip`]: nothing is written when the store holds the
    ///   latest snapshot;
    /// - [`QuitSave::WaitForRunning`]: otherwise, when a save is being
    ///   written, returns once it is done and writes nothing more (the next
    ///   launch replays what came after it);
    /// - [`QuitSave::Write`]: otherwise the latest snapshot is written, as
    ///   by [`checkpoint`](Self::checkpoint).
    ///
    /// No idle checkpoint starts afterwards; [`checkpoint`](Self::checkpoint)
    /// and [`close`](Self::close) still save.
    ///
    /// Errors: no store path is configured, or fx-store failed (the
    /// previous file is then untouched).
    pub fn checkpoint_for_quit(&self) -> Result<QuitSave, CheckpointError> {
        self.shared.checkpoints.save_for_quit(&self.shared.current)
    }

    /// Like [`shutdown`](Self::shutdown), but reports whether the final
    /// checkpoint was saved (`Ok` without a store path).
    pub fn close(mut self) -> Result<(), CheckpointError> {
        if let Some(handle) = self.stop() {
            if let Err(panic) = handle.join() {
                self.stop_checkpointer();
                std::panic::resume_unwind(panic);
            }
        }
        self.stop_checkpointer();
        if self.shared.checkpoints.enabled() {
            self.shared.checkpoints.save(&self.shared.current, true)
        } else {
            Ok(())
        }
    }

    fn stop_checkpointer(&mut self) {
        self.shared.checkpoints.stop();
        if let Some(handle) = self.checkpointer.take() {
            let _ = handle.join();
        }
    }

    /// The latest published snapshot. Lock-free: never waits for the
    /// writer, which keeps working on the next one.
    pub fn snapshot(&self) -> Arc<EngineSnapshot> {
        self.shared.current.load_full()
    }

    /// A channel that receives the generation of every snapshot published
    /// from now on (`u64::from` turns it into the number returned by
    /// [`EngineSnapshot::generation`]).
    pub fn subscribe(&self) -> Receiver<Generation> {
        let (tx, rx) = mpsc::channel();
        self.shared
            .subscribers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(tx);
        rx
    }

    /// The engine's current health: [`EngineHealth::Live`] while the change
    /// source delivers normally.
    pub fn health(&self) -> EngineHealth {
        self.shared.health.get()
    }

    /// A channel that receives every new health state from now on, sent
    /// when the state changes.
    pub fn subscribe_health(&self) -> Receiver<EngineHealth> {
        self.shared.health.subscribe()
    }

    /// Pauses monitoring: the writer stops reading the change source (which
    /// keeps buffering) after the batch in flight. Snapshots keep serving.
    pub fn pause(&self) {
        self.shared.paused.store(true, Ordering::Release);
    }

    /// Resumes monitoring after [`pause`](Engine::pause); buffered changes
    /// are applied in the next batch.
    pub fn resume(&self) {
        self.shared.paused.store(false, Ordering::Release);
        self.wake();
    }

    /// Whether monitoring is paused.
    pub fn is_paused(&self) -> bool {
        self.shared.paused.load(Ordering::Acquire)
    }

    /// Tells the engine whether a window shows the results (`true`, the
    /// default) or none does. Without one, nobody watches the results, so
    /// the writer collects changes for the background batch window
    /// ([`EngineConfig::with_background_batch_window`], 2 s by default)
    /// instead of the batch window, and publishes up to 20 times less
    /// often. Back in the foreground, a batch being collected ends within a
    /// quarter of a second, so a window that shows gets the pending changes
    /// at once.
    ///
    /// Going to the background also drops the result sets cached for
    /// refining a query (up to 8, each as large as its matches): nobody
    /// searches while no window shows the results (run 2.17).
    pub fn set_foreground(&self, on: bool) {
        self.shared.foreground.store(on, Ordering::Release);
        if !on {
            match self.searcher.try_lock() {
                Ok(mut searcher) => searcher.invalidate(),
                // A search holds the cache: it drops it when done.
                Err(_) => self.drop_cache.store(true, Ordering::Release),
            }
        }
    }

    /// How many result sets the search cache holds now, for tests and
    /// measurements. Waits for a search in progress.
    pub fn cached_searches(&self) -> usize {
        self.searcher.lock().map_or(0, |searcher| searcher.cached())
    }

    /// Whether a window shows the results ([`set_foreground`](Self::set_foreground)).
    pub fn is_foreground(&self) -> bool {
        self.shared.foreground.load(Ordering::Acquire)
    }

    /// Orderly shutdown: stops the writer thread and waits for it to exit,
    /// then, when a store path is configured, saves a final checkpoint of
    /// the latest snapshot with the clean-shutdown flag set. A panic on the
    /// writer thread is re-raised here. A failed final save is ignored; use
    /// [`close`](Self::close) to see it.
    pub fn shutdown(self) {
        let _ = self.close();
    }

    fn wake(&self) {
        if let Some(handle) = &self.writer {
            handle.thread().unpark();
        }
    }

    fn stop(&mut self) -> Option<JoinHandle<()>> {
        self.shared.stop.store(true, Ordering::Release);
        self.wake();
        self.writer.take()
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if let Some(handle) = self.stop() {
            let _ = handle.join();
        }
        self.stop_checkpointer();
    }
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("generation", &self.snapshot().generation())
            .field("paused", &self.is_paused())
            .field("health", &self.health())
            .finish_non_exhaustive()
    }
}
