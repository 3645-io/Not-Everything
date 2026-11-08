//! `catchup_bench`: offline catch-up after a restart (Phase 1 gate).
//!
//! ```text
//! cargo run --release -p fx-engine --example catchup_bench -- --changes 10000 --cache warm
//! ```
//!
//! 1. Builds a tree of `--entries` files (folders of 100 files), indexes it
//!    with the engine over a missing store (full scan), applies one live
//!    change so a resume cursor is published, and closes the engine, which
//!    checkpoints the store with that cursor.
//! 2. With the engine stopped, makes `--changes` scripted changes: per ten,
//!    3 creates, 2 deletes, 2 renames across folders, 2 size changes and 1
//!    new folder.
//! 3. Reopens the engine from the store and the saved cursor and times
//!    `search_usable` (open to the first result of a query for an untouched
//!    file) and `equal` (open to the first published snapshot equal to a
//!    fresh scan of the tree). Equality is required: without it the run
//!    fails with exit code 1.
//!
//! Sources: `--source fsevents` (macOS only, the default there) uses a real
//! temporary directory, the FSEvents source, `MacPathStat` and `MacScanner`;
//! `--source fake` (the default elsewhere) uses the in-memory fakes of the
//! integration tests (`tests/common/mod.rs`), whose source replays the
//! scripted change history.
//!
//! Output: `metric=value` lines (see the crate README) and the gate lines
//! `GATE catch-up search usable <= 2 s` and `GATE catch-up equal to a fresh
//! scan <= 10 s`. Exit codes: 0 after a run whose index equals the fresh
//! scan, 1 on an error or a mismatch, 2 for bad arguments. Only its own
//! temporary directories are written, and they are removed at the end.

#[path = "../tests/common/mod.rs"]
mod common;

mod bench_common;

#[cfg(target_os = "macos")]
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bench_common::{Flags, TempRoot, UsageError, gate, header, metric, ms};
use common::{FakeSource, FakeTree, ROOT, check, snapshot_entries};
use fx_engine::{Engine, EngineConfig, EngineSnapshot};
use fx_model::{Change, StartOutcome};
#[cfg(target_os = "macos")]
use fx_model::{Entry, EntryKind, FileKey};
use fx_sort::{Direction, SortKey};

const USAGE: &str = "\
usage:
  catchup_bench [--entries N] [--changes N] [--source fsevents|fake]
                [--cache warm|cold|unknown]

  --entries N   files in the initial tree, folders of 100, 1000..=10000000
                (default 50000)
  --changes N   scripted offline changes, 10..=1000000 (default 10000)
  --source S    fsevents (macOS only, default there) or fake (default elsewhere)
  --cache STATE cache state label for the report (default unknown)

Gates: search usable <= 2 s; equal to a fresh scan <= 10 s.";

const GATE_USABLE: Duration = Duration::from_secs(2);
const GATE_EQUAL: Duration = Duration::from_secs(10);
/// How long to wait for equality before failing the run.
const TIMEOUT: Duration = Duration::from_secs(300);
const FILES_PER_DIR: usize = 100;
/// File slots per folder: deletes take `f0..f39`, renames `f40..f79`, size
/// changes `f80..f99`; `f99` is never deleted or renamed.
const DELETE_SLOTS: usize = 40;
const RENAME_BASE: usize = 40;
const SIZE_BASE: usize = 80;

/// The relative path, kind and size of every entry below the root.
#[cfg(target_os = "macos")]
type Listing = BTreeMap<String, (FileKey, EntryKind, u64)>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let parsed = Flags::parse(
        &args,
        &["--entries", "--changes", "--source", "--cache"],
        &[],
    )
    .and_then(|f| {
        let entries = f.number("--entries", 50_000, 1000, 10_000_000)? as usize;
        let changes = f.number("--changes", 10_000, 10, 1_000_000)? as usize;
        let dirs = entries.div_ceil(FILES_PER_DIR);
        if changes.div_ceil(10) * 2 > dirs * DELETE_SLOTS {
            return Err(UsageError(format!(
                "--changes {changes} needs more files than --entries {entries} provide"
            )));
        }
        let fsevents = match f.get("--source") {
            None => cfg!(target_os = "macos"),
            Some("fake") => false,
            Some("fsevents") if cfg!(target_os = "macos") => true,
            Some(other) => {
                return Err(UsageError(format!(
                    "--source expects fake{}, got '{other}'",
                    if cfg!(target_os = "macos") {
                        " or fsevents"
                    } else {
                        " on this OS"
                    }
                )));
            }
        };
        Ok((dirs, changes, fsevents, f.cache()?))
    });
    let (dirs, changes, fsevents, cache) = match parsed {
        Ok(p) => p,
        Err(UsageError(msg)) => {
            eprintln!("catchup_bench: {msg}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    header("catchup", cache);
    metric("source", if fsevents { "fsevents" } else { "fake" });
    let result = if fsevents {
        run_fsevents(dirs, changes)
    } else {
        run_fake(dirs, changes)
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => {
            eprintln!("catchup_bench: the caught-up index does not equal a fresh scan");
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("catchup_bench: error: {e}");
            ExitCode::from(1)
        }
    }
}

/// The scripted file operations, on a fake tree or a real directory.
trait Disk {
    fn mkdir(&mut self, rel: &str) -> io::Result<()>;
    /// Creates the file, or sets the size of an existing one.
    fn write(&mut self, rel: &str, size: u64, new: bool) -> io::Result<()>;
    fn remove(&mut self, rel: &str) -> io::Result<()>;
    fn rename(&mut self, from: &str, to: &str) -> io::Result<()>;
}

/// The initial tree: `dirs` folders `d<k>` of files `f0.dat..f99.dat`.
fn build_tree(disk: &mut dyn Disk, dirs: usize) -> io::Result<()> {
    for d in 0..dirs {
        disk.mkdir(&format!("d{d}"))?;
        for f in 0..FILES_PER_DIR {
            disk.write(&format!("d{d}/f{f}.dat"), (d * 131 + f) as u64 % 4096, true)?;
        }
    }
    Ok(())
}

/// The offline change script; returns the number of operations.
fn offline_changes(disk: &mut dyn Disk, dirs: usize, changes: usize) -> io::Result<usize> {
    let (mut deletes, mut renames, mut sizes) = (0usize, 0usize, 0usize);
    for i in 0..changes {
        match i % 10 {
            0..=2 => disk.write(
                &format!("d{}/new{i}.txt", i % dirs),
                i as u64 % 4096 + 1,
                true,
            )?,
            3 | 4 => {
                let k = deletes;
                deletes += 1;
                disk.remove(&format!("d{}/f{}.dat", k % dirs, k / dirs))?;
            }
            5 | 6 => {
                let k = renames;
                renames += 1;
                // As many renames as deletes: `main` checks that both fit
                // in their 40 slots per folder.
                let from = format!("d{}/f{}.dat", k % dirs, RENAME_BASE + k / dirs);
                disk.rename(&from, &format!("d{}/ren{i}.dat", (k + 1) % dirs))?;
            }
            7 | 8 => {
                let k = sizes;
                sizes += 1;
                let f = SIZE_BASE + (k / dirs) % (FILES_PER_DIR - SIZE_BASE);
                disk.write(
                    &format!("d{}/f{f}.dat", k % dirs),
                    100_000 + i as u64,
                    false,
                )?;
            }
            _ => disk.mkdir(&format!("d{}/sub{i}", i % dirs))?,
        }
    }
    Ok(changes)
}

/// Opens the engine (`open`), then times search usable and the first
/// snapshot for which `equal` (equal to a fresh scan) holds, and prints the
/// metrics and gates. Returns whether equality was reached.
fn measure(
    open: impl FnOnce() -> io::Result<Engine>,
    equal: impl Fn(&EngineSnapshot) -> bool,
    ops: usize,
) -> io::Result<bool> {
    let t = Instant::now();
    let engine = open()?;
    let opened = t.elapsed();
    let usable = loop {
        let page = engine
            .search(
                "f99.dat",
                fx_engine::QueryOptions::default(),
                SortKey::Name,
                Direction::Ascending,
                0,
                1,
            )
            .map_err(|_| io::Error::other("search cancelled"))?;
        if !page.ids.is_empty() {
            break Some(t.elapsed());
        }
        if t.elapsed() > TIMEOUT {
            break None;
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    let equal_at = loop {
        let snap: Arc<EngineSnapshot> = engine.snapshot();
        if equal(&snap) {
            break Some((t.elapsed(), snap.generation()));
        }
        if t.elapsed() > TIMEOUT {
            break None;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let live = engine.snapshot().index().live_count();
    engine.shutdown();
    metric("changes", ops);
    metric("catchup_changes", ops);
    metric("live_entries", live);
    metric("open_ms", ms(opened));
    let usable_detail = match usable {
        Some(d) => {
            metric("search_usable_ms", ms(d));
            format!("{} ms", ms(d))
        }
        None => "never".to_string(),
    };
    let equal_detail = match equal_at {
        Some((d, g)) => {
            metric("equal_ms", ms(d));
            metric("equal_generation", g);
            format!("{} ms", ms(d))
        }
        None => format!("not equal after {} s", TIMEOUT.as_secs()),
    };
    metric("equal_to_fresh_scan", equal_at.is_some());
    gate(
        "catch-up search usable <= 2 s",
        usable.is_some_and(|d| d <= GATE_USABLE),
        &usable_detail,
    );
    gate(
        "catch-up equal to a fresh scan <= 10 s",
        equal_at.is_some_and(|(d, _)| d <= GATE_EQUAL),
        &equal_detail,
    );
    Ok(equal_at.is_some())
}

/// The fake tree plus the change history a source would report.
struct FakeDisk {
    tree: FakeTree,
    history: Vec<Change>,
}

impl FakeDisk {
    fn abs(rel: &str) -> String {
        format!("{ROOT}/{rel}")
    }
}

impl Disk for FakeDisk {
    fn mkdir(&mut self, rel: &str) -> io::Result<()> {
        let p = Self::abs(rel);
        self.tree.mkdir(&p);
        self.history.push(check(&p));
        Ok(())
    }

    fn write(&mut self, rel: &str, size: u64, new: bool) -> io::Result<()> {
        let p = Self::abs(rel);
        if new {
            self.tree.touch(&p, size);
        } else {
            self.tree.set_size(&p, size);
        }
        self.history.push(check(&p));
        Ok(())
    }

    fn remove(&mut self, rel: &str) -> io::Result<()> {
        let p = Self::abs(rel);
        self.tree.remove(&p);
        self.history.push(check(&p));
        Ok(())
    }

    fn rename(&mut self, from: &str, to: &str) -> io::Result<()> {
        let (f, t) = (Self::abs(from), Self::abs(to));
        self.tree.rename(&f, &t);
        self.history.push(check(&f));
        self.history.push(check(&t));
        Ok(())
    }
}

fn wait(what: &str, mut done: impl FnMut() -> bool) -> io::Result<()> {
    let end = Instant::now() + TIMEOUT;
    while !done() {
        if Instant::now() > end {
            return Err(io::Error::other(format!("timed out waiting for {what}")));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

fn run_fake(dirs: usize, changes: usize) -> io::Result<bool> {
    let store_dir = TempRoot::new("fx-catchup-bench-store")?;
    let store = store_dir.path().join("index.fxdb");
    let config = || {
        EngineConfig::new(ROOT)
            .with_batch_window(Duration::from_millis(50))
            .with_store_path(&store)
            .with_checkpoint_idle(Duration::from_secs(24 * 3600))
    };
    let mut disk = FakeDisk {
        tree: FakeTree::new(),
        history: Vec::new(),
    };
    build_tree(&mut disk, dirs)?;
    let tree = disk.tree.clone();

    // Index, publish a cursor with one live change, checkpoint on close.
    let source = FakeSource::new(StartOutcome::Resumed);
    let t = Instant::now();
    let engine = Engine::open(config(), source.clone(), tree.clone(), tree.clone())?;
    let want = tree.entries();
    wait("the first full scan", || {
        snapshot_entries(&engine.snapshot()) == want
    })?;
    metric("initial_entries", want.len());
    metric("initial_scan_ms", ms(t.elapsed()));
    tree.touch(&FakeDisk::abs("live.txt"), 7);
    source.push(vec![check(&FakeDisk::abs("live.txt"))]);
    wait("the cursor of the live change", || {
        engine.snapshot().cursor() == Some(source.current_cursor())
    })?;
    engine
        .close()
        .map_err(|e| io::Error::other(format!("checkpoint on close: {e:?}")))?;

    // Engine stopped: the scripted changes and their history.
    disk.history.clear();
    let ops = offline_changes(&mut disk, dirs, changes)?;
    let source = FakeSource::new(StartOutcome::Resumed);
    for batch in disk.history.chunks(100) {
        source.push(batch.to_vec());
    }
    let want = tree.entries();
    let ok = measure(
        || Engine::open(config(), source.clone(), tree.clone(), tree.clone()),
        |snap| snapshot_entries(snap) == want,
        ops,
    )?;
    Ok(ok && source.pending() == 0)
}

/// Relative paths of `entries` below the root: entries without a parent
/// (the root) are dropped and walks stop at a parent not in the map.
#[cfg(target_os = "macos")]
fn listing(entries: &HashMap<FileKey, Entry>) -> Listing {
    let mut out = Listing::new();
    for e in entries.values().filter(|e| e.parent.is_some()) {
        let mut parts = vec![e.name.as_str()];
        let mut parent = e.parent;
        while let Some(p) = parent.and_then(|k| entries.get(&k)) {
            if p.parent.is_none() || parts.len() > 4096 {
                break;
            }
            parts.push(p.name.as_str());
            parent = p.parent;
        }
        parts.reverse();
        let size = if e.kind == EntryKind::File { e.size } else { 0 };
        out.insert(parts.join("/"), (e.key, e.kind, size));
    }
    out
}

#[cfg(not(target_os = "macos"))]
fn run_fsevents(_dirs: usize, _changes: usize) -> io::Result<bool> {
    Err(io::Error::other("the fsevents source needs macOS"))
}

#[cfg(target_os = "macos")]
fn run_fsevents(dirs: usize, changes: usize) -> io::Result<bool> {
    use std::fs;
    use std::path::{Path, PathBuf};

    use fx_fs::VolumeEnumerator;
    use fx_fs_macos::enumerator::MacScanner;
    use fx_fs_macos::fsevents::source::FsEventsSource;
    use fx_fs_macos::fsevents::stream::StreamConfig;
    use fx_fs_macos::path_stat::MacPathStat;
    use fx_fs_macos::scan::ScanOptions;

    struct RealDisk(PathBuf);

    impl Disk for RealDisk {
        fn mkdir(&mut self, rel: &str) -> io::Result<()> {
            fs::create_dir(self.0.join(rel))
        }

        fn write(&mut self, rel: &str, size: u64, _new: bool) -> io::Result<()> {
            fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(self.0.join(rel))?
                .set_len(size)
        }

        fn remove(&mut self, rel: &str) -> io::Result<()> {
            fs::remove_file(self.0.join(rel))
        }

        fn rename(&mut self, from: &str, to: &str) -> io::Result<()> {
            fs::rename(self.0.join(from), self.0.join(to))
        }
    }

    fn fresh_scan(root: &Path) -> io::Result<Listing> {
        let mut entries = HashMap::new();
        MacScanner::new(ScanOptions::default())
            .enumerate(root, &mut |e: Entry| {
                entries.insert(e.key, e);
            })
            .map_err(|e| io::Error::other(format!("fresh scan: {e:?}")))?;
        Ok(listing(&entries))
    }

    let tree_dir = TempRoot::new("fx-catchup-bench-tree")?;
    let store_dir = TempRoot::new("fx-catchup-bench-store")?;
    let root = tree_dir.path().to_path_buf();
    let store = store_dir.path().join("index.fxdb");
    let open = || {
        let mut stream = StreamConfig::new(root.clone());
        stream.path_filter = Some(root.clone());
        stream.no_defer = true;
        let config = EngineConfig::new(&root)
            .with_store_path(&store)
            .with_checkpoint_idle(Duration::from_secs(24 * 3600));
        Engine::open(
            config,
            FsEventsSource::new(stream),
            MacPathStat::new(),
            MacScanner::new(ScanOptions::default()),
        )
    };
    let mut disk = RealDisk(root.clone());
    build_tree(&mut disk, dirs)?;

    let t = Instant::now();
    let engine = open()?;
    let want = fresh_scan(&root)?;
    wait("the first full scan", || {
        listing(&snapshot_entries(&engine.snapshot())) == want
    })?;
    metric("initial_entries", want.len());
    metric("initial_scan_ms", ms(t.elapsed()));
    // One live change, so the checkpoint holds a cursor past the scan.
    let before = engine.snapshot().cursor();
    disk.write("live.txt", 7, true)?;
    wait("the live change", || {
        let snap = engine.snapshot();
        snap.cursor() != before && listing(&snapshot_entries(&snap)).contains_key("live.txt")
    })?;
    engine
        .close()
        .map_err(|e| io::Error::other(format!("checkpoint on close: {e:?}")))?;

    let ops = offline_changes(&mut disk, dirs, changes)?;
    let want = fresh_scan(&root)?;
    let ok = measure(open, |snap| listing(&snapshot_entries(snap)) == want, ops)?;
    // The disk did not change during the catch-up: re-check the scan.
    Ok(ok && fresh_scan(&root)? == want)
}
