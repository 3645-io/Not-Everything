//! `day_replay_bench`: the relaunch after a `kill -9` a day after the last
//! save (run 2.15), on macOS.
//!
//! ```text
//! cargo run --release -p fx-engine --example day_replay_bench -- --hours 24 --cache warm
//! ```
//!
//! The app saves its index once a day, so after a crash the next launch
//! replays up to a day of FSEvents history from the saved cursor, or
//! rescans when macOS no longer has it. Phase 1's catch-up budget is 10 s.
//! This tool measures that replay on a real volume:
//!
//! 1. Indexes the root (`/` by default, or `--root`) into a store in a
//!    temporary folder, as the app's first launch does, and closes the
//!    engine; or, with `--store`, copies a saved store there.
//! 2. Sets the store's resume cursor `--hours` back (default 24): the
//!    volume's last FSEvents event ID before that time, with the volume's
//!    FSEvents database UUID. FSEvents has a call for that ID
//!    (`FSEventsGetLastEventIdForDeviceBeforeTime`), used when it answers;
//!    on macOS 27.2 it returns 0 for every time and volume tried, so the
//!    ID is estimated from the history instead (`estimate_event_id`: each
//!    reported path's last event ID against its status-change time). A
//!    copied store keeps its own cursor unless `--hours` is given. A
//!    freshly built index is current, so the replay re-checks a day of
//!    reported paths against the disk, which is where the time goes, but
//!    finds less to change than a day-old index would; for the real case,
//!    `--prepare FOLDER` saves a store to replay with `--store` a day
//!    later, from its own day-old cursor and index.
//! 3. Relaunches in a process of its own, as the app does after a
//!    `kill -9`: `Engine::open` over the store with the app's engine
//!    configuration (the FSEvents source filtered to the root,
//!    `MacPathStat`, `MacScanner` with the default scan options, the home
//!    folder's priority folders; no external volumes), optionally hidden
//!    (`--hidden`: the background batch window of a resident app whose
//!    window is closed). It times the replay until the history, up to
//!    FSEvents' HistoryDone marker, is handed out and the engine has
//!    published it, and reads the process's peak memory footprint. It then
//!    compares the index with a fresh scan (unless `--no-compare`): the
//!    disk keeps changing meanwhile, so entries that differ on a path
//!    FSEvents reported during the comparison are counted apart, and only
//!    the others make the index unequal.
//! 4. Back in the first process, reads the same history once more,
//!    untimed, and counts its raw events and the distinct paths created,
//!    removed, renamed and modified; from the created and removed paths and
//!    the index's size it projects the compactions, and so the saves, a day
//!    of such changes brings (`fx_bench::saves`).
//!
//! To mark the end of the history even when no live change follows it,
//! the replaying source hands the writer a check of the root with the
//! HistoryDone marker when that batch is otherwise empty: the root is
//! re-read and nothing changes, but the cursor is published.
//!
//! Output: `key=value` lines, numbers only (no path is printed), and the
//! gate line `GATE day replay caught up and equal to a fresh scan <= 10 s`.
//! Exit codes: 0 after a run whose index equals the fresh scan (or with
//! `--no-compare`), 1 on an error or a mismatch, 2 for bad arguments. Only
//! its own temporary folder is written, and it is removed at the end; the
//! root is only read. Indexing `/` needs Full Disk Access for the terminal
//! that runs it.

#[cfg(target_os = "macos")]
mod bench_common;

use std::process::ExitCode;

#[cfg(not(target_os = "macos"))]
fn main() -> ExitCode {
    eprintln!("day_replay_bench: FSEvents history needs macOS");
    ExitCode::from(2)
}

#[cfg(target_os = "macos")]
fn main() -> ExitCode {
    mac::main()
}

#[cfg(target_os = "macos")]
mod mac {
    use std::collections::{HashMap, HashSet};
    use std::fs;
    use std::hash::{DefaultHasher, Hash, Hasher};
    use std::io;
    use std::path::{Path, PathBuf};
    use std::process::{Command, ExitCode};
    use std::sync::{Arc, Mutex, MutexGuard};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use fx_bench::saves::{Activity, project};
    use fx_engine::{Engine, EngineConfig, EngineHealth, EngineSnapshot};
    use fx_fs::{ChangeSource, VolumeEnumerator};
    use fx_fs_macos::enumerator::MacScanner;
    use fx_fs_macos::fsevents::flags::EventFlags;
    use fx_fs_macos::fsevents::source::FsEventsSource;
    use fx_fs_macos::fsevents::stream::{RawEvent, StreamConfig, VolumeStream};
    use fx_fs_macos::fsevents::volume::{
        current_event_id, device_of, last_event_id_before, volume_uuid,
    };
    use fx_fs_macos::path_stat::MacPathStat;
    use fx_fs_macos::scan::ScanOptions;
    use fx_model::{Change, Entry, EntryKind, FileKey, ResumeCursor, StartOutcome, VolumeId};
    use fx_store::{MappedIndex, SaveInput, write_checkpoint};

    use crate::bench_common::{Flags, TempRoot, UsageError, gate, header, metric, ms};

    const USAGE: &str = "\
usage:
  day_replay_bench [--root PATH] [--hours N] [--store PATH] [--hidden]
                   [--no-compare] [--cache warm|cold|unknown]
  day_replay_bench --prepare FOLDER [--root PATH]

  --root PATH      the folder to index and replay, as the app's root
                   (default /; indexing / needs Full Disk Access)
  --hours N        set the saved cursor N hours back, 1..=720 (default 24;
                   with --store, the store's own cursor unless given)
  --store PATH     replay a copy of this saved store instead of indexing
                   the root first; it must have been built over the root
  --hidden         replay as a resident app with its window closed (the
                   engine's background batch window)
  --no-compare     skip the comparison with a fresh scan
  --cache STATE    cache state label for the report (default unknown)
  --prepare FOLDER index the root into FOLDER/index.fxdb and stop, to
                   replay it with --store a day later

Gate: caught up and equal to a fresh scan <= 10 s.";

    /// Phase 1's catch-up budget.
    const GATE_CAUGHT_UP: Duration = Duration::from_secs(10);
    /// How long any one phase may take before the run fails.
    const TIMEOUT: Duration = Duration::from_secs(30 * 60);
    /// How long the comparison waits for the engine to apply the changes
    /// FSEvents reported during the fresh scan.
    const SETTLE_TIMEOUT: Duration = Duration::from_secs(10);
    const MB: f64 = 1_048_576.0;
    /// The app's priority folders below the home folder (`fx-ui-macos`'s
    /// `your_files.rs`): the home folder, iCloud Drive and the cloud
    /// storage folder start the group, `Library` ends it.
    const ICLOUD_DRIVE: &str = "Library/Mobile Documents/com~apple~CloudDocs";
    const CLOUD_STORAGE: &str = "Library/CloudStorage";
    /// The switch that runs the replay in the child process.
    const CHILD: &str = "--child-replay";

    pub fn main() -> ExitCode {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.iter().any(|a| a == "--help" || a == "-h") {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        if args.first().map(String::as_str) == Some(CHILD) {
            return child_main(&args[1..]);
        }
        let parsed = Flags::parse(
            &args,
            &["--root", "--hours", "--store", "--cache", "--prepare"],
            &["--hidden", "--no-compare"],
        )
        .and_then(|f| {
            let hours = f.number("--hours", 24, 1, 720)?;
            let hours = (f.get("--store").is_none() || f.get("--hours").is_some()).then_some(hours);
            let root = canonical_root(f.get("--root").unwrap_or("/"))?;
            let prepare = f.get("--prepare").map(PathBuf::from);
            if prepare.is_some() && f.get("--store").is_some() {
                return Err(UsageError(
                    "--prepare and --store exclude each other".into(),
                ));
            }
            Ok(Options {
                root,
                hours,
                prepare,
                store: f.get("--store").map(PathBuf::from),
                hidden: f.has("--hidden"),
                compare: !f.has("--no-compare"),
                cache: f.cache()?,
            })
        });
        let options = match parsed {
            Ok(o) => o,
            Err(UsageError(msg)) => {
                eprintln!("day_replay_bench: {msg}\n\n{USAGE}");
                return ExitCode::from(2);
            }
        };
        header("day_replay", options.cache);
        let result = match &options.prepare {
            Some(folder) => prepare(&options.root, folder).map(|()| true),
            None => run(&options),
        };
        match result {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => {
                eprintln!("day_replay_bench: the replayed index does not equal a fresh scan");
                ExitCode::from(1)
            }
            Err(e) => {
                eprintln!("day_replay_bench: error: {e}");
                ExitCode::from(1)
            }
        }
    }

    struct Options {
        root: PathBuf,
        /// Hours to set the cursor back; `None` keeps a copied store's own.
        hours: Option<u64>,
        /// Index into this folder and stop.
        prepare: Option<PathBuf>,
        store: Option<PathBuf>,
        hidden: bool,
        compare: bool,
        cache: fx_bench::CacheState,
    }

    /// The root as the app holds it: absolute and canonical, so its paths
    /// read like the ones FSEvents reports.
    fn canonical_root(given: &str) -> Result<PathBuf, UsageError> {
        let path = Path::new(given);
        if !path.is_absolute() {
            return Err(UsageError("--root expects an absolute path".into()));
        }
        fs::canonicalize(path)
            .ok()
            .filter(|p| p.is_dir())
            .ok_or_else(|| UsageError("--root expects an existing folder".into()))
    }

    fn other(e: impl std::fmt::Display) -> io::Error {
        io::Error::other(e.to_string())
    }

    fn unix_now() -> io::Result<f64> {
        Ok(SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(other)?
            .as_secs_f64())
    }

    fn event_id(cursor: Option<ResumeCursor>) -> Option<u64> {
        match cursor? {
            ResumeCursor::FsEvents { event_id, .. } => Some(event_id),
            _ => None,
        }
    }

    fn wait<T>(what: &str, mut done: impl FnMut() -> Option<T>) -> io::Result<T> {
        let end = Instant::now() + TIMEOUT;
        loop {
            if let Some(value) = done() {
                return Ok(value);
            }
            if Instant::now() > end {
                return Err(other(format!("timed out waiting for {what}")));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// The FSEvents source the app opens over `root`.
    fn fsevents(root: &Path) -> FsEventsSource {
        let mut stream = StreamConfig::new(root.to_path_buf());
        stream.path_filter = Some(root.to_path_buf());
        FsEventsSource::new(stream)
    }

    /// Opens an engine over `store` as the app does (`appkit/app.rs`'s
    /// `open_engine`, with full access and without external volumes).
    fn app_engine<C>(root: &Path, store: &Path, source: C) -> io::Result<Engine>
    where
        C: ChangeSource + Send + 'static,
    {
        let mut config = EngineConfig::new(root).with_store_path(store);
        if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
            let home = PathBuf::from(home);
            let home = fs::canonicalize(&home).unwrap_or(home);
            config = config.with_priority_folders(
                [
                    home.clone(),
                    home.join(ICLOUD_DRIVE),
                    home.join(CLOUD_STORAGE),
                ],
                [home.join("Library")],
            );
        }
        Engine::open(
            config,
            source,
            MacPathStat::new(),
            MacScanner::new(ScanOptions::default()),
        )
    }

    // ------------------------------------------------------------ parent

    fn run(o: &Options) -> io::Result<bool> {
        metric("root_whole_disk", o.root == Path::new("/"));
        metric("hidden", o.hidden);
        let dir = TempRoot::new("fx-day-replay-store")?;
        let store = dir.path().join("index.fxdb");
        // How long the replayed history spans.
        let span = match &o.store {
            Some(source) => {
                let age = fs::metadata(source)?
                    .modified()?
                    .elapsed()
                    .unwrap_or(Duration::ZERO);
                fs::copy(source, &store)?;
                metric("store_copied", true);
                metric(
                    "store_age_hours",
                    format!("{:.2}", age.as_secs_f64() / 3600.0),
                );
                o.hours.map_or(age, |h| Duration::from_secs(h * 3600))
            }
            None => {
                index_root(&o.root, &store)?;
                Duration::from_secs(o.hours.unwrap_or(24) * 3600)
            }
        };
        let (entries, store_bytes) = {
            let mapped = MappedIndex::open(&store).map_err(other)?;
            (mapped.live_count() as u64, fs::metadata(&store)?.len())
        };
        metric("store_entries", entries);
        metric("store_mb", format!("{:.1}", store_bytes as f64 / MB));
        let (cursor, span_known) = match o.hours {
            Some(hours) => set_cursor_back(&store, &o.root, hours)?,
            None => (saved_cursor(&store)?, true),
        };
        let hours = format!("{:.2}", span.as_secs_f64() / 3600.0);
        if span_known {
            metric("replay_span_hours", hours);
        } else {
            // The estimate found no change under the root before the time
            // asked for, and replays from further back.
            metric("replay_span_hours_at_least", hours);
        }
        let cursor_id = event_id(Some(cursor)).ok_or_else(|| other("not an FSEvents cursor"))?;
        metric(
            "cursor_event_ids_behind",
            current_event_id().saturating_sub(cursor_id),
        );

        // The relaunch, in a process of its own.
        let mut child = Command::new(std::env::current_exe()?);
        child.arg(CHILD).arg(&store).arg(&o.root);
        if o.hidden {
            child.arg("--hidden");
        }
        if !o.compare {
            child.arg("--no-compare");
        }
        let status = child.status()?;

        // The history again, untimed, to count what it holds.
        let history = count_history(&o.root, cursor)?;
        history.print();
        // Less than an hour of history is too little to scale to a day.
        let projection = (span_known && span >= Duration::from_secs(3600))
            .then(|| {
                project(
                    entries,
                    store_bytes,
                    &Activity {
                        creates: history.created.len() as u64,
                        deletes: history.removed.len() as u64,
                        seconds: span.as_secs_f64(),
                    },
                )
            })
            .flatten();
        if let Some(p) = projection {
            metric(
                "projected_compactions_per_day",
                format!("{:.2}", p.compactions_per_day),
            );
            metric(
                "projected_compaction_saves_mb_per_day",
                format!("{:.0}", p.saves_per_day_bytes / MB),
            );
        }
        match status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(other(format!("the replay process failed: {status}"))),
        }
    }

    /// `--prepare`: indexes `root` into `folder/index.fxdb`, kept for a
    /// replay with `--store` a day later: the replay then starts from the
    /// store's own day-old cursor and day-old index, as after a real
    /// `kill -9` a day after the last save.
    fn prepare(root: &Path, folder: &Path) -> io::Result<()> {
        if !folder.is_dir() {
            return Err(other("--prepare expects an existing folder"));
        }
        let store = folder.join("index.fxdb");
        if store.exists() {
            return Err(other("--prepare: the folder already holds a store"));
        }
        metric("root_whole_disk", root == Path::new("/"));
        index_root(root, &store)?;
        metric("prepared", true);
        Ok(())
    }

    /// Indexes `root` into a missing store, as the app's first launch
    /// does, and closes the engine, which saves the index with its cursor.
    fn index_root(root: &Path, store: &Path) -> io::Result<()> {
        let t = Instant::now();
        let engine = app_engine(root, store, fsevents(root))?;
        wait("the first scan", || {
            (engine.health() == EngineHealth::Live && engine.snapshot().index().live_count() > 1)
                .then_some(())
        })?;
        metric("index_scan_ms", ms(t.elapsed()));
        metric("index_entries", engine.snapshot().index().live_count());
        wait("the first scan's save", || engine.saved_at())?;
        engine
            .close()
            .map_err(|e| other(format!("the save on close: {e}")))
    }

    /// The only cursor saved in the store.
    fn saved_cursor(store: &Path) -> io::Result<ResumeCursor> {
        let mapped = MappedIndex::open(store).map_err(other)?;
        match mapped.cursors() {
            [(_, cursor)] => Ok(*cursor),
            _ => Err(other("the store holds no cursor, or more than one")),
        }
    }

    /// Rewrites the store with its cursor set to the root's volume's last
    /// event before `hours` ago, and returns that cursor, with whether it
    /// is that event (`false`: an earlier one, see [`estimate_event_id`]).
    fn set_cursor_back(store: &Path, root: &Path, hours: u64) -> io::Result<(ResumeCursor, bool)> {
        let dev = device_of(root)?;
        let db_uuid =
            volume_uuid(dev).ok_or_else(|| other("the root's volume keeps no FSEvents history"))?;
        let target = unix_now()? - hours as f64 * 3600.0;
        // FSEvents' own answer, when it gives one: on macOS 27.2 it returns
        // 0 for every time and volume tried, so the history is read instead.
        let (event_id, exact) = match last_event_id_before(dev, target) {
            id if id != 0 && id < current_event_id() => {
                metric("cursor_from", "fsevents_time");
                (id, true)
            }
            _ => {
                metric("cursor_from", "history_estimate");
                estimate_event_id(root, db_uuid, target)?
            }
        };
        let cursor = ResumeCursor::FsEvents { event_id, db_uuid };
        let mapped = MappedIndex::open(store).map_err(other)?;
        mapped.verify_checksums().map_err(other)?;
        let parts = mapped.to_parts();
        let volumes: Vec<(VolumeId, ResumeCursor)> =
            mapped.cursors().iter().map(|&(v, _)| (v, cursor)).collect();
        if volumes.is_empty() {
            return Err(other("the store holds no cursor"));
        }
        let orders = mapped.orders();
        write_checkpoint(
            store,
            &SaveInput {
                parts: &parts,
                volumes: &volumes,
                orders: &orders,
                clean_shutdown: false,
            },
        )
        .map_err(other)?;
        Ok((cursor, exact))
    }

    /// How many times the estimate reads the history, each from 4 times
    /// further back than the last.
    const ESTIMATE_ROUNDS: u32 = 3;

    /// Estimates the root's volume's last event ID before `target` (Unix
    /// seconds) from its history, read from further back each round until
    /// it reaches before `target`. Every path the history reports comes with
    /// the ID of its last event, and the path's status-change time (ctime),
    /// which every reported change sets, is that event's time unless an
    /// unreported change came later. The estimate is the ID that best splits
    /// the paths last changed before `target` from those changed after it.
    /// When no path under the root was changed before `target` even in the
    /// last round, the start of that round's history stands in, and the
    /// second value is `false`: the replay then spans more than asked.
    fn estimate_event_id(root: &Path, db_uuid: [u8; 16], target: f64) -> io::Result<(u64, bool)> {
        use std::os::unix::fs::MetadataExt;

        let current = current_event_id();
        // A first guess at how far back `target` lies: the IDs FSEvents
        // hands out in a second, times its age.
        let rate = {
            let start = current_event_id();
            std::thread::sleep(Duration::from_secs(1));
            current_event_id().saturating_sub(start).max(10)
        };
        let age = (unix_now()? - target).max(1.0);
        let mut back = ((rate as f64 * age) as u64).max(100_000);
        let mut round = 1;
        loop {
            let from = current.saturating_sub(back).max(1);
            let last = last_event_ids(
                root,
                ResumeCursor::FsEvents {
                    event_id: from,
                    db_uuid,
                },
            )?;
            // Each path still there: its last event ID, and whether it was
            // changed before `target`.
            let mut points: Vec<(u64, bool)> = last
                .iter()
                .filter_map(|(path, &id)| {
                    let meta = fs::symlink_metadata(path).ok()?;
                    let ctime = meta.ctime() as f64 + meta.ctime_nsec() as f64 / 1e9;
                    Some((id, ctime <= target))
                })
                .collect();
            let before = points.iter().filter(|p| p.1).count();
            if before == 0 && round < ESTIMATE_ROUNDS && from > 1 {
                back = back.saturating_mul(4);
                round += 1;
                continue;
            }
            metric("cursor_estimate_rounds", round);
            metric("cursor_estimate_paths", points.len());
            metric("cursor_estimate_paths_before", before);
            if before == 0 {
                return Ok((from, false));
            }
            // The split with the fewest paths on the wrong side: changed
            // after `target` below it, or before `target` above it.
            points.sort_unstable();
            let (mut best, mut split) = (before, 0);
            let (mut after_below, mut before_below) = (0, 0);
            for (i, &(_, was_before)) in points.iter().enumerate() {
                if was_before {
                    before_below += 1;
                } else {
                    after_below += 1;
                }
                let misplaced = after_below + (before - before_below);
                if misplaced < best {
                    (best, split) = (misplaced, i + 1);
                }
            }
            metric("cursor_estimate_paths_misplaced", best);
            return Ok((
                if split == 0 {
                    from
                } else {
                    points[split - 1].0
                },
                true,
            ));
        }
    }

    /// The last event ID of each path in the root's history from `from` up
    /// to the HistoryDone marker.
    fn last_event_ids(root: &Path, from: ResumeCursor) -> io::Result<HashMap<PathBuf, u64>> {
        let mut config = StreamConfig::new(root.to_path_buf());
        config.path_filter = Some(root.to_path_buf());
        config.no_defer = true;
        let t = Instant::now();
        let (mut stream, outcome) = VolumeStream::start(config, Some(&from))?;
        if outcome != StartOutcome::Resumed {
            return Err(other("FSEvents would not replay the root's history"));
        }
        let mut last = HashMap::new();
        loop {
            match stream.recv_timeout(Duration::from_millis(100)) {
                Ok(ev) if ev.flags.contains(EventFlags::HISTORY_DONE) => break,
                Ok(ev) => {
                    let id = last.entry(ev.path).or_insert(0);
                    *id = ev.id.max(*id);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(other("the FSEvents stream stopped"));
                }
            }
            if t.elapsed() > TIMEOUT {
                return Err(other("timed out reading the history"));
            }
        }
        stream.stop();
        Ok(last)
    }

    /// What the replayed history holds.
    #[derive(Default)]
    struct History {
        outcome: Option<StartOutcome>,
        read: Duration,
        events: u64,
        /// Hashes of the distinct paths with each kind of event.
        created: HashSet<u64>,
        removed: HashSet<u64>,
        renamed: HashSet<u64>,
        modified: HashSet<u64>,
        must_scan: u64,
        dropped: u64,
    }

    impl History {
        fn tally(&mut self, ev: &RawEvent) {
            self.events += 1;
            let mut hasher = DefaultHasher::new();
            ev.path.hash(&mut hasher);
            let h = hasher.finish();
            let has = |f: EventFlags| ev.flags.contains(f);
            if has(EventFlags::ITEM_CREATED) {
                self.created.insert(h);
            }
            if has(EventFlags::ITEM_REMOVED) {
                self.removed.insert(h);
            }
            if has(EventFlags::ITEM_RENAMED) {
                self.renamed.insert(h);
            }
            if [
                EventFlags::ITEM_MODIFIED,
                EventFlags::ITEM_INODE_META_MOD,
                EventFlags::ITEM_XATTR_MOD,
                EventFlags::ITEM_CHANGE_OWNER,
                EventFlags::ITEM_FINDER_INFO_MOD,
            ]
            .into_iter()
            .any(has)
            {
                self.modified.insert(h);
            }
            if has(EventFlags::MUST_SCAN_SUB_DIRS) || ev.rescan_subtree {
                self.must_scan += 1;
            }
            if has(EventFlags::USER_DROPPED) || has(EventFlags::KERNEL_DROPPED) {
                self.dropped += 1;
            }
        }

        fn print(&self) {
            metric("history_start", self.outcome.map_or("none", outcome_name));
            metric("history_read_ms", ms(self.read));
            metric("history_raw_events", self.events);
            metric("history_created_paths", self.created.len());
            metric("history_removed_paths", self.removed.len());
            metric("history_renamed_paths", self.renamed.len());
            metric("history_modified_paths", self.modified.len());
            metric("history_must_scan_events", self.must_scan);
            metric("history_dropped_events", self.dropped);
        }
    }

    fn outcome_name(o: StartOutcome) -> &'static str {
        match o {
            StartOutcome::Resumed => "replay",
            StartOutcome::GapDetected => "rescan_gap",
            StartOutcome::NoCursorSupport => "rescan_no_cursor",
        }
    }

    /// Reads the root's history from `cursor` up to the HistoryDone marker.
    fn count_history(root: &Path, cursor: ResumeCursor) -> io::Result<History> {
        let mut config = StreamConfig::new(root.to_path_buf());
        config.path_filter = Some(root.to_path_buf());
        config.no_defer = true;
        let t = Instant::now();
        let (mut stream, outcome) = VolumeStream::start(config, Some(&cursor))?;
        let mut history = History {
            outcome: Some(outcome),
            ..History::default()
        };
        if outcome == StartOutcome::Resumed {
            loop {
                match stream.recv_timeout(Duration::from_millis(100)) {
                    Ok(ev) if ev.flags.contains(EventFlags::HISTORY_DONE) => break,
                    Ok(ev) => history.tally(&ev),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        return Err(other("the FSEvents stream stopped"));
                    }
                }
                if t.elapsed() > TIMEOUT {
                    return Err(other("timed out reading the history"));
                }
            }
        }
        history.read = t.elapsed();
        stream.stop();
        Ok(history)
    }

    // ------------------------------------------------------------- child

    /// What the replaying source saw.
    #[derive(Default)]
    struct Replay {
        outcome: Option<StartOutcome>,
        batches: u64,
        changes: u64,
        rescan_subtrees: u64,
        rescan_volume: u64,
        /// When the HistoryDone marker was handed out, and the cursor then.
        history_done: Option<(Instant, u64)>,
    }

    /// The app's FSEvents source, recording the replay.
    struct Recording {
        inner: FsEventsSource,
        root: PathBuf,
        replay: Arc<Mutex<Replay>>,
    }

    fn lock(m: &Mutex<Replay>) -> MutexGuard<'_, Replay> {
        m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    impl ChangeSource for Recording {
        fn start(&mut self, from: Option<&ResumeCursor>) -> fx_fs::Result<StartOutcome> {
            let outcome = self.inner.start(from)?;
            lock(&self.replay).outcome.get_or_insert(outcome);
            Ok(outcome)
        }

        fn next_batch(&mut self, deadline: Instant) -> fx_fs::Result<Vec<Change>> {
            let mut batch = self.inner.next_batch(deadline)?;
            let mut r = lock(&self.replay);
            if r.history_done.is_some() {
                return Ok(batch);
            }
            r.batches += u64::from(!batch.is_empty());
            r.changes += batch.len() as u64;
            for change in &batch {
                match change {
                    Change::RescanSubtree(_) => r.rescan_subtrees += 1,
                    Change::RescanVolume => r.rescan_volume += 1,
                    _ => {}
                }
            }
            if self.inner.history_done() {
                let cursor = event_id(Some(self.inner.cursor())).unwrap_or(0);
                r.history_done = Some((Instant::now(), cursor));
                if batch.is_empty() {
                    // Changes nothing, but has the writer publish the cursor
                    // past the history (see the module docs).
                    batch.push(Change::CheckPath(self.root.clone()));
                }
            }
            Ok(batch)
        }

        fn cursor(&self) -> ResumeCursor {
            self.inner.cursor()
        }
    }

    /// The process's memory footprint and its peak so far, in bytes.
    fn footprint() -> Option<(u64, u64)> {
        // SAFETY: `rusage_info_v4` is plain integers and bytes, for which
        // all zeroes is a valid value.
        let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
        let pid = i32::try_from(std::process::id()).ok()?;
        // SAFETY: `info` is a live, writable `rusage_info_v4`, which is what
        // the kernel fills for the `RUSAGE_INFO_V4` flavor.
        let rc =
            unsafe { libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V4, (&raw mut info).cast()) };
        (rc == 0).then_some((info.ri_phys_footprint, info.ri_lifetime_max_phys_footprint))
    }

    fn child_main(args: &[String]) -> ExitCode {
        let (store, root) = match args {
            [store, root, ..] => (PathBuf::from(store), PathBuf::from(root)),
            _ => {
                eprintln!("day_replay_bench: {CHILD} needs a store and a root");
                return ExitCode::from(2);
            }
        };
        let rest = &args[2..];
        let hidden = rest.iter().any(|a| a == "--hidden");
        let compare = !rest.iter().any(|a| a == "--no-compare");
        match child(&store, &root, hidden, compare) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::from(1),
            Err(e) => {
                eprintln!("day_replay_bench: replay error: {e}");
                ExitCode::from(3)
            }
        }
    }

    fn child(store: &Path, root: &Path, hidden: bool, compare: bool) -> io::Result<bool> {
        let replay = Arc::new(Mutex::new(Replay::default()));
        let source = Recording {
            inner: fsevents(root),
            root: root.to_path_buf(),
            replay: Arc::clone(&replay),
        };
        let saved_before = fs::metadata(store)?.modified()?;
        let t = Instant::now();
        let engine = app_engine(root, store, source)?;
        let opened = t.elapsed();
        if hidden {
            engine.set_foreground(false);
        }
        let (handed_out, history_id) =
            wait("the end of the history", || lock(&replay).history_done)?;
        wait("the history to be published", || {
            let snap = engine.snapshot();
            (event_id(snap.cursor()).is_some_and(|id| id >= history_id)
                && engine.health() == EngineHealth::Live)
                .then_some(())
        })?;
        let caught_up = t.elapsed();
        let memory = footprint();
        let live = engine.snapshot().index().live_count();
        {
            let r = lock(&replay);
            metric("start", r.outcome.map_or("none", outcome_name));
            metric(
                "full_rescan",
                r.outcome != Some(StartOutcome::Resumed) || r.rescan_volume > 0,
            );
            metric("replayed_batches", r.batches);
            metric("replayed_changes", r.changes);
            metric("replayed_rescan_subtrees", r.rescan_subtrees);
            metric("replayed_rescan_volume", r.rescan_volume);
        }
        metric("open_ms", ms(opened));
        metric("history_handed_out_ms", ms(handed_out.duration_since(t)));
        metric("caught_up_ms", ms(caught_up));
        metric("live_entries", live);
        metric(
            "store_saved_during_replay",
            fs::metadata(store)?.modified()? != saved_before,
        );
        if let Some((now, peak)) = memory {
            metric("footprint_mb", format!("{:.1}", now as f64 / MB));
            metric("footprint_peak_mb", format!("{:.1}", peak as f64 / MB));
        }
        let in_budget = caught_up <= GATE_CAUGHT_UP;
        let equal = if compare {
            compare_with_fresh_scan(&engine, root)?
        } else {
            true
        };
        gate(
            "day replay caught up and equal to a fresh scan <= 10 s",
            in_budget && equal,
            &format!(
                "{} ms{}",
                ms(caught_up),
                match (compare, equal) {
                    (false, _) => ", not compared",
                    (true, true) => ", equal",
                    (true, false) => ", not equal",
                }
            ),
        );
        drop(engine);
        Ok(equal)
    }

    /// One entry as the comparison sees it: its place, kind and, for a
    /// file, its size.
    #[derive(PartialEq, Eq)]
    struct Item {
        parent: FileKey,
        name: String,
        kind: EntryKind,
        size: u64,
    }

    /// Every entry below the root, by key.
    type Listing = HashMap<FileKey, Item>;

    fn item(e: Entry) -> Option<(FileKey, Item)> {
        let size = if e.kind == EntryKind::File { e.size } else { 0 };
        Some((
            e.key,
            Item {
                parent: e.parent?,
                name: e.name,
                kind: e.kind,
                size,
            },
        ))
    }

    fn engine_listing(snap: &EngineSnapshot) -> Listing {
        let idx = snap.index();
        idx.live_ids()
            .filter_map(|id| {
                let p = idx.parent(id);
                let parent = (!p.is_none() && p != id).then(|| idx.key(p));
                item(Entry {
                    key: idx.key(id),
                    parent,
                    name: idx.name(id).to_string(),
                    kind: idx.kind(id),
                    flags: idx.flags(id),
                    size: idx.size(id),
                    modified: idx.modified(id),
                    created: idx.created(id),
                })
            })
            .collect()
    }

    fn scan_listing(root: &Path) -> io::Result<Listing> {
        let mut out = Listing::new();
        MacScanner::new(ScanOptions::default())
            .enumerate(root, &mut |e: Entry| {
                if let Some((key, item)) = item(e) {
                    out.insert(key, item);
                }
            })
            .map_err(|e| other(format!("fresh scan: {e}")))?;
        Ok(out)
    }

    /// The path of `key` in `listing`, below `root`.
    fn path_of(listing: &Listing, root: &Path, key: FileKey) -> PathBuf {
        let mut names = Vec::new();
        let mut at = listing.get(&key);
        while let Some(item) = at {
            names.push(item.name.as_str());
            if names.len() > 4096 {
                break;
            }
            at = listing.get(&item.parent);
        }
        let mut path = root.to_path_buf();
        path.extend(names.iter().rev());
        path
    }

    /// Paths FSEvents reported while the comparison ran.
    struct Touched {
        stream: VolumeStream,
        paths: HashSet<PathBuf>,
        last_id: u64,
    }

    impl Touched {
        fn start(root: &Path) -> io::Result<Touched> {
            let mut config = StreamConfig::new(root.to_path_buf());
            config.path_filter = Some(root.to_path_buf());
            config.no_defer = true;
            let (stream, _) = VolumeStream::start(config, None)?;
            Ok(Touched {
                stream,
                paths: HashSet::new(),
                last_id: 0,
            })
        }

        fn drain(&mut self) {
            self.stream.flush_sync();
            for ev in self.stream.try_iter() {
                self.last_id = self.last_id.max(ev.id);
                self.paths.insert(ev.path);
            }
        }

        /// Whether FSEvents reported `path` or a folder above it.
        fn covers(&self, path: &Path) -> bool {
            path.ancestors().any(|p| self.paths.contains(p))
        }
    }

    /// Compares the engine's index with a fresh scan of `root` and prints
    /// the differences; returns whether every one is on a path FSEvents
    /// reported while the comparison ran.
    fn compare_with_fresh_scan(engine: &Engine, root: &Path) -> io::Result<bool> {
        let t = Instant::now();
        let mut touched = Touched::start(root)?;
        let fresh = scan_listing(root)?;
        // Let the engine apply what changed during the scan.
        touched.drain();
        let end = Instant::now() + SETTLE_TIMEOUT;
        while event_id(engine.snapshot().cursor()).is_some_and(|id| id < touched.last_id)
            && Instant::now() < end
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        let indexed = engine_listing(&engine.snapshot());
        touched.drain();
        touched.stream.stop();

        let (mut missing, mut extra, mut changed, mut unexplained) = (0u64, 0u64, 0u64, 0u64);
        for (key, item) in &fresh {
            let differs = match indexed.get(key) {
                None => {
                    missing += 1;
                    true
                }
                Some(i) if i != item => {
                    changed += 1;
                    true
                }
                Some(_) => false,
            };
            if differs
                && !touched.covers(&path_of(&fresh, root, *key))
                && !(indexed.contains_key(key) && touched.covers(&path_of(&indexed, root, *key)))
            {
                unexplained += 1;
            }
        }
        for key in indexed.keys().filter(|k| !fresh.contains_key(k)) {
            extra += 1;
            if !touched.covers(&path_of(&indexed, root, *key)) {
                unexplained += 1;
            }
        }
        metric("compare_fresh_entries", fresh.len());
        metric("compare_index_entries", indexed.len());
        metric("compare_missing", missing);
        metric("compare_extra", extra);
        metric("compare_changed", changed);
        metric("compare_touched_paths", touched.paths.len());
        metric("compare_unexplained", unexplained);
        metric("compare_ms", ms(t.elapsed()));
        let equal = unexplained == 0;
        metric("equal_to_fresh_scan", equal);
        Ok(equal)
    }
}
