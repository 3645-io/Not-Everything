//! `matcher_bench`: measures [`CompiledQuery`] and [`Searcher`] latency
//! over a synthetic file tree, and prints the Phase 1 search-budget gates.
//!
//! ```text
//! cargo run --release -p fx-query --example matcher_bench -- --entries 2000000 --cache warm
//! cargo run --release -p fx-query --example matcher_bench -- --entries 5000000 --cache warm
//! cargo run -p fx-query --example matcher_bench -- --quick
//! ```
//!
//! Setup, outside any timing: `fx_bench::generate_tree` builds `--entries`
//! entries from `--seed`; they go into a [`VecView`] (the root is named
//! `/`, files get a deterministic synthetic size so `size:` has work to
//! do) and the default folded search copy (case and diacritics, apostrophes
//! kept) is built. The synthetic names have no apostrophes; with
//! `--apostrophes N`, N names per million get `’s` before their extension
//! (`config’s.json`), so the copy's side table (the apostrophe-free forms,
//! see `fx_query::fold`) has something in it. The report states the copy's
//! heap bytes and the side table's.
//!
//! Each sample of a query is what the engine does for one keystroke:
//! [`parse`], [`CompiledQuery::compile`] and [`CompiledQuery::search`] with
//! a never-cancelled token, dispatched onto a rayon pool of `--threads`
//! threads. The fixed query set covers 1–4 character literals, a rare
//! literal, a multi-term AND, `*.ext`, a function (`size:>1mb`), a regex and
//! two `path:` substrings. A type-ahead sequence then runs through a fresh
//! [`Searcher`] per iteration (typing `config js`, then backspacing), timed
//! per keystroke and grouped by how the searcher answered (full scan,
//! refinement, cache hit); its results are checked against fresh searches.
//!
//! The last lines are `GATE` lines in the format `fx-bench gate-summary`
//! parses (`GATE <criterion>: PASS|FAIL|SKIP (<detail>)`), from
//! `docs/PLAN.md` Phase 1 Goal 2:
//!
//! - `engine`: every query that is not a regex or a path query (literals,
//!   AND, `*.ext`, `size:`): p95 ≤ 16 ms up to 2M entries, ≤ 30 ms up to
//!   5M entries, SKIP above 5M (no budget);
//! - `regex` and `full-path`: p95 ≤ 100 ms up to 2M entries, SKIP above.
//!
//! The budgets are never scaled: a smaller run is held to the 2M budget.
//! Debug builds and `--quick` runs are labelled as not gate evidence.
//! Gate results do not change the exit code.
//!
//! Exit codes: 0 after a completed benchmark, 1 if the benchmark cannot run
//! (thread pool, compile or a refinement result that disagrees with a fresh
//! search), 2 for bad arguments.

use std::hint::black_box;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use fx_bench::{BenchConfig, CacheState, MachineInfo, Rng, Stats, TreeShape, generate_tree, run};
use fx_query::eval::EvalContext;
use fx_query::matcher::Metadata;
use fx_query::{
    CancelToken, CompiledQuery, FoldOptions, FoldedColumn, SearchOptions, SearchStats, Searcher,
    VecView, parse,
};

const USAGE: &str = "\
usage:
  matcher_bench [--entries N] [--threads T] [--iters K] [--warmup W]
                [--seed S] [--cache warm|cold|unknown] [--apostrophes N]
                [--quick]

  --entries N       synthetic tree entries, 1..=50000000
                    (default 2000000; 50000 with --quick)
  --threads T       rayon threads for the search, 1..=256 (default 8)
  --iters K         timed searches per query, at least 1
                    (default 50; 3 with --quick)
  --warmup W        untimed searches per query first (default 5; 1 with --quick)
  --seed S          generator seed (default 1)
  --cache STATE     cache state label for the report (default unknown)
  --apostrophes N   names per million given an apostrophe (`’s` before the
                    extension), 0..=1000000 (default 0)
  --quick           small, fast run for debug builds; not gate evidence

Gates (docs/PLAN.md Phase 1 Goal 2):
  engine     p95 <= 16 ms up to 2M entries, <= 30 ms up to 5M
  regex      p95 <= 100 ms up to 2M entries
  full-path  p95 <= 100 ms up to 2M entries";

/// Largest accepted thread count.
const MAX_THREADS: usize = 256;
/// Largest accepted entry count (names stay well inside `u32` offsets).
const MAX_ENTRIES: usize = 50_000_000;
/// Entry count the 16 ms engine budget and the 100 ms budgets apply to.
const SMALL_SCALE: usize = 2_000_000;
/// Entry count the 30 ms engine budget applies to.
const LARGE_SCALE: usize = 5_000_000;
/// Seed offset for the synthetic file sizes.
const SIZE_SEED_OFFSET: u64 = 0x7369_7a65_7365_6564;
/// Seed offset for choosing the names that get an apostrophe.
const APOSTROPHE_SEED_OFFSET: u64 = 0x6170_6f73_7472_6f70;

/// Which kind of query a row measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Literal,
    And,
    Ext,
    Function,
    Regex,
    Path,
}

impl Class {
    fn as_str(self) -> &'static str {
        match self {
            Class::Literal => "literal",
            Class::And => "and",
            Class::Ext => "ext",
            Class::Function => "function",
            Class::Regex => "regex",
            Class::Path => "path",
        }
    }

    fn gate(self) -> Gate {
        match self {
            Class::Literal | Class::And | Class::Ext | Class::Function => Gate::Engine,
            Class::Regex => Gate::Regex,
            Class::Path => Gate::FullPath,
        }
    }
}

/// The fixed query set: label, class, query text (default options).
///
/// `conf`, `data` and `json` come from the generator's vocabulary and
/// extension table, `resources/` matches everything below a generated
/// `Resources` folder, `zqxj` hits almost nothing (pure scan cost), and
/// `path:zqxj` makes every entry build its full path (the path worst case).
const QUERIES: &[(&str, Class, &str)] = &[
    ("1-char", Class::Literal, "a"),
    ("2-char", Class::Literal, "re"),
    ("3-char", Class::Literal, "por"),
    ("4-char", Class::Literal, "conf"),
    ("rare", Class::Literal, "zqxj"),
    ("and", Class::And, "data json"),
    ("ext", Class::Ext, "*.png"),
    ("function", Class::Function, "size:>1mb"),
    ("regex", Class::Regex, r"regex:^img_\d+\.jpg$"),
    ("path", Class::Path, "path:resources/"),
    ("path-rare", Class::Path, "path:zqxj"),
];

/// Keystrokes of the type-ahead sequence: typing `config js`, then
/// backspacing to `co`. With the default LRU of 8 result sets every
/// backspace is a cache hit.
const TYPE_AHEAD: &[&str] = &[
    "c",
    "co",
    "con",
    "conf",
    "confi",
    "config",
    "config ",
    "config j",
    "config js",
    "config j",
    "config ",
    "config",
    "confi",
    "conf",
    "con",
    "co",
];

/// A budget group, printed as one `GATE` line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Gate {
    Engine,
    Regex,
    FullPath,
}

impl Gate {
    const ALL: [Gate; 3] = [Gate::Engine, Gate::Regex, Gate::FullPath];

    fn as_str(self) -> &'static str {
        match self {
            Gate::Engine => "engine",
            Gate::Regex => "regex",
            Gate::FullPath => "full-path",
        }
    }

    /// The p95 budget in milliseconds for `entries` and the scale it was
    /// set for, or `None` when the plan sets no budget at that size.
    fn budget(self, entries: usize) -> Option<(u64, &'static str)> {
        match self {
            Gate::Engine if entries <= SMALL_SCALE => Some((16, "2M")),
            Gate::Engine if entries <= LARGE_SCALE => Some((30, "5M")),
            Gate::Regex | Gate::FullPath if entries <= SMALL_SCALE => Some((100, "2M")),
            _ => None,
        }
    }

    /// The largest size with a budget, for SKIP lines.
    fn limit(self) -> &'static str {
        match self {
            Gate::Engine => "5M",
            Gate::Regex | Gate::FullPath => "2M",
        }
    }
}

struct UsageError(String);

struct Args {
    entries: usize,
    threads: usize,
    iters: u32,
    warmup: u32,
    seed: u64,
    cache: CacheState,
    apostrophes: u32,
    quick: bool,
}

/// Results for one query.
struct Row {
    label: &'static str,
    class: Class,
    query: &'static str,
    hits: usize,
    stats: Stats,
}

/// Type-ahead results: per-keystroke stats grouped by answer path.
struct TypeAhead {
    full: Option<Stats>,
    refined: Option<Stats>,
    hit: Option<Stats>,
    all: Option<Stats>,
    sequence: Option<Stats>,
    /// Counters of the last timed sequence.
    stats: SearchStats,
}

/// What the setup produced, for the report header.
struct Setup {
    view: VecView,
    folders: usize,
    name_bytes: usize,
    /// Heap bytes of the folded copy, side table included.
    folded_bytes: usize,
    /// Heap bytes of the side table alone.
    side_bytes: usize,
    /// Names in the side table (names with an apostrophe).
    side_names: usize,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let args = match parse_args(&args) {
        Ok(a) => a,
        Err(UsageError(msg)) => {
            eprintln!("matcher_bench: {msg}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match execute(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("matcher_bench: error: {msg}");
            ExitCode::from(1)
        }
    }
}

fn parse_args(args: &[String]) -> Result<Args, UsageError> {
    let mut entries: Option<usize> = None;
    let mut threads: Option<usize> = None;
    let mut iters: Option<u32> = None;
    let mut warmup: Option<u32> = None;
    let mut seed: Option<u64> = None;
    let mut cache: Option<CacheState> = None;
    let mut apostrophes: Option<u32> = None;
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
            "--entries" => entries.replace(number(flag, value()?)?).is_some(),
            "--threads" => threads.replace(number(flag, value()?)?).is_some(),
            "--iters" => iters.replace(number(flag, value()?)?).is_some(),
            "--warmup" => warmup.replace(number(flag, value()?)?).is_some(),
            "--seed" => seed.replace(number(flag, value()?)?).is_some(),
            "--cache" => cache.replace(cache_state(value()?)?).is_some(),
            "--apostrophes" => apostrophes.replace(number(flag, value()?)?).is_some(),
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

    let entries = entries.unwrap_or(if quick { 50_000 } else { SMALL_SCALE });
    if entries == 0 || entries > MAX_ENTRIES {
        return Err(UsageError(format!(
            "--entries must be 1..={MAX_ENTRIES}, got {entries}"
        )));
    }
    let threads = threads.unwrap_or(8);
    if threads == 0 || threads > MAX_THREADS {
        return Err(UsageError(format!(
            "--threads must be 1..={MAX_THREADS}, got {threads}"
        )));
    }
    let iters = iters.unwrap_or(if quick { 3 } else { 50 });
    if iters == 0 {
        return Err(UsageError("--iters must be at least 1".into()));
    }
    let apostrophes = apostrophes.unwrap_or(0);
    if apostrophes > 1_000_000 {
        return Err(UsageError(format!(
            "--apostrophes must be 0..=1000000, got {apostrophes}"
        )));
    }
    Ok(Args {
        entries,
        threads,
        iters,
        warmup: warmup.unwrap_or(if quick { 1 } else { 5 }),
        seed: seed.unwrap_or(1),
        cache: cache.unwrap_or(CacheState::Unknown),
        apostrophes,
        quick,
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

fn execute(args: &Args) -> Result<(), String> {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(args.threads)
        .build()
        .map_err(|e| format!("cannot build a {}-thread pool: {e}", args.threads))?;

    eprintln!(
        "matcher_bench: generating {} entries (seed {})...",
        args.entries, args.seed
    );
    let setup = build_view(args.entries, args.seed, args.apostrophes);
    let view = &setup.view;
    let opts = SearchOptions::default();
    let ctx = EvalContext::default();
    let token = CancelToken::never();

    let mut rows = Vec::with_capacity(QUERIES.len());
    for &(label, class, query) in QUERIES {
        eprintln!("matcher_bench: query {label} {query:?}...");
        // Hit count from one untimed search; also proves the query compiles.
        let parsed = parse(query, &opts);
        let compiled = CompiledQuery::compile(&parsed, &opts, ctx)
            .map_err(|e| format!("query {query:?}: {e}"))?;
        let hits = pool
            .install(|| compiled.search(view, &token))
            .map_err(|e| format!("query {query:?}: {e}"))?
            .len();
        let cfg = BenchConfig {
            warmup: args.warmup,
            iterations: args.iters,
            label: format!("{label} {query:?}"),
            cache: args.cache,
        };
        // One sample = parse + compile + parallel search, as per keystroke.
        let report = run(cfg, || {
            pool.install(|| {
                let parsed = parse(query, &opts);
                CompiledQuery::compile(&parsed, &opts, ctx)
                    .ok()
                    .map(|q| q.search(view, &token))
            })
        });
        rows.push(Row {
            label,
            class,
            query,
            hits,
            stats: report.stats,
        });
    }

    eprintln!("matcher_bench: type-ahead {:?}...", TYPE_AHEAD);
    let type_ahead = time_type_ahead(args, &pool, view, &opts)?;
    verify_type_ahead(&pool, view, &opts, ctx)?;

    print_report(args, &setup, &rows, &type_ahead);
    Ok(())
}

/// Builds the view from the synthetic tree, plus the default folded copy.
/// `apostrophes` names per million get `’s` before their extension.
fn build_view(entries: usize, seed: u64, apostrophes: u32) -> Setup {
    let tree = generate_tree(entries, seed, TreeShape::default());
    let mut rng = Rng::new(seed.wrapping_add(SIZE_SEED_OFFSET));
    let mut pick = Rng::new(seed.wrapping_add(APOSTROPHE_SEED_OFFSET));
    let mut view = VecView::new();
    let mut name_bytes = 0;
    for i in 0..tree.len() {
        let parent = (tree.parent[i] != u32::MAX).then_some(tree.parent[i]);
        // The view's root name is its full path.
        let name = if parent.is_none() {
            "/".to_string()
        } else if apostrophes > 0 && pick.below(1_000_000) < u64::from(apostrophes) {
            with_apostrophe(tree.names.get(i))
        } else {
            tree.names.get(i).to_string()
        };
        name_bytes += name.len();
        let is_dir = tree.is_dir[i];
        let meta = Metadata {
            size: (!is_dir).then(|| synthetic_size(&mut rng)),
            ..Metadata::default()
        };
        view.push(&name, parent, is_dir, meta);
    }
    let folded = FoldedColumn::build(&view.column(), FoldOptions::for_match(false, false));
    let (folded_bytes, side_bytes, side_names) = (
        folded.heap_bytes(),
        folded.side_table_bytes(),
        folded.side_table_len(),
    );
    drop(folded);
    view.build_folded(FoldOptions::for_match(false, false));
    Setup {
        view,
        folders: tree.dir_count(),
        name_bytes,
        folded_bytes,
        side_bytes,
        side_names,
    }
}

/// `name` with `’s` before its extension (`config’s.json`), or at its end.
fn with_apostrophe(name: &str) -> String {
    match name.rfind('.').filter(|&d| d > 0) {
        Some(dot) => format!("{}’s{}", &name[..dot], &name[dot..]),
        None => format!("{name}’s"),
    }
}

/// A file size: 2% empty, otherwise log-uniform between 1 byte and
/// 256 MB, so about a quarter of the files are above 1 MB.
fn synthetic_size(rng: &mut Rng) -> u64 {
    if rng.chance(20_000) {
        return 0;
    }
    let bits = rng.below(28);
    (1u64 << bits) + rng.below(1u64 << bits)
}

fn time_type_ahead(
    args: &Args,
    pool: &rayon::ThreadPool,
    view: &VecView,
    opts: &SearchOptions,
) -> Result<TypeAhead, String> {
    let token = CancelToken::never();
    let mut full = Vec::new();
    let mut refined = Vec::new();
    let mut hit = Vec::new();
    let mut all = Vec::new();
    let mut sequence = Vec::new();
    let mut last = SearchStats::default();
    for iter in 0..args.warmup + args.iters {
        let timed = iter >= args.warmup;
        let mut searcher = Searcher::new();
        let mut seq_time = Duration::ZERO;
        for &text in TYPE_AHEAD {
            let before = searcher.stats();
            let start = Instant::now();
            let result = pool.install(|| searcher.search(text, opts, view, &token));
            let elapsed = start.elapsed();
            drop(black_box(
                result.map_err(|e| format!("type-ahead {text:?}: {e}"))?,
            ));
            seq_time += elapsed;
            if !timed {
                continue;
            }
            let after = searcher.stats();
            if after.cache_hits > before.cache_hits {
                hit.push(elapsed);
            } else if after.refined > before.refined {
                refined.push(elapsed);
            } else {
                full.push(elapsed);
            }
            all.push(elapsed);
        }
        if timed {
            sequence.push(seq_time);
            last = searcher.stats();
        }
    }
    Ok(TypeAhead {
        full: Stats::from_samples(&full),
        refined: Stats::from_samples(&refined),
        hit: Stats::from_samples(&hit),
        all: Stats::from_samples(&all),
        sequence: Stats::from_samples(&sequence),
        stats: last,
    })
}

/// Runs the type-ahead sequence once more, untimed, and checks every
/// answer against a fresh compiled search.
fn verify_type_ahead(
    pool: &rayon::ThreadPool,
    view: &VecView,
    opts: &SearchOptions,
    ctx: EvalContext,
) -> Result<(), String> {
    let token = CancelToken::never();
    let mut searcher = Searcher::new();
    for &text in TYPE_AHEAD {
        let cached = pool
            .install(|| searcher.search(text, opts, view, &token))
            .map_err(|e| format!("type-ahead {text:?}: {e}"))?;
        let parsed = parse(text, opts);
        let fresh = CompiledQuery::compile(&parsed, opts, ctx)
            .map_err(|e| format!("type-ahead {text:?}: {e}"))?;
        let fresh = pool
            .install(|| fresh.search(view, &token))
            .map_err(|e| format!("type-ahead {text:?}: {e}"))?;
        if *cached != *fresh {
            return Err(format!(
                "type-ahead {text:?}: the searcher returned {} hits, a fresh search {}",
                cached.len(),
                fresh.len()
            ));
        }
    }
    Ok(())
}

fn ms_f64(d: Duration) -> f64 {
    d.as_nanos() as f64 / 1_000_000.0
}

fn ms3(d: Duration) -> String {
    format!("{:.3}", ms_f64(d))
}

/// Notes appended to every gate detail when the run is not gate evidence.
fn evidence_note(args: &Args) -> String {
    let mut reasons = Vec::new();
    if args.quick {
        reasons.push("quick mode");
    }
    if cfg!(debug_assertions) {
        reasons.push("debug build");
    }
    if reasons.is_empty() {
        String::new()
    } else {
        format!("; not gate evidence: {}", reasons.join(", "))
    }
}

/// One `GATE` line per budget group, in the format `fx-bench
/// gate-summary` parses. The criterion has no `:` of its own.
fn gate_lines(args: &Args, rows: &[Row]) -> Vec<String> {
    let note = evidence_note(args);
    Gate::ALL
        .iter()
        .filter_map(|&gate| {
            let worst = rows
                .iter()
                .filter(|r| r.class.gate() == gate)
                .max_by_key(|r| r.stats.p95)?;
            let measured = format!(
                "worst p95 {} ms, query {:?}; {} threads, cache {}",
                ms3(worst.stats.p95),
                worst.query,
                args.threads,
                args.cache
            );
            Some(match gate.budget(args.entries) {
                Some((ms, scale)) => {
                    let pass = worst.stats.p95 <= Duration::from_millis(ms);
                    format!(
                        "GATE {} p95 <= {ms} ms at {} entries: {} ({measured}; {scale} budget{note})",
                        gate.as_str(),
                        args.entries,
                        if pass { "PASS" } else { "FAIL" },
                    )
                }
                None => format!(
                    "GATE {} p95 at {} entries: SKIP (no budget above {} entries; {measured}{note})",
                    gate.as_str(),
                    args.entries,
                    gate.limit(),
                ),
            })
        })
        .collect()
}

fn print_report(args: &Args, setup: &Setup, rows: &[Row], ta: &TypeAhead) {
    let m = MachineInfo::detect();
    println!(
        "matcher_bench: {} entries ({} folders, seed {}, {:.1} MB names), {} threads, \
         cache {}, {} iterations, {} warmup{}",
        args.entries,
        setup.folders,
        args.seed,
        setup.name_bytes as f64 / 1_000_000.0,
        args.threads,
        args.cache,
        args.iters,
        args.warmup,
        if args.quick { ", quick" } else { "" }
    );
    let per_entry = |bytes: usize| bytes as f64 / args.entries as f64;
    println!(
        "folded copy: {} bytes ({:.2} B/entry), side table {} bytes ({:.3} B/entry) \
         for {} names with an apostrophe (--apostrophes {} per million)",
        setup.folded_bytes,
        per_entry(setup.folded_bytes),
        setup.side_bytes,
        per_entry(setup.side_bytes),
        setup.side_names,
        args.apostrophes
    );
    println!(
        "machine: {} {}, {} logical CPUs, {}; {} build",
        m.os,
        m.arch,
        m.parallelism,
        m.cpu_brand,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
    println!(
        "{:<10} {:<9} {:<24} {:>9} {:>9} {:>9} {:>9}  gate",
        "label", "class", "query", "hits", "p50_ms", "p95_ms", "max_ms"
    );
    for r in rows {
        println!(
            "{:<10} {:<9} {:<24} {:>9} {:>9} {:>9} {:>9}  {}",
            r.label,
            r.class.as_str(),
            format!("{:?}", r.query),
            r.hits,
            ms3(r.stats.p50),
            ms3(r.stats.p95),
            ms3(r.stats.max),
            r.class.gate().as_str()
        );
    }

    println!(
        "type-ahead: {} keystrokes per sequence (typing \"config js\", backspacing to \"co\"); \
         last sequence: {} full scans, {} refined, {} cache hits",
        TYPE_AHEAD.len(),
        ta.stats.full_scans,
        ta.stats.refined,
        ta.stats.cache_hits
    );
    println!(
        "{:<20} {:>8} {:>9} {:>9} {:>9}",
        "keystrokes", "samples", "p50_ms", "p95_ms", "max_ms"
    );
    for (label, stats) in [
        ("full scan", &ta.full),
        ("refined", &ta.refined),
        ("cache hit", &ta.hit),
        ("all keystrokes", &ta.all),
        ("whole sequence", &ta.sequence),
    ] {
        match stats {
            Some(s) => println!(
                "{:<20} {:>8} {:>9} {:>9} {:>9}",
                label,
                s.count,
                ms3(s.p50),
                ms3(s.p95),
                ms3(s.max)
            ),
            None => println!("{label:<20} {:>8}", 0),
        }
    }

    for line in gate_lines(args, rows) {
        println!("{line}");
    }
}
