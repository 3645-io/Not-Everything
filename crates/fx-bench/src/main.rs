//! `fx-bench`: generate synthetic names and trees, write a tree to disk, and
//! time the generator and a naive substring scan.
//!
//! Also summarises benchmark logs: `gate-summary` collects their `GATE`
//! lines into one results table, and `phase1-summary` turns a directory of
//! Phase 1 benchmark results into `summary.md`.
//!
//! For the performance gate: `sample` records a process's resource use
//! (macOS only), `churn` keeps files changing at a steady rate, and
//! `perf-compare` judges metrics against a baseline file, and
//! `perf-release` writes the performance section of a release's notes.
//!
//! Argument parsing is hand-written (std only). Bad arguments print usage
//! to stderr and exit with code 2; I/O failures exit with code 1, except in
//! `gate-summary`, `phase1-summary`, `perf-compare` and `perf-release`, where an unreadable
//! input exits with code 2 and code 1 means a gate did not pass. `sample`
//! and `churn` also exit with code 2 when their new folder already exists
//! or has no parent.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use fx_bench::{
    BenchConfig, CacheState, MachineInfo, Names, TreeShape, churn, gate, gate1, generate_names,
    generate_tree, materialize, perf, release, run,
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
  fx-bench sample --pid PID --duration SECS --out NEW_DIR [--interval SECS] [--label LABEL] [--burst-pct PCT]
      macOS only. Read process PID's resource counters every --interval
      seconds (default 10) for SECS seconds, or until it exits. Writes
      rusage.csv, intervals.csv and metrics.txt into NEW_DIR, which must not
      exist, and prints the metrics as LABEL_<metric>=<value> lines (default
      label: app). When an interval's CPU passes PCT percent of one core
      (default 150), records the stacks with /usr/bin/sample into
      NEW_DIR/burst-<n>.txt, at most 3 times. Exits 2 for bad arguments or
      an existing NEW_DIR, and 1 when the process can't be read.
  fx-bench churn --dir NEW_DIR --duration SECS [--rate N] [--seed S] [--max-files N]
      Create NEW_DIR (its parent must exist, it must not) with 16 folders,
      then create, rename and delete files in it at N operations per second
      (default 20) for SECS seconds, seeded by S (default 1), with at most
      --max-files files (default 5000). Touches nothing outside NEW_DIR and
      leaves it in place. Prints churn_ops, churn_seconds and churn_rate.
      Exits 2 for bad arguments or an existing NEW_DIR, 1 for I/O errors.
  fx-bench perf-compare --baseline FILE --tier quick|pr|rc [--format md|json] [--out FILE] [--print-values] [--partial] METRICS_FILE...
      Judge each metric of the tier in the baseline FILE against the
      key=value METRICS_FILEs: PASS, FAIL (target or regression), NOT
      MEASURED, NO BASELINE, TRACKED (ungated) or SKIPPED, then a
      'perf-compare:' count line. --out also writes the report to a new
      file; --print-values adds '<ID> <key> = <value>' lines for the
      measured metrics (markdown only); --partial shows a metric absent from
      every file as SKIPPED instead of NOT MEASURED. Exits 0 when no gated
      metric is FAIL or NOT MEASURED, 1 otherwise, and 2 for bad arguments,
      an unreadable or malformed file, or an existing --out file.
  fx-bench perf-release --baseline FILE --previous FILE --previous-label LABEL --label LABEL [--notes FILE] [--out FILE] METRICS_FILE...
      Print the performance section of a release's notes: LABEL's metrics
      (the METRICS_FILEs) against the previous release's (--previous) and the
      targets in the baseline FILE, as a summary table of the [meta] table's
      'summary' IDs, the --notes FILE's lines, then every metric in a folded
      <details> table, each row marked with coloured dots. --out also writes
      it to a new file. Exits 0, or 2 for bad arguments, an unreadable or
      malformed file, a baseline without a 'summary', or an existing --out.
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
/// Default `sample --interval`, in seconds.
const DEFAULT_SAMPLE_INTERVAL_S: u64 = 10;
/// Default `sample --label`.
const DEFAULT_SAMPLE_LABEL: &str = "app";
/// Default `sample --burst-pct`: one and a half cores.
const DEFAULT_BURST_PCT: f64 = 150.0;
/// Default `churn --rate`, operations per second.
const DEFAULT_CHURN_RATE: f64 = 20.0;
/// Default `churn --seed`.
const DEFAULT_CHURN_SEED: u64 = 1;
/// Default `churn --max-files`.
const DEFAULT_CHURN_MAX_FILES: usize = 5000;
/// The longest `--duration` or `--interval` accepted: a year, so adding it
/// to the clock can't overflow.
const MAX_SECONDS: f64 = 365.0 * 86_400.0;

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
        Ok(Command::Sample(args)) => cmd_sample(args),
        Ok(Command::Churn { dir, opts }) => cmd_churn(&dir, &opts),
        Ok(Command::PerfCompare(args)) => cmd_perf_compare(&args),
        Ok(Command::PerfRelease(args)) => cmd_perf_release(&args),
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
    Sample(SampleArgs),
    Churn {
        dir: PathBuf,
        opts: churn::Options,
    },
    PerfCompare(PerfCompareArgs),
    PerfRelease(PerfReleaseArgs),
}

/// The arguments of `sample`. They are only read on macOS, where the
/// command exists.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct SampleArgs {
    pid: i32,
    duration: Duration,
    interval: Duration,
    label: String,
    burst_pct: f64,
    out: PathBuf,
}

/// The arguments of `perf-compare`.
struct PerfCompareArgs {
    baseline: PathBuf,
    tier: perf::Tier,
    format: Format,
    out: Option<PathBuf>,
    print_values: bool,
    partial: bool,
    metrics: Vec<PathBuf>,
}

/// The arguments of `perf-release`.
struct PerfReleaseArgs {
    baseline: PathBuf,
    previous: PathBuf,
    previous_label: String,
    label: String,
    notes: Option<PathBuf>,
    out: Option<PathBuf>,
    metrics: Vec<PathBuf>,
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
    pid: Option<i32>,
    duration: Option<Duration>,
    interval: Option<Duration>,
    label: Option<String>,
    burst_pct: Option<f64>,
    dir: Option<PathBuf>,
    rate: Option<f64>,
    max_files: Option<usize>,
    baseline: Option<PathBuf>,
    tier: Option<perf::Tier>,
    print_values: bool,
    partial: bool,
    previous: Option<PathBuf>,
    previous_label: Option<String>,
    /// `perf-release --label`: a release's name, such as `0.4.0-RC-1`.
    release_label: Option<String>,
    notes: Option<PathBuf>,
    /// Positional arguments; only `gate-summary`, `phase1-summary` and
    /// `perf-compare` accept them.
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
        "sample" => &[
            "--pid",
            "--duration",
            "--out",
            "--interval",
            "--label",
            "--burst-pct",
        ],
        "churn" => &["--dir", "--duration", "--rate", "--seed", "--max-files"],
        "perf-compare" => &[
            "--baseline",
            "--tier",
            "--format",
            "--out",
            "--print-values",
            "--partial",
        ],
        "perf-release" => &[
            "--baseline",
            "--previous",
            "--previous-label",
            "--label",
            "--notes",
            "--out",
        ],
        other => return Err(UsageError(format!("unknown subcommand '{other}'"))),
    };
    let positional = matches!(
        sub.as_str(),
        "gate-summary" | "phase1-summary" | "perf-compare" | "perf-release"
    );
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
        "sample" => {
            let (Some(pid), Some(duration), Some(out)) = (flags.pid, flags.duration, flags.out)
            else {
                return Err(UsageError(format!(
                    "{sub}: --pid, --duration and --out are required"
                )));
            };
            Ok(Command::Sample(SampleArgs {
                pid,
                duration,
                interval: flags
                    .interval
                    .unwrap_or(Duration::from_secs(DEFAULT_SAMPLE_INTERVAL_S)),
                label: flags
                    .label
                    .unwrap_or_else(|| DEFAULT_SAMPLE_LABEL.to_string()),
                burst_pct: flags.burst_pct.unwrap_or(DEFAULT_BURST_PCT),
                out,
            }))
        }
        "churn" => {
            let (Some(dir), Some(duration)) = (flags.dir, flags.duration) else {
                return Err(UsageError(format!(
                    "{sub}: --dir and --duration are required"
                )));
            };
            Ok(Command::Churn {
                dir,
                opts: churn::Options {
                    seed: flags.seed.unwrap_or(DEFAULT_CHURN_SEED),
                    rate: flags.rate.unwrap_or(DEFAULT_CHURN_RATE),
                    duration,
                    max_files: flags.max_files.unwrap_or(DEFAULT_CHURN_MAX_FILES),
                },
            })
        }
        "perf-compare" => {
            let (Some(baseline), Some(tier)) = (flags.baseline, flags.tier) else {
                return Err(UsageError(format!(
                    "{sub}: --baseline and --tier are required"
                )));
            };
            if flags.positional.is_empty() {
                return Err(UsageError(format!("{sub}: no metrics files given")));
            }
            let format = flags.format.unwrap_or(Format::Markdown);
            if flags.print_values && matches!(format, Format::Json) {
                return Err(UsageError(format!(
                    "{sub}: --print-values is for the markdown report; the JSON report \
                     has each value as 'now'"
                )));
            }
            Ok(Command::PerfCompare(PerfCompareArgs {
                baseline,
                tier,
                format,
                out: flags.out,
                print_values: flags.print_values,
                partial: flags.partial,
                metrics: flags.positional,
            }))
        }
        "perf-release" => {
            let (Some(baseline), Some(previous), Some(previous_label), Some(label)) = (
                flags.baseline,
                flags.previous,
                flags.previous_label,
                flags.release_label,
            ) else {
                return Err(UsageError(format!(
                    "{sub}: --baseline, --previous, --previous-label and --label are required"
                )));
            };
            if flags.positional.is_empty() {
                return Err(UsageError(format!("{sub}: no metrics files given")));
            }
            Ok(Command::PerfRelease(PerfReleaseArgs {
                baseline,
                previous,
                previous_label,
                label,
                notes: flags.notes,
                out: flags.out,
                metrics: flags.positional,
            }))
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
            "--pid" => flags.pid.replace(pid(sub, value()?)?).is_some(),
            "--duration" => flags
                .duration
                .replace(seconds(sub, flag, value()?)?)
                .is_some(),
            "--interval" => flags
                .interval
                .replace(seconds(sub, flag, value()?)?)
                .is_some(),
            // A release's name has dots and capitals; a sample label is a
            // metric-key prefix.
            "--label" if sub == "perf-release" => flags
                .release_label
                .replace(label(sub, flag, value()?)?)
                .is_some(),
            "--label" => flags.label.replace(metric_label(sub, value()?)?).is_some(),
            "--previous" => flags.previous.replace(PathBuf::from(value()?)).is_some(),
            "--previous-label" => flags
                .previous_label
                .replace(label(sub, flag, value()?)?)
                .is_some(),
            "--notes" => flags.notes.replace(PathBuf::from(value()?)).is_some(),
            "--burst-pct" => flags
                .burst_pct
                .replace(positive(sub, flag, value()?)?)
                .is_some(),
            "--dir" => flags.dir.replace(PathBuf::from(value()?)).is_some(),
            "--rate" => flags.rate.replace(rate(sub, value()?)?).is_some(),
            "--max-files" => {
                let n: usize = number(sub, flag, value()?)?;
                if n == 0 {
                    return Err(UsageError(format!("{sub}: --max-files must be at least 1")));
                }
                flags.max_files.replace(n).is_some()
            }
            "--baseline" => flags.baseline.replace(PathBuf::from(value()?)).is_some(),
            "--tier" => flags.tier.replace(tier(sub, value()?)?).is_some(),
            "--print-values" => std::mem::replace(&mut flags.print_values, true),
            "--partial" => std::mem::replace(&mut flags.partial, true),
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

/// Parses a positive decimal number such as `2`, `0.5` or `150`.
fn positive(sub: &str, flag: &str, text: &str) -> Result<f64, UsageError> {
    let digits = text.bytes().filter(u8::is_ascii_digit).count();
    let dots = text.bytes().filter(|&b| b == b'.').count();
    let valid = digits > 0 && digits + dots == text.len() && dots <= 1;
    valid
        .then(|| text.parse::<f64>().ok())
        .flatten()
        .filter(|v| v.is_finite() && *v > 0.0)
        .ok_or_else(|| {
            UsageError(format!(
                "{sub}: {flag} expects a positive number, got '{text}'"
            ))
        })
}

/// A positive number of seconds, at most [`MAX_SECONDS`].
fn seconds(sub: &str, flag: &str, text: &str) -> Result<Duration, UsageError> {
    let s = positive(sub, flag, text)?;
    if s > MAX_SECONDS {
        return Err(UsageError(format!(
            "{sub}: {flag} must be at most {MAX_SECONDS} seconds"
        )));
    }
    Ok(Duration::from_secs_f64(s))
}

/// A process id: a positive integer.
fn pid(sub: &str, text: &str) -> Result<i32, UsageError> {
    let p: i32 = number(sub, "--pid", text)?;
    if p == 0 {
        return Err(UsageError(format!(
            "{sub}: --pid must be a positive integer"
        )));
    }
    Ok(p)
}

/// The metric-key prefix of `sample`: `[a-z0-9_]`, as `perf-compare`
/// reads keys.
fn metric_label(sub: &str, text: &str) -> Result<String, UsageError> {
    let valid = !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if !valid {
        return Err(UsageError(format!(
            "{sub}: --label must be made of a-z, 0-9 and '_', got '{text}'"
        )));
    }
    Ok(text.to_string())
}

/// Operations per second: positive, with a tick that fits a `Duration`.
fn rate(sub: &str, text: &str) -> Result<f64, UsageError> {
    let r = positive(sub, "--rate", text)?;
    if Duration::try_from_secs_f64(1.0 / r).is_err() {
        return Err(UsageError(format!("{sub}: --rate {text} is out of range")));
    }
    Ok(r)
}

fn tier(sub: &str, text: &str) -> Result<perf::Tier, UsageError> {
    perf::Tier::parse(text).ok_or_else(|| {
        UsageError(format!(
            "{sub}: --tier expects quick, pr or rc, got '{text}'"
        ))
    })
}

/// Refuses a new folder that already exists (even as a symlink) or whose
/// parent isn't an existing folder, so `sample` and `churn` only ever write
/// into a folder they made themselves.
fn check_new_dir(dir: &Path) -> Result<(), String> {
    if fs::symlink_metadata(dir).is_ok() {
        return Err(format!(
            "'{}' already exists; give a folder that doesn't exist yet",
            dir.display()
        ));
    }
    let parent = match dir.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    if !parent.is_dir() {
        return Err(format!(
            "the parent of '{}' is not an existing folder",
            dir.display()
        ));
    }
    Ok(())
}

/// `sample`: refuses an existing NEW_DIR (exit 2), samples the process and
/// prints its metric lines (exit 0), or reports an I/O error (exit 1).
#[cfg(target_os = "macos")]
fn cmd_sample(args: SampleArgs) -> ExitCode {
    if let Err(msg) = check_new_dir(&args.out) {
        eprintln!("fx-bench: sample: {msg}");
        return ExitCode::from(2);
    }
    let opts = fx_bench::sample::Options {
        pid: args.pid,
        duration: args.duration,
        interval: args.interval,
        label: args.label,
        burst_pct: args.burst_pct,
        out: args.out,
    };
    let mut log = |msg: &str| eprintln!("fx-bench: sample: {msg}");
    match fx_bench::sample::run(&opts, &mut log) {
        Ok(summary) => {
            for line in fx_bench::sample::metric_lines(&opts.label, &summary) {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("fx-bench: sample: {e}");
            ExitCode::from(1)
        }
    }
}

/// `sample` reads macOS's per-process counters; elsewhere it exits 2.
#[cfg(not(target_os = "macos"))]
fn cmd_sample(_args: SampleArgs) -> ExitCode {
    eprintln!("fx-bench: sample: only available on macOS (it reads proc_pid_rusage)");
    ExitCode::from(2)
}

/// `churn`: refuses an existing or parentless NEW_DIR (exit 2), runs the
/// stream and prints `churn_ops`, `churn_seconds` and `churn_rate` (exit
/// 0), or reports an I/O error (exit 1).
fn cmd_churn(dir: &Path, opts: &churn::Options) -> ExitCode {
    if let Err(msg) = check_new_dir(dir) {
        eprintln!("fx-bench: churn: {msg}");
        return ExitCode::from(2);
    }
    match churn::run(dir, opts) {
        Ok(stats) => {
            println!("churn_ops={}", stats.ops);
            println!("churn_seconds={:.3}", stats.seconds);
            println!("churn_rate={:.3}", stats.rate());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("fx-bench: churn: {e}");
            ExitCode::from(1)
        }
    }
}

/// `perf-compare`: reads the baseline and metrics files, prints the report
/// (and writes it to `--out`), and maps the outcome to an exit code: 0 when
/// no gated metric is FAIL or NOT MEASURED, 1 otherwise, 2 for an
/// unreadable or malformed file, an existing `--out` or a failed write.
fn cmd_perf_compare(args: &PerfCompareArgs) -> ExitCode {
    let fail = |msg: String| {
        eprintln!("fx-bench: perf-compare: {msg}");
        ExitCode::from(2)
    };
    if let Some(out) = &args.out {
        if fs::symlink_metadata(out).is_ok() {
            return fail(format!(
                "'{}' already exists; --out writes a new file",
                out.display()
            ));
        }
    }
    let read = |path: &Path| {
        fs::read_to_string(path).map_err(|e| format!("cannot read '{}': {e}", path.display()))
    };
    let baseline = match read(&args.baseline).and_then(|text| {
        perf::parse_baseline(&text, &args.baseline.display().to_string()).map_err(|e| e.to_string())
    }) {
        Ok(b) => b,
        Err(msg) => return fail(msg),
    };
    let mut samples = perf::Samples::default();
    for path in &args.metrics {
        let parsed = read(path).and_then(|text| {
            perf::parse_metrics(&text, &path.display().to_string(), &mut samples)
                .map_err(|e| e.to_string())
        });
        if let Err(msg) = parsed {
            return fail(msg);
        }
    }
    let rows = perf::compare(&baseline, &samples, args.tier, args.partial);
    let report = match args.format {
        Format::Markdown => perf::render_markdown(&baseline.meta, args.tier, &rows),
        Format::Json => format!("{}\n", perf::render_json(&baseline.meta, args.tier, &rows)),
    };
    if let Some(out) = &args.out {
        let written = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(out)
            .and_then(|mut f| std::io::Write::write_all(&mut f, report.as_bytes()));
        if let Err(e) = written {
            return fail(format!("cannot write '{}': {e}", out.display()));
        }
    }
    print!("{report}");
    if args.print_values {
        println!();
        for line in perf::value_lines(&rows) {
            println!("{line}");
        }
    }
    if perf::gate_passes(&rows) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// `perf-release`: reads the baseline, the previous release's metrics,
/// this release's metrics and the notes, then prints the section (and
/// writes it to `--out`). Exits 2 for an unreadable or malformed file, a
/// baseline without a `summary`, an existing `--out` or a failed write.
fn cmd_perf_release(args: &PerfReleaseArgs) -> ExitCode {
    let fail = |msg: String| {
        eprintln!("fx-bench: perf-release: {msg}");
        ExitCode::from(2)
    };
    if let Some(out) = &args.out {
        if fs::symlink_metadata(out).is_ok() {
            return fail(format!(
                "'{}' already exists; --out writes a new file",
                out.display()
            ));
        }
    }
    let read = |path: &Path| {
        fs::read_to_string(path).map_err(|e| format!("cannot read '{}': {e}", path.display()))
    };
    let baseline = match read(&args.baseline).and_then(|text| {
        perf::parse_baseline(&text, &args.baseline.display().to_string()).map_err(|e| e.to_string())
    }) {
        Ok(b) => b,
        Err(msg) => return fail(msg),
    };
    if baseline.meta.summary.is_empty() {
        return fail(format!(
            "{} has no 'summary' in [meta]: name the metric IDs of the summary table",
            args.baseline.display()
        ));
    }
    let load = |paths: &[PathBuf]| -> Result<perf::Samples, String> {
        let mut samples = perf::Samples::default();
        for path in paths {
            read(path).and_then(|text| {
                perf::parse_metrics(&text, &path.display().to_string(), &mut samples)
                    .map_err(|e| e.to_string())
            })?;
        }
        Ok(samples)
    };
    let previous = match load(std::slice::from_ref(&args.previous)) {
        Ok(s) => s,
        Err(msg) => return fail(msg),
    };
    let current = match load(&args.metrics) {
        Ok(s) => s,
        Err(msg) => return fail(msg),
    };
    let notes = match args.notes.as_deref().map(read).transpose() {
        Ok(n) => n,
        Err(msg) => return fail(msg),
    };
    let section = release::render(&release::ReleaseInput {
        baseline: &baseline,
        previous: &previous,
        current: &current,
        previous_label: &args.previous_label,
        label: &args.label,
        notes: notes.as_deref(),
    });
    if let Some(out) = &args.out {
        let written = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(out)
            .and_then(|mut f| std::io::Write::write_all(&mut f, section.as_bytes()));
        if let Err(e) = written {
            return fail(format!("cannot write '{}': {e}", out.display()));
        }
    }
    print!("{section}");
    ExitCode::SUCCESS
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
        Command::GateSummary { .. }
        | Command::Phase1Summary { .. }
        | Command::Sample(_)
        | Command::Churn { .. }
        | Command::PerfCompare(_)
        | Command::PerfRelease(_) => {
            unreachable!("these commands set their own exit code and are run from main")
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
