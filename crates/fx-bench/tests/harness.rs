//! Tests for the benchmark harness: nearest-rank percentile statistics,
//! warmup and iteration counts, machine info, and text/JSON reports.
//!
//! API contract exercised here (public items exported from the crate root):
//! - `BenchConfig { warmup: u32, iterations: u32, label: String, cache: CacheState }`
//! - `CacheState::{Warm, Cold, Unknown}`
//! - `run(cfg, FnMut() -> T) -> BenchReport`
//! - `Stats { min, max, mean, p50, p95, p99: Duration, count }` and
//!   `Stats::from_samples(&[Duration]) -> Option<Stats>`
//! - `BenchReport { label, cache, iterations, stats: Stats, machine: MachineInfo }`
//!   with `to_text()` and `to_json()`
//! - `MachineInfo { os, arch, parallelism, cpu_brand }` and `MachineInfo::detect()`

use std::cell::Cell;
use std::time::Duration;

use fx_bench::{BenchConfig, BenchReport, CacheState, MachineInfo, Stats, run};

fn ms(v: u64) -> Duration {
    Duration::from_millis(v)
}

fn cfg(warmup: u32, iterations: u32, label: &str, cache: CacheState) -> BenchConfig {
    BenchConfig {
        warmup,
        iterations,
        label: label.to_string(),
        cache,
    }
}

// ---------------------------------------------------------------- Stats ----

#[test]
fn empty_samples_give_none() {
    assert!(Stats::from_samples(&[]).is_none());
}

#[test]
fn one_to_hundred_ms_nearest_rank() {
    let samples: Vec<Duration> = (1..=100).map(ms).collect();
    let s = Stats::from_samples(&samples).expect("non-empty");
    assert_eq!(s.count as u64, 100);
    assert_eq!(s.min, ms(1));
    assert_eq!(s.max, ms(100));
    assert_eq!(s.p50, ms(50));
    assert_eq!(s.p95, ms(95));
    assert_eq!(s.p99, ms(99));
    let mean_err = s.mean.abs_diff(Duration::from_micros(50_500));
    assert!(mean_err <= Duration::from_micros(1), "mean = {:?}", s.mean);
}

#[test]
fn unsorted_input_gives_same_stats() {
    // Deterministic permutation of 1..=100 ms (37 is coprime with 100).
    let samples: Vec<Duration> = (0..100u64).map(|i| ms((i * 37) % 100 + 1)).collect();
    let mut sorted = samples.clone();
    sorted.sort();
    assert_ne!(samples, sorted, "test input should really be unsorted");
    let s = Stats::from_samples(&samples).expect("non-empty");
    assert_eq!(s.count as u64, 100);
    assert_eq!(s.min, ms(1));
    assert_eq!(s.max, ms(100));
    assert_eq!(s.p50, ms(50));
    assert_eq!(s.p95, ms(95));
    assert_eq!(s.p99, ms(99));
}

#[test]
fn from_samples_does_not_require_or_change_order() {
    let samples = vec![ms(30), ms(10), ms(40), ms(20)];
    let copy = samples.clone();
    let s = Stats::from_samples(&samples).expect("non-empty");
    assert_eq!(samples, copy);
    // n = 4: p50 rank ceil(2.0) = 2 -> 20; p95 rank ceil(3.8) = 4 -> 40.
    assert_eq!(s.p50, ms(20));
    assert_eq!(s.p95, ms(40));
    assert_eq!(s.p99, ms(40));
    assert_eq!(s.min, ms(10));
    assert_eq!(s.max, ms(40));
    assert_eq!(s.mean, ms(25));
}

#[test]
fn single_sample_is_every_statistic() {
    let d = Duration::from_nanos(1_234_567);
    let s = Stats::from_samples(&[d]).expect("non-empty");
    assert_eq!(s.count as u64, 1);
    assert_eq!(s.min, d);
    assert_eq!(s.max, d);
    assert_eq!(s.mean, d);
    assert_eq!(s.p50, d);
    assert_eq!(s.p95, d);
    assert_eq!(s.p99, d);
}

#[test]
fn nearest_rank_rounds_rank_up() {
    // n = 7: p50 rank ceil(3.5) = 4; p95 rank ceil(6.65) = 7; p99 rank 7.
    let samples: Vec<Duration> = [70, 10, 60, 20, 50, 30, 40].into_iter().map(ms).collect();
    let s = Stats::from_samples(&samples).expect("non-empty");
    assert_eq!(s.p50, ms(40));
    assert_eq!(s.p95, ms(70));
    assert_eq!(s.p99, ms(70));

    // n = 20: p50 rank 10; p95 rank 19; p99 rank ceil(19.8) = 20.
    let samples: Vec<Duration> = (1..=20).rev().map(ms).collect();
    let s = Stats::from_samples(&samples).expect("non-empty");
    assert_eq!(s.p50, ms(10));
    assert_eq!(s.p95, ms(19));
    assert_eq!(s.p99, ms(20));

    // n = 10: p50 rank 5; p95 rank ceil(9.5) = 10.
    let samples: Vec<Duration> = (1..=10).map(ms).collect();
    let s = Stats::from_samples(&samples).expect("non-empty");
    assert_eq!(s.p50, ms(5));
    assert_eq!(s.p95, ms(10));
}

#[test]
fn duplicate_samples() {
    let samples = vec![ms(5); 9];
    let s = Stats::from_samples(&samples).expect("non-empty");
    assert_eq!(s.count as u64, 9);
    assert_eq!(s.p50, ms(5));
    assert_eq!(s.p95, ms(5));
    assert_eq!(s.mean, ms(5));
}

// ------------------------------------------------------------------ run ----

#[test]
fn run_honours_warmup_and_iteration_counts() {
    let calls = Cell::new(0u32);
    let report = run(cfg(3, 7, "counts", CacheState::Warm), || {
        calls.set(calls.get() + 1);
        calls.get()
    });
    assert_eq!(calls.get(), 10, "warmup + iterations closure calls");
    assert_eq!(report.iterations as u64, 7);
    assert_eq!(report.stats.count as u64, 7);
    assert_eq!(report.label, "counts");
    assert!(matches!(report.cache, CacheState::Warm));
}

#[test]
fn run_without_warmup() {
    let mut calls = 0u32;
    let report = run(cfg(0, 5, "no-warmup", CacheState::Unknown), || {
        calls += 1;
        vec![calls; 4]
    });
    assert_eq!(calls, 5);
    assert_eq!(report.stats.count as u64, 5);
    assert!(matches!(report.cache, CacheState::Unknown));
}

#[test]
fn warmup_iterations_are_not_timed() {
    // Warmup calls return immediately; timed calls sleep. If warmups were
    // included in the sample, min would be near zero.
    let warmup = 4u32;
    let mut calls = 0u32;
    let report = run(cfg(warmup, 3, "sleepy", CacheState::Cold), || {
        calls += 1;
        if calls > warmup {
            std::thread::sleep(ms(2));
        }
        calls
    });
    assert_eq!(calls, 7);
    assert_eq!(report.stats.count as u64, 3);
    assert!(report.stats.min >= ms(2), "min = {:?}", report.stats.min);
    assert!(report.stats.min <= report.stats.p50);
    assert!(report.stats.p50 <= report.stats.p95);
    assert!(report.stats.p95 <= report.stats.p99);
    assert!(report.stats.p99 <= report.stats.max);
}

/// A value whose drop is slow and counted.
struct SlowDrop<'a> {
    drops: &'a Cell<u32>,
}

/// Time `SlowDrop::drop` sleeps for.
const SLOW_DROP: Duration = Duration::from_millis(50);

impl Drop for SlowDrop<'_> {
    fn drop(&mut self) {
        std::thread::sleep(SLOW_DROP);
        self.drops.set(self.drops.get() + 1);
    }
}

#[test]
fn closure_result_is_dropped_outside_the_timed_region() {
    // The closure itself returns at once; only dropping its result is slow.
    // If the drop were timed, every sample would be at least SLOW_DROP.
    let calls = Cell::new(0u32);
    let drops = Cell::new(0u32);
    let report = run(cfg(1, 3, "slow-drop", CacheState::Unknown), || {
        // Each previous result has been dropped before the next call.
        assert_eq!(drops.get(), calls.get(), "result kept across calls");
        calls.set(calls.get() + 1);
        SlowDrop { drops: &drops }
    });
    assert_eq!(calls.get(), 4);
    assert_eq!(drops.get(), 4, "every result, warmup included, is dropped");
    assert_eq!(report.stats.count as u64, 3);
    assert!(
        report.stats.max < SLOW_DROP,
        "dropping the result was timed: max = {:?}",
        report.stats.max
    );
}

// ---------------------------------------------------------- MachineInfo ----

#[test]
fn machine_info_is_filled_in() {
    let m = MachineInfo::detect();
    assert_eq!(m.os, std::env::consts::OS);
    assert_eq!(m.arch, std::env::consts::ARCH);
    assert!(m.parallelism >= 1);
    assert!(!m.cpu_brand.trim().is_empty(), "cpu brand or 'unknown'");

    let report = run(cfg(0, 1, "machine", CacheState::Unknown), || 1u8);
    assert_eq!(report.machine.os, std::env::consts::OS);
    assert_eq!(report.machine.arch, std::env::consts::ARCH);
}

// -------------------------------------------------------------- reports ----

/// A report whose stats are known: samples i * 1.234567 ms for i in 1..=100,
/// so p50 = 61.72835 ms and p95 = 117.283865 ms.
fn known_report(label: &str, cache: CacheState) -> BenchReport {
    let mut report = run(cfg(0, 1, label, cache), || 0u32);
    let samples: Vec<Duration> = (1..=100u64)
        .map(|i| Duration::from_nanos(i * 1_234_567))
        .collect();
    report.stats = Stats::from_samples(&samples).expect("non-empty");
    report
}

#[test]
fn text_report_is_one_line_with_p50_p95_in_ms() {
    let report = known_report("scan 5M names", CacheState::Cold);
    let text = report.to_text();
    assert_eq!(text.trim_end().lines().count(), 1, "one line: {text:?}");
    assert!(text.contains("scan 5M names"), "{text}");
    assert!(text.contains("p50"), "{text}");
    assert!(text.contains("p95"), "{text}");
    assert!(text.contains("ms"), "{text}");
    assert!(text.contains("61.728"), "p50 in ms with 3 decimals: {text}");
    assert!(
        text.contains("117.284"),
        "p95 in ms with 3 decimals: {text}"
    );
    assert!(text.to_lowercase().contains("cold"), "cache state: {text}");
}

#[test]
fn json_report_is_valid_and_has_p50_p95() {
    let label = "a \"quoted\" \\ label\nwith\ttab and \u{1} control and é";
    let report = known_report(label, CacheState::Warm);
    let json = report.to_json();
    let value = parse_json(&json).unwrap_or_else(|e| panic!("invalid JSON ({e}): {json}"));
    assert!(
        matches!(value, Json::Object(_)),
        "top level must be an object"
    );

    let mut keys = Vec::new();
    collect_keys(&value, &mut keys);
    assert!(keys.iter().any(|k| k.starts_with("p50")), "p50 key: {json}");
    assert!(keys.iter().any(|k| k.starts_with("p95")), "p95 key: {json}");
    assert!(
        find_string(&value, label),
        "label must round-trip through JSON escaping: {json}"
    );
    // The JSON unit (ms, µs, ns or s) is the implementation's choice, but the
    // known values must appear.
    let has_any = |needles: &[&str]| needles.iter().any(|n| json.contains(n));
    assert!(has_any(&["61.72", "61728", "0.0617"]), "p50 value: {json}");
    assert!(
        has_any(&["117.28", "117283", "0.1172"]),
        "p95 value: {json}"
    );
    assert!(json.to_lowercase().contains("warm"), "cache state: {json}");
    assert!(find_string(&value, std::env::consts::OS), "os: {json}");
}

// ------------------------------------------- minimal JSON validator ------

#[derive(Debug)]
enum Json {
    Null,
    Bool,
    Number,
    Str(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

fn collect_keys(v: &Json, out: &mut Vec<String>) {
    match v {
        Json::Object(fields) => {
            for (k, v) in fields {
                out.push(k.clone());
                collect_keys(v, out);
            }
        }
        Json::Array(items) => items.iter().for_each(|i| collect_keys(i, out)),
        _ => {}
    }
}

fn find_string(v: &Json, needle: &str) -> bool {
    match v {
        Json::Str(s) => s == needle,
        Json::Object(fields) => fields.iter().any(|(_, v)| find_string(v, needle)),
        Json::Array(items) => items.iter().any(|i| find_string(i, needle)),
        _ => false,
    }
}

fn parse_json(s: &str) -> Result<Json, String> {
    let chars: Vec<char> = s.chars().collect();
    let mut p = Parser { c: &chars, i: 0 };
    let v = p.value()?;
    p.ws();
    if p.i != chars.len() {
        return Err(format!("trailing data at {}", p.i));
    }
    Ok(v)
}

struct Parser<'a> {
    c: &'a [char],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.c.len() && matches!(self.c[self.i], ' ' | '\t' | '\n' | '\r') {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<char> {
        self.c.get(self.i).copied()
    }

    fn expect(&mut self, ch: char) -> Result<(), String> {
        if self.peek() == Some(ch) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("expected {ch:?} at {}", self.i))
        }
    }

    fn lit(&mut self, word: &str) -> Result<(), String> {
        for ch in word.chars() {
            self.expect(ch)?;
        }
        Ok(())
    }

    fn value(&mut self) -> Result<Json, String> {
        self.ws();
        match self.peek() {
            Some('{') => self.object(),
            Some('[') => self.array(),
            Some('"') => Ok(Json::Str(self.string()?)),
            Some('t') => self.lit("true").map(|_| Json::Bool),
            Some('f') => self.lit("false").map(|_| Json::Bool),
            Some('n') => self.lit("null").map(|_| Json::Null),
            Some(c) if c == '-' || c.is_ascii_digit() => self.number(),
            other => Err(format!("unexpected {other:?} at {}", self.i)),
        }
    }

    fn object(&mut self) -> Result<Json, String> {
        self.expect('{')?;
        let mut fields = Vec::new();
        self.ws();
        if self.peek() == Some('}') {
            self.i += 1;
            return Ok(Json::Object(fields));
        }
        loop {
            self.ws();
            let k = self.string()?;
            self.ws();
            self.expect(':')?;
            let v = self.value()?;
            fields.push((k, v));
            self.ws();
            match self.peek() {
                Some(',') => self.i += 1,
                Some('}') => {
                    self.i += 1;
                    return Ok(Json::Object(fields));
                }
                other => return Err(format!("bad object at {}: {other:?}", self.i)),
            }
        }
    }

    fn array(&mut self) -> Result<Json, String> {
        self.expect('[')?;
        let mut items = Vec::new();
        self.ws();
        if self.peek() == Some(']') {
            self.i += 1;
            return Ok(Json::Array(items));
        }
        loop {
            items.push(self.value()?);
            self.ws();
            match self.peek() {
                Some(',') => self.i += 1,
                Some(']') => {
                    self.i += 1;
                    return Ok(Json::Array(items));
                }
                other => return Err(format!("bad array at {}: {other:?}", self.i)),
            }
        }
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.i;
        if self.peek() == Some('-') {
            self.i += 1;
        }
        let digits = |p: &mut Self| {
            let s = p.i;
            while p.peek().is_some_and(|c| c.is_ascii_digit()) {
                p.i += 1;
            }
            p.i > s
        };
        if !digits(self) {
            return Err(format!("bad number at {start}"));
        }
        if self.peek() == Some('.') {
            self.i += 1;
            if !digits(self) {
                return Err(format!("bad fraction at {start}"));
            }
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            self.i += 1;
            if matches!(self.peek(), Some('+' | '-')) {
                self.i += 1;
            }
            if !digits(self) {
                return Err(format!("bad exponent at {start}"));
            }
        }
        Ok(Json::Number)
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let mut v = 0u32;
        for _ in 0..4 {
            let d = self
                .peek()
                .and_then(|c| c.to_digit(16))
                .ok_or_else(|| format!("bad \\u escape at {}", self.i))?;
            v = v * 16 + d;
            self.i += 1;
        }
        Ok(v)
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect('"')?;
        let mut out = String::new();
        loop {
            let c = self.peek().ok_or("unterminated string")?;
            self.i += 1;
            match c {
                '"' => return Ok(out),
                '\\' => {
                    let e = self.peek().ok_or("bad escape")?;
                    self.i += 1;
                    match e {
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        '/' => out.push('/'),
                        'b' => out.push('\u{8}'),
                        'f' => out.push('\u{c}'),
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        'u' => {
                            let hi = self.hex4()?;
                            let cp = if (0xD800..0xDC00).contains(&hi) {
                                self.lit("\\u")?;
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return Err("bad low surrogate".into());
                                }
                                0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                            } else {
                                hi
                            };
                            out.push(char::from_u32(cp).ok_or("bad code point")?);
                        }
                        other => return Err(format!("bad escape \\{other}")),
                    }
                }
                c if (c as u32) < 0x20 => {
                    return Err(format!(
                        "unescaped control char {:#x} at {}",
                        c as u32,
                        self.i - 1
                    ));
                }
                c => out.push(c),
            }
        }
    }
}
