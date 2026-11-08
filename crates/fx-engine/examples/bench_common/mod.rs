//! Helpers shared by the Phase 1 gate benchmarks (`startup_bench`,
//! `catchup_bench`, `first_run_bench`): flag parsing, scratch directories
//! and the `metric=value` output format documented in the crate README.

#![allow(dead_code)]

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fx_bench::{CacheState, MachineInfo, Stats};

/// A bad command line; the benchmark prints it with its usage, exit 2.
pub struct UsageError(pub String);

/// Parsed `--flag value` pairs and bare `--switch`es.
pub struct Flags {
    values: HashMap<String, String>,
    switches: Vec<String>,
}

impl Flags {
    /// Parses `args`: every flag in `with_value` takes the next argument,
    /// every flag in `switches` stands alone. Anything else is an error.
    pub fn parse(
        args: &[String],
        with_value: &[&str],
        switches: &[&str],
    ) -> Result<Flags, UsageError> {
        let mut flags = Flags {
            values: HashMap::new(),
            switches: Vec::new(),
        };
        let mut it = args.iter();
        while let Some(arg) = it.next() {
            let a = arg.as_str();
            if with_value.contains(&a) {
                let v = it
                    .next()
                    .ok_or_else(|| UsageError(format!("{a} needs a value")))?;
                if flags.values.insert(a.to_string(), v.clone()).is_some() {
                    return Err(UsageError(format!("{a} given more than once")));
                }
            } else if switches.contains(&a) {
                flags.switches.push(a.to_string());
            } else if a.starts_with('-') {
                return Err(UsageError(format!("unknown flag '{a}'")));
            } else {
                return Err(UsageError(format!("unexpected argument '{a}'")));
            }
        }
        Ok(flags)
    }

    /// Whether the switch was given.
    pub fn has(&self, switch: &str) -> bool {
        self.switches.iter().any(|s| s == switch)
    }

    /// The raw value of `flag`.
    pub fn get(&self, flag: &str) -> Option<&str> {
        self.values.get(flag).map(String::as_str)
    }

    /// An integer in `lo..=hi` (`_` separators allowed), or `default`.
    pub fn number(&self, flag: &str, default: u64, lo: u64, hi: u64) -> Result<u64, UsageError> {
        let Some(text) = self.get(flag) else {
            return Ok(default);
        };
        let digits: String = text.chars().filter(|&c| c != '_').collect();
        match digits.parse::<u64>() {
            Ok(n) if (lo..=hi).contains(&n) => Ok(n),
            _ => Err(UsageError(format!(
                "{flag} expects an integer in {lo}..={hi}, got '{text}'"
            ))),
        }
    }

    /// The `--cache` label (`warm`, `cold`, `unknown`; default unknown).
    pub fn cache(&self) -> Result<CacheState, UsageError> {
        match self.get("--cache").unwrap_or("unknown") {
            "warm" => Ok(CacheState::Warm),
            "cold" => Ok(CacheState::Cold),
            "unknown" => Ok(CacheState::Unknown),
            other => Err(UsageError(format!(
                "--cache expects warm, cold or unknown, got '{other}'"
            ))),
        }
    }
}

/// A unique, canonical directory under `std::env::temp_dir()` that
/// removes itself, and only itself, on drop.
pub struct TempRoot(PathBuf);

impl TempRoot {
    pub fn new(prefix: &str) -> io::Result<TempRoot> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir)?;
        // Canonical, so `/var/...` reads `/private/var/...` like the paths
        // FSEvents reports.
        Ok(TempRoot(fs::canonicalize(&dir)?))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Milliseconds with three decimals.
pub fn ms(d: Duration) -> String {
    format!("{:.3}", d.as_secs_f64() * 1000.0)
}

pub fn pass_fail(ok: bool) -> &'static str {
    if ok { "PASS" } else { "FAIL" }
}

/// Prints one `metric=value` line.
pub fn metric(name: &str, value: impl std::fmt::Display) {
    println!("{name}={value}");
}

/// Prints the bench name, machine and cache label as metrics.
pub fn header(bench: &str, cache: CacheState) {
    let m = MachineInfo::detect();
    metric("bench", bench);
    metric("machine", format!("{} {}, {}", m.os, m.arch, m.cpu_brand));
    metric("threads", m.parallelism);
    metric("cache", cache);
}

/// Prints `<prefix>_p50_ms`, `_p95_ms`, `_max_ms` and `_n` for `samples`;
/// returns the stats (`None` without samples).
pub fn stats(prefix: &str, samples: &[Duration]) -> Option<Stats> {
    let s = Stats::from_samples(samples)?;
    metric(&format!("{prefix}_p50_ms"), ms(s.p50));
    metric(&format!("{prefix}_p95_ms"), ms(s.p95));
    metric(&format!("{prefix}_max_ms"), ms(s.max));
    metric(&format!("{prefix}_n"), s.count);
    Some(s)
}

/// Prints a snapshot's memory report as `memory_<metric>=value` lines and
/// the gate line `GATE memory <= 100 B per entry`.
pub fn memory(report: &fx_engine::MemoryReport) {
    for line in report.to_string().lines() {
        println!("memory_{line}");
    }
    let per = report.bytes_per_entry();
    gate(
        "memory <= 100 B per entry",
        per <= 100.0,
        &format!("{per:.2} B per entry, {} entries", report.live_entries),
    );
}

/// Prints a gate line in the format fx-bench `gate-summary` reads.
pub fn gate(criterion: &str, ok: bool, detail: &str) {
    println!("GATE {criterion}: {} ({detail})", pass_fail(ok));
}
