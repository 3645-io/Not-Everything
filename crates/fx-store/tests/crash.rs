//! Crash safety (run 2.3): `kill -9` during checkpoints never corrupts the
//! store, and the next start always loads it.
//!
//! A child process (this test binary again, running `crash_child`) opens
//! the store with `open_or_rebuild`, as a start does, then writes
//! checkpoints back to back: each a new generation G of a synthetic index
//! (an entry added, one removed, one renamed, changed orders and clean
//! flag), with G saved as the cursor's event ID. Before each write it
//! reports `begin G <fingerprint>`, after it `done G`. The parent kills it
//! with SIGKILL, either paused at a named stage of one write
//! (`write_checkpoint_with_stages`) or after a seeded random delay, and
//! checks:
//!
//! - the store holds the last finished generation or the one being
//!   written, never anything else: it opens, passes `verify_checksums` and
//!   `to_index`, and its contents' fingerprint is the one the child
//!   reported for that generation;
//! - at a named stage, which of the two it is (the old one before the
//!   rename, the new one after it) and what temp file is left: none, an
//!   empty one, or a complete one;
//! - every other file is a temp file of the dead child, and the next start
//!   removes it;
//! - the next child's `open_or_rebuild` loads the store; it never rebuilds
//!   it.
//!
//! Every run starts from what the previous kill left. A series runs until
//! enough kills have landed during a checkpoint (a random kill between two
//! writes is checked but not counted): 20 in `cargo test`, reaching every
//! stage; 200 with `cargo test -p fx-store --test crash -- --ignored`.
//! `FX_CRASH_SEED` changes the seed.
//! Power loss is out of scope: a killed process loses nothing it wrote.
//!
//! Clean-room: written from docs/PLAN.md and the in-repo code only.

#![cfg(unix)]

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fx_bench::{Rng, TreeShape, generate_tree};
use fx_index::{Index, IndexParts};
use fx_model::{Entry, EntryFlags, EntryId, EntryKind, FileKey, ResumeCursor, VolumeId};
use fx_store::{
    MappedIndex, Opened, RebuiltIndex, RejectReason, SaveInput, VerifyMode, WriteStage,
    cleanup_stale_temps, open_or_rebuild, temp_name_prefix, write_checkpoint_with_stages,
};
use fx_testkit::crash::{self, Child, report};

/// The store's file name in the test folder.
const STORE: &str = "index.fxs";
/// The child test's name in this binary.
const CHILD: &str = "crash_child";
/// Entries in the synthetic index the first child builds.
const ENTRIES: usize = 50_000;
/// The default seed; `FX_CRASH_SEED` overrides it.
const SEED: u64 = 0x0203_0203_c4a5_4e00;
/// The volume and cursor UUID saved with every generation.
const VOL: VolumeId = VolumeId(3);
const UUID: [u8; 16] = [0x23; 16];
/// File IDs of the entries generations add; the synthetic tree's entries
/// have file IDs from 1 to [`ENTRIES`].
const ADDED_BASE: u64 = 1 << 40;
/// The named stages, in write order.
const STAGES: [WriteStage; 5] = [
    WriteStage::TempCreated,
    WriteStage::Written,
    WriteStage::Synced,
    WriteStage::Renamed,
    WriteStage::DirSynced,
];
/// How long the parent waits for one of the child's reports.
const WAIT: Duration = Duration::from_secs(120);

// ---------------------------------------------------------------------------
// The tests
// ---------------------------------------------------------------------------

/// A short series on every `cargo test`: 20 kills during checkpoints, each
/// named stage at least twice.
#[test]
fn kills_during_checkpoints_never_corrupt_the_store() {
    run_kills(20);
}

/// The plan's 200 kills during checkpoints (run 2.3).
#[test]
#[ignore = "200 kill -9 runs during checkpoints; run with --ignored"]
fn two_hundred_kills_during_checkpoints_never_corrupt_the_store() {
    run_kills(200);
}

// ---------------------------------------------------------------------------
// The child: checkpoints back to back until it is killed
// ---------------------------------------------------------------------------

#[test]
#[ignore = "the child process of the crash tests"]
fn crash_child() {
    if !crash::is_child() {
        return;
    }
    let dir = PathBuf::from(env("FX_CRASH_DIR"));
    let path = dir.join(STORE);
    let seed: u64 = env("FX_CRASH_SEED").parse().expect("seed");
    let entries: usize = env("FX_CRASH_ENTRIES").parse().expect("entries");
    // `<write>:<stage>`: pause at that stage of this child's n-th write.
    let pause: Option<(u64, WriteStage)> = std::env::var("FX_CRASH_PAUSE").ok().map(|p| {
        let (n, stage) = p.split_once(':').expect("write:stage");
        (n.parse().expect("write number"), stage_named(stage))
    });

    let opened = open_or_rebuild(&path, VerifyMode::Full, || {
        let first = Generation::first(entries, seed);
        report(format!("begin 0 {:016x}", first.fingerprint()));
        Ok::<_, io::Error>(first.rebuilt())
    })
    .expect("open or rebuild the store");
    let mut generation = match &opened {
        Opened::Loaded(index) => {
            let g = Generation::from_store(index, seed);
            report(format!("loaded {}", g.number));
            g
        }
        Opened::Rebuilt { index, reason } => {
            let g = Generation::from_store(index, seed);
            report(format!("rebuilt {} {}", g.number, reason_kind(reason)));
            g
        }
    };
    drop(opened);
    report("ready");

    // Until the parent kills this process.
    let mut write = 0;
    loop {
        write += 1;
        generation = generation.next();
        let g = generation.number;
        report(format!("begin {g} {:016x}", generation.fingerprint()));
        let orders = generation.orders();
        let orders: Vec<(u8, &[u32])> = orders.iter().map(|(k, o)| (*k, o.as_slice())).collect();
        let input = SaveInput {
            parts: &generation.parts,
            volumes: &generation.volumes(),
            orders: &orders,
            clean_shutdown: generation.clean(),
        };
        write_checkpoint_with_stages(&path, &input, &mut |stage| {
            if pause == Some((write, stage)) {
                report(format!("paused {g} {}", stage_name(stage)));
                loop {
                    thread::sleep(Duration::from_secs(3600));
                }
            }
        })
        .expect("write a checkpoint");
        report(format!("done {g}"));
    }
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
}

/// One generation of the index the child saves, and everything else one
/// checkpoint holds. The next generation follows from this one and the
/// seed alone, so the parent can tell generations apart by number.
struct Generation {
    number: u64,
    seed: u64,
    index: Index,
    parts: IndexParts,
}

impl Generation {
    /// Generation 0: the synthetic tree.
    fn first(entries: usize, seed: u64) -> Generation {
        let tree = generate_tree(entries, seed, TreeShape::default());
        let key = |i: usize| FileKey::new(VOL, i as u64 + 1);
        let mut index = Index::new();
        for i in 0..tree.len() {
            let parent = tree.parent[i];
            let is_dir = tree.is_dir[i];
            index
                .upsert(Entry {
                    key: key(i),
                    parent: (parent != u32::MAX).then(|| key(parent as usize)),
                    name: tree.names.get(i).to_string(),
                    kind: if is_dir {
                        EntryKind::Dir
                    } else {
                        EntryKind::File
                    },
                    flags: EntryFlags::empty(),
                    size: if is_dir { 0 } else { i as u64 * 4_099 },
                    modified: 1_700_000_000 + i as i64,
                    created: 1_600_000_000 + i as i64,
                })
                .expect("insert a tree entry");
        }
        index.compact();
        Generation::with_index(0, seed, index)
    }

    /// The generation a store holds: its number is the cursor's event ID.
    fn from_store(store: &MappedIndex, seed: u64) -> Generation {
        let number = match store.cursor(VOL) {
            Some(ResumeCursor::FsEvents { event_id, .. }) => event_id,
            other => panic!("the store's cursor is {other:?}"),
        };
        let index = store.to_index().expect("the store's index is valid");
        Generation::with_index(number, seed, index)
    }

    fn with_index(number: u64, seed: u64, mut index: Index) -> Generation {
        // Compaction is up to `next`, so a generation's IDs depend only on
        // its own history.
        index.set_compact_fraction(f64::INFINITY);
        let parts = index.snapshot().to_parts();
        Generation {
            number,
            seed,
            index,
            parts,
        }
    }

    /// The next generation: one file added under a random folder, the file
    /// three generations older removed, a random entry renamed, and the
    /// index compacted every eighth generation.
    fn next(self) -> Generation {
        let number = self.number + 1;
        let mut rng = Rng::new(self.seed ^ number.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        let mut index = self.index;
        let snap = index.snapshot();
        let live: Vec<EntryId> = snap.live_ids().collect();
        let pick = |rng: &mut Rng| live[rng.below(live.len() as u64) as usize];
        let entry = |id: EntryId| Entry {
            key: snap.key(id),
            parent: Some(snap.parent(id))
                .filter(|p| !p.is_none())
                .map(|p| snap.key(p)),
            name: snap.name(id).to_string(),
            kind: snap.kind(id),
            flags: snap.flags(id),
            size: snap.size(id),
            modified: snap.modified(id),
            created: snap.created(id),
        };
        let folder = (0..64)
            .map(|_| pick(&mut rng))
            .find(|&id| snap.kind(id) == EntryKind::Dir)
            .unwrap_or(live[0]);
        let added = Entry {
            key: FileKey::new(VOL, ADDED_BASE + number),
            parent: Some(snap.key(folder)),
            name: format!("added {number}.txt"),
            kind: EntryKind::File,
            flags: EntryFlags::empty(),
            size: number,
            modified: 1_800_000_000 + number as i64,
            created: 1_800_000_000 + number as i64,
        };
        let renamed = pick(&mut rng);
        let mut renamed = entry(renamed);
        renamed.name = format!("{} (renamed in {number})", renamed.name);
        drop(snap);

        index.upsert(added).expect("add a file");
        if number > 3 {
            index.remove(FileKey::new(VOL, ADDED_BASE + number - 3));
        }
        // The renamed entry may be the file just removed: then it comes
        // back under its new name.
        index.upsert(renamed).expect("rename an entry");
        if number % 8 == 0 {
            index.compact();
        }
        Generation::with_index(number, self.seed, index)
    }

    /// The saved orders: the live IDs ascending, and rotated by the
    /// generation number.
    fn orders(&self) -> Vec<(u8, Vec<u32>)> {
        let p = &self.parts;
        let live: Vec<u32> = (0..p.id_bound())
            .filter(|&i| p.is_live(i))
            .map(|i| i as u32)
            .collect();
        let mut rotated = live.clone();
        if !rotated.is_empty() {
            let by = (self.number as usize) % rotated.len();
            rotated.rotate_left(by);
        }
        vec![(1, live), (2, rotated)]
    }

    fn volumes(&self) -> Vec<(VolumeId, ResumeCursor)> {
        vec![(
            VOL,
            ResumeCursor::FsEvents {
                event_id: self.number,
                db_uuid: UUID,
            },
        )]
    }

    fn clean(&self) -> bool {
        self.number % 3 == 0
    }

    fn rebuilt(&self) -> RebuiltIndex {
        RebuiltIndex {
            parts: self.parts.clone(),
            volumes: self.volumes(),
            orders: self.orders(),
            clean_shutdown: self.clean(),
        }
    }

    fn fingerprint(&self) -> u64 {
        let orders = self.orders();
        let orders: Vec<(u8, &[u32])> = orders.iter().map(|(k, o)| (*k, o.as_slice())).collect();
        fingerprint(&self.parts, &orders, &self.volumes(), self.clean())
    }
}

/// A hash of everything one checkpoint holds, computed independently of
/// the store's own checksums.
fn fingerprint(
    parts: &IndexParts,
    orders: &[(u8, &[u32])],
    volumes: &[(VolumeId, ResumeCursor)],
    clean: bool,
) -> u64 {
    let mut h = DefaultHasher::new();
    parts.name_bytes.hash(&mut h);
    parts.name_offsets.hash(&mut h);
    parts.parents.hash(&mut h);
    parts.volumes.hash(&mut h);
    parts.file_ids.hash(&mut h);
    parts.meta.hash(&mut h);
    parts.modified.hash(&mut h);
    parts.created.hash(&mut h);
    parts.ext_ids.hash(&mut h);
    parts.ext_bytes.hash(&mut h);
    parts.ext_offsets.hash(&mut h);
    parts.live.hash(&mut h);
    for (key, order) in orders {
        key.hash(&mut h);
        order.hash(&mut h);
    }
    for (volume, cursor) in volumes {
        volume.0.hash(&mut h);
        format!("{cursor:?}").hash(&mut h);
    }
    clean.hash(&mut h);
    h.finish()
}

/// The fingerprint of a checkpoint file.
fn file_fingerprint(store: &MappedIndex) -> u64 {
    fingerprint(
        &store.to_parts(),
        &store.orders(),
        store.cursors(),
        store.clean_shutdown(),
    )
}

fn stage_name(stage: WriteStage) -> &'static str {
    match stage {
        WriteStage::TempCreated => "temp-created",
        WriteStage::Written => "written",
        WriteStage::Synced => "synced",
        WriteStage::Renamed => "renamed",
        WriteStage::DirSynced => "dir-synced",
    }
}

fn stage_named(name: &str) -> WriteStage {
    STAGES
        .into_iter()
        .find(|&s| stage_name(s) == name)
        .unwrap_or_else(|| panic!("no stage named {name}"))
}

fn reason_kind(reason: &RejectReason) -> String {
    let debug = format!("{reason:?}");
    debug
        .split(|c: char| !c.is_alphanumeric())
        .next()
        .unwrap_or_default()
        .to_string()
}

// ---------------------------------------------------------------------------
// The parent: kill, check, start the next child
// ---------------------------------------------------------------------------

/// Where a kill landed, for the summary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Landing {
    /// At a named stage of a write.
    Stage(WriteStage),
    /// At a random moment in a write, before its temp file existed (the
    /// writer checks the input and computes the checksums first).
    BeforeTemp,
    /// At a random moment in a write: its temp file was being written.
    MidWrite,
    /// At a random moment in a write: after the rename, before it returned.
    AfterRename,
    /// At a random moment: between two writes.
    Between,
}

/// One child's reports, as the parent read them.
#[derive(Default)]
struct Reports {
    /// The generation the child loaded or rebuilt, and whether it rebuilt.
    opened: Option<(u64, Option<String>)>,
    ready: bool,
    /// The last generation begun and not yet done.
    in_flight: Option<u64>,
    /// The last generation done.
    done: Option<u64>,
    /// The stage the child paused at, with its generation.
    paused: Option<(u64, WriteStage)>,
    /// The writes begun after `ready`, and when the last one began.
    begun: u64,
    begun_at: Option<Instant>,
    /// How long each finished write took, `begin` to `done`.
    writes: Vec<Duration>,
}

impl Reports {
    fn take(&mut self, line: &str, fingerprints: &mut HashMap<u64, u64>) {
        let words: Vec<&str> = line.split(' ').collect();
        let number = |i: usize| -> u64 {
            words
                .get(i)
                .and_then(|w| w.parse().ok())
                .unwrap_or_else(|| panic!("bad report {line:?}"))
        };
        match words[0] {
            "begin" => {
                let g = number(1);
                let fp = u64::from_str_radix(words[2], 16).expect("fingerprint");
                fingerprints.insert(g, fp);
                self.in_flight = Some(g);
                if self.ready {
                    self.begun += 1;
                    self.begun_at = Some(Instant::now());
                }
            }
            "done" => {
                let g = number(1);
                assert_eq!(self.in_flight, Some(g), "done {g} without its begin");
                self.in_flight = None;
                self.done = Some(g);
                if let Some(at) = self.begun_at {
                    self.writes.push(at.elapsed());
                }
            }
            "loaded" => self.opened = Some((number(1), None)),
            "rebuilt" => {
                // The rebuild's write is generation 0, done before `ready`.
                self.in_flight = None;
                self.opened = Some((number(1), Some(words[2].to_string())));
            }
            "ready" => self.ready = true,
            "paused" => self.paused = Some((number(1), stage_named(words[2]))),
            other => panic!("unknown report {other:?} in {line:?}"),
        }
    }
}

/// The store folder, under the system temp folder, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> TempDir {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "fx-store-crash-{}-{}-{nanos}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).expect("create the test folder");
        TempDir(dir)
    }

    /// The folder's file names, sorted.
    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.0)
            .expect("list the test folder")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn seed() -> u64 {
    match std::env::var("FX_CRASH_SEED") {
        Ok(s) => s.parse().expect("FX_CRASH_SEED is a number"),
        Err(_) => SEED,
    }
}

/// Kills children over one store, checking each kill, until `target`
/// kills have landed during a checkpoint: half at the named stages in turn,
/// half at random moments of a write (a random kill that lands between two
/// writes is checked too, but not counted).
fn run_kills(target: usize) {
    let seed = seed();
    println!("crash: {target} kills during checkpoints, seed {seed} (FX_CRASH_SEED)");
    let dir = TempDir::new();
    let path = dir.0.join(STORE);
    let prefix = temp_name_prefix(&path)
        .expect("temp prefix")
        .to_string_lossy()
        .into_owned();
    let mut rng = Rng::new(seed);
    let mut fingerprints: HashMap<u64, u64> = HashMap::new();
    // The generation the store held after the last kill.
    let mut stored: Option<u64> = None;
    // The temp files the last dead child left.
    let mut leftovers: Vec<String> = Vec::new();
    let mut landings: HashMap<Landing, usize> = HashMap::new();
    // How long one write takes, for the random delays.
    let mut write_time = Duration::from_millis(20);
    let start = Instant::now();
    let mut during = 0;
    let mut run = 0;

    while during < target {
        assert!(
            run < 3 * target,
            "only {during} of {run} kills landed during a checkpoint"
        );
        let stage = (run % 2 == 0).then(|| STAGES[(run / 2) % STAGES.len()]);
        let write = 1 + rng.below(3);
        let mut envs = vec![
            ("FX_CRASH_DIR", dir.0.to_string_lossy().into_owned()),
            ("FX_CRASH_SEED", seed.to_string()),
            ("FX_CRASH_ENTRIES", ENTRIES.to_string()),
        ];
        if let Some(stage) = stage {
            envs.push(("FX_CRASH_PAUSE", format!("{write}:{}", stage_name(stage))));
        }
        let mut child = Child::spawn(CHILD, envs).expect("start the child");
        let pid = child.id();
        let mut reports = Reports::default();

        // The start: it loads the store the last kill left, and removes
        // that child's temp files.
        while !reports.ready {
            let line = child
                .next_report(WAIT)
                .unwrap_or_else(|e| panic!("run {run}: the child did not start: {e}"));
            reports.take(&line, &mut fingerprints);
        }
        match (stored, &reports.opened) {
            (None, Some((0, Some(reason)))) => {
                assert_eq!(reason, "NotFound", "run {run}: the first start rebuilt");
            }
            (Some(g), Some((loaded, None))) => assert_eq!(
                *loaded, g,
                "run {run}: the start loaded another generation than the store held"
            ),
            (stored, opened) => panic!(
                "run {run}: the store held {stored:?}, the start reported {opened:?}: \
                 a start after a kill must load the store, never rebuild it"
            ),
        }
        assert_eq!(
            dir.names(),
            vec![STORE.to_string()],
            "run {run}: the start must remove the dead child's temp files {leftovers:?}"
        );
        let opened = reports.opened.as_ref().map(|(g, _)| *g).expect("opened");

        // The kill.
        match stage {
            Some(_) => {
                while reports.paused.is_none() {
                    let line = child
                        .next_report(WAIT)
                        .unwrap_or_else(|e| panic!("run {run}: no pause: {e}"));
                    reports.take(&line, &mut fingerprints);
                }
            }
            None => {
                // A random moment of this child's `write`-th write, or
                // shortly after it.
                while reports.begun < write {
                    let line = child
                        .next_report(WAIT)
                        .unwrap_or_else(|e| panic!("run {run}: no write: {e}"));
                    reports.take(&line, &mut fingerprints);
                }
                let span = (write_time * 5 / 4).as_micros().max(1) as u64;
                let began = reports.begun_at.expect("a write began");
                let until = began + Duration::from_micros(rng.below(span));
                loop {
                    let left = until.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        break;
                    }
                    match child.next_report(left) {
                        Ok(line) => reports.take(&line, &mut fingerprints),
                        Err(crash::ChildError::Timeout(_)) => break,
                        Err(e) => panic!("run {run}: the child stopped: {e}"),
                    }
                }
            }
        }
        let killed = child.kill9().expect("kill the child");
        for line in &killed.reports {
            reports.take(line, &mut fingerprints);
        }
        if !reports.writes.is_empty() {
            write_time = reports.writes.iter().sum::<Duration>() / reports.writes.len() as u32;
        }

        // What the kill left: the store, and the dead child's temp files.
        let names = dir.names();
        leftovers = names.iter().filter(|n| *n != STORE).cloned().collect();
        for name in &leftovers {
            let pid_part = name
                .strip_prefix(&prefix)
                .and_then(|rest| rest.split('.').next());
            assert_eq!(
                pid_part,
                Some(pid.to_string().as_str()),
                "run {run}: {name} is not a temp file of the dead child"
            );
        }
        assert!(
            names.iter().any(|n| n == STORE),
            "run {run}: the store is missing after the kill"
        );
        let store = MappedIndex::open(&path)
            .unwrap_or_else(|e| panic!("run {run}: the store does not open: {e}"));
        store
            .verify_checksums()
            .unwrap_or_else(|e| panic!("run {run}: the store fails its checksums: {e}"));
        store
            .to_index()
            .unwrap_or_else(|e| panic!("run {run}: the store's index is invalid: {e}"));
        let held = match store.cursor(VOL) {
            Some(ResumeCursor::FsEvents { event_id, .. }) => event_id,
            other => panic!("run {run}: the store's cursor is {other:?}"),
        };
        let last_done = reports.done.unwrap_or(opened);
        assert!(
            held == last_done || Some(held) == reports.in_flight,
            "run {run}: the store holds generation {held}; the last written was {last_done}, \
             the one in flight {:?}",
            reports.in_flight
        );
        assert_eq!(
            Some(file_fingerprint(&store)),
            fingerprints.get(&held).copied(),
            "run {run}: the store's contents are not generation {held}'s"
        );
        assert_eq!(
            store.clean_shutdown(),
            held % 3 == 0,
            "run {run}: clean flag"
        );
        drop(store);

        let landing = match reports.paused {
            Some((g, stage)) => {
                check_stage(run, &dir, &leftovers, g, stage, held, &fingerprints);
                Landing::Stage(stage)
            }
            None if Some(held) == reports.in_flight => Landing::AfterRename,
            None if !leftovers.is_empty() => Landing::MidWrite,
            None if reports.in_flight.is_some() => Landing::BeforeTemp,
            None => Landing::Between,
        };
        *landings.entry(landing).or_default() += 1;
        if landing != Landing::Between {
            during += 1;
        }
        stored = Some(held);
        run += 1;
    }

    // The last child's temp files go at the next start, as the others did.
    assert_eq!(
        cleanup_stale_temps(&path).expect("clean up"),
        leftovers.len(),
        "the last child's temp files {leftovers:?}"
    );
    assert_eq!(dir.names(), vec![STORE.to_string()]);
    let reopened = open_or_rebuild(&path, VerifyMode::Full, || {
        Err::<RebuiltIndex, _>(io::Error::other("a store left by a kill is never rebuilt"))
    })
    .expect("the store loads");
    assert!(matches!(reopened, Opened::Loaded(_)));

    let count = |l: Landing| landings.get(&l).copied().unwrap_or(0);
    let staged: usize = STAGES.iter().map(|&s| count(Landing::Stage(s))).sum();
    println!(
        "crash: {run} kills in {:.1?}, {during} during a checkpoint: {staged} at named \
         stages ({}); at random moments, {} \
         before the temp file, {} mid-write, {} after the rename, {} between writes; \
         one write took {write_time:.1?}; the store always loaded",
        start.elapsed(),
        STAGES
            .iter()
            .map(|&s| format!("{} {}", stage_name(s), count(Landing::Stage(s))))
            .collect::<Vec<_>>()
            .join(", "),
        count(Landing::BeforeTemp),
        count(Landing::MidWrite),
        count(Landing::AfterRename),
        count(Landing::Between),
    );
}

/// What a kill at a named stage of generation `g`'s write must leave.
fn check_stage(
    run: usize,
    dir: &TempDir,
    leftovers: &[String],
    g: u64,
    stage: WriteStage,
    held: u64,
    fingerprints: &HashMap<u64, u64>,
) {
    let temp = |what: &str| -> PathBuf {
        assert_eq!(
            leftovers.len(),
            1,
            "run {run}: at {} one temp file is left, {what}; found {leftovers:?}",
            stage_name(stage)
        );
        dir.0.join(&leftovers[0])
    };
    match stage {
        WriteStage::TempCreated => {
            assert_eq!(
                held,
                g - 1,
                "run {run}: before the rename the old store stays"
            );
            let len = fs::metadata(temp("empty")).expect("temp").len();
            assert_eq!(len, 0, "run {run}: the new temp file is empty");
        }
        WriteStage::Written | WriteStage::Synced => {
            assert_eq!(
                held,
                g - 1,
                "run {run}: before the rename the old store stays"
            );
            // Everything written before the kill is in the temp file: it
            // is the complete new generation.
            let complete = MappedIndex::open(temp("complete")).expect("the temp file opens");
            complete.verify_checksums().expect("the temp file verifies");
            assert_eq!(
                Some(file_fingerprint(&complete)),
                fingerprints.get(&g).copied(),
                "run {run}: the temp file holds generation {g}"
            );
        }
        WriteStage::Renamed | WriteStage::DirSynced => {
            assert_eq!(
                held, g,
                "run {run}: after the rename the new store is in place"
            );
            assert!(leftovers.is_empty(), "run {run}: no temp file is left");
        }
    }
}
