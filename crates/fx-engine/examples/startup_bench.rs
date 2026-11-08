//! `startup_bench`: store open to first query result (Phase 1 gate, goal 1).
//!
//! ```text
//! cargo run --release -p fx-engine --example startup_bench -- --entries 2000000 --runs 20 --cache warm
//! ```
//!
//! It generates a synthetic tree of N entries with fx-bench's generator,
//! saves it as an engine store (with a resume cursor, as a real checkpoint
//! has) and then, `--runs` times, measures two start paths from the file
//! alone, sharing nothing in memory between runs:
//!
//! - `engine`: `Engine::open` (store loaded, first snapshot published) and
//!   then the first result of `Engine::search`; `engine_open` is the open
//!   alone. Timed first in every run.
//! - `reader`: `StoreReader::open` (read-only, as the CLI does) to the
//!   first result of a query.
//!
//! `--cache cold` (after a reboot, with `--store-dir DIR --reuse` and a
//! store written before the reboot) times exactly one engine open to first
//! result, before anything else has read the store, and skips the reader
//! pass: any second read of the file would be page-cached. `--runs` is
//! then ignored (`runs=1`), and a cold run without a reusable store is
//! refused, since writing the store would cache it.
//!
//! Output: one `metric=value` per line (see the crate README), including
//! `startup_entries`, the gate line `GATE store open to first result p95 <=
//! 2 s` (and its 1 s stretch) for the engine path, and the reopened
//! snapshot's memory report as `memory_*` metrics with `GATE memory <= 100
//! B per entry`.
//! The full goal 1 (launch to first result painted) adds the app's launch
//! and first paint, measured by the fx-ui-macos gate probe.
//!
//! Exit codes: 0 after a completed run (gate results do not change it), 1
//! on an engine or I/O error, 2 for bad arguments. The benchmark writes
//! only its own store file, under `--store-dir` or a temporary directory
//! that it removes at the end.

mod bench_common;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use bench_common::{Flags, TempRoot, UsageError, gate, header, memory, metric, ms, stats};
use fx_bench::CacheState;
use fx_engine::{Engine, EngineConfig, ReadOptions, StoreReader};
use fx_fs::{ChangeSource, PathStat, VolumeEnumerator};
use fx_index::Index;
use fx_model::{
    Change, Entry, EntryFlags, EntryKind, FileKey, ResumeCursor, StartOutcome, VolumeId,
};
use fx_sort::{Direction, SortKey};

const USAGE: &str = "\
usage:
  startup_bench [--entries N] [--runs N] [--store-dir DIR] [--reuse]
                [--query TEXT] [--cache warm|cold|unknown]

  --entries N     synthetic entries in the store, 1..=50000000 (default 2000000)
  --runs N        timed opens per path, 1..=1000 (default 20; 1 when cold)
  --store-dir DIR keep the store in DIR (default: a temp dir, removed after)
  --reuse         reuse DIR's store for N entries if it exists
  --query TEXT    the first query (default: empty, every entry by name)
  --cache STATE   cache state (default unknown); cold times one engine open
                  before any other read of the store and skips the reader
                  pass; it needs --store-dir DIR --reuse and an existing store

Gate: store open to first result p95 <= 2 s (stretch 1 s).";

const GATE_P95: Duration = Duration::from_secs(2);
const STRETCH_P95: Duration = Duration::from_secs(1);
const SEED: u64 = 0x5354_4152;
/// The cursor saved with the store, handed back by the idle source.
const CURSOR: ResumeCursor = ResumeCursor::FsEvents {
    event_id: 1,
    db_uuid: [0x5a; 16],
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let parsed = Flags::parse(
        &args,
        &["--entries", "--runs", "--store-dir", "--query", "--cache"],
        &["--reuse"],
    )
    .and_then(|f| {
        Ok((
            f.number("--entries", 2_000_000, 1, 50_000_000)? as usize,
            f.number("--runs", 20, 1, 1000)? as usize,
            f.cache()?,
            f,
        ))
    });
    let (entries, runs, cache, flags) = match parsed {
        Ok(p) => p,
        Err(UsageError(msg)) => {
            eprintln!("startup_bench: {msg}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    header("startup", cache);
    match run(entries, runs, cache, &flags) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("startup_bench: error: {e}");
            ExitCode::from(1)
        }
    }
}

/// A change source with nothing to report: it resumes from any cursor.
struct IdleSource;

impl ChangeSource for IdleSource {
    fn start(&mut self, _from: Option<&ResumeCursor>) -> io::Result<StartOutcome> {
        Ok(StartOutcome::Resumed)
    }

    fn next_batch(&mut self, deadline: Instant) -> io::Result<Vec<Change>> {
        std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
        Ok(Vec::new())
    }

    fn cursor(&self) -> ResumeCursor {
        CURSOR
    }
}

/// An empty disk; never consulted, since the source reports no change.
struct NoDisk;

impl PathStat for NoDisk {
    fn stat(&self, _path: &Path) -> io::Result<Option<Entry>> {
        Ok(None)
    }
}

impl VolumeEnumerator for NoDisk {
    fn enumerate(&self, _root: &Path, _sink: &mut dyn FnMut(Entry)) -> io::Result<()> {
        Ok(())
    }
}

fn config(store: &Path) -> EngineConfig {
    EngineConfig::new("/fx-startup-bench")
        .with_store_path(store)
        .with_checkpoint_idle(Duration::from_secs(24 * 3600))
}

/// An index of `n` synthetic entries (fx-bench's tree generator).
fn synthetic_index(n: usize) -> io::Result<Index> {
    let tree = fx_bench::generate_tree(n, SEED, fx_bench::TreeShape::default());
    let key = |i: usize| FileKey::new(VolumeId(1), i as u64 + 1);
    let mut index = Index::new();
    for i in 0..tree.len() {
        let parent = tree.parent[i];
        let t = 1_700_000_000 + (i as i64 * 37) % 50_000_000;
        let dir = tree.is_dir[i];
        index
            .upsert(Entry {
                key: key(i),
                parent: (parent != u32::MAX).then(|| key(parent as usize)),
                name: tree.names.get(i).to_string(),
                kind: if dir { EntryKind::Dir } else { EntryKind::File },
                flags: EntryFlags::empty(),
                size: if dir {
                    0
                } else {
                    (i as u64 * 7919) % 10_000_000
                },
                modified: t,
                created: t - 1_000,
            })
            .map_err(|e| io::Error::other(format!("synthetic entry {i}: {e:?}")))?;
    }
    index.compact();
    Ok(index)
}

/// Writes the store: the engine over the synthetic index, checkpointed with
/// a resume cursor on an orderly close.
fn write_store(store: &Path, n: usize) -> io::Result<()> {
    let t = Instant::now();
    let index = synthetic_index(n)?;
    metric("generate_ms", ms(t.elapsed()));
    let t = Instant::now();
    let engine = Engine::start(
        index,
        config(store).with_resume_from(CURSOR),
        IdleSource,
        NoDisk,
        NoDisk,
    )?;
    engine
        .close()
        .map_err(|e| io::Error::other(format!("saving the store: {e:?}")))?;
    metric("store_write_ms", ms(t.elapsed()));
    Ok(())
}

/// Times `Engine::open` alone and `Engine::open` to the first result of
/// `query`; returns both, the hit count and the memory report.
fn engine_pass(
    store: &Path,
    query: &str,
) -> io::Result<(Duration, Duration, usize, fx_engine::MemoryReport)> {
    let t = Instant::now();
    let engine = Engine::open(config(store), IdleSource, NoDisk, NoDisk)?;
    let open = t.elapsed();
    let page = engine
        .search(
            query,
            fx_engine::QueryOptions::default(),
            SortKey::Name,
            Direction::Ascending,
            0,
            1,
        )
        .map_err(|_| io::Error::other("search cancelled"))?;
    let first = t.elapsed();
    if page.ids.is_empty() {
        return Err(io::Error::other(format!("no engine result for {query:?}")));
    }
    let report = engine.snapshot().memory_report();
    // Dropping stops the engine without rewriting the store.
    drop(engine);
    Ok((open, first, page.total, report))
}

/// Times `StoreReader::open` to the first result of `query`.
fn reader_pass(store: &Path, query: &str) -> io::Result<Duration> {
    let t = Instant::now();
    let reader = StoreReader::open(store)
        .map_err(|e| io::Error::other(format!("StoreReader::open: {e:?}")))?;
    let page = reader
        .search(
            query,
            &ReadOptions::default(),
            SortKey::Name,
            Direction::Ascending,
            0,
            1,
        )
        .map_err(|e| io::Error::other(format!("reader search: {e:?}")))?;
    let elapsed = t.elapsed();
    if page.ids.is_empty() {
        return Err(io::Error::other(format!("no result for {query:?}")));
    }
    Ok(elapsed)
}

fn run(entries: usize, runs: usize, cache: CacheState, flags: &Flags) -> io::Result<()> {
    let query = flags.get("--query").unwrap_or("");
    let cold = cache == CacheState::Cold;
    // Keep the temp dir alive for the whole run when no dir is given.
    let (_temp, dir): (Option<TempRoot>, PathBuf) = match flags.get("--store-dir") {
        Some(d) => {
            fs::create_dir_all(d)?;
            (None, PathBuf::from(d))
        }
        None => {
            let t = TempRoot::new("fx-startup-bench")?;
            let p = t.path().to_path_buf();
            (Some(t), p)
        }
    };
    let store = dir.join(format!("startup-{entries}.fxdb"));
    let reused = flags.has("--reuse") && store.exists();
    if cold && !reused {
        // Writing the store now would leave it in the page cache.
        return Err(io::Error::other(format!(
            "--cache cold needs a store written before the reboot: pass --store-dir DIR --reuse \
             with an existing {}",
            store.display()
        )));
    }
    if !reused {
        write_store(&store, entries)?;
    }
    // A cold run times only the first open after the reboot: every later
    // open (and any StoreReader pass) reads a page-cached file.
    let runs = if cold { 1 } else { runs };
    metric("entries", entries);
    metric("startup_entries", entries);
    metric("runs", runs);
    metric("reused_store", reused);
    metric("store_bytes", fs::metadata(&store)?.len());
    metric("query", format!("{query:?}"));

    let mut reader_first = Vec::with_capacity(runs);
    let mut engine_open = Vec::with_capacity(runs);
    let mut engine_first = Vec::with_capacity(runs);
    let mut hits = 0;
    let mut report = None;
    for _ in 0..runs {
        // The engine path goes first, so in a cold run nothing has read the
        // store file before `Engine::open`.
        let (open, first, total, r) = engine_pass(&store, query)?;
        engine_open.push(open);
        engine_first.push(first);
        hits = total;
        report = Some(r);
        if !cold {
            reader_first.push(reader_pass(&store, query)?);
        }
    }
    metric("query_total", hits);
    if let Some(report) = &report {
        memory(report);
    }
    if cold {
        metric(
            "reader_pass",
            "skipped (cold cache: it would warm the page cache)",
        );
    } else {
        stats("reader_open_to_first_result", &reader_first);
    }
    stats("engine_open", &engine_open);
    let s = stats("engine_open_to_first_result", &engine_first)
        .ok_or_else(|| io::Error::other("no samples"))?;
    let detail = if cold {
        format!("cold: first open after a reboot, {} ms", ms(s.p95))
    } else {
        format!("{cache}: p95 {} ms over {} runs", ms(s.p95), s.count)
    };
    gate(
        "store open to first result p95 <= 2 s",
        s.p95 <= GATE_P95,
        &detail,
    );
    gate(
        "stretch store open to first result p95 <= 1 s",
        s.p95 <= STRETCH_P95,
        &detail,
    );
    Ok(())
}
