//! `kill -9` during engine checkpoints, with the fakes.
//!
//! A child process (this test binary again, running [`crash_child`]) opens
//! an engine over a store in a scratch folder and loops: it creates one
//! file, pushes the change, waits for it to be published and saves a
//! checkpoint, while the engine's idle checkpoints save the same snapshots
//! from their own thread. The parent kills it with SIGKILL after a seeded
//! random delay, most often one within a checkpoint, and checks what the
//! kill left: a store that loads, passes
//! its checksums and converts to an index; a cursor between the last event
//! the child reported saved and the last it pushed; an index that is
//! exactly the files that cursor covers; and, beside the store, only temp
//! files of the dead child (a kill that landed while a checkpoint was
//! written). The next child starts from that store: it must resume from
//! its cursor without a rescan, and the dead child's temp files must be
//! gone by the time it is ready.
//!
//! Every child builds the same fake tree in the same order, so each file
//! has the same `FileKey` in every process: the root `/vol`, `/vol/bulk`
//! with [`BULK_FILES`] files, then `/vol/f1`, `/vol/f2`, ... Event `k`
//! creates `/vol/f{k}`, so a store's cursor names exactly the files its
//! index must hold.
//!
//! The child reports, one line each: `fresh` (no store: it scanned and
//! saved) or `resumed <E>`, then `ready`, then for each event `k` in turn
//! `push <k>` before it pushes the change, `save <k>` once it is published,
//! right before the checkpoint, and `saved <k>` once a checkpoint holds it.
//!
//! Three kills in four come at a random moment of one checkpoint (from
//! `save` to `saved`, plus a quarter), the fourth anywhere in the cycle. A
//! series runs until enough kills have landed during a checkpoint: a temp
//! file was left, or the child was between `save` and `saved`. Kills
//! outside one are checked as well, but not counted: 10 during checkpoints
//! in `cargo test`, 200 with `cargo test -p fx-engine --test crash --
//! --ignored`.
//!
//! `FX_CRASH_SEED` sets the seed of the kill delays (default
//! [`DEFAULT_SEED`]); every run prints the seed it used.
//!
//! Unix only: the kill is SIGKILL, and the temp files of a dead writer are
//! told apart by its process ID.

#![cfg(unix)]

mod common;

use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::*;
use fx_bench::Rng;
use fx_engine::{Engine, EngineHealth, Generation};
use fx_model::{ResumeCursor, StartOutcome};
use fx_store::{MappedIndex, temp_name_prefix};
use fx_testkit::crash::{self, Child, ChildError};

/// The child's test, by its name in this binary.
const CHILD_TEST: &str = "crash_child";

/// Children a series may start per kill it needs during a checkpoint
/// before it gives up.
const MAX_RUNS_PER_KILL: u32 = 10;

/// Names the store folder (the parent's scratch folder) for the child.
const DIR_ENV: &str = "FX_CRASH_DIR";

/// Overrides [`DEFAULT_SEED`].
const SEED_ENV: &str = "FX_CRASH_SEED";

/// The seed of the kill delays when [`SEED_ENV`] is not set.
const DEFAULT_SEED: u64 = 0x2303;

/// The store's file name in the store folder.
const STORE_NAME: &str = "index.fxdb";

/// Files in `/vol/bulk`: with their long names, every checkpoint writes
/// about 240 KB, a store of some size rather than a few bytes. (A bigger
/// one hardly lengthens a write, whose syncs take most of its time, and
/// slows every child's start.)
const BULK_FILES: usize = 2_000;

/// The name of every bulk file starts with this (then `<n>.dat`).
const BULK_PREFIX: &str = "a-bulk-file-whose-name-takes-room-in-the-store-";

/// How long the parent waits for a child to be ready (it may scan, or
/// load the store).
const STARTUP_WAIT: Duration = Duration::from_secs(60);

/// The push-to-saved time assumed before the first one is measured.
const FIRST_CYCLE_GUESS: Duration = Duration::from_millis(100);

/// The save-to-saved time (one checkpoint) assumed before the first one is
/// measured.
const FIRST_SAVE_GUESS: Duration = Duration::from_millis(10);

/// A child stops by itself after this long: one whose parent died would
/// otherwise keep writing into the scratch folder forever.
const CHILD_LIFETIME: Duration = Duration::from_secs(120);

/// The signal a kill must have ended the child with.
const SIGKILL: i32 = 9;

/// The path event `k` creates.
fn numbered(k: u64) -> String {
    format!("{ROOT}/f{k}")
}

/// The cursor after event `event_id`.
fn fs_events(event_id: u64) -> ResumeCursor {
    ResumeCursor::FsEvents {
        event_id,
        db_uuid: DB_UUID,
    }
}

/// The tree once events `1..=events` have happened, built in the same
/// order in every process, so its `FileKey`s match across processes.
fn build_tree(events: u64) -> FakeTree {
    let tree = FakeTree::new();
    tree.mkdir(&format!("{ROOT}/bulk"));
    tree.add_files(&format!("{ROOT}/bulk"), BULK_PREFIX, BULK_FILES);
    for k in 1..=events {
        tree.touch(&numbered(k), k);
    }
    tree
}

/// The event ID of the store's only cursor.
fn stored_event(mapped: &MappedIndex) -> u64 {
    match mapped.cursors() {
        [(_, ResumeCursor::FsEvents { event_id, db_uuid })] if *db_uuid == DB_UUID => *event_id,
        other => panic!("the store holds {other:?}, not one FSEvents cursor of the fake source"),
    }
}

// ------------------------------------------------------------------ child

/// The child: resumes from the store the previous child left (or scans
/// and saves when there is none), then creates, publishes and saves one
/// file after another until the parent kills it.
#[test]
#[ignore = "the child process of the kill -9 tests; does nothing when run by hand"]
fn crash_child() {
    if !crash::is_child() {
        return;
    }
    let dir = PathBuf::from(std::env::var_os(DIR_ENV).expect("the parent sets FX_CRASH_DIR"));
    let store = dir.join(STORE_NAME);
    // Read before the engine opens the store: `Engine::open` falls back to
    // a full rescan when it rejects a store, so only the cursor handed to
    // the source tells a resume from a rebuild.
    let resume = store.exists().then(|| {
        let mapped = MappedIndex::open(&store).expect("the store left by the last kill opens");
        stored_event(&mapped)
    });
    let first = resume.unwrap_or(0);
    let tree = build_tree(first);
    let source = FakeSource::starting_at(StartOutcome::Resumed, first);
    let config = config()
        .with_store_path(&store)
        .with_checkpoint_idle(Duration::from_millis(1))
        .with_checkpoint_max_delay(Duration::from_millis(10));
    let engine =
        Engine::open(config, source.clone(), tree.clone(), tree.clone()).expect("the engine opens");
    match resume {
        Some(e) => {
            assert_eq!(
                source.started_from(),
                Some(Some(fs_events(e))),
                "the engine did not resume from the store's cursor"
            );
            let snap = engine.snapshot();
            assert_eq!(
                snap.generation(),
                0,
                "the loaded index is the first snapshot"
            );
            assert_eq!(snap.cursor(), Some(fs_events(e)));
            assert!(
                snapshot_entries(&snap) == tree.entries(),
                "the loaded index is not the bulk files and f1..f{e}"
            );
            assert_eq!(
                engine.health(),
                EngineHealth::Live,
                "the engine rescans instead of resuming"
            );
            crash::report(format!("resumed {e}"));
        }
        None => {
            assert_eq!(source.started_from(), Some(None), "no store, no cursor");
            wait_for_tree(&engine, &tree, "the first full scan");
            engine.checkpoint().expect("the first full scan is saved");
            crash::report("fresh");
        }
    }
    let published = engine.subscribe();
    crash::report("ready");
    let end = Instant::now() + CHILD_LIFETIME;
    for k in first + 1.. {
        assert!(Instant::now() < end, "no kill came: the parent is gone");
        let path = numbered(k);
        tree.touch(&path, k);
        crash::report(format!("push {k}"));
        source.push(vec![check(&path)]);
        wait_for_event(&engine, &published, k);
        crash::report(format!("save {k}"));
        if let Err(e) = engine.checkpoint() {
            panic!("the checkpoint of event {k} failed: {e}");
        }
        crash::report(format!("saved {k}"));
    }
}

/// Waits until the latest snapshot covers event `k`: the engine publishes
/// a cursor only with the batch it covers.
fn wait_for_event(engine: &Engine, published: &Receiver<Generation>, k: u64) {
    let want = Some(fs_events(k));
    let end = Instant::now() + WAIT;
    while engine.snapshot().cursor() != want {
        let left = end.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "event {k} was not published in {WAIT:?}");
        let _ = published.recv_timeout(left);
    }
}

// ----------------------------------------------------------------- parent

#[test]
fn kills_during_engine_checkpoints() {
    run_kills(10);
}

#[test]
#[ignore = "200 kill -9 runs during engine checkpoints; run with --ignored"]
fn two_hundred_kills_during_engine_checkpoints() {
    run_kills(200);
}

/// The scratch folder the store lives in, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "fx-engine-crash-{name}-{}-{nanos}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).expect("create scratch dir");
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The seed of the kill delays: [`SEED_ENV`], else [`DEFAULT_SEED`].
fn seed() -> u64 {
    match std::env::var(SEED_ENV) {
        Ok(s) => s
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("{SEED_ENV} is not a number: {s:?}")),
        Err(_) => DEFAULT_SEED,
    }
}

/// The names in the store folder, sorted.
fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("the store folder lists")
        .map(|e| e.expect("a folder entry").file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Whether `name` is one of the temp files process `pid` writes a
/// checkpoint to: `prefix` (`fx_store::temp_name_prefix` of the store),
/// then `<pid>.<counter>`.
fn is_temp_of(name: &str, prefix: &str, pid: u32) -> bool {
    name.strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix(&format!("{pid}.")))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The event of a `push <k>`, `save <k>` or `saved <k>` report, when
/// `report` is that `kind`.
fn event_of(report: &str, kind: &str) -> Option<u64> {
    report.strip_prefix(kind)?.strip_prefix(' ')?.parse().ok()
}

/// The child's push-to-saved and save-to-saved times, measured as its
/// reports arrive. They set the range of the kill delays, so kills spread
/// over whole cycles, or whole checkpoints, whatever this machine's speed.
#[derive(Default)]
struct Cycles {
    /// The last push of this run not yet saved, and when it was reported.
    pushed: Option<(u64, Instant)>,
    /// The last `save` of this run not yet saved, and when it was reported.
    saving: Option<(u64, Instant)>,
    cycle: Mean,
    save: Mean,
}

/// A running mean of durations.
#[derive(Default)]
struct Mean {
    total: Duration,
    count: u32,
}

impl Mean {
    fn add(&mut self, d: Duration) {
        self.total += d;
        self.count += 1;
    }

    fn or(&self, guess: Duration) -> Duration {
        if self.count == 0 {
            guess
        } else {
            self.total / self.count
        }
    }
}

impl Cycles {
    fn new_run(&mut self) {
        self.pushed = None;
        self.saving = None;
    }

    fn record(&mut self, report: &str, at: Instant) {
        if let Some(k) = event_of(report, "push") {
            self.pushed = Some((k, at));
        } else if let Some(k) = event_of(report, "save") {
            self.saving = Some((k, at));
        } else if let Some(k) = event_of(report, "saved") {
            if let Some((pushed, since)) = self.pushed.take() {
                if pushed == k {
                    self.cycle.add(at - since);
                }
            }
            if let Some((saving, since)) = self.saving.take() {
                if saving == k {
                    self.save.add(at - since);
                }
            }
        }
    }

    /// A delay drawn uniformly from `0 .. 2 ×` the mean push-to-saved time.
    fn cycle_delay(&self, rng: &mut Rng) -> Duration {
        uniform(rng, 2 * self.cycle.or(FIRST_CYCLE_GUESS))
    }

    /// A delay drawn uniformly from `0 .. 1.25 ×` the mean save-to-saved
    /// time: a moment within one checkpoint, or shortly after it.
    fn save_delay(&self, rng: &mut Rng) -> Duration {
        uniform(rng, self.save.or(FIRST_SAVE_GUESS) * 5 / 4)
    }
}

/// A duration drawn uniformly from `0 ..= max`, in microseconds.
fn uniform(rng: &mut Rng, max: Duration) -> Duration {
    let max = u64::try_from(max.as_micros()).unwrap_or(u64::MAX / 4);
    Duration::from_micros(rng.below(max + 1))
}

/// The child's next report before it is ready.
fn startup_report(child: &mut Child, run: u32) -> String {
    child
        .next_report(STARTUP_WAIT)
        .unwrap_or_else(|e| panic!("run {run}: {e}"))
}

/// Checks the store a kill left: it loads, its checksums hold, it converts
/// to an index, and that index is exactly the tree its one cursor covers.
/// Returns the cursor's event.
fn check_store(store: &Path, run: u32) -> u64 {
    let mapped = MappedIndex::open(store)
        .unwrap_or_else(|e| panic!("run {run}: the store does not load: {e}"));
    mapped
        .verify_checksums()
        .unwrap_or_else(|e| panic!("run {run}: the store fails its checksums: {e}"));
    let index = mapped
        .to_index()
        .unwrap_or_else(|e| panic!("run {run}: the store's index is rejected: {e}"));
    let event = stored_event(&mapped);
    let saved = index_entries(&index.snapshot());
    let want = build_tree(event).entries();
    if saved != want {
        let missing = want.keys().filter(|k| !saved.contains_key(*k)).count();
        let extra = saved.keys().filter(|k| !want.contains_key(*k)).count();
        let differ = want
            .iter()
            .filter(|(k, e)| saved.get(*k).is_some_and(|s| s != *e))
            .count();
        panic!(
            "run {run}: the store's cursor is event {event}, but its index is not the bulk files \
             and f1..f{event}: {missing} missing, {extra} extra, {differ} different"
        );
    }
    event
}

/// Runs children one after another, each from the store the last kill
/// left, and checks the store and its folder after every kill, until
/// `target` kills have landed during a checkpoint.
fn run_kills(target: u32) {
    let seed = seed();
    eprintln!("kill -9 during engine checkpoints: seed {seed} (set {SEED_ENV} to change it)");
    let mut rng = Rng::new(seed);
    let dir = TempDir::new("engine");
    let store = dir.0.join(STORE_NAME);
    let temp_prefix = temp_name_prefix(&store)
        .and_then(|p| p.into_string().ok())
        .expect("the store's temp files have a name");
    let started = Instant::now();
    let mut cycles = Cycles::default();
    // The store's event as checked after the last kill; `None` before the
    // first child.
    let mut stored: Option<u64> = None;
    let mut kills_leaving_temp = 0;
    let mut kills_in_save = 0;
    let mut kills_after_save = 0;
    let mut runs = 0;

    while kills_leaving_temp + kills_in_save < target {
        runs += 1;
        let run = runs;
        // The kill delays follow the save times measured so far, so on a
        // shared CI runner, where a few saves take far longer than the rest,
        // most kills can come after the save they aimed at: GitHub's runner
        // once saw only 8 of 40 land. Ten runs per kill needed keep the test
        // from failing on that alone; a working machine needs fewer than two.
        assert!(
            run <= MAX_RUNS_PER_KILL * target,
            "only {} of {} kills landed during a checkpoint",
            kills_leaving_temp + kills_in_save,
            run - 1
        );
        let mut child =
            Child::spawn(CHILD_TEST, [(DIR_ENV, dir.0.as_os_str())]).expect("the child starts");
        let pid = child.id();

        // The child resumes from exactly what the last kill left.
        let want = stored.map_or_else(|| "fresh".to_string(), |e| format!("resumed {e}"));
        assert_eq!(startup_report(&mut child, run), want, "run {run}");
        assert_eq!(startup_report(&mut child, run), "ready", "run {run}");

        // Opening the store removed the dead child's temp files.
        assert_eq!(
            names_in(&dir.0),
            [STORE_NAME],
            "run {run}: when the child is ready, the store folder holds only the store; \
             the temp files of the child killed before it are left"
        );

        // Kill at a random moment: three runs in four within a checkpoint
        // (timed from its `save`), the fourth anywhere in the cycles.
        cycles.new_run();
        let mut reports = Vec::new();
        let deadline = if run % 4 == 0 {
            Instant::now() + cycles.cycle_delay(&mut rng)
        } else {
            loop {
                let report = child
                    .next_report(STARTUP_WAIT)
                    .unwrap_or_else(|e| panic!("run {run}: {e}"));
                let at = Instant::now();
                cycles.record(&report, at);
                let saving = event_of(&report, "save").is_some();
                reports.push(report);
                if saving {
                    break at + cycles.save_delay(&mut rng);
                }
            }
        };
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            match child.next_report(left) {
                Ok(report) => {
                    cycles.record(&report, Instant::now());
                    reports.push(report);
                }
                Err(ChildError::Timeout(_)) => break,
                Err(e) => panic!("run {run}: {e}"),
            }
        }
        let killed = child.kill9().expect("the kill");
        assert_eq!(
            killed.status.signal(),
            Some(SIGKILL),
            "run {run}: the child was not running when it was killed ({:?})",
            killed.status
        );
        reports.extend(killed.reports);

        // What the child reported before it died bounds what the store may
        // hold. A run that pushed nothing must leave the store as it found
        // it; one that saved event `k` must hold `k` or later.
        let before = stored.unwrap_or(0);
        for report in &reports {
            assert!(
                ["push", "save", "saved"]
                    .iter()
                    .any(|kind| event_of(report, kind).is_some()),
                "run {run}: unexpected report {report:?}"
            );
        }
        let pushed = reports.iter().rev().find_map(|r| event_of(r, "push"));
        let saved = reports.iter().rev().find_map(|r| event_of(r, "saved"));
        let event = check_store(&store, run);
        let (low, high) = (saved.unwrap_or(before), pushed.unwrap_or(before));
        assert!(
            (low..=high).contains(&event),
            "run {run}: the store holds event {event}; it held {before} before, and the child \
             then saved up to {low} and pushed up to {high}"
        );

        // Beside the store, only the dead child's temp files: a kill that
        // landed while a checkpoint was written.
        let others: Vec<String> = names_in(&dir.0)
            .into_iter()
            .filter(|name| name != STORE_NAME)
            .collect();
        for name in &others {
            assert!(
                is_temp_of(name, &temp_prefix, pid),
                "run {run}: {name} beside the store is not a temp file of the killed child"
            );
        }
        if !others.is_empty() {
            kills_leaving_temp += 1;
        } else if reports
            .last()
            .is_some_and(|r| event_of(r, "save").is_some())
        {
            // In the checkpoint, before its temp file or after the rename.
            kills_in_save += 1;
        }
        if saved.is_some() {
            kills_after_save += 1;
        }
        stored = Some(event);
    }

    fx_store::cleanup_stale_temps(&store).expect("the stale temp files are removed");
    assert_eq!(
        names_in(&dir.0),
        [STORE_NAME],
        "after the last kill, cleaning up leaves only the store"
    );
    eprintln!(
        "kill -9 during engine checkpoints: {runs} kills, {} during a checkpoint \
         ({kills_leaving_temp} left a temp file, {kills_in_save} in the checkpoint outside its \
         write), {kills_after_save} after at least one save; the store reached event {}; one \
         checkpoint took {:.1?}; {:.1} s (seed {seed})",
        kills_leaving_temp + kills_in_save,
        stored.unwrap_or(0),
        cycles.save.or(FIRST_SAVE_GUESS),
        started.elapsed().as_secs_f64()
    );
}
