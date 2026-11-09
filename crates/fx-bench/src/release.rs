//! The performance section of a release's notes: `fx-bench perf-release`.
//!
//! Every release candidate and release compares its metrics with the
//! previous release's and with the targets of the baseline file
//! (`docs/gates/perf-baseline.toml`). The section has a summary table (the
//! metric IDs in the `[meta]` table's `summary`, without their IDs) and,
//! inside a `<details>` block that GitHub folds like a release's assets,
//! every metric of the file under its group. Each row carries coloured
//! dots, because GitHub strips text colour from release notes:
//!
//! - Change: 🟢 better · ⚪ the same (within 1%) · 🟡 worse, but within the
//!   metric's allowed regression · 🔴 worse than the allowed regression,
//!   measured from the previous release's value.
//! - Target met: 🟢 met · 🟡 not yet · 🔴 missed, though the previous
//!   release met it. An ungated metric adds "(tracked)".
//!
//! The words stay next to the dots, so the table reads the same without
//! colour.

use std::fmt::Write as _;

use crate::gate::md_cell;
use crate::perf::{
    Baseline, Better, Metric, Samples, allowance, at_most, misses_target, nearly_equal,
};

/// What `perf-release` renders.
pub struct ReleaseInput<'a> {
    pub baseline: &'a Baseline,
    /// The previous release's samples.
    pub previous: &'a Samples,
    /// This release's samples.
    pub current: &'a Samples,
    /// The previous release's name, e.g. `0.3.1`.
    pub previous_label: &'a str,
    /// This release's name, e.g. `0.4.0-RC-1`.
    pub label: &'a str,
    /// Hand-written lines placed under the summary table.
    pub notes: Option<&'a str>,
}

/// Changes smaller than this, in percent, read as "same".
const SAME_PCT: f64 = 1.0;

/// The markdown section, ending with a newline.
pub fn render(input: &ReleaseInput) -> String {
    let b = input.baseline;
    let (prev, cur) = (input.previous_label, input.label);
    let mut out = String::from("## Performance\n\n");
    let mut measured = format!(
        "{cur} compared with {prev} and the targets, measured with `scripts/perf-check.sh`"
    );
    if let Some(machine) = &b.meta.machine {
        let _ = write!(measured, " on {machine}");
    }
    if let Some(cache) = &b.meta.cache {
        let _ = write!(measured, ", {cache} cache");
    }
    let _ = writeln!(out, "{}.\n", md_cell(&measured));
    let _ = writeln!(
        out,
        "🟢 better, or target met · ⚪ the same, within 1% · 🟡 worse but within the \
         allowed regression, or target not yet met · 🔴 worse than the allowed \
         regression, or a target {} met is missed\n",
        md_cell(prev)
    );

    let _ = writeln!(
        out,
        "| Metric | Target | {} | {} | Change | Target met |\n| --- | --- | --- | --- | --- | --- |",
        md_cell(prev),
        md_cell(cur)
    );
    for id in &b.meta.summary {
        if let Some(m) = b.metrics.iter().find(|m| &m.id == id) {
            let cells = row_cells(m, input);
            let _ = writeln!(out, "| {} |", cells.join(" | "));
        }
    }
    if let Some(notes) = input.notes.map(str::trim).filter(|n| !n.is_empty()) {
        let _ = writeln!(out, "\n{notes}");
    }

    // A blank line after <summary> and before </details>: GitHub renders
    // markdown inside <details> only with them.
    let _ = writeln!(
        out,
        "\n<details>\n<summary>All {} metrics</summary>\n",
        b.metrics.len()
    );
    let _ = writeln!(
        out,
        "| ID | Metric | Target | {} | {} | Change | Target met |\n| --- | --- | --- | --- | --- | --- | --- |",
        md_cell(prev),
        md_cell(cur)
    );
    let mut group = "";
    for m in &b.metrics {
        let g = group_of(&m.id);
        if g != group {
            let _ = writeln!(out, "| **{g}** | | | | | | |");
            group = g;
        }
        let cells = row_cells(m, input);
        let _ = writeln!(out, "| {} | {} |", md_cell(&m.id), cells.join(" | "));
    }
    out.push_str("\n</details>\n");
    out
}

/// The group a metric ID belongs to, by its first letter.
fn group_of(id: &str) -> &'static str {
    match id.chars().next() {
        Some('B') => "Background",
        Some('M') => "Memory",
        Some('S') => "Search",
        Some('F') => "Freshness and start-up",
        Some('C') => "Correctness",
        _ => "Other",
    }
}

/// Metric, Target, previous, current, Change and Target met.
fn row_cells(m: &Metric, input: &ReleaseInput) -> [String; 6] {
    let prev = m.aggregate.apply(input.previous.get(&m.key));
    let cur = m.aggregate.apply(input.current.get(&m.key));
    let shown =
        |v: Option<f64>| v.map_or_else(|| "not measured".to_string(), |v| value(&m.unit, v));
    [
        md_cell(&m.name),
        target_cell(m),
        shown(prev),
        shown(cur),
        change_cell(m, prev, cur),
        met_cell(m, prev, cur),
    ]
}

fn target_cell(m: &Metric) -> String {
    let Some(t) = m.target else {
        return "–".to_string();
    };
    if m.unit == "bool" {
        return yes_no(t).to_string();
    }
    let sign = match m.better {
        Better::Lower => "≤",
        Better::Higher => "≥",
        Better::Equal => "",
    };
    let shown = match m.unit.as_str() {
        "% of a core" => format!("{}% of a core", compact(t)),
        "ms" if t >= 10_000.0 => format!("{} s", compact(t / 1000.0)),
        "bytes" => format!("{} B", compact(t)),
        "per s" | "count" => compact(t),
        unit => format!("{} {unit}", compact(t)),
    };
    format!("{sign}{shown}")
}

/// A measured value in its unit, at a precision that reads naturally.
fn value(unit: &str, v: f64) -> String {
    match unit {
        "bool" => yes_no(v).to_string(),
        "% of a core" => format!("{v:.1}%"),
        "ms" if v >= 10_000.0 => format!("{:.1} s", v / 1000.0),
        "ms" => format!("{} ms", by_size(v)),
        "mW" | "MB" => format!("{} {unit}", by_size(v)),
        "B" => format!("{v:.1} B"),
        "bytes" => format!("{} B", grouped(v.round())),
        "per s" => format!("{v:.1}"),
        "count" => grouped(v.round()),
        other => format!("{} {other}", by_size(v)),
    }
}

/// Whole numbers from 100 up, one decimal from 10, two below.
fn by_size(v: f64) -> String {
    if v.abs() >= 100.0 {
        grouped(v.round())
    } else if v.abs() >= 10.0 {
        format!("{v:.1}")
    } else if v == 0.0 {
        "0".to_string()
    } else {
        format!("{v:.2}")
    }
}

/// A target as written: no trailing zeros, thousands grouped.
fn compact(v: f64) -> String {
    if v.fract() == 0.0 {
        grouped(v)
    } else {
        let s = format!("{v:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// A whole number with commas between thousands: 44057 → 44,057.
fn grouped(v: f64) -> String {
    let n = v.round() as i64;
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

fn yes_no(v: f64) -> &'static str {
    if v != 0.0 { "yes" } else { "no" }
}

fn change_cell(m: &Metric, prev: Option<f64>, cur: Option<f64>) -> String {
    let (Some(p), Some(c)) = (prev, cur) else {
        return "–".to_string();
    };
    if m.better == Better::Equal || m.unit == "bool" {
        return if nearly_equal(c, p) {
            "⚪ same".to_string()
        } else {
            "🔴 changed".to_string()
        };
    }
    if nearly_equal(c, p) {
        return "⚪ same".to_string();
    }
    let improved = match m.better {
        Better::Lower => c < p,
        _ => c > p,
    };
    let pct = (p != 0.0).then(|| (c - p).abs() * 100.0 / p.abs());
    if pct.is_some_and(|pct| pct < SAME_PCT) {
        return "⚪ same".to_string();
    }
    let size = pct.map_or_else(String::new, |pct| format!("{pct:.0}% "));
    if improved {
        return format!("🟢 {size}better");
    }
    let limit = allowance(m, p);
    let within = match m.better {
        Better::Lower => at_most(c, p + limit),
        _ => at_most(p - limit, c),
    };
    let dot = if within { "🟡" } else { "🔴" };
    format!("{dot} {size}worse")
}

fn met_cell(m: &Metric, prev: Option<f64>, cur: Option<f64>) -> String {
    let (Some(_), Some(c)) = (m.target, cur) else {
        return "–".to_string();
    };
    let verdict = if !misses_target(m, c) {
        "🟢 Met"
    } else if prev.is_some_and(|p| !misses_target(m, p)) {
        "🔴 Missed"
    } else {
        "🟡 Not yet"
    };
    if m.gated {
        verdict.to_string()
    } else {
        format!("{verdict} (tracked)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_group_and_round() {
        assert_eq!(grouped(44057.1), "44,057");
        assert_eq!(grouped(4200468.0), "4,200,468");
        assert_eq!(grouped(999.0), "999");
        assert_eq!(by_size(742.654), "743");
        assert_eq!(by_size(42.45), "42.5");
        assert_eq!(by_size(7.129), "7.13");
        assert_eq!(by_size(0.0), "0");
        assert_eq!(compact(8.3), "8.3");
        assert_eq!(compact(15.0), "15");
        assert_eq!(compact(1000.0), "1,000");
    }

    #[test]
    fn groups_follow_the_first_letter() {
        assert_eq!(group_of("B1"), "Background");
        assert_eq!(group_of("F6u"), "Freshness and start-up");
        assert_eq!(group_of("O2"), "Other");
    }
}
