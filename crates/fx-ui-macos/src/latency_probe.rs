//! The report of a latency probe of the window: the shortcut probe
//! ([`crate::summon_probe`]) and the re-sort probe ([`crate::sort_probe`])
//! (`measure` builds only).
//!
//! A [`LatencyProbe`] collects the samples of one kind of latency
//! ([`LatencyKind`]) and writes a report a script can read without the
//! rest of the app:
//!
//! ```text
//! fx-ui-macos <name> probe: <n> entries
//! GATE <what> p95 <= <budget> ms: PASS|FAIL (p95=<x>ms p50=<y>ms n=<k>)
//! <name>_p95_ms=<x>
//! <name>_p50_ms=<y>
//! <name>_samples=<k>
//! <name>_entries=<n>
//! ```
//!
//! The GATE line has the form `fx_bench::gate::parse_gate_lines` reads.
//! Percentiles are nearest-rank ([`fx_bench::Stats`]), a p95 exactly at the
//! budget passes, and without samples the GATE line is a FAIL and the p95
//! and p50 lines are left out, so a script never reads a made-up number.
//! Times are milliseconds with two decimals.
//!
//! [`list_settled`] says when the window lists its results steadily, which
//! both probes wait for before they start.

#![forbid(unsafe_code)]

use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

use fx_bench::Stats;

use crate::gate_probe::{Observed, gate_line, ms, write_new_file};

/// How long the results list must go without a new page before a probe
/// starts, so it does not time its first step against a re-run still on
/// its way (the generation re-runs of an index that just finished
/// scanning).
pub const QUIET: Duration = Duration::from_millis(300);

/// How long a probe waits for a steady list ([`list_settled`]) before it
/// starts anyway, so a root that keeps changing still gets a report.
pub const LIST_TIMEOUT: Duration = Duration::from_secs(300);

/// Whether the window lists its results steadily at `now`: its first
/// results have been painted, the source is not `indexing`, and no page has
/// been applied for [`QUIET`] (the last one painted).
pub fn list_settled(now: Instant, seen: &Observed, indexing: bool) -> bool {
    let painted = seen
        .first_results
        .is_some_and(|first| seen.last_draw.is_some_and(|d| d >= first));
    let quiet = seen.last_applied.is_none_or(|applied| {
        now.saturating_duration_since(applied) >= QUIET
            && seen.last_draw.is_some_and(|d| d >= applied)
    });
    painted && quiet && !indexing
}

/// One kind of latency a probe measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LatencyKind {
    /// The probe's name: the report's header says `<name> probe`, and its
    /// metric keys start with `<name>_`.
    pub name: &'static str,
    /// What is timed, as the GATE line's criterion starts.
    pub timed: &'static str,
    /// The p95 budget.
    pub budget: Duration,
}

/// The samples of one latency probe and their verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LatencyProbe {
    kind: LatencyKind,
    entries: usize,
    samples: Vec<Duration>,
}

impl LatencyProbe {
    /// An empty probe of `kind` over `entries` listed entries.
    pub fn new(kind: LatencyKind, entries: usize) -> LatencyProbe {
        LatencyProbe {
            kind,
            entries,
            samples: Vec::new(),
        }
    }

    /// What the probe measures.
    pub fn kind(&self) -> LatencyKind {
        self.kind
    }

    /// The entries listed while the probe ran, as named in the report.
    pub fn entries(&self) -> usize {
        self.entries
    }

    /// Updates the entry count (known once the list is steady).
    pub fn set_entries(&mut self, entries: usize) {
        self.entries = entries;
    }

    /// Records one latency.
    pub fn record(&mut self, latency: Duration) {
        self.samples.push(latency);
    }

    /// The latencies recorded, in order.
    pub fn samples(&self) -> &[Duration] {
        &self.samples
    }

    /// The statistics of the samples (nearest rank), or `None` without
    /// samples.
    pub fn stats(&self) -> Option<Stats> {
        Stats::from_samples(&self.samples)
    }

    /// Whether the p95 is within the budget; never without samples.
    pub fn passes(&self) -> bool {
        self.stats().is_some_and(|s| s.p95 <= self.kind.budget)
    }

    /// The GATE line's criterion, e.g. `ui re-sort to repaint p95 <= 50 ms`.
    pub fn criterion(&self) -> String {
        format!(
            "{} p95 <= {} ms",
            self.kind.timed,
            self.kind.budget.as_millis()
        )
    }

    /// The GATE line.
    pub fn gate_line(&self) -> String {
        let detail = match self.stats() {
            Some(s) => format!(
                "p95={:.2}ms p50={:.2}ms n={}",
                ms(s.p95),
                ms(s.p50),
                s.count
            ),
            None => "no samples".to_string(),
        };
        gate_line(&self.criterion(), self.passes(), &detail)
    }

    /// The `key=value` lines: the p95 and p50 (only with samples), the
    /// sample count and the entry count.
    pub fn metric_lines(&self) -> Vec<String> {
        let name = self.kind.name;
        let mut out = Vec::new();
        if let Some(s) = self.stats() {
            out.push(format!("{name}_p95_ms={:.2}", ms(s.p95)));
            out.push(format!("{name}_p50_ms={:.2}", ms(s.p50)));
        }
        out.push(format!("{name}_samples={}", self.samples.len()));
        out.push(format!("{name}_entries={}", self.entries));
        out
    }

    /// The report: the header, the GATE line and the metric lines.
    pub fn report_text(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "fx-ui-macos {} probe: {} entries",
            self.kind.name, self.entries
        );
        let _ = writeln!(out, "{}", self.gate_line());
        for line in self.metric_lines() {
            let _ = writeln!(out, "{line}");
        }
        out
    }

    /// Writes [`report_text`](Self::report_text) to `path`, a new file: a
    /// file already at `path` is never replaced (the error is
    /// [`std::io::ErrorKind::AlreadyExists`]).
    pub fn write_report(&self, path: &Path) -> std::io::Result<()> {
        write_new_file(path, &self.report_text())
    }
}
