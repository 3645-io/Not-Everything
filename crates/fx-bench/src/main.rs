//! `fx-bench`: generate synthetic names and trees, write a tree to disk, and
//! time the generator and a naive substring scan.
//!
//! Also summarises benchmark logs: `gate-summary` collects their `GATE`
//! lines into one results table, and `phase1-summary` turns a directory of
//! Phase 1 benchmark results into `summary.md`.
//!
//! Argument parsing is hand-written (std only). Bad arguments print usage
//! to stderr and exit with code 2; I/O failures exit with code 1, except in
//! `gate-summary` and `phase1-summary`, where an unreadable input exits
//! with code 2 and code 1 means a gate did not pass.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use fx_bench::{
    BenchConfig, CacheState, MachineInfo, Names, TreeShape, gate, gate1, generate_names,
    generate_tree, materialize, run,
};

const USAGE: &str = "\
usage:
  fx-bench gen --count N --seed S [--tree]
      Generate N names (or a tree of N entries with --tree) from seed S and
      print count, total bytes, mean name length, non-ASCII count, dir count
      (tree), FNV-1a checksum, generation time (generator only) and
      checksum time.
  fx-bench materialize --count N --seed S --out DIR
      Create the tree of N entries from seed S under DIR, which must be an
      existing empty directory. Only creates entries; never deletes.
  fx-bench self-bench [--count N] [--iterations K] [--json] [--cache warm|cold|unknown]
      Time name generation and a naive str::contains scan over the names,
      and print a p50/p95 report per benchmark (defaults: N=1000000, K=10,
      cache unknown).
  fx-bench gate-summary --machine LABEL --cache warm|cold|unknown [--format md|json] LOG...
      Collect the GATE lines that the Phase 0 benchmarks print from each LOG
      file and print one results table (markdown by default, or JSON), one
      row per gate line, or a MISSING row for a log with none. Exits 0 when
      every row is PASS, 1 when any row is FAIL, SKIP, UNPARSED or MISSING,
      and 2 for bad arguments or an unreadable LOG. Only reads files.
  fx-bench phase1-summary --machine LABEL --cache warm|cold|unknown DIR
      Read the Phase 1 benchmark results in DIR (key=value or JSON lines,
      every regular file except summary.md), evaluate each Phase 1 gate
      criterion (PASS, FAIL or NOT MEASURED), write DIR/summary.md and print
      it. With --cache cold, Goal 1 launches are judged by the slowest one.
      A row measured below its gate scale (e.g. fewer than 2M entries or 20
      warm launches) is NOT MEASURED, marked 'below gate scale'. Exits 0 when every criterion passes, 1 when the summary was
      written but some criterion failed or was not measured, and 2 for bad
      arguments, an unreadable DIR or a failed write.
  fx-bench help
      Print this message.";

/// Default name count for `self-bench`.
const DEFAULT_BENCH_COUNT: usize = 1_000_000;
/// Default timed iterations for `self-bench`.
const DEFAULT_BENCH_ITERATIONS: u32 = 10;
/// Seed used by `self-bench`.
const BENCH_SEED: u64 = 1;
/// Needle for the `self-bench` substring scan.
const SCAN_NEEDLE: &str = "re";

/// An argument error: reported with the usage text and exit code 2.
struct UsageError(String);

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse(&args) {
        Ok(Command::Help) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Command::GateSummary {
            machine,
            cache,
            format,
            logs,
        }) => cmd_gate_summary(&machine, cache, format, &logs),
        Ok(Command::Phase1Summary {
            machine,
            cache,
            dir,
        }) => cmd_phase1_summary(&machine, cache, &dir),
        Ok(cmd) => match execute(cmd) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("fx-bench: error: {msg}");
                ExitCode::from(1)
            }
        },
        Err(UsageError(msg)) => {
            eprintln!("fx-bench: {msg}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

enum Command {
    Help,
    Gen {
        count: usize,
        seed: u64,
        tree: bool,
    },
    Materialize {
        count: usize,
        seed: u64,
        out: PathBuf,
    },
    SelfBench {
        count: usize,
        iterations: u32,
        json: bool,
        cache: CacheState,
    },
    GateSummary {
        machine: String,
        cache: CacheState,
        format: Format,
        logs: Vec<PathBuf>,
    },
    Phase1Summary {
        machine: String,
        cache: CacheState,
        dir: PathBuf,
    },
}

/// Output format of `gate-summary`.
#[derive(Clone, Copy)]
enum Format {
    Markdown,
    Json,
}

/// Flags seen after a subcommand. Each flag may appear at most once.
#[derive(Default)]
struct Flags {
    count: Option<usize>,
    seed: Option<u64>,
    iterations: Option<u32>,
    out: Option<PathBuf>,
    cache: Option<CacheState>,
    machine: Option<String>,
    format: Option<Format>,
    tree: bool,
    json: bool,
    /// Positional arguments; only `gate-summary` accepts them.
    positional: Vec<PathBuf>,
}

fn parse(args: &[String]) -> Result<Command, UsageError> {
    let Some((sub, rest)) = args.split_first() else {
        return Err(UsageError("missing subcommand".into()));
    };
    let allowed: &[&str] = match sub.as_str() {
        "help" | "--help" | "-h" => {
            if let Some(extra) = rest.first() {
                return Err(UsageError(format!("unexpected argument '{extra}'")));
            }
            return Ok(Command::Help);
        }
        "gen" => &["--count", "--seed", "--tree"],
        "materialize" => &["--count", "--seed", "--out"],
        "self-bench" => &["--count", "--iterations", "--json", "--cache"],
        "gate-summary" => &["--machine", "--cache", "--format"],
        "phase1-summary" => &["--machine", "--cache"],
        other => return Err(UsageError(format!("unknown subcommand '{other}'"))),
    };
    let positional = matches!(sub.as_str(), "gate-summary" | "phase1-summary");
    let flags = parse_flags(sub, rest, allowed, positional)?;

    let required = |name: &str, present: bool| {
        if present {
            Ok(())
        } else {
            Err(UsageError(format!("{sub}: missing required {name}")))
        }
    };
    match sub.as_str() {
        "gen" => {
            required("--count", flags.count.is_some())?;
            required("--seed", flags.seed.is_some())?;
            let count = flags.count.unwrap_or_default();
            if flags.tree {
                check_tree_count(sub, count)?;
            }
            Ok(Command::Gen {
                count,
                seed: flags.seed.unwrap_or_default(),
                tree: flags.tree,
            })
        }
        "materialize" => {
            required("--count", flags.count.is_some())?;
            required("--seed", flags.seed.is_some())?;
            let Some(out) = flags.out else {
                return Err(UsageError(format!("{sub}: missing required --out")));
            };
            let count = flags.count.unwrap_or_default();
            check_tree_count(sub, count)?;
            Ok(Command::Materialize {
                count,
                seed: flags.seed.unwrap_or_default(),
                out,
            })
        }
        "gate-summary" => {
            let Some(machine) = flags.machine else {
                return Err(UsageError(format!("{sub}: missing required --machine")));
            };
            let Some(cache) = flags.cache else {
                return Err(UsageError(format!("{sub}: missing required --cache")));
            };
            if flags.positional.is_empty() {
                return Err(UsageError(format!("{sub}: no log files given")));
            }
            Ok(Command::GateSummary {
                machine,
                cache,
                format: flags.format.unwrap_or(Format::Markdown),
                logs: flags.positional,
            })
        }
        "phase1-summary" => {
            let Some(machine) = flags.machine else {
                return Err(UsageError(format!("{sub}: missing required --machine")));
            };
            let Some(cache) = flags.cache else {
                return Err(UsageError(format!("{sub}: missing required --cache")));
            };
            let mut positional = flags.positional.into_iter();
            let (Some(dir), None) = (positional.next(), positional.next()) else {
                return Err(UsageError(format!(
                    "{sub}: expects exactly one results directory"
                )));
            };
            Ok(Command::Phase1Summary {
                machine,
                cache,
                dir,
            })
        }
        _ => {
            let iterations = flags.iterations.unwrap_or(DEFAULT_BENCH_ITERATIONS);
            if iterations == 0 {
                return Err(UsageError(format!(
                    "{sub}: --iterations must be at least 1"
                )));
            }
            Ok(Command::SelfBench {
                count: flags.count.unwrap_or(DEFAULT_BENCH_COUNT),
                iterations,
                json: flags.json,
                cache: flags.cache.unwrap_or(CacheState::Unknown),
            })
        }
    }
}

fn check_tree_count(sub: &str, count: usize) -> Result<(), UsageError> {
    if count >= u32::MAX as usize {
        return Err(UsageError(format!(
            "{sub}: --count must be below {} for trees",
            u32::MAX
        )));
    }
    Ok(())
}

fn parse_flags(
    sub: &str,
    rest: &[String],
    allowed: &[&str],
    positional: bool,
) -> Result<Flags, UsageError> {
    let mut flags = Flags::default();
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        let flag = arg.as_str();
        if positional && !flag.starts_with('-') {
            flags.positional.push(PathBuf::from(flag));
            continue;
        }
        if !allowed.contains(&flag) {
            return Err(if flag.starts_with('-') {
                UsageError(format!("{sub}: unknown flag '{flag}'"))
            } else {
                UsageError(format!("{sub}: unexpected argument '{flag}'"))
            });
        }
        let mut value = || {
            it.next()
                .map(String::as_str)
                .ok_or_else(|| UsageError(format!("{sub}: {flag} needs a value")))
        };
        let duplicate = match flag {
            "--count" => flags.count.replace(number(sub, flag, value()?)?).is_some(),
            "--seed" => flags.seed.replace(number(sub, flag, value()?)?).is_some(),
            "--iterations" => flags
                .iterations
                .replace(number(sub, flag, value()?)?)
                .is_some(),
            "--out" => flags.out.replace(PathBuf::from(value()?)).is_some(),
            "--cache" => flags.cache.replace(cache_state(sub, value()?)?).is_some(),
            "--machine" => flags.machine.replace(label(sub, flag, value()?)?).is_some(),
            "--format" => flags.format.replace(format(sub, value()?)?).is_some(),
            "--tree" => std::mem::replace(&mut flags.tree, true),
            "--json" => std::mem::replace(&mut flags.json, true),
            _ => unreachable!("flag checked against the allowed list"),
        };
        if duplicate {
            return Err(UsageError(format!("{sub}: {flag} given more than once")));
        }
    }
    Ok(flags)
}

/// Parses a non-negative decimal integer; `_` separators are allowed.
fn number<T: std::str::FromStr>(sub: &str, flag: &str, text: &str) -> Result<T, UsageError> {
    let digits: String = text.chars().filter(|&c| c != '_').collect();
    let valid = !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit());
    valid.then(|| digits.parse().ok()).flatten().ok_or_else(|| {
        UsageError(format!(
            "{sub}: {flag} expects a non-negative integer, got '{text}'"
        ))
    })
}

fn cache_state(sub: &str, text: &str) -> Result<CacheState, UsageError> {
    match text {
        "warm" => Ok(CacheState::Warm),
        "cold" => Ok(CacheState::Cold),
        "unknown" => Ok(CacheState::Unknown),
        other => Err(UsageError(format!(
            "{sub}: --cache expects warm, cold or unknown, got '{other}'"
        ))),
    }
}

/// A non-empty label (leading and trailing whitespace removed).
fn label(sub: &str, flag: &str, text: &str) -> Result<String, UsageError> {
    let t = text.trim();
    if t.is_empty() {
        return Err(UsageError(format!("{sub}: {flag} must not be empty")));
    }
    Ok(t.to_string())
}

fn format(sub: &str, text: &str) -> Result<Format, UsageError> {
    match text {
        "md" => Ok(Format::Markdown),
        "json" => Ok(Format::Json),
        other => Err(UsageError(format!(
            "{sub}: --format expects md or json, got '{other}'"
        ))),
    }
}

/// `gate-summary`: reads every log (read-only), prints the table and maps
/// the outcome to an exit code (0 all pass, 1 otherwise, 2 unreadable log).
fn cmd_gate_summary(machine: &str, cache: CacheState, fmt: Format, logs: &[PathBuf]) -> ExitCode {
    let mut rows = Vec::new();
    for path in logs {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!(
                    "fx-bench: gate-summary: cannot read '{}': {e}",
                    path.display()
                );
                return ExitCode::from(2);
            }
        };
        let text = String::from_utf8_lossy(&bytes);
        rows.extend(gate::summarize(&source_name(path), &text, machine, cache));
    }
    match fmt {
        Format::Markdown => print!("{}", gate::to_markdown(&rows)),
        Format::Json => println!("{}", gate::to_json(&rows)),
    }
    if gate::all_pass(&rows) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// `phase1-summary`: reads the results in `dir`, writes `dir/summary.md`,
/// prints it and maps the outcome to an exit code (0 all pass, 1 otherwise,
/// 2 unreadable directory or failed write).
fn cmd_phase1_summary(machine: &str, cache: CacheState, dir: &Path) -> ExitCode {
    let results = match gate1::read_results_dir(dir) {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "fx-bench: phase1-summary: cannot read results directory '{}': {e}",
                dir.display()
            );
            return ExitCode::from(2);
        }
    };
    let rows = gate1::evaluate_gate(&results, cache);
    let md = gate1::render_summary(&rows, machine, cache);
    let out = dir.join(gate1::SUMMARY_FILE);
    if let Err(e) = std::fs::write(&out, &md) {
        eprintln!(
            "fx-bench: phase1-summary: cannot write '{}': {e}",
            out.display()
        );
        return ExitCode::from(2);
    }
    print!("{md}");
    if gate1::all_pass(&rows) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// The log's file name, or the whole path when it has none.
fn source_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn execute(cmd: Command) -> Result<(), String> {
    match cmd {
        Command::Help => {
            println!("{USAGE}");
            Ok(())
        }
        Command::Gen { count, seed, tree } => {
            cmd_gen(count, seed, tree);
            Ok(())
        }
        Command::Materialize { count, seed, out } => cmd_materialize(count, seed, out),
        Command::SelfBench {
            count,
            iterations,
            json,
            cache,
        } => {
            cmd_self_bench(count, iterations, json, cache);
            Ok(())
        }
        Command::GateSummary { .. } | Command::Phase1Summary { .. } => {
            unreachable!("summary commands set their own exit code and are run from main")
        }
    }
}

fn cmd_gen(count: usize, seed: u64, tree: bool) {
    // In both branches the generation timer covers generation only: it is
    // read as soon as the generator returns, before any checksum or stats
    // work. The checksum is timed on its own and reported separately.
    let (names, dirs, checksum, elapsed, checksum_time) = if tree {
        let start = Instant::now();
        let t = generate_tree(count, seed, TreeShape::default());
        let elapsed = start.elapsed();

        let sum_start = Instant::now();
        let sum = tree_checksum(&t);
        let checksum_time = sum_start.elapsed();

        let dirs = t.dir_count();
        (t.names, Some(dirs), sum, elapsed, checksum_time)
    } else {
        let start = Instant::now();
        let names = generate_names(count, seed);
        let elapsed = start.elapsed();

        let sum_start = Instant::now();
        let sum = names.fnv1a64();
        let checksum_time = sum_start.elapsed();

        (names, None, sum, elapsed, checksum_time)
    };

    let what = if tree { "tree entries" } else { "names" };
    println!("{what}: {}", names.len());
    println!("seed: {seed}");
    println!("total bytes: {}", names.total_bytes());
    println!("mean name length: {} B", mean_len(&names));
    println!(
        "non-ASCII names: {}",
        names.iter().filter(|n| !n.is_ascii()).count()
    );
    if let Some(dirs) = dirs {
        println!("directories: {dirs}");
        println!("checksum (FNV-1a 64, names + structure): {checksum:016x}");
    } else {
        println!("checksum (FNV-1a 64): {checksum:016x}");
    }
    println!("generation time: {} ms", ms3(elapsed));
    println!("checksum time: {} ms", ms3(checksum_time));
}

/// FNV-1a 64 over the names buffer, then each parent index (little-endian
/// `u32`) and each directory flag (one byte). FNV-1a's state is its hash,
/// so this continues the names checksum.
fn tree_checksum(tree: &fx_bench::Tree) -> u64 {
    let mut h = tree.names.fnv1a64();
    let mut feed = |bytes: &[u8]| {
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for &p in &tree.parent {
        feed(&p.to_le_bytes());
    }
    for &d in &tree.is_dir {
        feed(&[u8::from(d)]);
    }
    h
}

/// Mean name length in bytes with two decimals, computed in integers.
fn mean_len(names: &Names) -> String {
    if names.is_empty() {
        return "0.00".into();
    }
    let x100 = (names.total_bytes() as u128 * 100 + names.len() as u128 / 2) / names.len() as u128;
    format!("{}.{:02}", x100 / 100, x100 % 100)
}

fn ms3(d: Duration) -> String {
    format!("{:.3}", d.as_nanos() as f64 / 1_000_000.0)
}

fn cmd_materialize(count: usize, seed: u64, out: PathBuf) -> Result<(), String> {
    let gen_start = Instant::now();
    let tree = generate_tree(count, seed, TreeShape::default());
    let gen_time = gen_start.elapsed();

    let start = Instant::now();
    let stats = materialize(&tree, &out, None).map_err(|e| e.to_string())?;
    let elapsed = start.elapsed();

    println!("output: {}", out.display());
    println!("seed: {seed}");
    println!("entries: {}", stats.entries);
    println!("directories: {}", stats.dirs);
    println!("files: {}", stats.files);
    println!("generation time: {} ms", ms3(gen_time));
    println!("materialize time: {} ms", ms3(elapsed));
    Ok(())
}

fn cmd_self_bench(count: usize, iterations: u32, json: bool, cache: CacheState) {
    let cfg = |label: String| BenchConfig {
        warmup: 1,
        iterations,
        label,
        cache,
    };

    let gen_report = run(
        cfg(format!("generate_names {count} (seed {BENCH_SEED})")),
        || generate_names(count, BENCH_SEED),
    );

    let names = generate_names(count, BENCH_SEED);
    let mut matches = 0;
    let scan_report = run(
        cfg(format!("str::contains {SCAN_NEEDLE:?} over {count} names")),
        || {
            matches = names
                .iter()
                .filter(|n| std::hint::black_box(n).contains(SCAN_NEEDLE))
                .count();
            matches
        },
    );

    let reports = [gen_report, scan_report];
    if json {
        let body: Vec<String> = reports.iter().map(|r| r.to_json()).collect();
        println!("[{}]", body.join(","));
    } else {
        let m = MachineInfo::detect();
        println!(
            "machine: {} {}, {} threads, {}; cache: {cache}",
            m.os, m.arch, m.parallelism, m.cpu_brand
        );
        for r in &reports {
            println!("{}", r.to_text());
        }
        println!("scan matches: {matches}");
    }
}
