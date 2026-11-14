//! `rerun_bench`: what one search re-run costs over the engine's own
//! snapshot, with a delta at the compaction limit (run 2.16).
//!
//! ```text
//! cargo run --release -p fx-engine --example rerun_bench -- --entries 2000000 --cache warm
//! cargo run --release -p fx-engine --example rerun_bench -- --entries 2000000 --delta 0
//! ```
//!
//! A window re-runs its search for every new generation, and a new
//! generation is a new snapshot, so the refinement cache can't answer it:
//! every re-run tests every entry. fx-query's `matcher_bench` measures that
//! scan over its own `VecView`, which keeps a folded copy of every name and
//! has no delta; this benchmark measures it over what the engine publishes
//! ([`fx_engine::EngineSnapshot`]), whose rows come from fx-index's base
//! columns and its delta.
//!
//! Setup, outside any timing: fx-bench's tree generator builds `--entries`
//! entries, which go into an [`Index`] and are compacted into its base. The
//! base is saved as an engine store in a temporary directory and served
//! from the file's memory map, as the app serves it after a launch or a
//! compaction (`--heap-base` keeps it on the heap instead). Then a delta of `--delta` percent of the live entries is applied, half
//! new files (a copy of an existing file's name in an existing folder) and
//! half updates of base entries (every other one renamed, the others with a
//! new modification time), as the writer leaves it just before its next
//! compaction (`--delta 10`, the default, is the compaction limit). The
//! engine is started over that index with a change source that reports
//! nothing, and its first published snapshot is searched.
//!
//! Each sample is one re-run: a fresh [`Searcher`] (no cache, as for a new
//! generation) searching the snapshot for one query with the default
//! options (case and diacritics folded), on rayon's global pool, as the
//! engine searches. The queries are `matcher_bench`'s engine group: 1-4
//! character literals, a rare literal, an AND and `*.png`.
//!
//! Output: `metric=value` lines (see the crate README): the setup (base
//! entries, delta rows, the share of names that folding changes and the
//! bytes a folded copy of them would take), `rerun_<label>_p50_ms`,
//! `_p95_ms`, `_max_ms` and `_n` per query, `rerun_worst_p95_ms` and
//! `rerun_mean_p50_ms` over the queries, and the snapshot's memory report
//! as `memory_*` lines.
//!
//! Exit codes: 0 after a completed run, 1 on an engine error or a query
//! whose hit count changes between runs, 2 for bad arguments.

mod bench_common;

use std::io;
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use bench_common::{Flags, TempRoot, UsageError, header, metric, ms, stats};
use fx_bench::{CacheState, Rng, TreeShape, generate_tree};
use fx_engine::{Engine, EngineConfig};
use fx_fs::{ChangeSource, PathStat, VolumeEnumerator};
use fx_index::Index;
use fx_model::{
    Change, Entry, EntryFlags, EntryKind, FileKey, ResumeCursor, StartOutcome, VolumeId,
};
use fx_query::eval::EvalContext;
use fx_query::{CancelToken, FoldOptions, SearchOptions, Searcher, fold_str};

const USAGE: &str = "\
usage:
  rerun_bench [--entries N] [--delta PCT] [--iters K] [--warmup W]
              [--seed S] [--cache warm|cold|unknown] [--heap-base]

  --entries N   synthetic entries in the base, 1000..=50000000 (default 2000000)
  --delta PCT   delta rows as a percentage of the live entries, 0..=10
                (default 10, the writer's compaction limit)
  --iters K     timed re-runs per query, 1..=10000 (default 30)
  --warmup W    untimed re-runs per query first, 0..=1000 (default 3)
  --seed S      generator seed (default 1)
  --cache STATE cache state label for the report (default unknown)
  --heap-base   keep the base columns on the heap instead of serving them
                from a saved store's memory map";

/// The queries: label and text (`matcher_bench`'s engine group).
const QUERIES: &[(&str, &str)] = &[
    ("1-char", "a"),
    ("2-char", "re"),
    ("3-char", "por"),
    ("4-char", "conf"),
    ("rare", "zqxj"),
    ("and", "data json"),
    ("ext", "*.png"),
];

/// Seed offset for the delta's choices.
const DELTA_SEED_OFFSET: u64 = 0x6465_6c74_6173_6565;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let parsed = Flags::parse(
        &args,
        &[
            "--entries",
            "--delta",
            "--iters",
            "--warmup",
            "--seed",
            "--cache",
        ],
        &["--heap-base"],
    )
    .and_then(|f| {
        Ok(Args {
            entries: f.number("--entries", 2_000_000, 1000, 50_000_000)? as usize,
            delta_pct: f.number("--delta", 10, 0, 10)? as usize,
            iters: f.number("--iters", 30, 1, 10_000)? as usize,
            warmup: f.number("--warmup", 3, 0, 1000)? as usize,
            seed: f.number("--seed", 1, 0, u64::MAX)?,
            cache: f.cache()?,
            heap_base: f.has("--heap-base"),
        })
    });
    let args = match parsed {
        Ok(a) => a,
        Err(UsageError(msg)) => {
            eprintln!("rerun_bench: {msg}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    header("rerun", args.cache);
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rerun_bench: error: {e}");
            ExitCode::from(1)
        }
    }
}

struct Args {
    entries: usize,
    delta_pct: usize,
    iters: usize,
    warmup: usize,
    seed: u64,
    cache: CacheState,
    heap_base: bool,
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
        ResumeCursor::FsEvents {
            event_id: 1,
            db_uuid: [0x5a; 16],
        }
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

fn key(i: usize) -> FileKey {
    FileKey::new(VolumeId(1), i as u64 + 1)
}

fn entry(
    i: usize,
    parent: Option<usize>,
    name: String,
    dir: bool,
    modified: i64,
    size: u64,
) -> Entry {
    Entry {
        key: key(i),
        parent: parent.map(key),
        name,
        kind: if dir { EntryKind::Dir } else { EntryKind::File },
        flags: EntryFlags::empty(),
        size: if dir { 0 } else { size },
        modified,
        created: modified - 1_000,
    }
}

/// What the setup built.
struct Setup {
    index: Index,
    /// The saved base's directory, removed when the run ends.
    _store_dir: Option<TempRoot>,
    base: usize,
    new_rows: usize,
    overrides: usize,
    renamed: usize,
    /// Base names that folding (case, diacritics, apostrophes kept)
    /// changes, and their folded bytes plus a NUL each.
    folded_names: usize,
    folded_bytes: usize,
}

/// The synthetic base, compacted, then the delta.
fn build(args: &Args) -> io::Result<Setup> {
    let tree = generate_tree(args.entries, args.seed, TreeShape::default());
    let n = tree.len();
    let time = |i: usize| 1_700_000_000 + (i as i64 * 37) % 50_000_000;
    let size = |i: usize| (i as u64 * 7919) % 10_000_000;
    let parent_of = |i: usize| (tree.parent[i] != u32::MAX).then(|| tree.parent[i] as usize);
    let mut index = Index::new();
    for i in 0..n {
        let e = entry(
            i,
            parent_of(i),
            tree.names.get(i).to_string(),
            tree.is_dir[i],
            time(i),
            size(i),
        );
        index
            .upsert(e)
            .map_err(|e| io::Error::other(format!("synthetic entry {i}: {e:?}")))?;
    }
    index.compact();
    // Kept until the end: the mapped base reads the file.
    let mut store_dir = None;
    if !args.heap_base {
        let dir = TempRoot::new("fx-rerun-bench")?;
        let store = dir.path().join("rerun.fxdb");
        let engine = Engine::start(
            index,
            EngineConfig::new("/fx-rerun-bench").with_store_path(&store),
            IdleSource,
            NoDisk,
            NoDisk,
        )?;
        engine
            .close()
            .map_err(|e| io::Error::other(format!("saving the store: {e:?}")))?;
        let mapped = fx_store::MappedIndex::open(&store)
            .map_err(|e| io::Error::other(format!("mapping the store: {e:?}")))?;
        index = mapped
            .share_index()
            .map_err(|e| io::Error::other(format!("serving the store: {e:?}")))?;
        store_dir = Some(dir);
    }
    index.set_compact_fraction(f64::INFINITY);

    let fold = FoldOptions::for_match(false, false);
    let (mut folded_names, mut folded_bytes) = (0, 0);
    for i in 0..n {
        if let std::borrow::Cow::Owned(f) = fold_str(tree.names.get(i), fold) {
            folded_names += 1;
            folded_bytes += f.len() + 1;
        }
    }

    // Half the delta new files, half updates of base entries, so that the
    // delta is `delta_pct` percent of the live entries afterwards.
    let new_rows = n * args.delta_pct / 200;
    let delta = (n + new_rows) * args.delta_pct / 100;
    let overrides = delta - new_rows;
    let mut rng = Rng::new(args.seed.wrapping_add(DELTA_SEED_OFFSET));
    let folders: Vec<usize> = (0..n).filter(|&i| tree.is_dir[i]).collect();
    let files: Vec<usize> = (0..n).filter(|&i| !tree.is_dir[i]).collect();
    for j in 0..new_rows {
        let parent = folders[rng.below(folders.len() as u64) as usize];
        let src = files[rng.below(files.len() as u64) as usize];
        let name = format!("{} copy", tree.names.get(src));
        index
            .upsert(entry(
                n + j,
                Some(parent),
                name,
                false,
                time(src) + 7,
                size(src),
            ))
            .map_err(|e| io::Error::other(format!("new entry {j}: {e:?}")))?;
    }
    // Spread the updates over the base, skipping the root (entry 0).
    let step = ((n - 1) / overrides.max(1)).max(1);
    let mut renamed = 0;
    for j in 0..overrides {
        let i = 1 + (j * step) % (n - 1);
        let mut name = tree.names.get(i).to_string();
        let mut modified = time(i);
        if j % 2 == 0 {
            name.push_str(" 2");
            renamed += 1;
        } else {
            modified += 60;
        }
        index
            .upsert(entry(
                i,
                parent_of(i),
                name,
                tree.is_dir[i],
                modified,
                size(i),
            ))
            .map_err(|e| io::Error::other(format!("update {j}: {e:?}")))?;
    }
    let made = index.delta_len();
    if made != new_rows + overrides {
        return Err(io::Error::other(format!(
            "the delta has {made} rows, expected {}",
            new_rows + overrides
        )));
    }
    Ok(Setup {
        index,
        _store_dir: store_dir,
        base: n,
        new_rows,
        overrides,
        renamed,
        folded_names,
        folded_bytes,
    })
}

fn run(args: &Args) -> io::Result<()> {
    let t = Instant::now();
    let setup = build(args)?;
    metric("setup_ms", ms(t.elapsed()));
    let live = setup.index.live_count();
    metric("entries", live);
    metric("base_entries", setup.base);
    metric("delta_new", setup.new_rows);
    metric("delta_updates", setup.overrides);
    metric("delta_renamed", setup.renamed);
    metric("delta_rows", setup.index.delta_len());
    metric("base_mapped_bytes", setup.index.mapped_bytes());
    metric(
        "delta_pct_of_live",
        format!(
            "{:.2}",
            100.0 * setup.index.delta_len() as f64 / live as f64
        ),
    );
    metric("names_changed_by_folding", setup.folded_names);
    metric(
        "names_changed_by_folding_pct",
        format!(
            "{:.1}",
            100.0 * setup.folded_names as f64 / setup.base as f64
        ),
    );
    metric("folded_changed_bytes", setup.folded_bytes);

    let Setup {
        index, _store_dir, ..
    } = setup;
    let engine = Engine::start(
        index,
        EngineConfig::new("/fx-rerun-bench"),
        IdleSource,
        NoDisk,
        NoDisk,
    )?;
    let snap = engine.snapshot();
    let opts = SearchOptions::default();
    let ctx = EvalContext::default();
    let never = CancelToken::never();
    let rerun = |text: &str| -> io::Result<(Duration, usize)> {
        let t = Instant::now();
        let mut searcher = Searcher::with_capacity(0);
        searcher.set_context(ctx);
        let hits = searcher
            .search(text, &opts, &*snap, &never)
            .map_err(|_| io::Error::other("search cancelled"))?;
        Ok((t.elapsed(), hits.len()))
    };

    let mut worst = Duration::ZERO;
    let mut p50_sum = Duration::ZERO;
    for &(label, text) in QUERIES {
        eprintln!("rerun_bench: query {label} {text:?}...");
        let (_, hits) = rerun(text)?;
        for _ in 0..args.warmup {
            rerun(text)?;
        }
        let mut samples = Vec::with_capacity(args.iters);
        for _ in 0..args.iters {
            let (d, h) = rerun(text)?;
            if h != hits {
                return Err(io::Error::other(format!(
                    "{text:?} found {h} entries, then {hits}"
                )));
            }
            samples.push(d);
        }
        metric(&format!("rerun_{label}_hits"), hits);
        let s = stats(&format!("rerun_{label}"), &samples)
            .ok_or_else(|| io::Error::other("no samples"))?;
        worst = worst.max(s.p95);
        p50_sum += s.p50;
    }
    metric("rerun_worst_p95_ms", ms(worst));
    metric("rerun_mean_p50_ms", ms(p50_sum / QUERIES.len() as u32));
    bench_common::memory(&snap.memory_report());
    drop(snap);
    engine.shutdown();
    drop(_store_dir);
    Ok(())
}
