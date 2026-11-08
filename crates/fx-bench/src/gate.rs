//! Gate lines: parse the `GATE ...` verdicts that the Phase 0 benchmarks
//! print, and turn them into one results table (markdown or JSON).
//!
//! Recognised line formats (each line is trimmed first):
//!
//! - (a) `GATE <criterion>: PASS|FAIL[ (<detail>)]`, printed by
//!   fx-fs-macos `fsevents_bench` and fx-store `load_bench`;
//! - (b) `GATE: PASS|FAIL [<text>]`, printed by fx-ui-macos `frame_stats`
//!   (`GATE: PASS p95=5.00ms budget=16.7ms`). The criterion is the text
//!   after the verdict, or `frame p95` when there is none;
//! - `gate <criterion>: PASS|FAIL (<detail>)`, printed by fx-query
//!   `search_bench`;
//! - `PASS|FAIL|SKIP: <criterion> (<detail>)`, printed by fx-fs-macos
//!   `scan_bench`.
//!
//! A line that starts with the word `GATE` but has no verdict becomes a
//! [`Verdict::Unparsed`] entry instead of being dropped. Lowercase `gate`
//! lines count only when they carry a verdict, since other output could
//! start with that word.
//!
//! This module only parses text; it never touches the file system.

use crate::harness::{CacheState, json_string};

/// The outcome of one gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The gate was met.
    Pass,
    /// The gate was missed.
    Fail,
    /// The benchmark skipped the gate (scan_bench `SKIP:`); not a pass.
    Skip,
    /// A `GATE` line without a recognisable verdict.
    Unparsed,
    /// A log file with no gate line at all. Only [`summarize`] produces
    /// this; [`parse_gate_lines`] never does.
    Missing,
}

impl Verdict {
    /// The upper-case label used in tables and JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Pass => "PASS",
            Verdict::Fail => "FAIL",
            Verdict::Skip => "SKIP",
            Verdict::Unparsed => "UNPARSED",
            Verdict::Missing => "MISSING",
        }
    }
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One parsed gate line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateLine {
    /// What the gate checks, e.g. `live p95 <= 50 ms`.
    pub criterion: String,
    /// Pass, fail, skip or unparsed.
    pub verdict: Verdict,
    /// Measured detail, e.g. `12.3 ms`, without the surrounding parentheses.
    pub detail: Option<String>,
}

/// Returns every gate line in `text`, in order. Other lines are ignored.
pub fn parse_gate_lines(text: &str) -> Vec<GateLine> {
    text.lines().filter_map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<GateLine> {
    let t = line.trim();
    if let Some(rest) = strip_word(t, "GATE") {
        return Some(parse_upper(rest));
    }
    if let Some(rest) = strip_word(t, "gate") {
        // search_bench: only lines with a verdict count.
        return criterion_then_verdict(rest);
    }
    for (prefix, verdict) in [
        ("PASS:", Verdict::Pass),
        ("FAIL:", Verdict::Fail),
        ("SKIP:", Verdict::Skip),
    ] {
        if let Some(rest) = t.strip_prefix(prefix) {
            // scan_bench: `<status>: <name> (<detail>)`.
            let (criterion, detail) = split_trailing_detail(rest);
            return Some(GateLine {
                criterion,
                verdict,
                detail,
            });
        }
    }
    None
}

/// Strips `word` from the start of `t` when it is followed by the end of
/// the line, whitespace or `:` (so `GATEWAY` is not a gate line).
fn strip_word<'a>(t: &'a str, word: &str) -> Option<&'a str> {
    let rest = t.strip_prefix(word)?;
    match rest.chars().next() {
        None => Some(rest),
        Some(c) if c.is_whitespace() || c == ':' => Some(rest),
        Some(_) => None,
    }
}

/// Parses what follows `GATE`: format (b) when it starts with `:`, else
/// format (a); anything else is unparsed.
fn parse_upper(rest: &str) -> GateLine {
    if let Some(after) = rest.strip_prefix(':') {
        if let Some((verdict, text)) = verdict_token(after) {
            let text = text.trim();
            let criterion = if text.is_empty() { "frame p95" } else { text };
            return GateLine {
                criterion: criterion.to_string(),
                verdict,
                detail: None,
            };
        }
    } else if let Some(g) = criterion_then_verdict(rest) {
        return g;
    }
    GateLine {
        criterion: rest.trim().trim_start_matches(':').trim().to_string(),
        verdict: Verdict::Unparsed,
        detail: Some("no PASS/FAIL verdict".to_string()),
    }
}

/// Parses `<criterion>: PASS|FAIL[ (<detail>)]`, splitting at the first
/// `:` that is followed by a verdict.
fn criterion_then_verdict(rest: &str) -> Option<GateLine> {
    rest.match_indices(':').find_map(|(i, _)| {
        let (verdict, tail) = verdict_token(&rest[i + 1..])?;
        let criterion = rest[..i].trim().to_string();
        let tail = tail.trim();
        let detail = if tail.is_empty() {
            None
        } else if let Some(inner) = tail.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
            Some(inner.trim().to_string())
        } else {
            Some(tail.to_string())
        };
        Some(GateLine {
            criterion,
            verdict,
            detail,
        })
    })
}

/// Reads a leading `PASS`, `FAIL` or `SKIP` token (after optional
/// whitespace) and returns the verdict and the remaining text.
fn verdict_token(s: &str) -> Option<(Verdict, &str)> {
    let s = s.trim_start();
    [
        ("PASS", Verdict::Pass),
        ("FAIL", Verdict::Fail),
        ("SKIP", Verdict::Skip),
    ]
    .into_iter()
    .find_map(|(tok, v)| {
        let rest = s.strip_prefix(tok)?;
        match rest.chars().next() {
            None => Some((v, rest)),
            Some(c) if c.is_whitespace() || c == '(' => Some((v, rest)),
            Some(_) => None,
        }
    })
}

/// Splits `name (detail)` into the name and the detail of the trailing
/// balanced parenthesised group, if there is one.
fn split_trailing_detail(s: &str) -> (String, Option<String>) {
    let s = s.trim();
    if s.ends_with(')') {
        let mut depth = 0usize;
        for (i, c) in s.char_indices().rev() {
            match c {
                ')' => depth += 1,
                '(' => {
                    depth -= 1;
                    if depth == 0 {
                        let name = s[..i].trim();
                        if name.is_empty() {
                            break;
                        }
                        let inner = s[i + 1..s.len() - 1].trim();
                        return (name.to_string(), Some(inner.to_string()));
                    }
                }
                _ => {}
            }
        }
    }
    (s.to_string(), None)
}

/// One row of a gate summary: a gate line from one log file, labelled with
/// the machine and cache state it was measured on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateRow {
    /// The log file's name.
    pub source: String,
    /// What the gate checks.
    pub criterion: String,
    /// The verdict; [`Verdict::Missing`] when the file had no gate line.
    pub verdict: Verdict,
    /// Measured detail, if printed.
    pub detail: Option<String>,
    /// Machine label given on the command line.
    pub machine: String,
    /// Cache state given on the command line.
    pub cache: CacheState,
}

/// Turns one log file's text into rows: one per gate line, or a single
/// [`Verdict::Missing`] row when the text has none.
pub fn summarize(source: &str, text: &str, machine: &str, cache: CacheState) -> Vec<GateRow> {
    let row = |criterion: String, verdict, detail| GateRow {
        source: source.to_string(),
        criterion,
        verdict,
        detail,
        machine: machine.to_string(),
        cache,
    };
    let lines = parse_gate_lines(text);
    if lines.is_empty() {
        return vec![row(
            "(no gate line)".to_string(),
            Verdict::Missing,
            Some("the log has no GATE line".to_string()),
        )];
    }
    lines
        .into_iter()
        .map(|g| row(g.criterion, g.verdict, g.detail))
        .collect()
}

/// True when there is at least one row and every row passed.
pub fn all_pass(rows: &[GateRow]) -> bool {
    !rows.is_empty() && rows.iter().all(|r| r.verdict == Verdict::Pass)
}

/// Renders rows as a markdown table followed by a one-line result.
pub fn to_markdown(rows: &[GateRow]) -> String {
    let mut out = String::from(
        "| Source | Criterion | Verdict | Detail | Machine | Cache |\n\
         |---|---|---|---|---|---|\n",
    );
    for r in rows {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            md_cell(&r.source),
            md_cell(&r.criterion),
            r.verdict,
            md_cell(r.detail.as_deref().unwrap_or("")),
            md_cell(&r.machine),
            r.cache,
        ));
    }
    let passed = rows.iter().filter(|r| r.verdict == Verdict::Pass).count();
    out.push_str(&format!(
        "\nResult: {} ({passed} of {} gates passed)\n",
        if all_pass(rows) { "PASS" } else { "FAIL" },
        rows.len()
    ));
    out
}

/// Renders rows as a JSON object with an overall result and a row array.
pub fn to_json(rows: &[GateRow]) -> String {
    let body: Vec<String> = rows
        .iter()
        .map(|r| {
            format!(
                "{{\"source\":{},\"criterion\":{},\"verdict\":{},\"detail\":{},\
                 \"machine\":{},\"cache\":{}}}",
                json_string(&r.source),
                json_string(&r.criterion),
                json_string(r.verdict.as_str()),
                r.detail.as_deref().map_or("null".to_string(), json_string),
                json_string(&r.machine),
                json_string(r.cache.as_str()),
            )
        })
        .collect();
    format!(
        "{{\"result\":{},\"rows\":[{}]}}",
        json_string(if all_pass(rows) { "PASS" } else { "FAIL" }),
        body.join(",")
    )
}

/// Escapes a markdown table cell: pipes are escaped and control characters
/// become spaces.
pub(crate) fn md_cell(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '|' => out.push_str("\\|"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_is_not_a_gate_line() {
        assert!(parse_gate_lines("GATEWAY: PASS").is_empty());
    }

    #[test]
    fn lowercase_gate_without_verdict_is_ignored() {
        assert!(parse_gate_lines("gate threshold: 10 ms").is_empty());
    }

    #[test]
    fn nested_parens_in_scan_detail() {
        let g = parse_gate_lines("FAIL: x (a (b) c)");
        assert_eq!(g[0].criterion, "x");
        assert_eq!(g[0].detail.as_deref(), Some("a (b) c"));
    }

    #[test]
    fn pipes_are_escaped_in_markdown() {
        let rows = summarize("a|b.log", "GATE x|y: PASS", "m", CacheState::Warm);
        let md = to_markdown(&rows);
        assert!(md.contains("a\\|b.log"), "{md}");
        assert!(md.contains("x\\|y"), "{md}");
    }
}
