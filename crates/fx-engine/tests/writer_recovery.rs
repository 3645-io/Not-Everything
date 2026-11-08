//! The writer thread recovers from a failing or stopped change source and
//! reports its health.
//!
//! Contract these tests rely on (in addition to `tests/common/mod.rs`):
//!
//! - `fx_engine::EngineHealth` (`Debug + Clone + PartialEq`) with the variants
//!   `Live`, `Recovering` and `Failed(String)`.
//! - `Engine::health(&self) -> EngineHealth`: the current state. An engine
//!   whose source is delivering normally is `Live`.
//! - `Engine::subscribe_health(&self)`: a receiver (with `recv_timeout` and
//!   `try_recv`) that gets every new health state from then on, sent when
//!   the state changes.
//! - `EngineConfig::with_restart_threshold(u32)` / `restart_threshold()`:
//!   how many consecutive `next_batch` errors make the writer call
//!   `ChangeSource::start(Some(&cursor))` again. Default
//!   `fx_engine::DEFAULT_RESTART_THRESHOLD` = 3.
//! - `EngineConfig::with_restart_backoff(Duration)` / `restart_backoff()`:
//!   the shortest wait between two restart attempts (later waits may grow).
//! - A restart that reports `GapDetected` or `NoCursorSupport` schedules a
//!   full rescan; the previous snapshot keeps serving until it is done.
//! - While the source cannot be restarted the health is `Recovering` or
//!   `Failed(_)`, never `Live`, and `shutdown` still returns promptly.

mod common;

use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::Path;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use common::{
    FakeTree, ROOT, WAIT, assert_orders_match, check, snapshot_entries, wait_for, wait_for_tree,
};
use fx_engine::{DEFAULT_RESTART_THRESHOLD, Engine, EngineConfig, EngineHealth};
use fx_fs::{ChangeSource, VolumeEnumerator};
use fx_model::{Change, Entry, FileKey, ResumeCursor, StartOutcome};

const DB_UUID: [u8; 16] = [9; 16];

/// What one `start` call does.
#[derive(Clone, Copy, Debug)]
enum StartStep {
    Ok(StartOutcome),
    Fail,
}

struct SourceState {
    /// Steps for the next `start` calls; `fallback` once empty.
    script: VecDeque<StartStep>,
    fallback: StartStep,
    /// Every cursor `start` was called with, in order.
    starts: Vec<Option<ResumeCursor>>,
    /// Whether the stream is running. A stopped stream fails every
    /// `next_batch` until a successful `start`.
    running: bool,
    /// Errors to return from the next `next_batch` calls of a running stream.
    fail_next: u32,
    queue: VecDeque<Vec<Change>>,
    event_id: u64,
    errors: u64,
}

/// A scripted `ChangeSource` that can fail, stop and be restarted.
#[derive(Clone)]
struct ScriptedSource {
    inner: Arc<Mutex<SourceState>>,
}

impl ScriptedSource {
    /// A source whose first `start` returns `first`; later starts follow
    /// [`script_start`](Self::script_start), then `fallback`.
    fn new(first: StartOutcome, fallback: StartStep) -> ScriptedSource {
        ScriptedSource {
            inner: Arc::new(Mutex::new(SourceState {
                script: VecDeque::from([StartStep::Ok(first)]),
                fallback,
                starts: Vec::new(),
                running: false,
                fail_next: 0,
                queue: VecDeque::new(),
                event_id: 0,
                errors: 0,
            })),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SourceState> {
        self.inner.lock().unwrap()
    }

    fn script_start(&self, step: StartStep) {
        self.lock().script.push_back(step);
    }

    fn set_fallback(&self, step: StartStep) {
        self.lock().fallback = step;
    }

    /// Stops the stream for good: every `next_batch` fails until restarted.
    fn kill(&self) {
        self.lock().running = false;
    }

    /// Makes the next `n` `next_batch` calls fail while the stream stays up.
    fn fail_next(&self, n: u32) {
        self.lock().fail_next += n;
    }

    fn push(&self, batch: Vec<Change>) {
        self.lock().queue.push_back(batch);
    }

    fn starts(&self) -> Vec<Option<ResumeCursor>> {
        self.lock().starts.clone()
    }

    fn start_count(&self) -> usize {
        self.lock().starts.len()
    }

    fn errors(&self) -> u64 {
        self.lock().errors
    }

    fn pending(&self) -> usize {
        self.lock().queue.len()
    }
}

impl ChangeSource for ScriptedSource {
    fn start(&mut self, from: Option<&ResumeCursor>) -> io::Result<StartOutcome> {
        let mut st = self.lock();
        st.starts.push(from.copied());
        let step = st.script.pop_front().unwrap_or(st.fallback);
        match step {
            StartStep::Ok(outcome) => {
                st.running = true;
                st.fail_next = 0;
                Ok(outcome)
            }
            StartStep::Fail => {
                st.running = false;
                Err(io::Error::other("scripted start failure"))
            }
        }
    }

    fn next_batch(&mut self, deadline: Instant) -> io::Result<Vec<Change>> {
        loop {
            {
                let mut st = self.lock();
                if !st.running {
                    st.errors += 1;
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "event stream stopped",
                    ));
                }
                if st.fail_next > 0 {
                    st.fail_next -= 1;
                    st.errors += 1;
                    return Err(io::Error::other("scripted next_batch failure"));
                }
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
        ResumeCursor::FsEvents {
            event_id: self.lock().event_id,
            db_uuid: DB_UUID,
        }
    }
}

#[derive(Default)]
struct GateState {
    armed: bool,
    entered: bool,
}

/// Wraps the fake tree's enumerator; while armed, every enumeration blocks
/// until the test disarms the gate.
#[derive(Clone)]
struct GatedEnum {
    tree: FakeTree,
    gate: Arc<(Mutex<GateState>, Condvar)>,
}

impl GatedEnum {
    fn new(tree: &FakeTree) -> GatedEnum {
        GatedEnum {
            tree: tree.clone(),
            gate: Arc::new((Mutex::new(GateState::default()), Condvar::new())),
        }
    }

    fn arm(&self) {
        let mut st = self.gate.0.lock().unwrap();
        st.armed = true;
        st.entered = false;
    }

    fn disarm(&self) {
        self.gate.0.lock().unwrap().armed = false;
        self.gate.1.notify_all();
    }

    /// Waits until an enumeration is blocked in the gate.
    fn wait_entered(&self) {
        let end = Instant::now() + WAIT;
        let mut st = self.gate.0.lock().unwrap();
        while !st.entered {
            let now = Instant::now();
            assert!(now < end, "timed out waiting for a rescan to start");
            st = self.gate.1.wait_timeout(st, end - now).unwrap().0;
        }
    }
}

impl VolumeEnumerator for GatedEnum {
    fn enumerate(&self, root: &Path, sink: &mut dyn FnMut(Entry)) -> io::Result<()> {
        {
            let mut st = self.gate.0.lock().unwrap();
            if st.armed {
                st.entered = true;
                self.gate.1.notify_all();
                while st.armed {
                    st = self.gate.1.wait(st).unwrap();
                }
            }
        }
        self.tree.enumerate(root, sink)
    }
}

fn config() -> EngineConfig {
    EngineConfig::new(ROOT)
        .with_batch_window(Duration::from_millis(50))
        .with_restart_threshold(3)
        .with_restart_backoff(Duration::from_millis(20))
}

fn sample_tree() -> FakeTree {
    let tree = FakeTree::new();
    tree.mkdir("/vol/docs");
    tree.mkdir("/vol/docs/old");
    tree.touch("/vol/docs/a.txt", 10);
    tree.touch("/vol/docs/old/b.txt", 20);
    tree.mkdir("/vol/src");
    tree.add_files("/vol/src", "f", 25);
    tree
}

fn start_engine(tree: &FakeTree, source: &ScriptedSource, enumerator: GatedEnum) -> Engine {
    Engine::start(
        tree.build_index(),
        config().with_resume_from(ResumeCursor::FsEvents {
            event_id: 0,
            db_uuid: DB_UUID,
        }),
        source.clone(),
        tree.clone(),
        enumerator,
    )
    .expect("engine starts")
}

fn wait_health(engine: &Engine, what: &str, mut cond: impl FnMut(&EngineHealth) -> bool) {
    let end = Instant::now() + WAIT;
    loop {
        let h = engine.health();
        if cond(&h) {
            return;
        }
        assert!(
            Instant::now() < end,
            "timed out waiting for health: {what}; last {h:?}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn wait_starts(source: &ScriptedSource, n: usize, what: &str) {
    let end = Instant::now() + WAIT;
    while source.start_count() < n {
        assert!(
            Instant::now() < end,
            "timed out waiting for {what}: start called {} times, wanted {n}",
            source.start_count()
        );
        thread::sleep(Duration::from_millis(2));
    }
}

/// Shuts the engine down on another thread and fails if it hangs.
fn shutdown_promptly(engine: Engine) {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        engine.shutdown();
        let _ = tx.send(());
    });
    rx.recv_timeout(Duration::from_secs(5))
        .expect("shutdown returns promptly even while the source is failing");
}

fn is_live(h: &EngineHealth) -> bool {
    matches!(h, EngineHealth::Live)
}

#[test]
fn default_restart_threshold_is_three() {
    assert_eq!(DEFAULT_RESTART_THRESHOLD, 3);
    assert_eq!(EngineConfig::new(ROOT).restart_threshold(), 3);
    let cfg = EngineConfig::new(ROOT)
        .with_restart_threshold(5)
        .with_restart_backoff(Duration::from_millis(40));
    assert_eq!(cfg.restart_threshold(), 5);
    assert_eq!(cfg.restart_backoff(), Duration::from_millis(40));
}

#[test]
fn healthy_engine_reports_live() {
    let tree = sample_tree();
    let source = ScriptedSource::new(StartOutcome::Resumed, StartStep::Ok(StartOutcome::Resumed));
    let engine = start_engine(&tree, &source, GatedEnum::new(&tree));
    wait_health(&engine, "Live after a resumed start", is_live);

    tree.touch("/vol/docs/new.txt", 5);
    source.push(vec![check("/vol/docs/new.txt")]);
    wait_for_tree(&engine, &tree, "change applied");
    assert!(is_live(&engine.health()));
    assert_eq!(
        source.start_count(),
        1,
        "a healthy source is never restarted"
    );
    shutdown_promptly(engine);
}

#[test]
fn errors_below_threshold_do_not_restart() {
    let tree = sample_tree();
    let source = ScriptedSource::new(StartOutcome::Resumed, StartStep::Ok(StartOutcome::Resumed));
    let engine = start_engine(&tree, &source, GatedEnum::new(&tree));

    source.fail_next(2);
    let end = Instant::now() + WAIT;
    while source.errors() < 2 {
        assert!(Instant::now() < end, "engine polls the source");
        thread::sleep(Duration::from_millis(2));
    }
    tree.touch("/vol/docs/after.txt", 7);
    source.push(vec![check("/vol/docs/after.txt")]);
    wait_for_tree(&engine, &tree, "change after two errors applied");
    // Give a wrongly eager restart time to happen.
    thread::sleep(Duration::from_millis(300));
    assert_eq!(
        source.start_count(),
        1,
        "two errors stay below the threshold of three; no restart"
    );
    wait_health(&engine, "Live after the source delivers again", is_live);
    shutdown_promptly(engine);
}

#[test]
fn stopped_source_is_restarted_from_cursor_and_returns_to_live() {
    let tree = sample_tree();
    let source = ScriptedSource::new(StartOutcome::Resumed, StartStep::Ok(StartOutcome::Resumed));
    // The first restart attempt fails; the second one succeeds.
    source.script_start(StartStep::Fail);
    source.script_start(StartStep::Ok(StartOutcome::Resumed));
    let engine = start_engine(&tree, &source, GatedEnum::new(&tree));

    // One normal batch, so the published cursor moves past the start cursor.
    tree.touch("/vol/docs/first.txt", 1);
    source.push(vec![check("/vol/docs/first.txt")]);
    let snap = wait_for_tree(&engine, &tree, "first batch applied");
    let cursor = snap.cursor().expect("published snapshot carries a cursor");
    assert_eq!(
        cursor,
        ResumeCursor::FsEvents {
            event_id: 1,
            db_uuid: DB_UUID
        }
    );
    wait_health(&engine, "Live before the failure", is_live);

    let health = engine.subscribe_health();
    source.kill();

    wait_starts(&source, 3, "a failed restart followed by a retry");
    let starts = source.starts();
    for (i, from) in starts.iter().enumerate().skip(1) {
        assert_eq!(
            *from,
            Some(cursor),
            "restart #{i} resumes from the last published cursor"
        );
    }
    wait_health(&engine, "Live again after the successful restart", is_live);

    // Changes after the restart are applied.
    tree.touch("/vol/src/after_restart.rs", 42);
    tree.rename("/vol/docs/a.txt", "/vol/src/a_moved.txt");
    source.push(vec![
        check("/vol/src/after_restart.rs"),
        check("/vol/docs/a.txt"),
        check("/vol/src/a_moved.txt"),
    ]);
    let snap = wait_for_tree(&engine, &tree, "changes after the restart applied");
    assert_orders_match(&snap);
    assert!(is_live(&engine.health()));

    let mut seen = Vec::new();
    while let Ok(h) = health.try_recv() {
        seen.push(h);
    }
    assert!(
        seen.iter().any(|h| !is_live(h)),
        "subscribers were told the engine left Live: {seen:?}"
    );
    assert!(
        seen.last().is_some_and(is_live),
        "subscribers were told the engine is Live again: {seen:?}"
    );
    assert!(
        source.start_count() <= 4,
        "no extra restarts once the source delivers again: {:?}",
        source.starts()
    );
    shutdown_promptly(engine);
}

/// A restart reporting `outcome` must trigger a full rescan that leaves the
/// index equal to a fresh enumeration, while the old snapshot keeps serving.
fn restart_with_lost_history_rescans(outcome: StartOutcome) {
    let tree = sample_tree();
    let source = ScriptedSource::new(StartOutcome::Resumed, StartStep::Ok(StartOutcome::Resumed));
    source.script_start(StartStep::Ok(outcome));
    let gated = GatedEnum::new(&tree);
    let engine = start_engine(&tree, &source, gated.clone());
    wait_health(&engine, "Live after start", is_live);
    let before: HashMap<FileKey, Entry> = tree.entries();
    assert_eq!(snapshot_entries(&engine.snapshot()), before);

    // Changes the stream never reports: they happen while it is down.
    tree.touch("/vol/docs/unseen.txt", 99);
    tree.add_files("/vol/docs/old", "lost", 10);
    tree.remove("/vol/src/f3.dat");
    tree.rename("/vol/docs/old", "/vol/src/old_moved");
    tree.set_size("/vol/docs/a.txt", 1234);

    gated.arm();
    source.kill();
    wait_starts(&source, 2, "a restart after the stream stopped");
    gated.wait_entered();

    // The rescan is blocked: readers still get the old snapshot.
    let snap = engine.snapshot();
    assert_eq!(
        snapshot_entries(&snap),
        before,
        "the previous snapshot keeps serving during the rescan ({outcome:?})"
    );
    assert!(!matches!(engine.health(), EngineHealth::Failed(_)));

    gated.disarm();
    let snap = wait_for_tree(&engine, &tree, "full rescan equals a fresh enumeration");
    assert_orders_match(&snap);
    wait_health(&engine, "Live after the rescan", is_live);

    // The stream works again after the rescan.
    tree.touch("/vol/later.txt", 3);
    source.push(vec![check("/vol/later.txt")]);
    let snap = wait_for_tree(&engine, &tree, "change after the rescan applied");
    assert_orders_match(&snap);
    shutdown_promptly(engine);
}

#[test]
fn restart_reporting_gap_triggers_full_rescan() {
    restart_with_lost_history_rescans(StartOutcome::GapDetected);
}

#[test]
fn restart_without_cursor_support_triggers_full_rescan() {
    restart_with_lost_history_rescans(StartOutcome::NoCursorSupport);
}

#[test]
fn persistent_failure_is_never_reported_live() {
    let tree = sample_tree();
    let source = ScriptedSource::new(StartOutcome::Resumed, StartStep::Fail);
    let engine = start_engine(&tree, &source, GatedEnum::new(&tree));
    wait_health(&engine, "Live after start", is_live);
    let before = tree.entries();

    let health = engine.subscribe_health();
    source.kill();
    wait_health(
        &engine,
        "Recovering or Failed after the stream stops",
        |h| !is_live(h),
    );
    wait_starts(&source, 2, "at least one restart attempt");

    // Watch for a while: never Live, restarts keep backing off.
    let watch = Duration::from_millis(1500);
    let starts_before = source.start_count();
    let end = Instant::now() + watch;
    while Instant::now() < end {
        let h = engine.health();
        assert!(
            matches!(h, EngineHealth::Recovering | EngineHealth::Failed(_)),
            "a source that cannot restart is never Live, got {h:?}"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let attempts = source.start_count() - starts_before;
    let max_attempts = (watch.as_millis() / config().restart_backoff().as_millis()) as usize + 2;
    assert!(
        attempts <= max_attempts,
        "restarts back off: {attempts} attempts in {watch:?} (at most {max_attempts})"
    );

    let mut seen = Vec::new();
    while let Ok(h) = health.try_recv() {
        seen.push(h);
    }
    assert!(!seen.is_empty(), "subscribers were told the health changed");
    assert!(
        seen.iter().all(|h| !is_live(h)),
        "subscribers were never told Live while failing: {seen:?}"
    );

    // The old snapshot keeps serving the whole time.
    let snap = wait_for(&engine, "snapshot still served", |_| true);
    assert_eq!(snapshot_entries(&snap), before);
    shutdown_promptly(engine);
}

#[test]
fn failed_engine_recovers_when_the_source_comes_back() {
    let tree = sample_tree();
    let source = ScriptedSource::new(StartOutcome::Resumed, StartStep::Fail);
    let engine = start_engine(&tree, &source, GatedEnum::new(&tree));
    wait_health(&engine, "Live after start", is_live);

    source.kill();
    wait_starts(&source, 3, "repeated restart attempts");
    assert!(!is_live(&engine.health()));

    // The source becomes restartable again.
    source.set_fallback(StartStep::Ok(StartOutcome::Resumed));
    wait_health(&engine, "Live once a restart succeeds", is_live);

    tree.touch("/vol/src/back.txt", 8);
    source.push(vec![check("/vol/src/back.txt")]);
    let snap = wait_for_tree(&engine, &tree, "change after recovery applied");
    assert_orders_match(&snap);
    assert_eq!(source.pending(), 0);
    shutdown_promptly(engine);
}
