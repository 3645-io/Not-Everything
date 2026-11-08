//! `positions_bench`: how long a window takes to find its selected files
//! after a refresh (`Engine::positions`).
//!
//! ```text
//! cargo run --release -p fx-engine --example positions_bench -- --entries 2700000 --selected 1000 --runs 20 --cache warm
//! ```
//!
//! It indexes N synthetic entries (fx-bench's tree generator) in an engine
//! with no change source, runs `--query` once in Name order (as the
//! window's worker does before the lookup, so the query is in the
//! refinement cache), picks `--selected` results spread evenly over it,
//! from the first row to the last (so the walk covers the whole order: the
//! worst case), and times `Engine::positions` for their paths, ascending
//! and descending. Every answer is checked against the rows the files were
//! picked from. It also times the path lookups alone (`find_path`) and one
//! file on the first row (the walk stops at once).
//!
//! Output: one `metric=value` per line (see the crate README), with
//! `positions_entries`, `positions_selected`, p50/p95/max per case and the
//! line `GATE selection lookup p95 <= 20 ms` (the target for keeping the
//! selection, judged on the slower direction).
//!
//! Exit codes: 0 after a completed run (the target does not change it), 1
//! on an engine error or a wrong answer, 2 for bad arguments. Nothing is
//! written to disk.

mod bench_common;

use std::io;
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use bench_common::{Flags, UsageError, gate, header, metric, ms, stats};
use fx_engine::{Engine, EngineConfig};
use fx_fs::{ChangeSource, PathStat, VolumeEnumerator};
use fx_index::Index;
use fx_model::{
    Change, Entry, EntryFlags, EntryId, EntryKind, FileKey, ResumeCursor, StartOutcome, VolumeId,
};
use fx_sort::{Direction, SortKey};

const USAGE: &str = "\
usage:
  positions_bench [--entries N] [--selected N] [--runs N] [--query TEXT]
                  [--cache warm|cold|unknown]

  --entries N     synthetic entries, 1..=50000000 (default 2700000)
  --selected N    selected files to find, 1..=100000 (default 1000)
  --runs N        timed lookups per case, 1..=1000 (default 20)
  --query TEXT    the query (default: empty, every entry)
  --cache STATE   cache label (default unknown)

Target: selection lookup p95 <= 20 ms.";

const TARGET_P95: Duration = Duration::from_millis(20);
const SEED: u64 = 0x504f_5349;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let parsed = Flags::parse(
        &args,
        &["--entries", "--selected", "--runs", "--query", "--cache"],
        &[],
    )
    .and_then(|f| {
        Ok((
            f.number("--entries", 2_700_000, 1, 50_000_000)? as usize,
            f.number("--selected", 1_000, 1, 100_000)? as usize,
            f.number("--runs", 20, 1, 1000)? as usize,
            f.cache()?,
            f,
        ))
    });
    let (entries, selected, runs, cache, flags) = match parsed {
        Ok(p) => p,
        Err(UsageError(msg)) => {
            eprintln!("positions_bench: {msg}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    header("positions", cache);
    let query = flags.get("--query").unwrap_or("");
    match run(entries, selected, runs, query) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("positions_bench: error: {e}");
            ExitCode::from(1)
        }
    }
}

/// A change source with nothing to report.
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
        ResumeCursor::None { snapshot_epoch: 0 }
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

fn cancelled<T>(_: T) -> io::Error {
    io::Error::other("lookup cancelled")
}

/// Times `runs` lookups of `paths` and checks every answer is `want`.
fn time_positions(
    engine: &Engine,
    query: &str,
    direction: Direction,
    paths: &[&str],
    want: &[Option<usize>],
    runs: usize,
) -> io::Result<Vec<Duration>> {
    let mut samples = Vec::with_capacity(runs);
    for _ in 0..runs {
        let t = Instant::now();
        let found = engine
            .positions(
                query,
                fx_engine::QueryOptions::default(),
                SortKey::Name,
                direction,
                paths,
            )
            .map_err(cancelled)?;
        samples.push(t.elapsed());
        if found.rows != want {
            return Err(io::Error::other(format!(
                "wrong rows for {direction:?}: the lookup disagrees with the search"
            )));
        }
    }
    Ok(samples)
}

fn run(entries: usize, selected: usize, runs: usize, query: &str) -> io::Result<()> {
    let t = Instant::now();
    let index = synthetic_index(entries)?;
    metric("generate_ms", ms(t.elapsed()));
    let engine = Engine::start(
        index,
        EngineConfig::new("/fx-positions-bench"),
        IdleSource,
        NoDisk,
        NoDisk,
    )?;
    let snap = engine.snapshot();

    // The window's worker has just run this search when it asks for the
    // selection's rows, so the query is in the refinement cache.
    let t = Instant::now();
    let all = engine
        .search(
            query,
            fx_engine::QueryOptions::default(),
            SortKey::Name,
            Direction::Ascending,
            0,
            usize::MAX,
        )
        .map_err(cancelled)?;
    metric("search_all_ms", ms(t.elapsed()));
    let total = all.total;
    if total == 0 {
        return Err(io::Error::other(format!("no results for {query:?}")));
    }
    let k = selected.min(total);
    let rows: Vec<usize> = if k == 1 {
        vec![total - 1]
    } else {
        (0..k).map(|i| i * (total - 1) / (k - 1)).collect()
    };
    let paths: Vec<String> = rows
        .iter()
        .map(|&r| snap.index().path(EntryId(all.ids[r])))
        .collect();
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();

    metric("entries", entries);
    metric("positions_entries", entries);
    metric("positions_selected", k);
    metric("query", format!("{query:?}"));
    metric("query_total", total);
    metric("runs", runs);

    let mut finds = Vec::with_capacity(runs);
    for _ in 0..runs {
        let t = Instant::now();
        let found = refs.iter().filter(|p| snap.find_path(p).is_some()).count();
        finds.push(t.elapsed());
        if found != k {
            return Err(io::Error::other("a selected path was not found"));
        }
    }
    stats("find_path_all", &finds);

    let ascending: Vec<Option<usize>> = rows.iter().map(|&r| Some(r)).collect();
    let descending: Vec<Option<usize>> = rows.iter().map(|&r| Some(total - 1 - r)).collect();
    let asc = time_positions(
        &engine,
        query,
        Direction::Ascending,
        &refs,
        &ascending,
        runs,
    )?;
    let desc = time_positions(
        &engine,
        query,
        Direction::Descending,
        &refs,
        &descending,
        runs,
    )?;
    let asc = stats("positions_ascending", &asc);
    let desc = stats("positions_descending", &desc);

    let first = snap.index().path(EntryId(all.ids[0]));
    let top = time_positions(
        &engine,
        query,
        Direction::Ascending,
        &[first.as_str()],
        &[Some(0)],
        runs,
    )?;
    stats("positions_one_top", &top);

    let worst = asc
        .iter()
        .chain(desc.iter())
        .map(|s| s.p95)
        .max()
        .ok_or_else(|| io::Error::other("no samples"))?;
    gate(
        "selection lookup p95 <= 20 ms",
        worst <= TARGET_P95,
        &format!(
            "{} ms for {k} files over {total} results of {entries} entries",
            ms(worst)
        ),
    );
    engine.shutdown();
    Ok(())
}
