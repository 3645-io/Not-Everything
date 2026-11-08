//! `scan_bench`: measures the `getattrlistbulk` walker's throughput per
//! thread count.
//!
//! ```text
//! cargo run --release -p fx-fs-macos --example scan_bench -- \
//!     --root / --threads 1,2,4,6,8 --iterations 5 --warmup 1 --cache warm [--json]
//! ```
//!
//! For each thread count it runs [`fx_fs_macos::scan::scan`] with
//! `collect = false` through `fx-bench`'s harness and reports the counters
//! of the last run, elapsed min/p50/p95, entries/s at p50 and the speedup
//! over 1 thread, plus the machine and cache state. It then prints one
//! PASS/FAIL line per gate: at least 180,000 entries/s on 1 thread, and
//! (only when the root is `/`) a whole-root scan in at most 10 s at the
//! best thread count. Gate results do not change the exit code.
//!
//! The root may also be given as a bare path. `--exclude-preset
//! none|hidden|system|dev` applies an exclusion preset to every scan, and
//! `--check-duplicates` (the scanner policy gate) runs one collecting scan
//! instead of the benchmark and reports emitted paths under
//! `/System/Volumes/Data` and repeated (device, inode) pairs; it exits 1 if
//! the gate fails:
//! `cargo run --release -p fx-fs-macos --example scan_bench -- / --check-duplicates`.
//!
//! Exit codes: 0 after a completed benchmark, 1 when the scan fails, 2 for
//! bad arguments and on hosts other than macOS.
//!
//! The scan is read-only: it only opens and lists directories.

use std::process::ExitCode;

#[cfg(not(target_os = "macos"))]
fn main() -> ExitCode {
    eprintln!("scan_bench requires macOS");
    ExitCode::from(2)
}

#[cfg(target_os = "macos")]
fn main() -> ExitCode {
    bench::main()
}

#[cfg(target_os = "macos")]
mod bench {
    use std::cell::RefCell;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;
    use std::time::Duration;

    use fx_bench::{BenchConfig, BenchReport, CacheState, MachineInfo, run};
    use fx_fs::{ExclusionConfig, Exclusions};
    use fx_fs_macos::scan::{ScanOptions, ScanStats, relative_path, scan};

    const USAGE: &str = "\
usage:
  scan_bench [--root DIR] [--threads LIST] [--iterations K] [--warmup W]
             [--cache warm|cold|unknown] [--json]

  --root DIR        directory to scan (default /)
  --threads LIST    comma-separated worker counts, each 1..=256
                    (default 1,2,4,6,8)
  --iterations K    timed scans per thread count, at least 1 (default 5)
  --warmup W        untimed scans per thread count before the timed ones
                    (default 1)
  --cache STATE     cache state label for the report (default unknown)
  --json            print one JSON object instead of text

Gates: >= 180000 entries/s at 1 thread; whole-root scan <= 10 s at the
best thread count (only checked when --root is /).";

    /// Gate: entries per second on one thread.
    const GATE_ONE_THREAD_EPS: f64 = 180_000.0;
    /// Gate: whole-`/` scan time at the best thread count.
    const GATE_ROOT_SECS: f64 = 10.0;
    /// Largest accepted thread count.
    const MAX_THREADS: usize = 256;

    struct UsageError(String);

    struct Args {
        root: PathBuf,
        threads: Vec<usize>,
        iterations: u32,
        warmup: u32,
        cache: CacheState,
        json: bool,
        check_duplicates: bool,
        exclusions: Exclusions,
    }

    /// Results for one thread count.
    struct Row {
        threads: usize,
        last: ScanStats,
        report: BenchReport,
    }

    impl Row {
        fn entries_per_sec(&self) -> f64 {
            rate(self.last.entries, self.report.stats.p50)
        }
    }

    pub fn main() -> ExitCode {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.iter().any(|a| a == "--help" || a == "-h") {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        let args = match parse(&args) {
            Ok(a) => a,
            Err(UsageError(msg)) => {
                eprintln!("scan_bench: {msg}\n\n{USAGE}");
                return ExitCode::from(2);
            }
        };
        if args.check_duplicates {
            return match check_duplicates(&args) {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::from(1),
                Err(e) => {
                    eprintln!(
                        "scan_bench: error: scan of {} failed: {e}",
                        args.root.display()
                    );
                    ExitCode::from(1)
                }
            };
        }
        match execute(&args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!(
                    "scan_bench: error: scan of {} failed: {e}",
                    args.root.display()
                );
                ExitCode::from(1)
            }
        }
    }

    fn parse(args: &[String]) -> Result<Args, UsageError> {
        let mut root: Option<PathBuf> = None;
        let mut threads: Option<Vec<usize>> = None;
        let mut iterations: Option<u32> = None;
        let mut warmup: Option<u32> = None;
        let mut cache: Option<CacheState> = None;
        let mut json = false;
        let mut check_duplicates = false;
        let mut exclude: Option<ExclusionConfig> = None;

        let mut it = args.iter();
        while let Some(arg) = it.next() {
            let flag = arg.as_str();
            let mut value = || {
                it.next()
                    .map(String::as_str)
                    .ok_or_else(|| UsageError(format!("{flag} needs a value")))
            };
            let duplicate = match flag {
                "--root" => {
                    let v = value()?;
                    if v.is_empty() {
                        return Err(UsageError("--root must not be empty".into()));
                    }
                    root.replace(PathBuf::from(v)).is_some()
                }
                "--threads" => threads.replace(thread_list(value()?)?).is_some(),
                "--iterations" => iterations.replace(number(flag, value()?)?).is_some(),
                "--warmup" => warmup.replace(number(flag, value()?)?).is_some(),
                "--cache" => cache.replace(cache_state(value()?)?).is_some(),
                "--json" => std::mem::replace(&mut json, true),
                "--check-duplicates" => std::mem::replace(&mut check_duplicates, true),
                "--exclude-preset" => exclude.replace(preset(value()?)?).is_some(),
                other if other.starts_with('-') => {
                    return Err(UsageError(format!("unknown flag '{other}'")));
                }
                // A bare path is the root, as in `scan_bench -- / --check-duplicates`.
                other if !other.is_empty() => root.replace(PathBuf::from(other)).is_some(),
                other => return Err(UsageError(format!("unexpected argument '{other}'"))),
            };
            if duplicate {
                return Err(UsageError(format!("{flag} given more than once")));
            }
        }

        let iterations = iterations.unwrap_or(5);
        if iterations == 0 {
            return Err(UsageError("--iterations must be at least 1".into()));
        }
        let root = root.unwrap_or_else(|| PathBuf::from("/"));
        match std::fs::symlink_metadata(&root) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => {
                return Err(UsageError(format!(
                    "--root {} is not a directory (symlinks are not followed)",
                    root.display()
                )));
            }
            Err(e) => {
                return Err(UsageError(format!("--root {}: {e}", root.display())));
            }
        }
        Ok(Args {
            root,
            threads: threads.unwrap_or_else(|| vec![1, 2, 4, 6, 8]),
            iterations,
            warmup: warmup.unwrap_or(1),
            cache: cache.unwrap_or(CacheState::Unknown),
            json,
            check_duplicates,
            exclusions: Exclusions::new(exclude.unwrap_or_default())
                .map_err(|e| UsageError(format!("--exclude-preset: {e}")))?,
        })
    }

    /// Exclusion presets: `none`, `hidden` (hidden entries), `system`
    /// (system entries) and `dev` (hidden plus `**/node_modules`,
    /// `**/target` and `**/.git`).
    fn preset(name: &str) -> Result<ExclusionConfig, UsageError> {
        let mut cfg = ExclusionConfig::default();
        match name {
            "none" => {}
            "hidden" => cfg.exclude_hidden = true,
            "system" => cfg.exclude_system = true,
            "dev" => {
                cfg.exclude_hidden = true;
                cfg.wildcard_patterns = ["**/node_modules", "**/target", "**/.git"]
                    .map(String::from)
                    .to_vec();
            }
            other => {
                return Err(UsageError(format!(
                    "--exclude-preset expects none, hidden, system or dev, got '{other}'"
                )));
            }
        }
        Ok(cfg)
    }

    /// `--check-duplicates`: one collecting scan, then reports every emitted
    /// path under `/System/Volumes/Data` and every repeated (device, inode)
    /// pair among live directories and single-link entries. Returns whether
    /// the scanner policy gate holds.
    fn check_duplicates(args: &Args) -> io::Result<bool> {
        use std::collections::HashSet;
        use std::os::unix::fs::MetadataExt;
        let opts = ScanOptions {
            threads: 0,
            collect: true,
            exclusions: args.exclusions.clone(),
            ..ScanOptions::default()
        };
        let start = std::time::Instant::now();
        let result = scan(&args.root, &opts)?;
        let elapsed = start.elapsed();
        let entries = result.entries.unwrap_or_default();
        let data = std::path::Path::new("/System/Volumes/Data");
        let (mut aliases, mut dups) = (0u64, 0u64);
        let mut seen = HashSet::with_capacity(entries.len());
        for i in 0..entries.len() {
            let path = args.root.join(relative_path(&entries, i));
            if path.starts_with(data) && path != data {
                aliases += 1;
                if aliases <= 20 {
                    println!("data-volume path: {}", path.display());
                }
            }
            if let Ok(md) = std::fs::symlink_metadata(&path) {
                let single = md.is_dir() || md.nlink() == 1;
                if single && !seen.insert((md.dev(), md.ino())) {
                    dups += 1;
                    if dups <= 20 {
                        println!("duplicate (device, inode): {}", path.display());
                    }
                }
            }
        }
        let s = &result.stats;
        println!(
            "root {}: {} entries in {:.2?} ({} errors, {} mount points and {} firmlink aliases \
             skipped, {} dataless dirs not entered, materialization off: {})",
            args.root.display(),
            entries.len(),
            elapsed,
            s.errors,
            s.skipped_mount_points,
            s.skipped_firmlink_aliases,
            s.skipped_dataless,
            s.materialization_disabled
        );
        println!("data-volume paths: {aliases}, duplicate (device, inode) pairs: {dups}");
        let ok = aliases == 0 && dups == 0 && s.materialization_disabled;
        println!(
            "gate 'no duplicates from /System/Volumes/Data; 0 cloud downloads': {}",
            if ok { "PASS" } else { "FAIL" }
        );
        Ok(ok)
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

    fn thread_list(text: &str) -> Result<Vec<usize>, UsageError> {
        let mut out = Vec::new();
        for part in text.split(',') {
            let n: usize = number("--threads", part.trim())?;
            if n == 0 || n > MAX_THREADS {
                return Err(UsageError(format!(
                    "--threads values must be 1..={MAX_THREADS}, got {n}"
                )));
            }
            if out.contains(&n) {
                return Err(UsageError(format!("--threads lists {n} more than once")));
            }
            out.push(n);
        }
        Ok(out)
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

    fn execute(args: &Args) -> io::Result<()> {
        let mut rows = Vec::with_capacity(args.threads.len());
        for &threads in &args.threads {
            if !args.json {
                eprintln!("scan_bench: {threads} thread(s)...");
            }
            rows.push(bench_threads(args, threads)?);
        }
        let gates = gates(&args.root, &rows);
        if args.json {
            println!("{}", to_json(args, &rows, &gates));
        } else {
            print_text(args, &rows, &gates);
        }
        Ok(())
    }

    fn bench_threads(args: &Args, threads: usize) -> io::Result<Row> {
        let opts = ScanOptions {
            threads,
            collect: false,
            exclusions: args.exclusions.clone(),
            ..ScanOptions::default()
        };
        // The closure keeps the last run's counters and the first error.
        let last: RefCell<Option<ScanStats>> = RefCell::new(None);
        let error: RefCell<Option<io::Error>> = RefCell::new(None);
        let cfg = BenchConfig {
            warmup: args.warmup,
            iterations: args.iterations,
            label: format!("scan {} threads={threads}", args.root.display()),
            cache: args.cache,
        };
        let report = run(cfg, || {
            if error.borrow().is_some() {
                return;
            }
            match scan(&args.root, &opts) {
                Ok(r) => *last.borrow_mut() = Some(r.stats),
                Err(e) => *error.borrow_mut() = Some(e),
            }
        });
        if let Some(e) = error.into_inner() {
            return Err(e);
        }
        let last = last
            .into_inner()
            .ok_or_else(|| io::Error::other("no scan completed"))?;
        Ok(Row {
            threads,
            last,
            report,
        })
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Status {
        Pass,
        Fail,
        Skip,
    }

    impl Status {
        fn as_str(self) -> &'static str {
            match self {
                Status::Pass => "PASS",
                Status::Fail => "FAIL",
                Status::Skip => "SKIP",
            }
        }

        fn from_bool(ok: bool) -> Status {
            if ok { Status::Pass } else { Status::Fail }
        }
    }

    struct Gate {
        name: &'static str,
        status: Status,
        detail: String,
    }

    fn gates(root: &Path, rows: &[Row]) -> Vec<Gate> {
        let mut out = Vec::new();
        out.push(match rows.iter().find(|r| r.threads == 1) {
            Some(r) => {
                let eps = r.entries_per_sec();
                Gate {
                    name: "1-thread throughput >= 180000 entries/s",
                    status: Status::from_bool(eps >= GATE_ONE_THREAD_EPS),
                    detail: format!("{eps:.0} entries/s at p50"),
                }
            }
            None => Gate {
                name: "1-thread throughput >= 180000 entries/s",
                status: Status::Skip,
                detail: "1 thread not in --threads".into(),
            },
        });
        if root == Path::new("/") {
            if let Some(best) = best_row(rows) {
                let secs = best.report.stats.p50.as_secs_f64();
                out.push(Gate {
                    name: "whole-root scan of / <= 10 s at best thread count",
                    status: Status::from_bool(secs <= GATE_ROOT_SECS),
                    detail: format!(
                        "p50 {secs:.3} s at {} threads, {} entries",
                        best.threads, best.last.entries
                    ),
                });
            }
        }
        out
    }

    /// The thread count with the lowest p50.
    fn best_row(rows: &[Row]) -> Option<&Row> {
        rows.iter().min_by_key(|r| r.report.stats.p50)
    }

    fn rate(count: u64, d: Duration) -> f64 {
        let secs = d.as_secs_f64();
        if secs > 0.0 { count as f64 / secs } else { 0.0 }
    }

    /// Entries/s relative to the 1-thread row, if there is one.
    fn speedup(rows: &[Row], row: &Row) -> Option<f64> {
        let base = rows.iter().find(|r| r.threads == 1)?.entries_per_sec();
        (base > 0.0).then(|| row.entries_per_sec() / base)
    }

    fn ms3(d: Duration) -> String {
        format!("{:.3}", d.as_nanos() as f64 / 1_000_000.0)
    }

    fn print_text(args: &Args, rows: &[Row], gates: &[Gate]) {
        let m = MachineInfo::detect();
        println!(
            "scan_bench: root {}, cache {}, {} iterations, {} warmup",
            args.root.display(),
            args.cache,
            args.iterations,
            args.warmup
        );
        println!(
            "machine: {} {}, {} threads, {}",
            m.os, m.arch, m.parallelism, m.cpu_brand
        );
        println!(
            "{:>7} {:>10} {:>9} {:>7} {:>6} {:>8} {:>11} {:>11} {:>11} {:>13} {:>7}",
            "threads",
            "entries",
            "dirs",
            "errors",
            "mounts",
            "dataless",
            "min_ms",
            "p50_ms",
            "p95_ms",
            "entries/s",
            "speedup"
        );
        for r in rows {
            let s = &r.report.stats;
            let speed = speedup(rows, r).map_or_else(|| "n/a".to_string(), |x| format!("{x:.2}x"));
            println!(
                "{:>7} {:>10} {:>9} {:>7} {:>6} {:>8} {:>11} {:>11} {:>11} {:>13.0} {:>7}",
                r.threads,
                r.last.entries,
                r.last.dirs,
                r.last.errors,
                r.last.skipped_mount_points,
                r.last.skipped_dataless,
                ms3(s.min),
                ms3(s.p50),
                ms3(s.p95),
                r.entries_per_sec(),
                speed
            );
        }
        println!(
            "(entries, dirs, errors, skipped mount points and skipped dataless dirs are from \
             the last run of each thread count)"
        );
        if rows.iter().any(|r| r.last.errors > 0) {
            println!(
                "note: some directories could not be read; without Full Disk Access a scan \
                 of / covers fewer entries"
            );
        }
        for g in gates {
            println!("{}: {} ({})", g.status.as_str(), g.name, g.detail);
        }
    }

    fn to_json(args: &Args, rows: &[Row], gates: &[Gate]) -> String {
        let m = MachineInfo::detect();
        let results: Vec<String> = rows
            .iter()
            .map(|r| {
                let s = &r.report.stats;
                let l = &r.last;
                let speed =
                    speedup(rows, r).map_or_else(|| "null".to_string(), |x| format!("{x:.4}"));
                format!(
                    "{{\"threads\":{},\"entries\":{},\"dirs\":{},\"files\":{},\"symlinks\":{},\
                     \"errors\":{},\"skipped_mount_points\":{},\"skipped_dataless\":{},\
                     \"min_ms\":{:.6},\"p50_ms\":{:.6},\"p95_ms\":{:.6},\
                     \"entries_per_sec_p50\":{:.1},\"speedup_vs_1\":{},\"report\":{}}}",
                    r.threads,
                    l.entries,
                    l.dirs,
                    l.files,
                    l.symlinks,
                    l.errors,
                    l.skipped_mount_points,
                    l.skipped_dataless,
                    ms_f64(s.min),
                    ms_f64(s.p50),
                    ms_f64(s.p95),
                    r.entries_per_sec(),
                    speed,
                    r.report.to_json()
                )
            })
            .collect();
        let gates: Vec<String> = gates
            .iter()
            .map(|g| {
                format!(
                    "{{\"gate\":{},\"status\":{},\"detail\":{}}}",
                    json_string(g.name),
                    json_string(g.status.as_str()),
                    json_string(&g.detail)
                )
            })
            .collect();
        format!(
            "{{\"root\":{},\"cache\":{},\"iterations\":{},\"warmup\":{},\
             \"machine\":{{\"os\":{},\"arch\":{},\"parallelism\":{},\"cpu_brand\":{}}},\
             \"results\":[{}],\"gates\":[{}]}}",
            json_string(&args.root.to_string_lossy()),
            json_string(args.cache.as_str()),
            args.iterations,
            args.warmup,
            json_string(&m.os),
            json_string(&m.arch),
            m.parallelism,
            json_string(&m.cpu_brand),
            results.join(","),
            gates.join(",")
        )
    }

    fn ms_f64(d: Duration) -> f64 {
        d.as_nanos() as f64 / 1_000_000.0
    }

    /// Encodes `s` as a JSON string literal.
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
}
