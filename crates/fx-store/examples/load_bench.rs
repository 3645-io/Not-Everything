//! `load_bench`: measures the store gate, mmap load to first search result,
//! and the cost of the other checkpoint stages.
//!
//! ```text
//! cargo run --release -p fx-store --example load_bench -- --entries 5000000
//! cargo run -p fx-store --example load_bench -- --quick     # debug smoke run
//! ```
//!
//! Setup, outside the read timings: it generates `--entries` (alias
//! `--count`) entries with `fx_bench::generate_tree`, puts them in a
//! synthetic `fx_index::Index` (entry `i` gets ID `i`, with deterministic
//! sizes and times), and writes a checkpoint with
//! `fx_store::write_checkpoint` (one saved order, one volume cursor).
//!
//! It times, through `fx-bench`'s harness, and reports p50/p95/p99/max for:
//!
//! - `write`: `write_checkpoint` (temp file, `sync_all`/`F_FULLFSYNC`,
//!   rename, directory sync), over `min(--iters, 5)` repeated writes;
//! - `open (header)`: `MappedIndex::open` (map the file, validate the
//!   header, volume table and column table);
//! - `open+full verify`: the above plus `verify_checksums` over every
//!   column (what `VerifyMode::Full` costs at startup);
//! - `open+to_index`: the above `open` plus `MappedIndex::to_index`, which
//!   copies and validates everything into an `fx_index::Index`;
//! - `open+column`: `open` plus `NameColumn::new` over `name_bytes_nul()`
//!   and `name_offsets()` (which walks every offset);
//! - `load to first result` (mmap to first result): `open+column` plus an
//!   `fx_query` substring search for `--query`, taking the first (smallest)
//!   matching entry ID. The search API has no early-exit mode, so the full
//!   parallel scan runs and its first ID is taken; this is an upper bound
//!   on "first hit".
//!
//! Unmapping and dropping happen after the timer stops.
//!
//! The last line is the gate: `GATE load-to-first-result p95 <= 50 ms:
//! PASS|FAIL (<value> ms)`. The gate result does not change the exit code.
//!
//! The cache label defaults to `warm` for a freshly written image (writing
//! it puts it in the page cache, and warm runs do two untimed warm-up
//! samples per stage) and to `unknown` with `--reuse`. `--cache` only sets
//! the label. For cold-cache numbers write the image once with `--dir D
//! --keep`, run `sudo purge` (or reboot), then time it with `--dir D
//! --reuse --cache cold --iters 1`: after the first sample the file is in
//! the page cache again. `--reuse` skips generation and writing when
//! `D/index.fxs` already opens, and never deletes that file.
//!
//! `--quick` is for debug builds and smoke tests: 50000 entries and 3
//! samples per stage unless `--entries`/`--count` or `--iters` are given.
//!
//! Exit codes: 0 after a completed benchmark, 1 if the benchmark cannot run
//! (write, open or search fails), 2 for bad arguments.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fx_bench::{BenchConfig, BenchReport, CacheState, MachineInfo, TreeShape, generate_tree, run};
use fx_index::Index;
use fx_model::{Entry, EntryFlags, EntryKind, FileKey, ResumeCursor, VolumeId};
use fx_query::substring::{CancelToken, CaseMode, NameColumn, SubstringQuery, search};
use fx_store::{MappedIndex, SaveInput, write_checkpoint};

const USAGE: &str = "\
usage:
  load_bench [--entries N] [--seed S] [--iters K] [--query Q] [--dir DIR]
             [--cache warm|cold|unknown] [--keep] [--reuse] [--quick]

  --entries N      entries to generate, 1..=100000000 (default 5000000);
                   --count N is the same option
  --seed S         generator seed (default 1)
  --iters K        timed samples per stage, at least 1 (default 20);
                   the write stage uses min(K, 5) samples
  --query Q        case-sensitive substring to search for (default \"conf\")
  --quick          debug smoke run: 50000 entries and 3 samples per stage
                   unless --entries or --iters are given
  --dir DIR        directory for the image file index.fxs
                   (default: a fresh directory under the system temp dir)
  --cache STATE    cache state label for the report (default warm for a
                   freshly written image, unknown with --reuse);
                   for cold numbers see --reuse
  --keep           keep the image file after the run
  --reuse          with --dir: if DIR/index.fxs already opens, time it
                   instead of generating and writing a new one (it is never
                   deleted). Cold runs: write once with --dir DIR --keep,
                   `sudo purge`, then --dir DIR --reuse --cache cold --iters 1

Gate: load to first result (open + column + first substring hit)
p95 <= 50 ms.";

/// Gate: p95 of open + column + first substring hit.
const GATE_P95: Duration = Duration::from_millis(50);
/// Largest accepted entry count (offsets are `u32`, IDs fit easily).
const MAX_COUNT: usize = 100_000_000;
/// Most samples of the write stage (each write is a full fsync'd file).
const MAX_WRITE_ITERS: u32 = 5;
/// Key code of the saved order the benchmark writes (an identity order;
/// only its size matters here).
const ORDER_KEY: u8 = 0;
/// File name of the image inside `--dir`.
const IMAGE_NAME: &str = "index.fxs";

struct UsageError(String);

struct Args {
    count: usize,
    /// `--count` was given explicitly (only matters with `--reuse`).
    count_given: bool,
    seed: u64,
    iters: u32,
    query: String,
    dir: Option<PathBuf>,
    cache: CacheState,
    keep: bool,
    reuse: bool,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let args = match parse(&args) {
        Ok(a) => a,
        Err(UsageError(msg)) => {
            eprintln!("load_bench: {msg}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match execute(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("load_bench: error: {msg}");
            ExitCode::from(1)
        }
    }
}

fn parse(args: &[String]) -> Result<Args, UsageError> {
    let mut count: Option<usize> = None;
    let mut seed: Option<u64> = None;
    let mut iters: Option<u32> = None;
    let mut query: Option<String> = None;
    let mut dir: Option<PathBuf> = None;
    let mut cache: Option<CacheState> = None;
    let mut keep = false;
    let mut reuse = false;
    let mut quick = false;

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let flag = arg.as_str();
        let mut value = || {
            it.next()
                .map(String::as_str)
                .ok_or_else(|| UsageError(format!("{flag} needs a value")))
        };
        let duplicate = match flag {
            "--count" | "--entries" => count.replace(number(flag, value()?)?).is_some(),
            "--seed" => seed.replace(number(flag, value()?)?).is_some(),
            "--iters" => iters.replace(number(flag, value()?)?).is_some(),
            "--query" => query.replace(value()?.to_owned()).is_some(),
            "--dir" => dir.replace(PathBuf::from(value()?)).is_some(),
            "--cache" => cache.replace(cache_state(value()?)?).is_some(),
            "--keep" => std::mem::replace(&mut keep, true),
            "--reuse" => std::mem::replace(&mut reuse, true),
            "--quick" => std::mem::replace(&mut quick, true),
            other if other.starts_with('-') => {
                return Err(UsageError(format!("unknown flag '{other}'")));
            }
            other => return Err(UsageError(format!("unexpected argument '{other}'"))),
        };
        if duplicate {
            return Err(UsageError(format!("{flag} given more than once")));
        }
    }

    let count_given = count.is_some();
    let count = count.unwrap_or(if quick { 50_000 } else { 5_000_000 });
    if count == 0 || count > MAX_COUNT {
        return Err(UsageError(format!(
            "--entries must be 1..={MAX_COUNT}, got {count}"
        )));
    }
    let iters = iters.unwrap_or(if quick { 3 } else { 20 });
    if iters == 0 {
        return Err(UsageError("--iters must be at least 1".into()));
    }
    let query = query.unwrap_or_else(|| "conf".to_owned());
    if query.is_empty() || query.contains('\0') {
        return Err(UsageError(
            "--query must be non-empty and must not contain NUL".into(),
        ));
    }
    if reuse && dir.is_none() {
        return Err(UsageError("--reuse needs --dir".into()));
    }
    Ok(Args {
        count,
        count_given,
        seed: seed.unwrap_or(1),
        iters,
        query,
        dir,
        // A freshly written image is in the page cache, so warm is the
        // honest default; a reused image's state is unknown.
        cache: cache.unwrap_or(if reuse {
            CacheState::Unknown
        } else {
            CacheState::Warm
        }),
        keep,
        reuse,
    })
}

/// Parses a non-negative decimal integer; `_` separators are allowed.
fn number<T: std::str::FromStr>(flag: &str, text: &str) -> Result<T, UsageError> {
    let digits: String = text.chars().filter(|&c| c != '_').collect();
    let valid = !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit());
    valid.then(|| digits.parse().ok()).flatten().ok_or_else(|| {
        UsageError(format!(
            "{flag} expects a non-negative integer, got '{text}'"
        ))
    })
}

fn cache_state(text: &str) -> Result<CacheState, UsageError> {
    match text {
        "warm" => Ok(CacheState::Warm),
        "cold" => Ok(CacheState::Cold),
        "unknown" => Ok(CacheState::Unknown),
        other => Err(UsageError(format!(
            "--cache expects warm, cold or unknown, got '{other}'"
        ))),
    }
}

/// Removes the image (and the directory, if this run created it) on drop,
/// unless `--keep` was given.
struct Cleanup {
    file: PathBuf,
    created_dir: Option<PathBuf>,
    keep: bool,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        let _ = fs::remove_file(&self.file);
        if let Some(dir) = &self.created_dir {
            let _ = fs::remove_dir(dir);
        }
    }
}

fn prepare_dir(args: &Args) -> Result<Cleanup, String> {
    let (dir, created) = match &args.dir {
        Some(d) => (d.clone(), false),
        None => {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let d = std::env::temp_dir().join(format!(
                "fx-store-load-bench-{}-{nanos}",
                std::process::id()
            ));
            (d, true)
        }
    };
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    Ok(Cleanup {
        file: dir.join(IMAGE_NAME),
        created_dir: created.then_some(dir),
        keep: args.keep,
    })
}

/// The timed gate path: open, wrap the name column, search, take the first
/// ID. Returns the image so unmapping happens outside the timed region.
fn load_to_first_result(path: &Path, needle: &str) -> Result<(MappedIndex, Option<u32>), String> {
    let img = MappedIndex::open(path).map_err(|e| e.to_string())?;
    let first = {
        let col =
            NameColumn::new(img.name_bytes_nul(), img.name_offsets()).map_err(|e| e.to_string())?;
        let query = SubstringQuery::new(needle, CaseMode::Sensitive);
        let ids = search(col, &query, &CancelToken::never()).map_err(|e| e.to_string())?;
        ids.first().copied()
    };
    Ok((img, first))
}

/// Times `f` with the `fx-bench` harness. The first failing sample's error
/// is returned, so a failure during timing is not silently lost.
fn timed<T, F>(label: &str, args: &Args, warmup: u32, f: F) -> Result<BenchReport, String>
where
    F: FnMut() -> Result<T, String>,
{
    timed_n(label, args.cache, warmup, args.iters, f)
}

/// [`timed`] with an explicit sample count.
fn timed_n<T, F>(
    label: &str,
    cache: CacheState,
    warmup: u32,
    iterations: u32,
    mut f: F,
) -> Result<BenchReport, String>
where
    F: FnMut() -> Result<T, String>,
{
    let mut failure: Option<String> = None;
    let cfg = BenchConfig {
        warmup,
        iterations,
        label: label.to_owned(),
        cache,
    };
    let report = run(cfg, || {
        let r = f();
        if let Err(e) = &r {
            failure.get_or_insert_with(|| e.clone());
        }
        r
    });
    match failure {
        Some(e) => Err(format!("{label}: {e}")),
        None => Ok(report),
    }
}

fn execute(args: &Args) -> Result<(), String> {
    let mut cleanup = prepare_dir(args)?;
    let path = cleanup.file.clone();

    // Setup, outside any timing.
    let setup = match reusable(args, &path) {
        Some(setup) => {
            // Never delete an image this run did not write.
            cleanup.keep = true;
            setup
        }
        None => build_image(args, &path)?,
    };
    let Setup {
        entries,
        file_len,
        times,
        write_report,
    } = setup;

    // Warm runs get two untimed samples (page cache, rayon pool start-up);
    // cold or unknown runs time from the first sample on. The gate stage
    // runs first so a cold run's first sample is the cold one.
    let warmup = if args.cache == CacheState::Warm { 2 } else { 0 };

    eprintln!("load_bench: timing {} samples per stage...", args.iters);
    let gate_report = timed("load to first result", args, warmup, || {
        load_to_first_result(&path, &args.query)
    })?;
    let open_report = timed("open (header)", args, warmup, || {
        MappedIndex::open(&path).map_err(|e| e.to_string())
    })?;
    let full_report = timed("open+full verify", args, warmup, || {
        let img = MappedIndex::open(&path).map_err(|e| e.to_string())?;
        img.verify_checksums().map_err(|e| e.to_string())?;
        Ok(img)
    })?;
    let to_index_report = timed("open+to_index", args, warmup.min(1), || {
        let img = MappedIndex::open(&path).map_err(|e| e.to_string())?;
        let index = img.to_index().map_err(|e| e.to_string())?;
        Ok((img, index))
    })?;
    let column_report = timed("open+column", args, warmup, || {
        let img = MappedIndex::open(&path).map_err(|e| e.to_string())?;
        let len = NameColumn::new(img.name_bytes_nul(), img.name_offsets())
            .map_err(|e| e.to_string())?
            .len();
        Ok((img, len))
    })?;

    // One untimed pass after the timing (so it cannot warm a cold run):
    // the hit count and the first result's name.
    let (hits, first_name) = {
        let img = MappedIndex::open(&path).map_err(|e| format!("MappedIndex::open: {e}"))?;
        let col = NameColumn::new(img.name_bytes_nul(), img.name_offsets())
            .map_err(|e| format!("NameColumn::new: {e}"))?;
        let query = SubstringQuery::new(&args.query, CaseMode::Sensitive);
        let ids = search(col, &query, &CancelToken::never()).map_err(|e| format!("search: {e}"))?;
        let first = ids.first().map(|&id| {
            (
                id,
                String::from_utf8_lossy(col.name_bytes(id as usize)).into_owned(),
            )
        });
        (ids.len(), first)
    };

    // Report.
    let m = MachineInfo::detect();
    let mb = file_len as f64 / 1_000_000.0;
    let bytes_per_entry = file_len as f64 / entries.max(1) as f64;
    match times {
        Some(gen_time) => {
            println!(
                "load_bench: {entries} entries (seed {}), generated in {} ms",
                args.seed,
                ms3(gen_time)
            );
            println!(
                "image: {} ({mb:.1} MB, {bytes_per_entry:.1} B/entry), written by \
                 write_checkpoint (temp file + sync_all/F_FULLFSYNC + rename + dir sync)",
                path.display(),
            );
        }
        None => {
            println!("load_bench: {entries} entries, reused existing image");
            println!(
                "image: {} ({mb:.1} MB, {bytes_per_entry:.1} B/entry)",
                path.display()
            );
        }
    }
    match &first_name {
        Some((id, name)) => println!(
            "query {:?}: {hits} hits, first result entry {id} {name:?}",
            args.query
        ),
        None => println!(
            "query {:?}: no hits (warning: load to first result measures a full miss)",
            args.query
        ),
    }
    println!(
        "{:<22} {:>9} {:>9} {:>9} {:>9}",
        "stage", "p50_ms", "p95_ms", "p99_ms", "max_ms"
    );
    match &write_report {
        Some(r) => print_row(&r.label, r),
        None => println!("{:<22} (skipped: reused image)", "write"),
    }
    for r in [
        &open_report,
        &full_report,
        &to_index_report,
        &column_report,
        &gate_report,
    ] {
        print_row(&r.label, r);
    }
    let verify_secs = full_report.stats.p50.as_secs_f64();
    if verify_secs > 0.0 {
        println!(
            "open+full verify and open+to_index read the whole file and are not on \
             the gate path (full verify {:.0} MB/s at p50)",
            mb / verify_secs
        );
    }
    println!(
        "gate path: load to first result = mmap + header check + name column + \
         first substring hit"
    );
    println!(
        "machine: {} {}, {} logical CPUs, {}",
        m.os, m.arch, m.parallelism, m.cpu_brand
    );
    let write_samples = write_report.as_ref().map_or(0, |r| r.iterations);
    println!(
        "cache: {} ({} samples per read stage, {write_samples} write samples, {warmup} warmup)",
        args.cache, args.iters
    );
    if args.cache == CacheState::Warm {
        println!(
            "note: the cache is warm: the image was just written or read, so it is in the page cache"
        );
    }
    if cleanup.keep {
        println!("kept image: {}", path.display());
    }
    let p95 = gate_report.stats.p95;
    println!(
        "GATE load-to-first-result p95 <= 50 ms: {} ({} ms)",
        if p95 <= GATE_P95 { "PASS" } else { "FAIL" },
        ms3(p95)
    );
    drop(cleanup);
    Ok(())
}

/// What the setup produced: entry count, file size, the generation time
/// and the write timings (`None` for a reused image).
struct Setup {
    entries: usize,
    file_len: u64,
    times: Option<Duration>,
    write_report: Option<BenchReport>,
}

/// With `--reuse`, an existing image at `path` that opens cleanly. Only the
/// header is read, so this barely touches the page cache.
fn reusable(args: &Args, path: &Path) -> Option<Setup> {
    if !args.reuse || !path.exists() {
        return None;
    }
    match MappedIndex::open(path) {
        Ok(img) => {
            if args.count_given && img.id_bound() != args.count {
                eprintln!(
                    "load_bench: note: reused checkpoint has {} entries (--count {} ignored)",
                    img.id_bound(),
                    args.count
                );
            }
            Some(Setup {
                entries: img.id_bound(),
                file_len: img.file_len(),
                times: None,
                write_report: None,
            })
        }
        Err(e) => {
            eprintln!(
                "load_bench: {} cannot be reused ({e}); writing a new image",
                path.display()
            );
            None
        }
    }
}

/// Generates the tree and writes the image to `path`.
fn build_image(args: &Args, path: &Path) -> Result<Setup, String> {
    eprintln!(
        "load_bench: generating {} entries (seed {})...",
        args.count, args.seed
    );
    let gen_start = Instant::now();
    let tree = generate_tree(args.count, args.seed, TreeShape::default());
    // Entry `i` of the tree gets entry ID `i`: parents come before children.
    let vol = VolumeId(1);
    let key = |i: usize| FileKey::new(vol, i as u64 + 1);
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
                size: if is_dir {
                    0
                } else {
                    (i as u64).wrapping_mul(2_654_435_761) % (1 << 32)
                },
                modified: 1_600_000_000 + (i as i64 % 100_000_000),
                created: 1_600_000_000,
            })
            .map_err(|e| format!("Index::upsert: {e}"))?;
    }
    let parts = index.snapshot().to_parts();
    drop(index);
    let gen_time = gen_start.elapsed();
    let entries = tree.len();
    drop(tree);

    let vols = [(
        vol,
        ResumeCursor::FsEvents {
            event_id: 1,
            db_uuid: [0; 16],
        },
    )];
    // One saved order, as the engine would write for its default sort.
    // A saved order is a permutation of the live entry IDs.
    let order: Vec<u32> = (0..parts.id_bound() as u32)
        .filter(|&i| parts.is_live(i as usize))
        .collect();
    let orders = [(ORDER_KEY, order.as_slice())];
    let input = SaveInput {
        parts: &parts,
        volumes: &vols,
        orders: &orders,
        clean_shutdown: true,
    };
    let write_iters = args.iters.min(MAX_WRITE_ITERS);
    eprintln!(
        "load_bench: writing {} ({write_iters} timed writes)...",
        path.display()
    );
    // Each sample atomically replaces the file; the last one stays.
    let mut file_len = 0;
    let write_report = timed_n("write", args.cache, 0, write_iters, || {
        file_len = write_checkpoint(path, &input).map_err(|e| format!("write_checkpoint: {e}"))?;
        Ok(())
    })?;

    // The in-memory parts are dropped on return, so the timed part runs
    // with the checkpoint file as the only copy of the data.
    Ok(Setup {
        entries,
        file_len,
        times: Some(gen_time),
        write_report: Some(write_report),
    })
}

fn print_row(label: &str, r: &BenchReport) {
    let s = &r.stats;
    println!(
        "{:<22} {:>9} {:>9} {:>9} {:>9}",
        label,
        ms3(s.p50),
        ms3(s.p95),
        ms3(s.p99),
        ms3(s.max)
    );
}

fn ms3(d: Duration) -> String {
    format!("{:.3}", d.as_nanos() as f64 / 1_000_000.0)
}
