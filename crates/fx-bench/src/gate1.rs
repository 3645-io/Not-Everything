//! Phase 1 gate: the criteria table from `docs/PLAN.md`, measured samples
//! read from benchmark output, per-criterion verdicts and a markdown
//! `summary.md`.
//!
//! The Phase 1 benchmarks print their measurements as one sample per line,
//! in either of two forms (each line is trimmed first):
//!
//! - `<key>=<value>`, where the key is made of `[a-z0-9_]` and the value is
//!   a finite number or `true`/`false`;
//! - a JSON line `{"metric":"<key>","value":<number|true|false>, ...}`;
//!   extra fields are allowed and ignored.
//!
//! Every other line (prose, Phase 0 `GATE ...` lines) is ignored, and
//! repeated keys accumulate samples. Timing metrics are in milliseconds.
//!
//! | id                | metric(s)                          | rule                              |
//! |-------------------|------------------------------------|-----------------------------------|
//! | `syntax`          | `syntax_cases`, `syntax_failures`  | max cases >= 300, failures 0      |
//! | `goal1_launch`    | `launch_ms`                        | p95 <= 2000 (stretch p95 <= 1000) |
//! | `goal2_engine_2m` | `engine_2m_ms`                     | p95 <= 16                         |
//! | `goal2_engine_5m` | `engine_5m_ms`                     | p95 <= 30                         |
//! | `goal2_keystroke` | `keystroke_ms`                     | p95 <= 50                         |
//! | `goal2_regex`     | `regex_2m_ms`                      | p95 <= 100                        |
//! | `goal2_fullpath`  | `fullpath_2m_ms`                   | p95 <= 100                        |
//! | `goal3_freshness` | `freshness_ms`                     | p99 <= 1000 (stretch p95 <= 250)  |
//! | `catchup_equal`   | `catchup_equal` (flag)             | every value true                  |
//! | `catchup_time`    | `catchup_ms`                       | worst (max) <= 10000              |
//! | `catchup_usable`  | `catchup_usable_ms`                | worst (max) <= 2000               |
//! | `first_run`       | `first_run_ms`                     | worst (max) <= 15000              |
//! | `memory`          | `bytes_per_entry`                  | worst (max) <= 100                |
//! | `cli`             | `cli_ok` (flag)                    | every value true                  |
//!
//! Percentiles are nearest-rank, as in the harness's [`Stats`], computed on
//! the exact sample values; thresholds are inclusive. A criterion whose
//! metric has no value is [`Status::NotMeasured`]. Stretch goals are
//! reported but never change the verdict.
//!
//! [`evaluate`] applies only these rules. [`evaluate_gate`], which
//! `fx-bench phase1-summary` uses, adds two gate checks: on a cold pass
//! `launch_ms` is judged by its worst sample, and each row with a
//! [`gate_scale`] is [`Status::NotMeasured`] ("below gate scale") unless the
//! results show it ran at the gate's scale:
//!
//! | id                         | scale metric          | gate scale            |
//! |----------------------------|-----------------------|-----------------------|
//! | `goal1_launch`             | `ui_entries`          | >= 2,000,000          |
//! | `goal1_launch` (not cold)  | count of `launch_ms`  | >= 20 launches        |
//! | `goal2_engine_2m`          | `engine_2m_entries`   | >= 2,000,000          |
//! | `goal2_engine_5m`          | `engine_5m_entries`   | >= 5,000,000          |
//! | `goal2_regex`              | `regex_2m_entries`    | >= 2,000,000          |
//! | `goal2_fullpath`           | `fullpath_2m_entries` | >= 2,000,000          |
//! | `goal3_freshness`          | `freshness_ops`       | >= 1,000 operations   |
//! | `catchup_*`                | `catchup_changes`     | >= 10,000 changes     |
//! | `first_run`                | `first_run_entries`   | ~2M: >= 1,800,000     |
//!
//! `syntax`, `memory` (the B/entry estimate), `cli` and `goal2_keystroke`
//! have no scale requirement.
//!
//! Only [`read_results_dir`] touches the file system, and it only reads.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::time::Duration;

use crate::gate::md_cell;
use crate::harness::{CacheState, Stats};

/// The file name `fx-bench phase1-summary` writes into the results
/// directory; [`read_results_dir`] skips it.
pub const SUMMARY_FILE: &str = "summary.md";

/// How a criterion turns its metric's values into a verdict.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Rule {
    /// Nearest-rank percentile `p` of a millisecond metric must be at most
    /// `max`. `stretch` is an optional `(percentile, max)` stretch goal.
    Percentile {
        p: u32,
        max: f64,
        stretch: Option<(u32, f64)>,
    },
    /// The largest value must be at most `max`. `timing` marks millisecond
    /// metrics, which also get [`Stats`].
    Worst { max: f64, timing: bool },
    /// Every recorded flag must be true.
    Flag,
    /// The largest `syntax_cases` must be at least `min_cases` and the
    /// largest `syntax_failures` (0 when absent) must be 0.
    Syntax { min_cases: f64 },
}

/// One row of the Phase 1 gate.
#[derive(Clone, Debug, PartialEq)]
pub struct Criterion {
    /// Stable id, e.g. `goal1_launch`.
    pub id: &'static str,
    /// The gate row's wording.
    pub description: &'static str,
    /// The result-file metric the rule reads (for `syntax`, `syntax_cases`).
    pub metric: &'static str,
    /// How the verdict is computed.
    pub rule: Rule,
}

/// The metric holding the number of failing syntax cases.
const SYNTAX_FAILURES: &str = "syntax_failures";

/// The Phase 1 gate criteria, one per row of the gate in `docs/PLAN.md`.
pub fn criteria() -> Vec<Criterion> {
    let pct = |p, max, stretch| Rule::Percentile { p, max, stretch };
    vec![
        Criterion {
            id: "syntax",
            description: "Syntax: >=300 conformance cases from the public docs pass 100%",
            metric: "syntax_cases",
            rule: Rule::Syntax { min_cases: 300.0 },
        },
        Criterion {
            id: "goal1_launch",
            description: "Goal 1: launch to first result painted <=2.0 s p95 over 20 launches \
                          at 2M entries (stretch <=1.0 s), warm and after a reboot",
            metric: "launch_ms",
            rule: pct(95, 2000.0, Some((95, 1000.0))),
        },
        Criterion {
            id: "goal2_engine_2m",
            description: "Goal 2: engine p95 <=16 ms at 2M entries",
            metric: "engine_2m_ms",
            rule: pct(95, 16.0, None),
        },
        Criterion {
            id: "goal2_engine_5m",
            description: "Goal 2: engine p95 <=30 ms at 5M entries",
            metric: "engine_5m_ms",
            rule: pct(95, 30.0, None),
        },
        Criterion {
            id: "goal2_keystroke",
            description: "Goal 2: keystroke to repaint p95 <=50 ms",
            metric: "keystroke_ms",
            rule: pct(95, 50.0, None),
        },
        Criterion {
            id: "goal2_regex",
            description: "Goal 2: regex search p95 <=100 ms at 2M entries",
            metric: "regex_2m_ms",
            rule: pct(95, 100.0, None),
        },
        Criterion {
            id: "goal2_fullpath",
            description: "Goal 2: full-path search p95 <=100 ms at 2M entries",
            metric: "fullpath_2m_ms",
            rule: pct(95, 100.0, None),
        },
        Criterion {
            id: "goal3_freshness",
            description: "Goal 3: create, rename or delete shown in an open result list \
                          <=1 s p99 over 1,000 scripted operations (stretch <=250 ms p95)",
            metric: "freshness_ms",
            rule: pct(99, 1000.0, Some((95, 250.0))),
        },
        Criterion {
            id: "catchup_equal",
            description: "Offline catch-up: after 10,000 changes made while quit, the index \
                          equals a fresh scan",
            metric: "catchup_equal",
            rule: Rule::Flag,
        },
        Criterion {
            id: "catchup_time",
            description: "Offline catch-up: index equals a fresh scan within 10 s of relaunch",
            metric: "catchup_ms",
            rule: Rule::Worst {
                max: 10_000.0,
                timing: true,
            },
        },
        Criterion {
            id: "catchup_usable",
            description: "Offline catch-up: search usable within 2 s of relaunch",
            metric: "catchup_usable_ms",
            rule: Rule::Worst {
                max: 2000.0,
                timing: true,
            },
        },
        Criterion {
            id: "first_run",
            description: "First run: ~2M entries scanned in <=15 s warm (results shown \
                          progressively moved to Phase 2)",
            metric: "first_run_ms",
            rule: Rule::Worst {
                max: 15_000.0,
                timing: true,
            },
        },
        Criterion {
            id: "memory",
            description: "Memory: <=100 B per entry with default columns",
            metric: "bytes_per_entry",
            rule: Rule::Worst {
                max: 100.0,
                timing: false,
            },
        },
        Criterion {
            id: "cli",
            description: "es-style CLI with csv, json and txt output and documented exit codes",
            metric: "cli_ok",
            rule: Rule::Flag,
        },
    ]
}

/// Measured values per metric: numeric samples and boolean flags.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Results {
    samples: BTreeMap<String, Vec<f64>>,
    flags: BTreeMap<String, Vec<bool>>,
}

impl Results {
    /// Records one numeric sample for `metric`.
    pub fn add_sample(&mut self, metric: &str, value: f64) {
        self.samples
            .entry(metric.to_string())
            .or_default()
            .push(value);
    }

    /// Records one boolean value for `metric`.
    pub fn add_flag(&mut self, metric: &str, value: bool) {
        self.flags
            .entry(metric.to_string())
            .or_default()
            .push(value);
    }

    /// The samples recorded for `metric`, in recording order; empty when
    /// there are none.
    pub fn samples(&self, metric: &str) -> &[f64] {
        self.samples.get(metric).map_or(&[], Vec::as_slice)
    }

    /// `None` when no flag was recorded for `metric`, `Some(false)` when any
    /// recorded value is false, else `Some(true)`.
    pub fn flag(&self, metric: &str) -> Option<bool> {
        let values = self.flags.get(metric).filter(|v| !v.is_empty())?;
        Some(values.iter().all(|&b| b))
    }

    /// Appends every sample and flag of `other`.
    pub fn merge(&mut self, other: Results) {
        for (k, v) in other.samples {
            self.samples.entry(k).or_default().extend(v);
        }
        for (k, v) in other.flags {
            self.flags.entry(k).or_default().extend(v);
        }
    }
}

/// A parsed value: a number or a flag.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Value {
    Num(f64),
    Bool(bool),
}

/// Parses benchmark output into [`Results`]; see the module docs for the
/// recognised line forms. Unrecognised lines are ignored.
pub fn parse_results(text: &str) -> Results {
    let mut r = Results::default();
    for line in text.lines() {
        let t = line.trim();
        let parsed = if t.starts_with('{') {
            parse_json_line(t)
        } else {
            parse_key_value(t)
        };
        match parsed {
            Some((key, Value::Num(v))) => r.add_sample(&key, v),
            Some((key, Value::Bool(b))) => r.add_flag(&key, b),
            None => {}
        }
    }
    r
}

/// Reads every regular file in `dir` except [`SUMMARY_FILE`], in file-name
/// order, and merges their results. Fails when `dir` cannot be listed or a
/// file cannot be read.
pub fn read_results_dir(dir: &Path) -> io::Result<Results> {
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_name() == SUMMARY_FILE {
            continue;
        }
        let path = entry.path();
        if std::fs::metadata(&path)?.is_file() {
            paths.push(path);
        }
    }
    paths.sort();
    let mut results = Results::default();
    for path in paths {
        let bytes = std::fs::read(&path)?;
        results.merge(parse_results(&String::from_utf8_lossy(&bytes)));
    }
    Ok(results)
}

fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn parse_value(text: &str) -> Option<Value> {
    match text {
        "true" => Some(Value::Bool(true)),
        "false" => Some(Value::Bool(false)),
        _ => {
            // Rust also accepts `inf` and `NaN`; only finite numbers that
            // start like a number count.
            let first = text.bytes().next()?;
            if !(first.is_ascii_digit() || matches!(first, b'-' | b'+' | b'.')) {
                return None;
            }
            let v: f64 = text.parse().ok()?;
            v.is_finite().then_some(Value::Num(v))
        }
    }
}

fn parse_key_value(t: &str) -> Option<(String, Value)> {
    let (key, value) = t.split_once('=')?;
    if !valid_key(key) {
        return None;
    }
    Some((key.to_string(), parse_value(value)?))
}

/// A JSON value, as far as this parser needs to know it.
enum Json {
    Str(String),
    Num(f64),
    Bool(bool),
    Other,
}

/// Parses `{"metric":"<key>","value":<number|bool>, ...}`.
fn parse_json_line(t: &str) -> Option<(String, Value)> {
    let mut p = JsonParser {
        s: t.as_bytes(),
        i: 0,
    };
    let fields = p.object()?;
    p.ws();
    if p.i != p.s.len() {
        return None;
    }
    let mut metric = None;
    let mut value = None;
    for (k, v) in fields {
        match (k.as_str(), v) {
            ("metric", Json::Str(s)) => metric = Some(s),
            ("value", Json::Num(n)) if n.is_finite() => value = Some(Value::Num(n)),
            ("value", Json::Bool(b)) => value = Some(Value::Bool(b)),
            _ => {}
        }
    }
    let metric = metric.filter(|m| valid_key(m))?;
    Some((metric, value?))
}

/// A minimal JSON parser for one line: objects, arrays, strings, numbers,
/// booleans and null. Nested objects and arrays are parsed but discarded.
struct JsonParser<'a> {
    s: &'a [u8],
    i: usize,
}

impl JsonParser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\r' | b'\n') {
            self.i += 1;
        }
    }

    fn eat(&mut self, b: u8) -> bool {
        self.ws();
        if self.s.get(self.i) == Some(&b) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn object(&mut self) -> Option<Vec<(String, Json)>> {
        if !self.eat(b'{') {
            return None;
        }
        let mut fields = Vec::new();
        if self.eat(b'}') {
            return Some(fields);
        }
        loop {
            self.ws();
            let key = self.string()?;
            if !self.eat(b':') {
                return None;
            }
            let value = self.value()?;
            fields.push((key, value));
            if self.eat(b',') {
                continue;
            }
            if self.eat(b'}') {
                return Some(fields);
            }
            return None;
        }
    }

    fn array(&mut self) -> Option<()> {
        if !self.eat(b'[') {
            return None;
        }
        if self.eat(b']') {
            return Some(());
        }
        loop {
            self.value()?;
            if self.eat(b',') {
                continue;
            }
            if self.eat(b']') {
                return Some(());
            }
            return None;
        }
    }

    fn value(&mut self) -> Option<Json> {
        self.ws();
        match *self.s.get(self.i)? {
            b'"' => self.string().map(Json::Str),
            b'{' => self.object().map(|_| Json::Other),
            b'[' => self.array().map(|()| Json::Other),
            b't' => self.literal("true").map(|()| Json::Bool(true)),
            b'f' => self.literal("false").map(|()| Json::Bool(false)),
            b'n' => self.literal("null").map(|()| Json::Other),
            _ => self.number().map(Json::Num),
        }
    }

    fn literal(&mut self, word: &str) -> Option<()> {
        let end = self.i + word.len();
        if self.s.get(self.i..end)? == word.as_bytes() {
            self.i = end;
            Some(())
        } else {
            None
        }
    }

    fn number(&mut self) -> Option<f64> {
        let start = self.i;
        while self.i < self.s.len()
            && matches!(
                self.s[self.i],
                b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E'
            )
        {
            self.i += 1;
        }
        let text = std::str::from_utf8(&self.s[start..self.i]).ok()?;
        let first = text.bytes().next()?;
        if !(first.is_ascii_digit() || first == b'-') {
            return None;
        }
        text.parse().ok()
    }

    /// Parses a string literal at the current position (no leading
    /// whitespace skipped).
    fn string(&mut self) -> Option<String> {
        if self.s.get(self.i) != Some(&b'"') {
            return None;
        }
        self.i += 1;
        let mut out = Vec::new();
        loop {
            let b = *self.s.get(self.i)?;
            self.i += 1;
            match b {
                b'"' => return String::from_utf8(out).ok(),
                b'\\' => {
                    let e = *self.s.get(self.i)?;
                    self.i += 1;
                    let c = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hex = std::str::from_utf8(self.s.get(self.i..self.i + 4)?).ok()?;
                            self.i += 4;
                            // Surrogate pairs are not needed for metric
                            // names; a lone surrogate becomes U+FFFD.
                            char::from_u32(u32::from_str_radix(hex, 16).ok()?).unwrap_or('\u{fffd}')
                        }
                        _ => return None,
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
                b => out.push(b),
            }
        }
    }
}

/// The verdict for one criterion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Measured and within the threshold.
    Pass,
    /// Measured and over the threshold.
    Fail,
    /// No value for the criterion's metric.
    NotMeasured,
}

impl Status {
    /// The label used in summaries: `PASS`, `FAIL` or `NOT MEASURED`.
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pass => "PASS",
            Status::Fail => "FAIL",
            Status::NotMeasured => "NOT MEASURED",
        }
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The evaluated verdict of one criterion.
#[derive(Clone, Debug, PartialEq)]
pub struct CriterionResult {
    /// The criterion's id.
    pub id: &'static str,
    /// The criterion's description.
    pub description: &'static str,
    /// Pass, fail or not measured.
    pub status: Status,
    /// The value compared with the threshold (the percentile, the worst
    /// value, the case count, or 1/0 for flags); `None` when not measured.
    pub measured: Option<f64>,
    /// Nearest-rank stats of a timing metric's samples (ms converted to
    /// [`Duration`]).
    pub stats: Option<Stats>,
    /// Whether the stretch goal was met; `None` for criteria without one or
    /// when not measured.
    pub stretch_met: Option<bool>,
    /// Human-readable measurement and threshold, for the summary.
    pub detail: String,
}

/// Nearest-rank percentile `p` of ascending, non-empty `sorted`: the value
/// at rank `ceil(p * n / 100)`, clamped to 1..=n (the harness's method).
fn nearest_rank(sorted: &[f64], p: u32) -> f64 {
    let n = sorted.len();
    let rank = (p as usize * n).div_ceil(100).clamp(1, n);
    sorted[rank - 1]
}

fn ms_to_duration(ms: f64) -> Duration {
    Duration::try_from_secs_f64(ms.max(0.0) / 1000.0).unwrap_or(Duration::MAX)
}

fn timing_stats(samples: &[f64]) -> Option<Stats> {
    let d: Vec<Duration> = samples.iter().map(|&v| ms_to_duration(v)).collect();
    Stats::from_samples(&d)
}

fn num(v: f64) -> String {
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    s.to_string()
}

fn verdict(ok: bool) -> Status {
    if ok { Status::Pass } else { Status::Fail }
}

/// Evaluates one criterion's rule. With `cold_worst`, a percentile rule is
/// judged by the worst sample instead (see [`evaluate_gate`]).
fn evaluate_one(c: &Criterion, r: &Results, cold_worst: bool) -> CriterionResult {
    let mut out = CriterionResult {
        id: c.id,
        description: c.description,
        status: Status::NotMeasured,
        measured: None,
        stats: None,
        stretch_met: None,
        detail: format!("no `{}` value", c.metric),
    };
    match c.rule {
        Rule::Percentile { p, max, stretch } => {
            let mut sorted = r.samples(c.metric).to_vec();
            if sorted.is_empty() {
                return out;
            }
            sorted.sort_by(f64::total_cmp);
            if cold_worst {
                let worst = sorted[sorted.len() - 1];
                out.measured = Some(worst);
                out.status = verdict(worst <= max);
                out.stats = timing_stats(&sorted);
                out.detail = format!(
                    "cold: worst {} ms (n={}, p{p} not used: no cold sample may be dropped); \
                     budget <= {} ms",
                    num(worst),
                    sorted.len(),
                    num(max)
                );
                if let Some((_, smax)) = stretch {
                    let met = worst <= smax;
                    out.stretch_met = Some(met);
                    out.detail.push_str(&format!(
                        "; stretch worst <= {} ms {}",
                        num(smax),
                        if met { "met" } else { "not met" }
                    ));
                }
                return out;
            }
            let m = nearest_rank(&sorted, p);
            out.measured = Some(m);
            out.status = verdict(m <= max);
            out.stats = timing_stats(&sorted);
            out.detail = format!(
                "p{p} {} ms (n={}, p50 {} ms, p99 {} ms); budget <= {} ms",
                num(m),
                sorted.len(),
                num(nearest_rank(&sorted, 50)),
                num(nearest_rank(&sorted, 99)),
                num(max)
            );
            if let Some((sp, smax)) = stretch {
                let sv = nearest_rank(&sorted, sp);
                let met = sv <= smax;
                out.stretch_met = Some(met);
                out.detail.push_str(&format!(
                    "; stretch p{sp} <= {} ms {}",
                    num(smax),
                    if met { "met" } else { "not met" }
                ));
            }
        }
        Rule::Worst { max, timing } => {
            let samples = r.samples(c.metric);
            let Some(worst) = samples.iter().copied().reduce(f64::max) else {
                return out;
            };
            out.measured = Some(worst);
            out.status = verdict(worst <= max);
            let unit = if timing { " ms" } else { "" };
            if timing {
                out.stats = timing_stats(samples);
            }
            out.detail = format!(
                "worst {}{unit} (n={}); limit <= {}{unit}",
                num(worst),
                samples.len(),
                num(max)
            );
        }
        Rule::Flag => {
            let Some(ok) = r.flag(c.metric) else {
                return out;
            };
            out.measured = Some(if ok { 1.0 } else { 0.0 });
            out.status = verdict(ok);
            out.detail = if ok {
                "every check true".to_string()
            } else {
                "a check reported false".to_string()
            };
        }
        Rule::Syntax { min_cases } => {
            let Some(cases) = r.samples(c.metric).iter().copied().reduce(f64::max) else {
                return out;
            };
            let failures = r
                .samples(SYNTAX_FAILURES)
                .iter()
                .copied()
                .reduce(f64::max)
                .unwrap_or(0.0);
            out.measured = Some(cases);
            out.status = verdict(cases >= min_cases && failures == 0.0);
            out.detail = format!(
                "{} cases, {} failures; need >= {} cases and 0 failures",
                num(cases),
                num(failures),
                num(min_cases)
            );
        }
    }
    out
}

/// Evaluates every criterion against `results`, in [`criteria`] order.
pub fn evaluate(results: &Results) -> Vec<CriterionResult> {
    criteria()
        .iter()
        .map(|c| evaluate_one(c, results, false))
        .collect()
}

/// The scale a criterion must be measured at for its verdict to count.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GateScale {
    /// The result metric holding the scale the benchmark ran at; with
    /// several values the smallest counts.
    pub metric: &'static str,
    /// The smallest scale that counts as gate evidence (inclusive).
    pub min: f64,
    /// The gate's own wording of the scale, e.g. `2,000,000 entries`.
    pub label: &'static str,
    /// Unit used when reporting the measured scale, e.g. `entries`.
    pub unit: &'static str,
}

/// The number of `launch_ms` samples a warm (or unknown-cache) Goal 1 pass
/// needs: p95 over 20 launches.
pub const WARM_LAUNCHES: usize = 20;

/// Metrics judged by their worst sample on a cold pass: only the first
/// launch or open after a reboot is cold, so no sample may be dropped by a
/// percentile. `startup_bench`'s own store-open timing has no gate row; its
/// GATE line is kept in the log.
pub const COLD_WORST_METRICS: [&str; 1] = ["launch_ms"];

/// The gate scale of criterion `id`, or `None` for rows with no scale
/// requirement (`syntax`, `memory`'s B/entry estimate, `cli`, keystroke).
///
/// `first_run` asks for "~2M entries", the size of a real disk: at least
/// 1,800,000 (90 %) counts.
pub fn gate_scale(id: &str) -> Option<GateScale> {
    let entries = |metric, min, label| GateScale {
        metric,
        min,
        label,
        unit: "entries",
    };
    let catchup = GateScale {
        metric: "catchup_changes",
        min: 10_000.0,
        label: "10,000 changes",
        unit: "changes",
    };
    Some(match id {
        "goal1_launch" => entries("ui_entries", 2_000_000.0, "2,000,000 entries"),
        "goal2_engine_2m" => entries("engine_2m_entries", 2_000_000.0, "2,000,000 entries"),
        "goal2_engine_5m" => entries("engine_5m_entries", 5_000_000.0, "5,000,000 entries"),
        "goal2_regex" => entries("regex_2m_entries", 2_000_000.0, "2,000,000 entries"),
        "goal2_fullpath" => entries("fullpath_2m_entries", 2_000_000.0, "2,000,000 entries"),
        "goal3_freshness" => GateScale {
            metric: "freshness_ops",
            min: 1000.0,
            label: "1,000 operations",
            unit: "operations",
        },
        "catchup_equal" | "catchup_time" | "catchup_usable" => catchup,
        "first_run" => entries("first_run_entries", 1_800_000.0, "~2,000,000 entries"),
        _ => return None,
    })
}

/// A count with thousands separators: `2000000` -> `2,000,000`. Fractions
/// are kept as [`num`] prints them.
fn grouped(v: f64) -> String {
    if v.fract() != 0.0 || v.abs() >= 1e15 {
        return num(v);
    }
    let digits = format!("{}", v.abs() as u64);
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if v < 0.0 {
        out.insert(0, '-');
    }
    out
}

/// Evaluates every criterion as the gate reports it, in [`criteria`] order.
///
/// On top of [`evaluate`]'s rules:
///
/// - With [`CacheState::Cold`], the metrics in [`COLD_WORST_METRICS`]
///   (`launch_ms`, Goal 1) are judged by their worst sample, not the
///   nearest-rank percentile, so one slow cold launch always counts.
/// - Each row with a [`gate_scale`] reads the scale its benchmark ran at.
///   When it is missing or below the gate scale, the row is
///   [`Status::NotMeasured`] (never PASS or FAIL) and its detail starts with
///   `below gate scale (N ...)`; otherwise the detail names the measured
///   scale. A warm or unknown-cache Goal 1 pass also needs
///   [`WARM_LAUNCHES`] `launch_ms` samples.
pub fn evaluate_gate(results: &Results, cache: CacheState) -> Vec<CriterionResult> {
    let cold = cache == CacheState::Cold;
    criteria()
        .iter()
        .map(|c| {
            let cold_worst = cold && COLD_WORST_METRICS.contains(&c.metric);
            let mut row = evaluate_one(c, results, cold_worst);
            apply_scale(c, results, cold, &mut row);
            row
        })
        .collect()
}

/// Applies the gate-scale check of [`evaluate_gate`] to one evaluated row.
fn apply_scale(c: &Criterion, r: &Results, cold: bool, row: &mut CriterionResult) {
    let Some(scale) = gate_scale(c.id) else {
        return;
    };
    let mut short = Vec::new();
    let mut met = Vec::new();
    match r.samples(scale.metric).iter().copied().reduce(f64::min) {
        Some(n) if n >= scale.min => {
            met.push(format!("{} {}", grouped(n), scale.unit));
        }
        Some(n) => short.push(format!(
            "{} {}; gate needs {}",
            grouped(n),
            scale.unit,
            scale.label
        )),
        None => short.push(format!(
            "no `{}` value; gate needs {}",
            scale.metric, scale.label
        )),
    }
    if c.metric == "launch_ms" && !cold {
        let n = r.samples(c.metric).len();
        if n >= WARM_LAUNCHES {
            met.push(format!("{n} launches"));
        } else {
            short.push(format!("{n} launches; a warm pass needs {WARM_LAUNCHES}"));
        }
    }
    if short.is_empty() {
        row.detail = format!("at {}; {}", met.join(", "), row.detail);
    } else {
        // The small-scale value stays in `measured` and the detail for
        // reference, but it is not gate evidence either way.
        row.status = Status::NotMeasured;
        row.stretch_met = None;
        row.detail = format!("below gate scale ({}); {}", short.join(", "), row.detail);
    }
}

/// True when there is at least one row and every row passed.
pub fn all_pass(rows: &[CriterionResult]) -> bool {
    !rows.is_empty() && rows.iter().all(|r| r.status == Status::Pass)
}

/// Renders `summary.md`: a title, the machine label and cache state, one
/// table row per criterion and an overall result line. The output depends
/// only on its inputs (no timestamps), so re-running gives the same file.
pub fn render_summary(rows: &[CriterionResult], machine: &str, cache: CacheState) -> String {
    let machine: String = machine
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut out = format!(
        "# Phase 1 gate summary\n\n\
         Machine: {machine}\n\n\
         Cache: {cache}\n\n\
         | Criterion | Description | Status | Measured | Detail |\n\
         |---|---|---|---|---|\n"
    );
    for r in rows {
        let measured = r.measured.map(num).unwrap_or_default();
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            md_cell(r.id),
            md_cell(r.description),
            r.status,
            measured,
            md_cell(&r.detail),
        ));
    }
    let passed = rows.iter().filter(|r| r.status == Status::Pass).count();
    let not_measured = rows
        .iter()
        .filter(|r| r.status == Status::NotMeasured)
        .count();
    out.push_str(&format!(
        "\nResult: {} ({passed} of {} criteria passed, {not_measured} not measured)\n",
        if all_pass(rows) { "PASS" } else { "FAIL" },
        rows.len()
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_finite_values_are_ignored() {
        let r = parse_results("a=inf\nb=NaN\nc=1e400\n{\"metric\":\"d\",\"value\":1e400}\n");
        for k in ["a", "b", "c", "d"] {
            assert!(r.samples(k).is_empty(), "{k}");
        }
    }

    #[test]
    fn json_with_nested_fields_and_escapes() {
        let r = parse_results(
            "{\"x\":[1,{\"y\":null}],\"metric\":\"k\\u0031\",\"value\":3,\"s\":\"a\\\"b\"}",
        );
        assert_eq!(r.samples("k1"), &[3.0]);
    }

    #[test]
    fn json_with_trailing_garbage_is_ignored() {
        let r = parse_results("{\"metric\":\"k\",\"value\":3} x");
        assert!(r.samples("k").is_empty());
    }

    #[test]
    fn grouped_counts() {
        assert_eq!(grouped(0.0), "0");
        assert_eq!(grouped(999.0), "999");
        assert_eq!(grouped(1000.0), "1,000");
        assert_eq!(grouped(20_000.0), "20,000");
        assert_eq!(grouped(2_000_000.0), "2,000,000");
        assert_eq!(grouped(1.5), "1.5");
    }

    #[test]
    fn first_run_scale_accepts_about_2m() {
        let base = "first_run_ms=9000\n";
        let at = |n: &str| {
            let rows = evaluate_gate(
                &parse_results(&format!("{base}first_run_entries={n}\n")),
                CacheState::Warm,
            );
            rows.into_iter()
                .find(|r| r.id == "first_run")
                .unwrap()
                .status
        };
        assert_eq!(at("1900000"), Status::Pass);
        assert_eq!(at("1799999"), Status::NotMeasured);
    }

    #[test]
    fn cold_launch_count_is_not_required() {
        let rows = evaluate_gate(
            &parse_results("launch_ms=900\nui_entries=2000000\n"),
            CacheState::Cold,
        );
        assert_eq!(rows[1].status, Status::Pass, "{}", rows[1].detail);
        let rows = evaluate_gate(
            &parse_results("launch_ms=900\nui_entries=2000000\n"),
            CacheState::Unknown,
        );
        assert_eq!(rows[1].status, Status::NotMeasured, "{}", rows[1].detail);
    }

    #[test]
    fn huge_ms_values_do_not_panic() {
        let rows = evaluate(&parse_results("launch_ms=1e300\n"));
        assert_eq!(rows[1].status, Status::Fail);
    }
}
