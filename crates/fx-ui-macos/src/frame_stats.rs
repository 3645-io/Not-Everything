//! Frame-time statistics for the scroll gate.
//!
//! [`FrameStats`] collects one duration per rendered frame and reports the
//! count, nearest-rank p50, p95 and p99 (computed by
//! [`fx_bench::Stats::from_samples`]), the maximum, and how many frames ran
//! over the budget. The Phase 0 gate is p95 at or below the budget, 16.7 ms
//! (one 60 Hz frame) by default. [`FrameStats::report_text`] ends with a
//! single machine-readable line:
//!
//! ```text
//! GATE: PASS p95=5.00ms budget=16.7ms
//! ```
//!
//! An empty run never passes.

#![forbid(unsafe_code)]

use std::fmt::Write as _;
use std::time::Duration;

use fx_bench::Stats;

/// The default frame budget: one frame at 60 Hz, 16.7 ms.
pub const DEFAULT_BUDGET: Duration = Duration::from_micros(16_700);

/// Collected frame durations and the budget they are judged against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameStats {
    samples: Vec<Duration>,
    budget: Duration,
}

impl Default for FrameStats {
    fn default() -> Self {
        FrameStats::new()
    }
}

impl FrameStats {
    /// Empty statistics with the default 16.7 ms budget.
    pub fn new() -> FrameStats {
        FrameStats::with_budget(DEFAULT_BUDGET)
    }

    /// Empty statistics with a custom frame budget.
    pub fn with_budget(budget: Duration) -> FrameStats {
        FrameStats {
            samples: Vec::new(),
            budget,
        }
    }

    /// Records one frame's duration.
    pub fn record(&mut self, frame: Duration) {
        self.samples.push(frame);
    }

    /// The frame budget.
    pub fn budget(&self) -> Duration {
        self.budget
    }

    /// The recorded durations, in recording order.
    pub fn samples(&self) -> &[Duration] {
        &self.samples
    }

    /// Number of recorded frames.
    pub fn count(&self) -> usize {
        self.samples.len()
    }

    /// Summary statistics (nearest-rank percentiles), or `None` if no frame
    /// was recorded.
    pub fn stats(&self) -> Option<Stats> {
        Stats::from_samples(&self.samples)
    }

    /// Median frame time (nearest rank).
    pub fn p50(&self) -> Option<Duration> {
        self.stats().map(|s| s.p50)
    }

    /// 95th-percentile frame time (nearest rank).
    pub fn p95(&self) -> Option<Duration> {
        self.stats().map(|s| s.p95)
    }

    /// 99th-percentile frame time (nearest rank).
    pub fn p99(&self) -> Option<Duration> {
        self.stats().map(|s| s.p99)
    }

    /// Slowest frame.
    pub fn max(&self) -> Option<Duration> {
        self.samples.iter().copied().max()
    }

    /// Number of frames strictly slower than the budget.
    pub fn over_budget(&self) -> usize {
        self.samples.iter().filter(|&&d| d > self.budget).count()
    }

    /// Fraction of frames strictly slower than the budget; 0.0 when empty.
    pub fn over_budget_fraction(&self) -> f64 {
        if self.samples.is_empty() {
            0.0
        } else {
            self.over_budget() as f64 / self.samples.len() as f64
        }
    }

    /// Whether the gate passes: at least one frame, and p95 at or below the
    /// budget.
    pub fn passes(&self) -> bool {
        self.p95().is_some_and(|p95| p95 <= self.budget)
    }

    /// The final gate line, e.g. `GATE: PASS p95=5.00ms budget=16.7ms`
    /// (`p95=n/a` when no frame was recorded).
    pub fn gate_line(&self) -> String {
        let verdict = if self.passes() { "PASS" } else { "FAIL" };
        let p95 = match self.p95() {
            Some(d) => format!("{:.2}ms", ms(d)),
            None => "n/a".to_string(),
        };
        format!("GATE: {verdict} p95={p95} budget={:.1}ms", ms(self.budget))
    }

    /// A human-readable report: frame count, p50, p95, p99, max and the
    /// over-budget count, ending with the [`gate_line`](Self::gate_line).
    pub fn report_text(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "frames: {}", self.count());
        match self.stats() {
            Some(s) => {
                let _ = writeln!(out, "p50:    {:.2} ms", ms(s.p50));
                let _ = writeln!(out, "p95:    {:.2} ms", ms(s.p95));
                let _ = writeln!(out, "p99:    {:.2} ms", ms(s.p99));
                let _ = writeln!(out, "max:    {:.2} ms", ms(s.max));
            }
            None => {
                let _ = writeln!(out, "p50/p95/p99/max: n/a (no frames)");
            }
        }
        let _ = writeln!(
            out,
            "over budget (>{:.1} ms): {} ({:.1}%)",
            ms(self.budget),
            self.over_budget(),
            self.over_budget_fraction() * 100.0
        );
        out.push_str(&self.gate_line());
        out.push('\n');
        out
    }

    /// The statistics as one JSON object (milliseconds as numbers, `null`
    /// when no frame was recorded), for the spike's `--json` output.
    pub fn report_json(&self) -> String {
        let opt = |d: Option<Duration>| match d {
            Some(d) => format!("{:.3}", ms(d)),
            None => "null".to_string(),
        };
        let s = self.stats();
        format!(
            "{{\"frames\":{},\"p50_ms\":{},\"p95_ms\":{},\"p99_ms\":{},\"max_ms\":{},\
             \"budget_ms\":{:.3},\"over_budget\":{},\"over_budget_fraction\":{:.6},\"pass\":{}}}",
            self.count(),
            opt(s.map(|s| s.p50)),
            opt(s.map(|s| s.p95)),
            opt(s.map(|s| s.p99)),
            opt(s.map(|s| s.max)),
            ms(self.budget),
            self.over_budget(),
            self.over_budget_fraction(),
            self.passes()
        )
    }
}

/// A duration in milliseconds.
fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_report_is_one_object() {
        let mut s = FrameStats::new();
        assert!(s.report_json().contains("\"p95_ms\":null"));
        s.record(Duration::from_millis(4));
        let j = s.report_json();
        assert!(j.starts_with('{') && j.ends_with('}'), "{j}");
        assert!(j.contains("\"pass\":true"), "{j}");
    }
}
