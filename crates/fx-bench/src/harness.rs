//! A small benchmark harness: run a closure a fixed number of times, time
//! each call with [`Instant`], and summarise the samples as [`Stats`]
//! (min, max, mean and nearest-rank p50/p95/p99). A sample covers the call
//! only: the closure's result is dropped after the timer stops.
//!
//! Reports carry the machine they ran on ([`MachineInfo`]) and whether the
//! file-system cache was warm or cold ([`CacheState`]), so recorded numbers
//! can be compared fairly. They render as one human-readable line
//! ([`BenchReport::to_text`]) or as a JSON object ([`BenchReport::to_json`]).

use std::fmt;
use std::hint::black_box;
use std::time::{Duration, Instant};

/// Whether the file-system cache was warm or cold during a benchmark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheState {
    /// Data was already cached (for example, after a previous run).
    Warm,
    /// Caches were dropped or the machine was freshly booted.
    Cold,
    /// Not recorded.
    Unknown,
}

impl CacheState {
    /// Lowercase name used in reports: `warm`, `cold` or `unknown`.
    pub fn as_str(self) -> &'static str {
        match self {
            CacheState::Warm => "warm",
            CacheState::Cold => "cold",
            CacheState::Unknown => "unknown",
        }
    }
}

impl fmt::Display for CacheState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What to run and how often.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BenchConfig {
    /// Untimed calls made before the timed ones.
    pub warmup: u32,
    /// Timed calls; each contributes one sample.
    pub iterations: u32,
    /// Human-readable name of the benchmark.
    pub label: String,
    /// Cache state the benchmark ran under.
    pub cache: CacheState,
}

/// Summary statistics of a set of timing samples.
///
/// Percentiles use the nearest-rank method: with the `n` samples sorted in
/// ascending order, the p-th percentile is the sample at 1-based rank
/// `ceil(p / 100 * n)` (and at least rank 1). It is always one of the
/// measured samples, never an interpolation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub min: Duration,
    pub max: Duration,
    /// Arithmetic mean, truncated to whole nanoseconds.
    pub mean: Duration,
    pub p50: Duration,
    pub p95: Duration,
    pub p99: Duration,
    /// Number of samples.
    pub count: usize,
}

impl Stats {
    /// Computes statistics from `samples` in any order. Returns `None` if
    /// `samples` is empty. The input is not modified.
    pub fn from_samples(samples: &[Duration]) -> Option<Stats> {
        if samples.is_empty() {
            return None;
        }
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        let n = sorted.len();
        let total: u128 = sorted.iter().map(Duration::as_nanos).sum();
        let mean_nanos = total / n as u128;
        let mean = duration_from_nanos_u128(mean_nanos);
        Some(Stats {
            min: sorted[0],
            max: sorted[n - 1],
            mean,
            p50: nearest_rank(&sorted, 50),
            p95: nearest_rank(&sorted, 95),
            p99: nearest_rank(&sorted, 99),
            count: n,
        })
    }
}

/// Nearest-rank percentile of non-empty, ascending `sorted` for integer
/// percentile `p` in 0..=100: the value at rank `ceil(p * n / 100)`,
/// clamped to 1..=n. Integer arithmetic avoids floating-point rounding.
fn nearest_rank(sorted: &[Duration], p: u32) -> Duration {
    let n = sorted.len() as u128;
    let rank = (u128::from(p) * n).div_ceil(100).clamp(1, n);
    sorted[(rank - 1) as usize]
}

fn duration_from_nanos_u128(nanos: u128) -> Duration {
    let secs = (nanos / 1_000_000_000).min(u128::from(u64::MAX)) as u64;
    let sub = (nanos % 1_000_000_000) as u32;
    Duration::new(secs, sub)
}

/// The machine a benchmark ran on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineInfo {
    /// `std::env::consts::OS`, for example `macos`.
    pub os: String,
    /// `std::env::consts::ARCH`, for example `aarch64`.
    pub arch: String,
    /// `std::thread::available_parallelism`, or 1 if unavailable.
    pub parallelism: usize,
    /// CPU brand string, or `unknown` if it could not be read.
    pub cpu_brand: String,
}

impl MachineInfo {
    /// Detects the current machine. Never panics; fields that cannot be
    /// determined fall back to `1` or `unknown`.
    pub fn detect() -> MachineInfo {
        MachineInfo {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            parallelism: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1),
            cpu_brand: cpu_brand().unwrap_or_else(|| "unknown".to_string()),
        }
    }
}

#[cfg(target_os = "macos")]
fn cpu_brand() -> Option<String> {
    let out = std::process::Command::new("sysctl")
        .args(["-n", "machdep.cpu.brand_string"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    non_empty(String::from_utf8_lossy(&out.stdout).trim())
}

#[cfg(target_os = "linux")]
fn cpu_brand() -> Option<String> {
    let info = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    // x86 uses "model name"; some ARM kernels only provide "Hardware" or
    // "Processor".
    for key in ["model name", "Hardware", "Processor", "cpu model"] {
        for line in info.lines() {
            let value = line
                .split_once(':')
                .filter(|(k, _)| k.trim() == key)
                .and_then(|(_, v)| non_empty(v.trim()));
            if value.is_some() {
                return value;
            }
        }
    }
    None
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn cpu_brand() -> Option<String> {
    None
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn non_empty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// The result of [`run`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BenchReport {
    pub label: String,
    pub cache: CacheState,
    /// Number of timed iterations requested.
    pub iterations: u32,
    pub stats: Stats,
    pub machine: MachineInfo,
}

impl BenchReport {
    /// One human-readable line with the label, cache state, sample count
    /// and the statistics in milliseconds with three decimals.
    pub fn to_text(&self) -> String {
        let s = &self.stats;
        format!(
            "{}: p50 {} ms, p95 {} ms, p99 {} ms, min {} ms, max {} ms, mean {} ms \
             (n={}, cache {}, {} {}, {} threads, {})",
            one_line(&self.label),
            ms3(s.p50),
            ms3(s.p95),
            ms3(s.p99),
            ms3(s.min),
            ms3(s.max),
            ms3(s.mean),
            s.count,
            self.cache,
            self.machine.os,
            self.machine.arch,
            self.machine.parallelism,
            one_line(&self.machine.cpu_brand),
        )
    }

    /// A JSON object with the label, cache state, iteration count, the
    /// statistics (each as integer nanoseconds `*_ns` and as milliseconds
    /// `*_ms`) and the machine info.
    pub fn to_json(&self) -> String {
        let s = &self.stats;
        let mut stats = String::new();
        for (i, (name, d)) in [
            ("min", s.min),
            ("max", s.max),
            ("mean", s.mean),
            ("p50", s.p50),
            ("p95", s.p95),
            ("p99", s.p99),
        ]
        .into_iter()
        .enumerate()
        {
            if i > 0 {
                stats.push(',');
            }
            stats.push_str(&format!(
                "\"{name}_ns\":{},\"{name}_ms\":{:.6}",
                d.as_nanos(),
                ms_f64(d)
            ));
        }
        format!(
            "{{\"label\":{},\"cache\":{},\"iterations\":{},\"stats\":{{\"count\":{},{}}},\
             \"machine\":{{\"os\":{},\"arch\":{},\"parallelism\":{},\"cpu_brand\":{}}}}}",
            json_string(&self.label),
            json_string(self.cache.as_str()),
            self.iterations,
            s.count,
            stats,
            json_string(&self.machine.os),
            json_string(&self.machine.arch),
            self.machine.parallelism,
            json_string(&self.machine.cpu_brand),
        )
    }
}

fn ms_f64(d: Duration) -> f64 {
    d.as_nanos() as f64 / 1_000_000.0
}

fn ms3(d: Duration) -> String {
    format!("{:.3}", ms_f64(d))
}

/// Replaces line breaks and other control characters so text output stays
/// on one line.
fn one_line(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Encodes `s` as a JSON string literal, escaping quotes, backslashes and
/// all control characters.
pub(crate) fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Runs `f` `cfg.warmup` times untimed, then `cfg.iterations` times timed,
/// and returns a report with one sample per timed call.
///
/// Each sample covers only the call to `f`: the timer stops as soon as `f`
/// returns. The result is then passed through [`black_box`] (so the work is
/// not optimised away) and dropped outside the timed region, so freeing
/// what `f` returned (for example a large buffer) is not part of the sample.
///
/// If `cfg.iterations` is 0 the stats are all zero with `count` 0.
pub fn run<T, F>(cfg: BenchConfig, mut f: F) -> BenchReport
where
    F: FnMut() -> T,
{
    for _ in 0..cfg.warmup {
        black_box(f());
    }
    let mut samples = Vec::with_capacity(cfg.iterations as usize);
    for _ in 0..cfg.iterations {
        let start = Instant::now();
        let out = f();
        let elapsed = start.elapsed();
        // Untimed: keep the result observable, then drop it.
        drop(black_box(out));
        samples.push(elapsed);
    }
    let stats = Stats::from_samples(&samples).unwrap_or(Stats {
        min: Duration::ZERO,
        max: Duration::ZERO,
        mean: Duration::ZERO,
        p50: Duration::ZERO,
        p95: Duration::ZERO,
        p99: Duration::ZERO,
        count: 0,
    });
    BenchReport {
        label: cfg.label,
        cache: cfg.cache,
        iterations: cfg.iterations,
        stats,
        machine: MachineInfo::detect(),
    }
}
