//! `search_bench`: measures [`fx_query::search`] latency over synthetic
//! names.
//!
//! ```text
//! cargo run --release -p fx-query --example search_bench -- --threads 8 --cache warm
//! ```
//!
//! It generates `--count` names with `fx_bench::generate_names` once,
//! outside any timing, and builds an ASCII-folded copy of the buffer. It then
//! times `search` for a fixed query set through `fx-bench`'s harness, on a
//! rayon pool with `--threads` threads, and reports the hit count and
//! p50/p95/p99/max per query, plus the machine and cache label.
//!
//! The last line is the Phase 0 search gate: p95 at most 10 ms for every
//! case-sensitive query. The ASCII case-insensitive query is reported but
//! does not count towards the gate. Gate results do not change the exit
//! code.
//!
//! Exit codes: 0 after a completed benchmark, 1 if the benchmark cannot run
//! (thread pool or name column setup fails), 2 for bad arguments.

use std::process::ExitCode;
use std::time::Duration;

use fx_bench::{BenchConfig, BenchReport, CacheState, MachineInfo, generate_names, run};
use fx_query::{CancelToken, CaseMode, NameColumn, SubstringQuery, fold_ascii_lowercase, search};

const USAGE: &str = "\
usage:
  search_bench [--count N] [--seed S] [--threads T] [--warmup W]
               [--iterations K] [--cache warm|cold|unknown] [--json]

  --count N         synthetic names to generate, 1..=100000000 (default 5000000)
  --seed S          generator seed (default 1)
  --threads T       rayon threads for the search, 1..=256 (default 8)
  --warmup W        untimed searches per query before the timed ones
                    (default 10)
  --iterations K    timed searches per query, at least 1 (default 100)
  --cache STATE     cache state label for the report (default unknown)
  --json            print one JSON object instead of text

Gate: p95 <= 10 ms for every case-sensitive query.";

/// Gate: p95 latency of every case-sensitive query.
const GATE_P95: Duration = Duration::from_millis(10);
/// Largest accepted thread count.
const MAX_THREADS: usize = 256;
/// Largest accepted name count (about 1.8 GB of names; offsets are `u32`).
const MAX_COUNT: usize = 100_000_000;

/// The fixed query set: label, needle, case mode.
///
/// `config.rs` joins a word from the generator's vocabulary with the `rs`
/// extension; `zqxj` is chosen to hit almost nothing. The folded query uses
/// mixed case on purpose: it is lowercased and run on the folded copy, and
/// matches both `readme` (a vocabulary word) and `README`.
const QUERIES: &[(&str, &str, CaseMode)] = &[
    ("1-char", "a", CaseMode::Sensitive),
    ("2-char", "re", CaseMode::Sensitive),
    ("3-char", "por", CaseMode::Sensitive),
    ("stem+ext", "config.rs", CaseMode::Sensitive),
    ("rare", "zqxj", CaseMode::Sensitive),
    ("folded", "ReadMe", CaseMode::AsciiInsensitive),
];

struct UsageError(String);

struct Args {
    count: usize,
    seed: u64,
    threads: usize,
    warmup: u32,
    iterations: u32,
    cache: CacheState,
    json: bool,
}

/// Results for one query.
struct Row {
    label: &'static str,
    needle: &'static str,
    case: CaseMode,
    hits: usize,
    report: BenchReport,
}

impl Row {
    fn gated(&self) -> bool {
        self.case == CaseMode::Sensitive
    }
}

/// The gate outcome: the gated row with the worst p95.
struct Gate<'a> {
    pass: bool,
    worst: &'a Row,
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
            eprintln!("search_bench: {msg}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match execute(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("search_bench: error: {msg}");
            ExitCode::from(1)
        }
    }
}

fn parse(args: &[String]) -> Result<Args, UsageError> {
    let mut count: Option<usize> = None;
    let mut seed: Option<u64> = None;
    let mut threads: Option<usize> = None;
    let mut warmup: Option<u32> = None;
    let mut iterations: Option<u32> = None;
    let mut cache: Option<CacheState> = None;
    let mut json = false;

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let flag = arg.as_str();
        let mut value = || {
            it.next()
                .map(String::as_str)
                .ok_or_else(|| UsageError(format!("{flag} needs a value")))
        };
        let duplicate = match flag {
            "--count" => count.replace(number(flag, value()?)?).is_some(),
            "--seed" => seed.replace(number(flag, value()?)?).is_some(),
            "--threads" => threads.replace(number(flag, value()?)?).is_some(),
            "--warmup" => warmup.replace(number(flag, value()?)?).is_some(),
            "--iterations" => iterations.replace(number(flag, value()?)?).is_some(),
            "--cache" => cache.replace(cache_state(value()?)?).is_some(),
            "--json" => std::mem::replace(&mut json, true),
            other if other.starts_with('-') => {
                return Err(UsageError(format!("unknown flag '{other}'")));
            }
            other => return Err(UsageError(format!("unexpected argument '{other}'"))),
        };
        if duplicate {
            return Err(UsageError(format!("{flag} given more than once")));
        }
    }

    let count = count.unwrap_or(5_000_000);
    if count == 0 || count > MAX_COUNT {
        return Err(UsageError(format!(
            "--count must be 1..={MAX_COUNT}, got {count}"
        )));
    }
    let threads = threads.unwrap_or(8);
    if threads == 0 || threads > MAX_THREADS {
        return Err(UsageError(format!(
            "--threads must be 1..={MAX_THREADS}, got {threads}"
        )));
    }
    let iterations = iterations.unwrap_or(100);
    if iterations == 0 {
        return Err(UsageError("--iterations must be at least 1".into()));
    }
    Ok(Args {
        count,
        seed: seed.unwrap_or(1),
        threads,
        warmup: warmup.unwrap_or(10),
        iterations,
        cache: cache.unwrap_or(CacheState::Unknown),
        json,
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

    // Setup, all outside any timing.
    if !args.json {
        eprintln!(
            "search_bench: generating {} names (seed {})...",
            args.count, args.seed
        );
    }
    let names = generate_names(args.count, args.seed);
    let folded = fold_ascii_lowercase(names.buffer());
    let exact = NameColumn::new(names.buffer(), names.offsets())
        .map_err(|e| format!("generated names are not a valid column: {e}"))?;
    let lower = NameColumn::new(&folded, names.offsets())
        .map_err(|e| format!("folded names are not a valid column: {e}"))?;

    let token = CancelToken::never();
    let mut rows = Vec::with_capacity(QUERIES.len());
    for &(label, needle, case) in QUERIES {
        if !args.json {
            eprintln!("search_bench: query {label} {needle:?}...");
        }
        let col = match case {
            CaseMode::Sensitive => exact,
            CaseMode::AsciiInsensitive => lower,
        };
        let query = SubstringQuery::new(needle, case);
        // Hit count from one untimed search; the token is never cancelled.
        let hits = pool
            .install(|| search(col, &query, &token))
            .map_err(|e| format!("query {needle:?}: {e}"))?
            .len();
        let cfg = BenchConfig {
            warmup: args.warmup,
            iterations: args.iterations,
            label: format!("search {label} {needle:?}"),
            cache: args.cache,
        };
        // Each sample is one complete search dispatched onto the pool, as a
        // caller outside the pool would issue it.
        let report = run(cfg, || pool.install(|| search(col, &query, &token)));
        rows.push(Row {
            label,
            needle,
            case,
            hits,
            report,
        });
    }

    let gate = gate(&rows).ok_or("no case-sensitive queries to gate")?;
    let buffer_bytes = names.buffer().len();
    if args.json {
        println!("{}", to_json(args, buffer_bytes, &rows, &gate));
    } else {
        print_text(args, buffer_bytes, &rows, &gate);
    }
    Ok(())
}

fn gate(rows: &[Row]) -> Option<Gate<'_>> {
    let worst = rows
        .iter()
        .filter(|r| r.gated())
        .max_by_key(|r| r.report.stats.p95)?;
    Some(Gate {
        pass: worst.report.stats.p95 <= GATE_P95,
        worst,
    })
}

fn gate_line(args: &Args, gate: &Gate<'_>) -> String {
    format!(
        "gate p95<=10ms over {} names on {} threads: {} (worst p95 {} ms, query {:?})",
        args.count,
        args.threads,
        if gate.pass { "PASS" } else { "FAIL" },
        ms3(gate.worst.report.stats.p95),
        gate.worst.needle
    )
}

fn case_str(case: CaseMode) -> &'static str {
    match case {
        CaseMode::Sensitive => "sensitive",
        CaseMode::AsciiInsensitive => "ascii-insensitive",
    }
}

fn ms_f64(d: Duration) -> f64 {
    d.as_nanos() as f64 / 1_000_000.0
}

fn ms3(d: Duration) -> String {
    format!("{:.3}", ms_f64(d))
}

fn print_text(args: &Args, buffer_bytes: usize, rows: &[Row], gate: &Gate<'_>) {
    let m = MachineInfo::detect();
    println!(
        "search_bench: {} names (seed {}, {:.1} MB buffer), {} threads, cache {}, \
         {} iterations, {} warmup",
        args.count,
        args.seed,
        buffer_bytes as f64 / 1_000_000.0,
        args.threads,
        args.cache,
        args.iterations,
        args.warmup
    );
    println!(
        "machine: {} {}, {} logical CPUs, {}",
        m.os, m.arch, m.parallelism, m.cpu_brand
    );
    println!(
        "{:<9} {:<12} {:<17} {:>9} {:>9} {:>9} {:>9} {:>9}  gate",
        "query", "needle", "case", "hits", "p50_ms", "p95_ms", "p99_ms", "max_ms"
    );
    for r in rows {
        let s = &r.report.stats;
        println!(
            "{:<9} {:<12} {:<17} {:>9} {:>9} {:>9} {:>9} {:>9}  {}",
            r.label,
            format!("{:?}", r.needle),
            case_str(r.case),
            r.hits,
            ms3(s.p50),
            ms3(s.p95),
            ms3(s.p99),
            ms3(s.max),
            if r.gated() { "yes" } else { "info" }
        );
    }
    println!("{}", gate_line(args, gate));
}

fn to_json(args: &Args, buffer_bytes: usize, rows: &[Row], gate: &Gate<'_>) -> String {
    let m = MachineInfo::detect();
    let results: Vec<String> = rows
        .iter()
        .map(|r| {
            let s = &r.report.stats;
            format!(
                "{{\"query\":{},\"needle\":{},\"case\":{},\"gated\":{},\"hits\":{},\
                 \"p50_ms\":{:.6},\"p95_ms\":{:.6},\"p99_ms\":{:.6},\"max_ms\":{:.6},\
                 \"report\":{}}}",
                json_string(r.label),
                json_string(r.needle),
                json_string(case_str(r.case)),
                r.gated(),
                r.hits,
                ms_f64(s.p50),
                ms_f64(s.p95),
                ms_f64(s.p99),
                ms_f64(s.max),
                r.report.to_json()
            )
        })
        .collect();
    format!(
        "{{\"count\":{},\"seed\":{},\"buffer_bytes\":{},\"threads\":{},\"cache\":{},\
         \"iterations\":{},\"warmup\":{},\
         \"machine\":{{\"os\":{},\"arch\":{},\"parallelism\":{},\"cpu_brand\":{}}},\
         \"results\":[{}],\
         \"gate\":{{\"p95_limit_ms\":{:.3},\"status\":{},\"worst_p95_ms\":{:.6},\
         \"worst_query\":{},\"line\":{}}}}}",
        args.count,
        args.seed,
        buffer_bytes,
        args.threads,
        json_string(args.cache.as_str()),
        args.iterations,
        args.warmup,
        json_string(&m.os),
        json_string(&m.arch),
        m.parallelism,
        json_string(&m.cpu_brand),
        results.join(","),
        ms_f64(GATE_P95),
        json_string(if gate.pass { "PASS" } else { "FAIL" }),
        ms_f64(gate.worst.report.stats.p95),
        json_string(gate.worst.needle),
        json_string(&gate_line(args, gate))
    )
}

/// Encodes `s` as a JSON string literal, escaping quotes, backslashes and
/// control characters.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
