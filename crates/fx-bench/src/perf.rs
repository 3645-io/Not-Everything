//! The performance gate's comparison: measured metrics against a baseline
//! file, one verdict per metric. `fx-bench perf-compare` uses it.
//!
//! # The baseline file
//!
//! A strict subset of TOML, parsed by hand ([`parse_baseline`]): `#`
//! comments, blank lines, `[ID]` table headers and `key = value` lines.
//! Table IDs are made of ASCII letters, digits, `_` and `-` (`S1`, `F6u`);
//! `[meta]` is reserved for the header table (`machine`, `cache`, `build`
//! and `date` strings). Values are numbers (`-?digits[.digits]`), `true`,
//! `false` or double-quoted strings with only `\"` and `\\` escapes. Every
//! other table describes one metric ([`Metric`]); its keys are listed in
//! [`METRIC_KEYS`]. Unknown keys, duplicate keys or tables, wrong value
//! types and missing required keys are errors that name the file and line,
//! so a typo can't pass silently.
//!
//! # Metrics files
//!
//! `key=value` lines as the benchmark scripts write them ([`parse_metrics`]).
//! A key may repeat, one line per sample; `true` and `false` read as 1 and
//! 0. Blank lines and `#` lines are ignored, and any other line is an error,
//! so a garbled file can't drop a metric unnoticed.
//!
//! # Verdicts
//!
//! [`compare`] judges each metric whose `tiers` include the tier being run:
//!
//! 1. No sample of its key: NOT MEASURED (SKIPPED with `partial`, for a run
//!    of one benchmark alone).
//! 2. At the pr and rc tiers, a scale below `scale_min` (or no scale
//!    reported while `scale_min` is set): NOT MEASURED, because a number
//!    measured at a smaller scale says nothing about the gate's scale.
//! 3. The value is the `aggregate` of every sample of the key.
//! 4. The target is checked at the pr and rc tiers; the quick tier runs at
//!    a small scale where the targets don't apply. An `equal` target is the
//!    exception: it is checked at every tier, because it states correctness
//!    (a count or a flag), not speed, and `equal` metrics have no other
//!    check.
//! 5. The regression check compares with `quick_baseline` at the quick tier
//!    and `baseline` otherwise, allowing max(baseline × `allow_pct` / 100,
//!    `allow_floor`). No baseline for the tier gives NO BASELINE, which is
//!    not a failure: the first run sets the baseline.
//! 6. An ungated metric (`gated = false`) is TRACKED and shows what its
//!    verdict would have been; it never fails the gate.
//!
//! The gate passes when no gated metric is FAIL or NOT MEASURED
//! ([`gate_passes`]).

use std::collections::BTreeMap;
use std::fmt;

use crate::gate::md_cell;
use crate::harness::json_string;

/// A tier of the performance gate: how much is measured, and at what scale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// Small scale, on every change; targets (other than `equal`) are not
    /// checked.
    Quick,
    /// Gate scale, before a pull request merges.
    Pr,
    /// Gate scale, before a release candidate.
    Rc,
}

impl Tier {
    /// Parses `quick`, `pr` or `rc`.
    pub fn parse(text: &str) -> Option<Tier> {
        match text {
            "quick" => Some(Tier::Quick),
            "pr" => Some(Tier::Pr),
            "rc" => Some(Tier::Rc),
            _ => None,
        }
    }

    /// `quick`, `pr` or `rc`.
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Quick => "quick",
            Tier::Pr => "pr",
            Tier::Rc => "rc",
        }
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which direction of a metric is better.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Better {
    /// Smaller is better; the target is a ceiling.
    Lower,
    /// Larger is better; the target is a floor.
    Higher,
    /// The value must equal the target exactly (counts and flags).
    Equal,
}

impl Better {
    fn parse(text: &str) -> Option<Better> {
        match text {
            "lower" => Some(Better::Lower),
            "higher" => Some(Better::Higher),
            "equal" => Some(Better::Equal),
            _ => None,
        }
    }

    /// `lower`, `higher` or `equal`.
    pub fn as_str(self) -> &'static str {
        match self {
            Better::Lower => "lower",
            Better::Higher => "higher",
            Better::Equal => "equal",
        }
    }

    /// The comparison a target is shown with.
    fn sign(self) -> &'static str {
        match self {
            Better::Lower => "<=",
            Better::Higher => ">=",
            Better::Equal => "=",
        }
    }
}

/// How the repeated samples of one key combine into one value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aggregate {
    /// Nearest-rank p50, as the harness computes it: the lower of the two
    /// middle samples when their count is even.
    Median,
    /// Nearest-rank p95.
    P95,
    Max,
    Min,
    /// Arithmetic mean.
    Mean,
    /// The last sample read (files in argument order, lines in file order).
    Last,
}

impl Aggregate {
    /// Parses `median`, `p95`, `max`, `min`, `mean` or `last`.
    pub fn parse(text: &str) -> Option<Aggregate> {
        match text {
            "median" => Some(Aggregate::Median),
            "p95" => Some(Aggregate::P95),
            "max" => Some(Aggregate::Max),
            "min" => Some(Aggregate::Min),
            "mean" => Some(Aggregate::Mean),
            "last" => Some(Aggregate::Last),
            _ => None,
        }
    }

    /// The name used in the baseline file.
    pub fn as_str(self) -> &'static str {
        match self {
            Aggregate::Median => "median",
            Aggregate::P95 => "p95",
            Aggregate::Max => "max",
            Aggregate::Min => "min",
            Aggregate::Mean => "mean",
            Aggregate::Last => "last",
        }
    }

    /// Combines `samples`, given in reading order. `None` when empty.
    pub fn apply(self, samples: &[f64]) -> Option<f64> {
        let last = *samples.last()?;
        let sorted = || {
            let mut s = samples.to_vec();
            s.sort_by(f64::total_cmp);
            s
        };
        Some(match self {
            Aggregate::Median => nearest_rank(&sorted(), 50),
            Aggregate::P95 => nearest_rank(&sorted(), 95),
            Aggregate::Max => samples.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            Aggregate::Min => samples.iter().copied().fold(f64::INFINITY, f64::min),
            Aggregate::Mean => samples.iter().sum::<f64>() / samples.len() as f64,
            Aggregate::Last => last,
        })
    }
}

/// Nearest-rank percentile `p` of ascending, non-empty `sorted`: the value
/// at rank `ceil(p * n / 100)`, clamped to 1..=n (the harness's method).
fn nearest_rank(sorted: &[f64], p: usize) -> f64 {
    let n = sorted.len();
    let rank = (p * n).div_ceil(100).clamp(1, n);
    sorted[rank - 1]
}

/// The `[meta]` table: shown in the report header.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Meta {
    pub machine: Option<String>,
    pub cache: Option<String>,
    pub build: Option<String>,
    pub date: Option<String>,
    /// The build and date of the `pr_baseline` values: the pr tier's own
    /// conditions (file activity, shorter phases), shown in a pr report.
    pub pr_build: Option<String>,
    pub pr_date: Option<String>,
    /// The metric IDs of a release's summary table, in order
    /// (`fx-bench perf-release`); each one is a metric table of the file.
    pub summary: Vec<String>,
}

/// One metric table of the baseline file.
#[derive(Clone, Debug, PartialEq)]
pub struct Metric {
    /// The table name, e.g. `S1`.
    pub id: String,
    /// Line of the `[ID]` header, for error messages.
    pub line: usize,
    pub name: String,
    /// The key of its samples in the metrics files.
    pub key: String,
    /// Free text shown after values (`ms`, `bytes`, ...); `bool` shows 1 and
    /// 0 as `true` and `false`.
    pub unit: String,
    pub better: Better,
    /// Ceiling (`lower`), floor (`higher`) or exact value (`equal`).
    pub target: Option<f64>,
    /// Allowed worsening against the baseline, in percent.
    pub allow_pct: Option<f64>,
    /// The least allowance, in the metric's unit.
    pub allow_floor: Option<f64>,
    pub aggregate: Aggregate,
    /// Metric key holding the scale the benchmark ran at.
    pub scale_key: Option<String>,
    /// Below this scale the metric is NOT MEASURED at the pr and rc tiers.
    pub scale_min: Option<f64>,
    pub tiers: Vec<Tier>,
    /// Baseline at gate scale: the rc tier's, and the pr tier's when the
    /// metric has no `pr_baseline`.
    pub baseline: Option<f64>,
    /// Baseline at the pr tier, for metrics whose pr-tier conditions differ
    /// from the rc tier's (the background phases run under file activity).
    pub pr_baseline: Option<f64>,
    /// Baseline at the quick tier's scale.
    pub quick_baseline: Option<f64>,
    /// `false`: reported as TRACKED, never fails.
    pub gated: bool,
    pub baseline_build: Option<String>,
    pub baseline_date: Option<String>,
    /// Why the baseline moved unusually, naming the PR as `#<number>`.
    pub exception: Option<String>,
}

/// A parsed baseline file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Baseline {
    pub meta: Meta,
    /// In file order.
    pub metrics: Vec<Metric>,
}

/// The keys a metric table may hold.
pub const METRIC_KEYS: &[&str] = &[
    "name",
    "key",
    "unit",
    "better",
    "target",
    "allow_pct",
    "allow_floor",
    "aggregate",
    "scale_key",
    "scale_min",
    "tiers",
    "baseline",
    "pr_baseline",
    "quick_baseline",
    "gated",
    "baseline_build",
    "baseline_date",
    "exception",
];

/// The keys the `[meta]` table may hold, all strings. `summary` lists
/// metric IDs separated by spaces.
pub const META_KEYS: &[&str] = &[
    "machine", "cache", "build", "date", "pr_build", "pr_date", "summary",
];

/// The reserved table name of the header table.
const META_TABLE: &str = "meta";

/// A malformed baseline or metrics file: where, and what is wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// The file, as given on the command line.
    pub source: String,
    /// 1-based line number.
    pub line: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.source, self.line, self.message)
    }
}

impl std::error::Error for ParseError {}

/// A value of the baseline file.
#[derive(Clone, Debug, PartialEq)]
enum Value {
    Num(f64),
    Bool(bool),
    Str(String),
}

impl Value {
    fn kind(&self) -> &'static str {
        match self {
            Value::Num(_) => "a number",
            Value::Bool(_) => "true or false",
            Value::Str(_) => "a quoted string",
        }
    }
}

/// The table being read.
enum Current {
    Meta,
    /// Index into the raw metric tables.
    Metric(usize),
}

/// A metric table's `key = value` lines before validation.
struct RawTable {
    id: String,
    line: usize,
    fields: BTreeMap<String, (Value, usize)>,
}

/// Parses a baseline file. `source` names the file in errors.
pub fn parse_baseline(text: &str, source: &str) -> Result<Baseline, ParseError> {
    let err = |line: usize, message: String| ParseError {
        source: source.to_string(),
        line,
        message,
    };
    let mut meta_fields: BTreeMap<String, (Value, usize)> = BTreeMap::new();
    let mut tables: Vec<RawTable> = Vec::new();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut current: Option<Current> = None;

    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        let t = raw.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if let Some(rest) = t.strip_prefix('[') {
            let Some((id, after)) = rest.split_once(']') else {
                return Err(err(line, "table header without a closing ']'".into()));
            };
            if !only_comment(after) {
                return Err(err(line, "unexpected text after the table header".into()));
            }
            let id = id.trim();
            if !valid_id(id) {
                return Err(err(
                    line,
                    format!("invalid table name '[{id}]': use ASCII letters, digits, '_' and '-'"),
                ));
            }
            if let Some(prev) = seen.insert(id.to_string(), line) {
                return Err(err(
                    line,
                    format!("table [{id}] is already defined on line {prev}"),
                ));
            }
            current = Some(if id == META_TABLE {
                Current::Meta
            } else {
                tables.push(RawTable {
                    id: id.to_string(),
                    line,
                    fields: BTreeMap::new(),
                });
                Current::Metric(tables.len() - 1)
            });
            continue;
        }
        let Some((key, value)) = t.split_once('=') else {
            return Err(err(
                line,
                "expected 'key = value', a '[table]' header or a '#' comment".into(),
            ));
        };
        let key = key.trim();
        if !valid_bare_key(key) {
            return Err(err(line, format!("invalid key '{key}'")));
        }
        let value = parse_value(value.trim()).map_err(|m| err(line, m))?;
        let (fields, known, table) = match current {
            None => {
                return Err(err(
                    line,
                    format!("'{key}' is outside a table; start one with '[ID]'"),
                ));
            }
            Some(Current::Meta) => (&mut meta_fields, META_KEYS, META_TABLE),
            Some(Current::Metric(idx)) => {
                let t = &mut tables[idx];
                (&mut t.fields, METRIC_KEYS, t.id.as_str())
            }
        };
        if !known.contains(&key) {
            return Err(err(
                line,
                format!(
                    "unknown key '{key}' in [{table}]; known keys: {}",
                    known.join(", ")
                ),
            ));
        }
        if let Some((_, prev)) = fields.get(key) {
            return Err(err(
                line,
                format!("'{key}' is already set in [{table}] on line {prev}"),
            ));
        }
        fields.insert(key.to_string(), (value, line));
    }

    let mut meta = Meta::default();
    let mut summary_line = 0;
    for (key, (value, line)) in meta_fields {
        let Value::Str(s) = value else {
            return Err(err(
                line,
                format!("'{key}' in [meta] must be a quoted string"),
            ));
        };
        match key.as_str() {
            "machine" => meta.machine = Some(s),
            "cache" => meta.cache = Some(s),
            "build" => meta.build = Some(s),
            "pr_build" => meta.pr_build = Some(s),
            "pr_date" => meta.pr_date = Some(s),
            "summary" => {
                meta.summary = s.split_whitespace().map(str::to_string).collect();
                summary_line = line;
            }
            _ => meta.date = Some(s),
        }
    }

    let metrics = tables
        .into_iter()
        .map(|t| build_metric(t, &err))
        .collect::<Result<Vec<_>, _>>()?;
    // The summary names metrics of this file, each once, so a typo can't
    // drop a row from a release's notes unnoticed.
    for (i, id) in meta.summary.iter().enumerate() {
        if !metrics.iter().any(|m| &m.id == id) {
            return Err(err(
                summary_line,
                format!("'summary' in [meta] lists {id}, which is not a metric table"),
            ));
        }
        if meta.summary[..i].contains(id) {
            return Err(err(
                summary_line,
                format!("'summary' in [meta] lists {id} twice"),
            ));
        }
    }
    Ok(Baseline { meta, metrics })
}

/// Validates one metric table's fields.
fn build_metric(
    t: RawTable,
    err: &dyn Fn(usize, String) -> ParseError,
) -> Result<Metric, ParseError> {
    let id = t.id.as_str();
    let typed = |key: &str, want: &str, value: &Value, line: usize| {
        err(
            line,
            format!("'{key}' in [{id}] must be {want}, not {}", value.kind()),
        )
    };
    let string = |key: &str| -> Result<Option<(String, usize)>, ParseError> {
        match t.fields.get(key) {
            None => Ok(None),
            Some((Value::Str(s), line)) => Ok(Some((s.clone(), *line))),
            Some((v, line)) => Err(typed(key, "a quoted string", v, *line)),
        }
    };
    let number = |key: &str| -> Result<Option<(f64, usize)>, ParseError> {
        match t.fields.get(key) {
            None => Ok(None),
            Some((Value::Num(n), line)) => Ok(Some((*n, *line))),
            Some((v, line)) => Err(typed(key, "a number", v, *line)),
        }
    };
    let required = |key: &str| -> Result<(String, usize), ParseError> {
        string(key)?.ok_or_else(|| err(t.line, format!("[{id}] is missing '{key}'")))
    };
    let non_negative = |key: &str| -> Result<Option<f64>, ParseError> {
        match number(key)? {
            Some((n, line)) if n < 0.0 => {
                Err(err(line, format!("'{key}' in [{id}] must not be negative")))
            }
            other => Ok(other.map(|(n, _)| n)),
        }
    };

    let (name, line) = required("name")?;
    if name.trim().is_empty() {
        return Err(err(line, format!("'name' in [{id}] must not be empty")));
    }
    let (key, line) = required("key")?;
    if !valid_metric_key(&key) {
        return Err(err(
            line,
            format!("'key' in [{id}] must be made of a-z, 0-9 and '_', got '{key}'"),
        ));
    }
    let (unit, _) = required("unit")?;
    let (better_text, line) = required("better")?;
    let better = Better::parse(&better_text).ok_or_else(|| {
        err(
            line,
            format!("'better' in [{id}] must be lower, higher or equal, got '{better_text}'"),
        )
    })?;
    let (aggregate_text, line) = required("aggregate")?;
    let aggregate = Aggregate::parse(&aggregate_text).ok_or_else(|| {
        err(
            line,
            format!(
                "'aggregate' in [{id}] must be median, p95, max, min, mean or last, \
                 got '{aggregate_text}'"
            ),
        )
    })?;
    let (tiers_text, line) = required("tiers")?;
    let mut tiers = Vec::new();
    for word in tiers_text.split_whitespace() {
        let tier = Tier::parse(word).ok_or_else(|| {
            err(
                line,
                format!("'tiers' in [{id}] lists '{word}'; tiers are quick, pr and rc"),
            )
        })?;
        if tiers.contains(&tier) {
            return Err(err(line, format!("'tiers' in [{id}] lists {word} twice")));
        }
        tiers.push(tier);
    }
    if tiers.is_empty() {
        return Err(err(line, format!("'tiers' in [{id}] must name a tier")));
    }

    let scale_key = string("scale_key")?;
    if let Some((k, line)) = &scale_key {
        if !valid_metric_key(k) {
            return Err(err(
                *line,
                format!("'scale_key' in [{id}] must be made of a-z, 0-9 and '_', got '{k}'"),
            ));
        }
    }
    let scale_min = number("scale_min")?;
    if let (Some((_, line)), None) = (&scale_min, &scale_key) {
        return Err(err(
            *line,
            format!("'scale_min' in [{id}] needs a 'scale_key' to read the scale from"),
        ));
    }

    let pr_baseline = number("pr_baseline")?;
    if let Some((_, line)) = &pr_baseline {
        if !tiers.contains(&Tier::Pr) {
            return Err(err(
                *line,
                format!("'pr_baseline' in [{id}] needs pr in its 'tiers'"),
            ));
        }
    }

    let gated = match t.fields.get("gated") {
        None => true,
        Some((Value::Bool(b), _)) => *b,
        Some((v, line)) => return Err(typed("gated", "true or false", v, *line)),
    };

    let exception = string("exception")?;
    if let Some((text, line)) = &exception {
        if !names_a_pr(text) {
            return Err(err(
                *line,
                format!("'exception' in [{id}] must name the PR that accepted it, as #<number>"),
            ));
        }
    }

    Ok(Metric {
        id: t.id.clone(),
        line: t.line,
        name,
        key,
        unit,
        better,
        target: number("target")?.map(|(n, _)| n),
        allow_pct: non_negative("allow_pct")?,
        allow_floor: non_negative("allow_floor")?,
        aggregate,
        scale_key: scale_key.map(|(k, _)| k),
        scale_min: scale_min.map(|(n, _)| n),
        tiers,
        baseline: number("baseline")?.map(|(n, _)| n),
        pr_baseline: pr_baseline.map(|(n, _)| n),
        quick_baseline: number("quick_baseline")?.map(|(n, _)| n),
        gated,
        baseline_build: string("baseline_build")?.map(|(s, _)| s),
        baseline_date: string("baseline_date")?.map(|(s, _)| s),
        exception: exception.map(|(s, _)| s),
    })
}

/// Whether `rest` (after a value or header) is empty or a comment.
fn only_comment(rest: &str) -> bool {
    let r = rest.trim_start();
    r.is_empty() || r.starts_with('#')
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn valid_bare_key(key: &str) -> bool {
    !key.is_empty() && key.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
}

/// Metric keys as the benchmarks print them.
fn valid_metric_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Whether `text` holds `#` followed by at least one digit.
fn names_a_pr(text: &str) -> bool {
    text.match_indices('#')
        .any(|(i, _)| text[i + 1..].starts_with(|c: char| c.is_ascii_digit()))
}

/// Parses the text after `=` (trimmed): a quoted string, `true`, `false` or
/// a number, then optionally a comment.
fn parse_value(text: &str) -> Result<Value, String> {
    if let Some(rest) = text.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = rest.char_indices();
        while let Some((i, c)) = chars.next() {
            match c {
                '"' => {
                    if !only_comment(&rest[i + 1..]) {
                        return Err("unexpected text after the string".into());
                    }
                    return Ok(Value::Str(out));
                }
                '\\' => match chars.next() {
                    Some((_, '"')) => out.push('"'),
                    Some((_, '\\')) => out.push('\\'),
                    _ => return Err("only \\\" and \\\\ escapes are allowed in strings".into()),
                },
                c => out.push(c),
            }
        }
        return Err("string without a closing '\"'".into());
    }
    let token = text.split('#').next().unwrap_or_default().trim();
    match token {
        "" => Err("missing value".into()),
        "true" => Ok(Value::Bool(true)),
        "false" => Ok(Value::Bool(false)),
        _ if is_toml_number(token) => token
            .parse()
            .ok()
            .filter(|n: &f64| n.is_finite())
            .map(Value::Num)
            .ok_or_else(|| format!("number '{token}' is out of range")),
        _ => Err(format!(
            "invalid value '{token}': expected a number, true, false or a quoted string"
        )),
    }
}

/// `-?digits` or `-?digits.digits`.
fn is_toml_number(s: &str) -> bool {
    let s = s.strip_prefix('-').unwrap_or(s);
    let digits = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    match s.split_once('.') {
        Some((int, frac)) => digits(int) && digits(frac),
        None => digits(s),
    }
}

/// The samples read from metrics files, per key, in reading order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Samples {
    values: BTreeMap<String, Vec<f64>>,
}

impl Samples {
    /// Records one sample of `key`.
    pub fn add(&mut self, key: &str, value: f64) {
        self.values.entry(key.to_string()).or_default().push(value);
    }

    /// The samples of `key` in reading order; empty when there are none.
    pub fn get(&self, key: &str) -> &[f64] {
        self.values.get(key).map_or(&[], Vec::as_slice)
    }
}

/// Reads one metrics file into `into`. `source` names the file in errors.
pub fn parse_metrics(text: &str, source: &str, into: &mut Samples) -> Result<(), ParseError> {
    for (i, raw) in text.lines().enumerate() {
        let t = raw.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let parsed = t.split_once('=').and_then(|(k, v)| {
            let (k, v) = (k.trim(), v.trim());
            valid_metric_key(k).then_some(())?;
            Some((k, metric_value(v)?))
        });
        let Some((key, value)) = parsed else {
            return Err(ParseError {
                source: source.to_string(),
                line: i + 1,
                message: format!(
                    "expected key=value with a key of a-z, 0-9 and '_' and a number, \
                     true or false, got '{t}'"
                ),
            });
        };
        into.add(key, value);
    }
    Ok(())
}

/// A finite number (starting like one: Rust also reads `inf` and `NaN`), or
/// `true`/`false` as 1/0.
fn metric_value(text: &str) -> Option<f64> {
    match text {
        "true" => Some(1.0),
        "false" => Some(0.0),
        _ => {
            let first = text.bytes().next()?;
            if !(first.is_ascii_digit() || matches!(first, b'-' | b'+' | b'.')) {
                return None;
            }
            text.parse::<f64>().ok().filter(|v| v.is_finite())
        }
    }
}

/// The verdict of one metric.
#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    Pass,
    /// Which checks failed.
    Fail {
        target: bool,
        regression: bool,
    },
    /// Why: no samples, or the wrong scale.
    NotMeasured(String),
    /// No baseline for the tier; not a failure.
    NoBaseline,
    /// An ungated metric, with the verdict it would have had.
    Tracked(Box<Verdict>),
    /// Absent from every metrics file in a partial run.
    Skipped,
}

impl Verdict {
    /// The verdict's name: `PASS`, `FAIL`, `NOT MEASURED`, `NO BASELINE`,
    /// `TRACKED` or `SKIPPED`.
    pub fn label(&self) -> &'static str {
        match self {
            Verdict::Pass => "PASS",
            Verdict::Fail { .. } => "FAIL",
            Verdict::NotMeasured(_) => "NOT MEASURED",
            Verdict::NoBaseline => "NO BASELINE",
            Verdict::Tracked(_) => "TRACKED",
            Verdict::Skipped => "SKIPPED",
        }
    }

    /// Whether it fails the gate: FAIL or NOT MEASURED of a gated metric.
    pub fn fails_gate(&self) -> bool {
        matches!(self, Verdict::Fail { .. } | Verdict::NotMeasured(_))
    }

    /// The checks that failed, for FAIL (also inside TRACKED).
    pub fn failed_checks(&self) -> Vec<&'static str> {
        match self {
            Verdict::Fail { target, regression } => {
                let mut v = Vec::new();
                if *target {
                    v.push("target");
                }
                if *regression {
                    v.push("regression");
                }
                v
            }
            Verdict::Tracked(inner) => inner.failed_checks(),
            _ => Vec::new(),
        }
    }

    /// Why it was not measured (also inside TRACKED).
    pub fn reason(&self) -> Option<&str> {
        match self {
            Verdict::NotMeasured(r) => Some(r),
            Verdict::Tracked(inner) => inner.reason(),
            _ => None,
        }
    }

    /// The text of the report's Verdict column, e.g. `FAIL (target)`.
    pub fn text(&self) -> String {
        match self {
            Verdict::Fail { .. } => format!("FAIL ({})", self.failed_checks().join(", ")),
            Verdict::NotMeasured(r) => format!("NOT MEASURED ({r})"),
            Verdict::Tracked(inner) => format!("TRACKED (would be {})", inner.nested_text()),
            v => v.label().to_string(),
        }
    }

    /// [`Verdict::text`] without parentheses, for inside TRACKED (...).
    fn nested_text(&self) -> String {
        match self {
            Verdict::Fail { .. } => format!("FAIL: {}", self.failed_checks().join(", ")),
            Verdict::NotMeasured(r) => format!("NOT MEASURED: {r}"),
            v => v.text(),
        }
    }

    /// The verdict a TRACKED metric would have had; itself otherwise.
    fn inner(&self) -> &Verdict {
        match self {
            Verdict::Tracked(inner) => inner,
            v => v,
        }
    }
}

/// One line of the report.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub id: String,
    pub name: String,
    pub key: String,
    pub unit: String,
    pub better: Better,
    /// The baseline of the tier being run.
    pub baseline: Option<f64>,
    /// The aggregated value, when the key has samples.
    pub value: Option<f64>,
    pub target: Option<f64>,
    /// Whether the target was checked at this tier.
    pub target_checked: bool,
    /// The baseline itself misses the target and so does the value: only
    /// the allowance holds the metric until a change meets the target.
    pub target_pending: bool,
    pub verdict: Verdict,
    pub gated: bool,
    pub exception: Option<String>,
}

impl Row {
    /// The signed change against the baseline in percent, for measured
    /// metrics with a non-zero baseline.
    pub fn change_pct(&self) -> Option<f64> {
        if matches!(
            self.verdict.inner(),
            Verdict::NotMeasured(_) | Verdict::Skipped
        ) {
            return None;
        }
        let (base, now) = (self.baseline?, self.value?);
        (base != 0.0).then(|| (now - base) * 100.0 / base.abs())
    }

    /// The Verdict column: [`Verdict::text`], with a pending target noted
    /// on a pass.
    pub fn verdict_text(&self) -> String {
        if self.target_pending && self.verdict == Verdict::Pass {
            "PASS (target not yet met)".to_string()
        } else {
            self.verdict.text()
        }
    }

    /// Whether the value counts as measured (for `--print-values`).
    fn measured(&self) -> bool {
        self.value.is_some()
            && !matches!(
                self.verdict.inner(),
                Verdict::NotMeasured(_) | Verdict::Skipped
            )
    }
}

/// Judges every metric of `tier`, in baseline order. With `partial`, a
/// metric absent from every metrics file is SKIPPED instead of NOT
/// MEASURED (for running one benchmark alone).
pub fn compare(baseline: &Baseline, samples: &Samples, tier: Tier, partial: bool) -> Vec<Row> {
    baseline
        .metrics
        .iter()
        .filter(|m| m.tiers.contains(&tier))
        .map(|m| judge(m, samples, tier, partial))
        .collect()
}

fn judge(m: &Metric, samples: &Samples, tier: Tier, partial: bool) -> Row {
    let base = match tier {
        Tier::Quick => m.quick_baseline,
        Tier::Pr => m.pr_baseline.or(m.baseline),
        Tier::Rc => m.baseline,
    };
    let target_checked = tier != Tier::Quick || m.better == Better::Equal;
    let pending = target_checked && target_pending(m, base);
    let value = m.aggregate.apply(samples.get(&m.key));
    let verdict = match value {
        None if partial => Verdict::Skipped,
        None => gated(m, Verdict::NotMeasured(format!("no {} samples", m.key))),
        Some(v) => gated(
            m,
            check(m, samples, tier, v, base, target_checked && !pending),
        ),
    };
    Row {
        id: m.id.clone(),
        name: m.name.clone(),
        key: m.key.clone(),
        unit: m.unit.clone(),
        better: m.better,
        baseline: base,
        value,
        target: m.target,
        target_checked,
        // Noted only while the value still misses the target too.
        target_pending: pending && value.is_some_and(|v| misses_target(m, v)),
        verdict,
        gated: m.gated,
        exception: m.exception.clone(),
    }
}

/// Whether the baseline itself misses a `lower` or `higher` target. Until a
/// change meets the target (0.3.1's memory, for example, at 1.5 GB against
/// 200 MB), only the allowance holds the metric, so it can't get worse while
/// its fix is pending, and pull requests don't all fail in the meantime. An
/// `equal` target has no allowance to fall back on, so it is always checked.
fn target_pending(m: &Metric, base: Option<f64>) -> bool {
    m.better != Better::Equal && base.is_some_and(|b| misses_target(m, b))
}

/// Whether `value` misses the metric's target; false without one.
pub(crate) fn misses_target(m: &Metric, value: f64) -> bool {
    m.target.is_some_and(|t| match m.better {
        Better::Lower => !at_most(value, t),
        Better::Higher => !at_most(t, value),
        Better::Equal => !nearly_equal(value, t),
    })
}

/// Wraps the verdict of an ungated metric in TRACKED.
fn gated(m: &Metric, v: Verdict) -> Verdict {
    if m.gated {
        v
    } else {
        Verdict::Tracked(Box::new(v))
    }
}

/// The scale, target and regression checks of a measured value.
fn check(
    m: &Metric,
    samples: &Samples,
    tier: Tier,
    value: f64,
    base: Option<f64>,
    target_checked: bool,
) -> Verdict {
    let scale_check = match (tier, m.scale_min, &m.scale_key) {
        (Tier::Pr | Tier::Rc, Some(min), Some(scale_key)) => Some((min, scale_key)),
        _ => None,
    };
    if let Some((min, scale_key)) = scale_check {
        // With several scale samples the smallest counts, so one run at a
        // smaller scale can't hide among full-scale ones.
        match Aggregate::Min.apply(samples.get(scale_key)) {
            None => return Verdict::NotMeasured(format!("{scale_key} not reported")),
            Some(scale) if scale < min => {
                return Verdict::NotMeasured(format!(
                    "below scale: {scale_key} {} < {}",
                    num(scale),
                    num(min)
                ));
            }
            Some(_) => {}
        }
    }
    let target = target_checked && misses_target(m, value);
    let regression = base.is_some_and(|b| {
        let allow = allowance(m, b);
        match m.better {
            Better::Lower => !at_most(value, b + allow),
            Better::Higher => !at_most(b - allow, value),
            // An exact value has no allowance to compare; the target is
            // its check.
            Better::Equal => false,
        }
    });
    if target || regression {
        Verdict::Fail { target, regression }
    } else if base.is_none() {
        Verdict::NoBaseline
    } else {
        Verdict::Pass
    }
}

/// max(|baseline| × allow_pct / 100, allow_floor), or 0 when neither is set.
pub(crate) fn allowance(m: &Metric, base: f64) -> f64 {
    let pct = m.allow_pct.map_or(0.0, |p| base.abs() * p / 100.0);
    pct.max(m.allow_floor.unwrap_or(0.0))
}

/// `a <= b`, forgiving the last bits of decimal rounding so a value exactly
/// at a limit such as 9.3 + 10% passes.
pub(crate) fn at_most(a: f64, b: f64) -> bool {
    a <= b + tolerance(b)
}

pub(crate) fn nearly_equal(a: f64, b: f64) -> bool {
    (a - b).abs() <= tolerance(b)
}

fn tolerance(b: f64) -> f64 {
    1e-9 * b.abs().max(1.0)
}

/// How many metrics got each verdict.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub pass: usize,
    pub fail: usize,
    pub not_measured: usize,
    pub no_baseline: usize,
    pub tracked: usize,
    pub skipped: usize,
}

impl Counts {
    /// Counts the verdicts of `rows`.
    pub fn of(rows: &[Row]) -> Counts {
        let mut c = Counts::default();
        for r in rows {
            match r.verdict {
                Verdict::Pass => c.pass += 1,
                Verdict::Fail { .. } => c.fail += 1,
                Verdict::NotMeasured(_) => c.not_measured += 1,
                Verdict::NoBaseline => c.no_baseline += 1,
                Verdict::Tracked(_) => c.tracked += 1,
                Verdict::Skipped => c.skipped += 1,
            }
        }
        c
    }

    /// `perf-compare: <p> pass, <f> fail, <n> not measured, <b> no
    /// baseline, <t> tracked, <s> skipped`.
    pub fn line(&self) -> String {
        format!(
            "perf-compare: {} pass, {} fail, {} not measured, {} no baseline, {} tracked, \
             {} skipped",
            self.pass, self.fail, self.not_measured, self.no_baseline, self.tracked, self.skipped
        )
    }
}

/// True when no gated metric is FAIL or NOT MEASURED.
pub fn gate_passes(rows: &[Row]) -> bool {
    !rows.iter().any(|r| r.verdict.fails_gate())
}

/// The markdown report: a header with the tier and the `[meta]` table, the
/// table of verdicts, any exceptions, and the [`Counts::line`].
pub fn render_markdown(meta: &Meta, tier: Tier, rows: &[Row]) -> String {
    let mut out = format!("# Performance gate: {tier} tier\n\n");
    let none = None;
    let (pr_build, pr_date) = match tier {
        Tier::Pr => (&meta.pr_build, &meta.pr_date),
        _ => (&none, &none),
    };
    for (label, value) in [
        ("Machine", &meta.machine),
        ("Cache", &meta.cache),
        ("Baseline build", &meta.build),
        ("Baseline date", &meta.date),
        ("PR-tier baseline build", pr_build),
        ("PR-tier baseline date", pr_date),
    ] {
        if let Some(v) = value {
            out.push_str(&format!("{label}: {}\n\n", md_cell(v)));
        }
    }
    out.push_str(
        "| ID | Metric | Baseline | Now | Change | Target | Verdict |\n\
         |---|---|---|---|---|---|---|\n",
    );
    for r in rows {
        let show = |v: Option<f64>| v.map_or_else(|| "-".to_string(), |v| with_unit(v, &r.unit));
        let change = r
            .change_pct()
            .map_or_else(|| "-".to_string(), |c| format!("{c:+.1}%"));
        let target = r.target.map_or_else(
            || "-".to_string(),
            |t| {
                let t = format!("{} {}", r.better.sign(), with_unit(t, &r.unit));
                if r.target_checked {
                    t
                } else {
                    format!("({t})")
                }
            },
        );
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} |\n",
            md_cell(&r.id),
            md_cell(&r.name),
            md_cell(&show(r.baseline)),
            md_cell(&show(r.value)),
            change,
            md_cell(&target),
            md_cell(&r.verdict_text()),
        ));
    }
    if rows.iter().any(|r| r.target.is_some() && !r.target_checked) {
        out.push_str("\nTargets in parentheses are not checked at the quick tier.\n");
    }
    let exceptions: Vec<&Row> = rows.iter().filter(|r| r.exception.is_some()).collect();
    if !exceptions.is_empty() {
        out.push_str("\nExceptions:\n\n");
        for r in exceptions {
            out.push_str(&format!(
                "- {}: {}\n",
                md_cell(&r.id),
                md_cell(r.exception.as_deref().unwrap_or_default())
            ));
        }
    }
    out.push('\n');
    out.push_str(&Counts::of(rows).line());
    out.push('\n');
    out
}

/// The same report as JSON: `tier`, `meta`, one object per metric, the
/// verdict `counts` and `result` (`PASS` or `FAIL`).
pub fn render_json(meta: &Meta, tier: Tier, rows: &[Row]) -> String {
    let opt_str = |s: Option<&str>| s.map_or_else(|| "null".to_string(), json_string);
    let opt_num = |v: Option<f64>| v.map_or_else(|| "null".to_string(), json_num);
    let metrics: Vec<String> = rows
        .iter()
        .map(|r| {
            let failed: Vec<String> = r
                .verdict
                .failed_checks()
                .into_iter()
                .map(json_string)
                .collect();
            let would_be = match &r.verdict {
                Verdict::Tracked(inner) => json_string(inner.label()),
                _ => "null".to_string(),
            };
            format!(
                "{{\"id\":{},\"name\":{},\"key\":{},\"unit\":{},\"better\":{},\"gated\":{},\
                 \"baseline\":{},\"now\":{},\"change_pct\":{},\"target\":{},\
                 \"target_checked\":{},\"target_pending\":{},\"verdict\":{},\"failed\":[{}],\
                 \"reason\":{},\"would_be\":{},\"exception\":{}}}",
                json_string(&r.id),
                json_string(&r.name),
                json_string(&r.key),
                json_string(&r.unit),
                json_string(r.better.as_str()),
                r.gated,
                opt_num(r.baseline),
                opt_num(r.value),
                opt_num(r.change_pct()),
                opt_num(r.target),
                r.target_checked,
                r.target_pending,
                json_string(r.verdict.label()),
                failed.join(","),
                opt_str(r.verdict.reason()),
                would_be,
                opt_str(r.exception.as_deref()),
            )
        })
        .collect();
    let c = Counts::of(rows);
    format!(
        "{{\"tier\":{},\"meta\":{{\"machine\":{},\"cache\":{},\"build\":{},\"date\":{},\
         \"pr_build\":{},\"pr_date\":{}}},\"metrics\":[{}],\"counts\":{{\"pass\":{},\"fail\":{},\"not_measured\":{},\
         \"no_baseline\":{},\"tracked\":{},\"skipped\":{}}},\"result\":{}}}",
        json_string(tier.as_str()),
        opt_str(meta.machine.as_deref()),
        opt_str(meta.cache.as_deref()),
        opt_str(meta.build.as_deref()),
        opt_str(meta.date.as_deref()),
        opt_str(meta.pr_build.as_deref()),
        opt_str(meta.pr_date.as_deref()),
        metrics.join(","),
        c.pass,
        c.fail,
        c.not_measured,
        c.no_baseline,
        c.tracked,
        c.skipped,
        json_string(if gate_passes(rows) { "PASS" } else { "FAIL" }),
    )
}

/// `<ID> <key> = <value>` for each measured metric, for writing a new
/// baseline at a release.
pub fn value_lines(rows: &[Row]) -> Vec<String> {
    rows.iter()
        .filter(|r| r.measured())
        .filter_map(|r| Some(format!("{} {} = {}", r.id, r.key, json_num(r.value?))))
        .collect()
}

/// A value with its unit; `bool` shows 1 and 0 as `true` and `false`.
fn with_unit(v: f64, unit: &str) -> String {
    match unit {
        "bool" if v == 1.0 => "true".to_string(),
        "bool" if v == 0.0 => "false".to_string(),
        "" => num(v),
        _ => format!("{} {unit}", num(v)),
    }
}

/// Up to three decimals, trailing zeros removed.
fn num(v: f64) -> String {
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" {
        "0".to_string()
    } else {
        s.to_string()
    }
}

/// Up to six decimals in the shortest form (`10`, `9.3`), for JSON and
/// `--print-values`.
fn json_num(v: f64) -> String {
    let r = (v * 1e6).round() / 1e6;
    let r = if r == 0.0 { 0.0 } else { r };
    format!("{r}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Baseline, ParseError> {
        parse_baseline(text, "perf-baseline.toml")
    }

    fn err_line(text: &str) -> (usize, String) {
        let e = parse(text).expect_err("must not parse");
        assert_eq!(e.source, "perf-baseline.toml");
        assert!(
            e.to_string()
                .starts_with(&format!("perf-baseline.toml:{}: ", e.line)),
            "{e}"
        );
        (e.line, e.message)
    }

    const GOOD: &str = r#"# The performance baseline.

[meta]
machine = "m4pro"   # the owner's Mac
cache = "warm"
build = "0.3.1"
date = "2026-11-09"

[S1]
name = "Engine search p95 at 2M entries"
key = "engine_2m_ms"            # metric key in the metrics files
unit = "ms"
better = "lower"
target = 16
allow_pct = 10
allow_floor = 1
aggregate = "median"
scale_key = "engine_2m_entries"
scale_min = 2000000
tiers = "quick pr rc"
baseline = 9.3
quick_baseline = 1.1
gated = true
baseline_build = "0.3.1"
baseline_date = "2026-11-09"
exception = "Raised in #57: the \"store\" rewrite \\ save"

[F6u]
name = "Freshness, # not a comment"
key = "freshness_ms"
unit = "ms"
better = "higher"
aggregate = "p95"
tiers = "rc"
target = -2.5
gated = false

[C3]
name = "Correct"
key = "catchup_equal"
unit = "bool"
better = "equal"
aggregate = "min"
tiers = "pr"

[x_1-b]
name = "Dashes"
key = "x"
unit = ""
better = "lower"
aggregate = "last"
tiers = "pr"
"#;

    #[test]
    fn parses_a_good_file() {
        let b = parse(GOOD).expect("parses");
        assert_eq!(b.meta.machine.as_deref(), Some("m4pro"));
        assert_eq!(b.meta.cache.as_deref(), Some("warm"));
        assert_eq!(b.meta.build.as_deref(), Some("0.3.1"));
        assert_eq!(b.meta.date.as_deref(), Some("2026-11-09"));
        let ids: Vec<&str> = b.metrics.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["S1", "F6u", "C3", "x_1-b"]);

        let s1 = &b.metrics[0];
        assert_eq!(s1.line, 9);
        assert_eq!(s1.name, "Engine search p95 at 2M entries");
        assert_eq!(s1.key, "engine_2m_ms");
        assert_eq!(s1.better, Better::Lower);
        assert_eq!(s1.target, Some(16.0));
        assert_eq!(s1.allow_pct, Some(10.0));
        assert_eq!(s1.allow_floor, Some(1.0));
        assert_eq!(s1.aggregate, Aggregate::Median);
        assert_eq!(s1.scale_key.as_deref(), Some("engine_2m_entries"));
        assert_eq!(s1.scale_min, Some(2_000_000.0));
        assert_eq!(s1.tiers, [Tier::Quick, Tier::Pr, Tier::Rc]);
        assert_eq!(s1.baseline, Some(9.3));
        assert_eq!(s1.quick_baseline, Some(1.1));
        assert!(s1.gated);
        assert_eq!(s1.baseline_build.as_deref(), Some("0.3.1"));
        assert_eq!(
            s1.exception.as_deref(),
            Some("Raised in #57: the \"store\" rewrite \\ save")
        );

        let f6u = &b.metrics[1];
        assert_eq!(f6u.name, "Freshness, # not a comment");
        assert_eq!(f6u.better, Better::Higher);
        assert_eq!(f6u.target, Some(-2.5));
        assert!(!f6u.gated);
        assert_eq!(f6u.baseline, None);

        let c3 = &b.metrics[2];
        assert!(c3.gated, "gated defaults to true");
        assert_eq!(c3.better, Better::Equal);
    }

    #[test]
    fn errors_name_the_line() {
        let cases: &[(&str, usize, &str)] = &[
            ("[S1]\nname = \"a\"\nbogus = 1\n", 3, "unknown key 'bogus'"),
            ("name = \"a\"\n", 1, "outside a table"),
            ("[meta]\nmachine = 1\n", 2, "quoted string"),
            ("[meta]\nhost = \"x\"\n", 2, "unknown key 'host'"),
            ("[S1]\n\n[S1]\n", 3, "already defined on line 1"),
            ("[meta]\n[meta]\n", 2, "already defined"),
            ("[S 1]\n", 1, "invalid table name"),
            ("[S1.2]\n", 1, "invalid table name"),
            ("[]\n", 1, "invalid table name"),
            ("[S1\n", 1, "closing ']'"),
            ("[S1] x\n", 1, "after the table header"),
            ("[S1]\nname \"a\"\n", 2, "key = value"),
            ("[S1]\nName = \"a\"\n", 2, "invalid key"),
            ("[S1]\nname = \"a\nkey = \"b\"\n", 2, "closing"),
            ("[S1]\nname = \"a\\n\"\n", 2, "escapes"),
            ("[S1]\nname = \"a\" b\n", 2, "after the string"),
            ("[S1]\ntarget = 1_000\n", 2, "invalid value"),
            ("[S1]\ntarget = 1e3\n", 2, "invalid value"),
            ("[S1]\ntarget = .5\n", 2, "invalid value"),
            ("[S1]\ntarget = 5.\n", 2, "invalid value"),
            ("[S1]\ntarget =\n", 2, "missing value"),
            ("[S1]\ngated = yes\n", 2, "invalid value"),
            ("[S1]\nname = \"a\"\nname = \"b\"\n", 3, "already set"),
        ];
        for &(text, line, needle) in cases {
            let (got_line, message) = err_line(text);
            assert_eq!(got_line, line, "{text:?}: {message}");
            assert!(message.contains(needle), "{text:?}: {message}");
        }
    }

    /// A complete metric table at line 1, with `extra` lines after it.
    fn metric_with(extra: &str) -> String {
        format!(
            "[S1]\nname = \"n\"\nkey = \"k\"\nunit = \"ms\"\nbetter = \"lower\"\n\
             aggregate = \"max\"\ntiers = \"pr\"\n{extra}"
        )
    }

    #[test]
    fn metric_values_are_validated() {
        let cases: &[(&str, usize, &str)] = &[
            ("target = \"16\"\n", 8, "must be a number"),
            ("gated = 1\n", 8, "true or false"),
            ("scale_key = \"Bad Key\"\n", 8, "scale_key"),
            ("scale_min = 5\n", 8, "needs a 'scale_key'"),
            ("allow_pct = -1\n", 8, "negative"),
            ("allow_floor = -0.5\n", 8, "negative"),
            ("exception = \"\"\n", 8, "#<number>"),
            ("exception = \"approved\"\n", 8, "#<number>"),
            ("exception = \"see # 12\"\n", 8, "#<number>"),
        ];
        for &(extra, line, needle) in cases {
            let (got_line, message) = err_line(&metric_with(extra));
            assert_eq!(got_line, line, "{extra:?}: {message}");
            assert!(message.contains(needle), "{extra:?}: {message}");
        }
        assert!(parse(&metric_with("exception = \"ok per #3\"\n")).is_ok());
    }

    #[test]
    fn enumerations_are_validated() {
        let swap = |from: &str, to: &str| metric_with("").replace(from, to);
        for (text, line, needle) in [
            (swap("\"lower\"", "\"less\""), 5, "lower, higher or equal"),
            (swap("\"max\"", "\"p99\""), 6, "median, p95"),
            (
                swap("tiers = \"pr\"", "tiers = \"pr nightly\""),
                7,
                "nightly",
            ),
            (swap("tiers = \"pr\"", "tiers = \"pr pr\""), 7, "twice"),
            (
                swap("tiers = \"pr\"", "tiers = \" \""),
                7,
                "must name a tier",
            ),
            (swap("key = \"k\"", "key = \"K-1\""), 3, "'key'"),
            (swap("name = \"n\"", "name = \" \""), 2, "empty"),
        ] {
            let (got_line, message) = err_line(&text);
            assert_eq!(got_line, line, "{text:?}: {message}");
            assert!(message.contains(needle), "{text:?}: {message}");
        }
    }

    #[test]
    fn missing_required_keys_name_the_table_header() {
        for key in ["name", "key", "unit", "better", "aggregate", "tiers"] {
            let text = metric_with("")
                .lines()
                .filter(|l| !l.starts_with(&format!("{key} =")))
                .collect::<Vec<_>>()
                .join("\n");
            let text = format!("# header comment\n\n{text}\n");
            let (line, message) = err_line(&text);
            assert_eq!(line, 3, "{key}: {message}");
            assert!(message.contains(&format!("missing '{key}'")), "{message}");
        }
    }

    #[test]
    fn each_aggregate() {
        let s = [10.0, 1.0, 4.0, 2.0, 7.0];
        assert_eq!(Aggregate::Median.apply(&s), Some(4.0));
        assert_eq!(Aggregate::P95.apply(&s), Some(10.0));
        assert_eq!(Aggregate::Max.apply(&s), Some(10.0));
        assert_eq!(Aggregate::Min.apply(&s), Some(1.0));
        assert_eq!(Aggregate::Mean.apply(&s), Some(4.8));
        assert_eq!(Aggregate::Last.apply(&s), Some(7.0));
        for a in [
            Aggregate::Median,
            Aggregate::P95,
            Aggregate::Max,
            Aggregate::Min,
            Aggregate::Mean,
            Aggregate::Last,
        ] {
            assert_eq!(a.apply(&[]), None);
            assert_eq!(a.apply(&[3.5]), Some(3.5));
            assert_eq!(Aggregate::parse(a.as_str()), Some(a));
        }
        // Even count: nearest-rank p50 is the lower middle sample.
        assert_eq!(Aggregate::Median.apply(&[4.0, 1.0, 3.0, 2.0]), Some(2.0));
        // p95 drops the top sample of 21, unlike max.
        let mut many: Vec<f64> = (1..=20).map(f64::from).collect();
        many.push(100.0);
        assert_eq!(Aggregate::P95.apply(&many), Some(20.0));
        assert_eq!(Aggregate::Max.apply(&many), Some(100.0));
    }

    #[test]
    fn metrics_files() {
        let mut s = Samples::default();
        parse_metrics(
            "# comment\n\na_ms=1.5\n  a_ms = 2 \nok=true\nok=false\nneg=-3\n",
            "m.txt",
            &mut s,
        )
        .expect("parses");
        assert_eq!(s.get("a_ms"), [1.5, 2.0]);
        assert_eq!(s.get("ok"), [1.0, 0.0]);
        assert_eq!(s.get("neg"), [-3.0]);
        assert!(s.get("missing").is_empty());

        for (text, line) in [
            ("a=1\nGATE engine: PASS\n", 2),
            ("a=fast\n", 1),
            ("A=1\n", 1),
            ("a=inf\n", 1),
            ("a=NaN\n", 1),
            ("a=\n", 1),
            ("=1\n", 1),
            ("\n\n{\"metric\":\"a\",\"value\":1}\n", 3),
        ] {
            let e =
                parse_metrics(text, "m.txt", &mut Samples::default()).expect_err("must not parse");
            assert_eq!(e.line, line, "{text:?}");
            assert!(e.to_string().starts_with(&format!("m.txt:{line}: ")), "{e}");
        }
    }

    fn one(extra: &str) -> Metric {
        parse(&metric_with(extra))
            .expect("parses")
            .metrics
            .remove(0)
    }

    fn verdict_of(m: &Metric, tier: Tier, text: &str) -> Verdict {
        let mut s = Samples::default();
        parse_metrics(text, "m.txt", &mut s).unwrap();
        let b = Baseline {
            meta: Meta::default(),
            metrics: vec![Metric {
                tiers: vec![tier],
                ..m.clone()
            }],
        };
        compare(&b, &s, tier, false).remove(0).verdict
    }

    #[test]
    fn allowance_takes_the_larger_of_percent_and_floor() {
        let m = one("baseline = 1.0\nallow_pct = 10\nallow_floor = 1\n");
        assert_eq!(verdict_of(&m, Tier::Pr, "k=2.0"), Verdict::Pass);
        assert_eq!(
            verdict_of(&m, Tier::Pr, "k=2.0001"),
            Verdict::Fail {
                target: false,
                regression: true
            }
        );
        // 9.3 + 10% = 10.23 exactly passes, despite decimal rounding.
        let m = one("baseline = 9.3\nallow_pct = 10\n");
        assert_eq!(verdict_of(&m, Tier::Pr, "k=10.23"), Verdict::Pass);
    }

    #[test]
    fn verdict_texts() {
        let fail = Verdict::Fail {
            target: true,
            regression: true,
        };
        assert_eq!(fail.text(), "FAIL (target, regression)");
        assert_eq!(
            Verdict::NotMeasured("no a samples".into()).text(),
            "NOT MEASURED (no a samples)"
        );
        assert_eq!(
            Verdict::Tracked(Box::new(Verdict::Fail {
                target: false,
                regression: true
            }))
            .text(),
            "TRACKED (would be FAIL: regression)"
        );
        assert_eq!(Verdict::Skipped.text(), "SKIPPED");
        assert!(!Verdict::Tracked(Box::new(fail)).fails_gate());
        assert!(Verdict::NotMeasured(String::new()).fails_gate());
        assert!(!Verdict::NoBaseline.fails_gate());
        assert!(!Verdict::Skipped.fails_gate());
    }

    #[test]
    fn numbers_are_shown_briefly() {
        assert_eq!(num(9.3), "9.3");
        assert_eq!(num(2_089_192_544.0), "2089192544");
        assert_eq!(num(-0.0001), "0");
        assert_eq!(json_num(10.000_000_000_1), "10");
        assert_eq!(json_num(4.8), "4.8");
        assert_eq!(with_unit(1.0, "bool"), "true");
        assert_eq!(with_unit(0.5, "bool"), "0.5 bool");
        assert_eq!(with_unit(3.0, ""), "3");
    }
}
