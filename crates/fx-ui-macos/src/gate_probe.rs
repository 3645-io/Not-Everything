//! The Phase 1 gate probe: what `fx-ui-macos --gate-probe <out>` measures
//! over the real engine, and the report it writes (`measure` builds only).
//!
//! Started with `--gate-probe <out>`, the app opens the engine over its
//! fx-store index for `--root` (never the synthetic source) and measures:
//!
//! - goal 1: process start to the first result row painted, one sample per
//!   launch (the gate script runs 20 launches and takes the p95), budget
//!   [`STARTUP_BUDGET`] (2.0 s);
//! - goal 2: query change to table repaint for every step of a scripted
//!   typing sequence ([`default_keystroke_script`]), p95 budget
//!   [`KEYSTROKE_BUDGET`] (50 ms);
//! - autoscroll frame times over the real engine, p95 budget
//!   [`crate::frame_stats::DEFAULT_BUDGET`] (16.7 ms).
//!
//! This module holds the platform-agnostic parts: the timing aggregation and
//! report ([`GateProbe`]), the scripted keystrokes ([`keystroke_script`]) and
//! the order of the measurements ([`ProbeRun`], a state machine the AppKit
//! window drives from its display-rate timer). The window only performs the
//! actions [`ProbeRun::tick`] asks for and reports the times it observed.
//!
//! The report is text with one `GATE <criterion>: PASS|FAIL (<detail>)` line
//! per criterion, as `fx_bench::gate::parse_gate_lines` reads it. A criterion
//! without samples is a FAIL line, never a missing one; percentiles are
//! nearest-rank ([`fx_bench::Stats`]); a value exactly at its budget passes.
//! After the GATE lines come the measured values as `key=value` lines
//! ([`GateProbe::metric_lines`]) for the performance gate's script; the
//! lines before them are unchanged, as `scripts/phase1-gate.sh` reads them.
//!
//! The probe only reads the index and writes its own report file, which it
//! creates: it never replaces an existing file, so the app stays read-only.

#![forbid(unsafe_code)]

use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

use fx_bench::Stats;

use crate::frame_stats::FrameStats;

/// Goal 1: process start to the first result row painted, at most 2.0 s.
pub const STARTUP_BUDGET: Duration = Duration::from_secs(2);

/// Goal 2: keystroke to table repaint, p95 at most 50 ms.
pub const KEYSTROKE_BUDGET: Duration = Duration::from_millis(50);

/// Frames scrolled by the probe's autoscroll phase.
pub const PROBE_SCROLL_FRAMES: u64 = 600;

/// How long one probe step (the first result, one keystroke) may take
/// before the probe records it as that slow and moves on, so an empty or
/// broken index still produces a report.
pub const STEP_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the probe waits for the first result row before giving up on
/// goal 1 (its line is then a FAIL with no sample).
pub const FIRST_RESULT_TIMEOUT: Duration = Duration::from_secs(60);

/// The queries the scripted typing sequence types, one character at a time.
/// Common file-name fragments, so a real disk has hits for most steps; no
/// query's last step repeats the next query's first, so every step changes
/// the query.
pub const DEFAULT_QUERIES: &[&str] = &[
    "readme",
    "config.json",
    "main.rs",
    "index",
    "png",
    "test",
    "lib",
    "photo",
];

/// The typing sequence for `queries`: each non-empty query typed one
/// character (not byte) at a time, e.g. `["ab", "c"]` gives `a`, `ab`, `c`.
pub fn keystroke_script(queries: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for q in queries {
        let mut typed = String::new();
        for c in q.chars() {
            typed.push(c);
            out.push(typed.clone());
        }
    }
    out
}

/// [`keystroke_script`] over [`DEFAULT_QUERIES`].
pub fn default_keystroke_script() -> Vec<String> {
    keystroke_script(DEFAULT_QUERIES)
}

/// The samples of one probe run and their verdicts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateProbe {
    entries: usize,
    first_result: Option<Duration>,
    keystrokes: Vec<Duration>,
    frames: FrameStats,
}

impl GateProbe {
    /// An empty probe over an index of `entries` entries.
    pub fn new(entries: usize) -> GateProbe {
        GateProbe {
            entries,
            first_result: None,
            keystrokes: Vec::new(),
            frames: FrameStats::new(),
        }
    }

    /// Number of entries in the index, as named in the report.
    pub fn entries(&self) -> usize {
        self.entries
    }

    /// Updates the index size (known once the store has loaded).
    pub fn set_entries(&mut self, entries: usize) {
        self.entries = entries;
    }

    /// Records process start to the first result row painted.
    pub fn set_first_result(&mut self, elapsed: Duration) {
        self.first_result = Some(elapsed);
    }

    /// Process start to the first result row painted, if recorded.
    pub fn first_result(&self) -> Option<Duration> {
        self.first_result
    }

    /// Records one keystroke-to-repaint latency.
    pub fn record_keystroke(&mut self, latency: Duration) {
        self.keystrokes.push(latency);
    }

    /// Keystroke latency statistics (nearest rank), or `None` without
    /// samples.
    pub fn keystroke_stats(&self) -> Option<Stats> {
        Stats::from_samples(&self.keystrokes)
    }

    /// Records one autoscroll frame time.
    pub fn record_frame(&mut self, frame: Duration) {
        self.frames.record(frame);
    }

    /// The autoscroll frame times, judged against
    /// [`crate::frame_stats::DEFAULT_BUDGET`].
    pub fn frames(&self) -> &FrameStats {
        &self.frames
    }

    fn startup_passes(&self) -> bool {
        self.first_result.is_some_and(|d| d <= STARTUP_BUDGET)
    }

    fn keystrokes_pass(&self) -> bool {
        self.keystroke_stats()
            .is_some_and(|s| s.p95 <= KEYSTROKE_BUDGET)
    }

    /// Whether every criterion passes; a criterion without samples fails.
    pub fn passes(&self) -> bool {
        self.startup_passes() && self.keystrokes_pass() && self.frames.passes()
    }

    /// The three gate lines, in order: first result, keystroke, frame.
    pub fn gate_lines(&self) -> [String; 3] {
        let startup_detail = match self.first_result {
            Some(d) => format!("{:.2} ms, {} entries", ms(d), self.entries),
            None => format!("no first result painted, {} entries", self.entries),
        };
        let keystroke_detail = match self.keystroke_stats() {
            Some(s) => format!(
                "p95={:.2}ms p50={:.2}ms max={:.2}ms n={}",
                ms(s.p95),
                ms(s.p50),
                ms(s.max),
                s.count
            ),
            None => "no keystroke samples".to_string(),
        };
        let frame_detail = match self.frames.stats() {
            Some(s) => format!(
                "p95={:.2}ms p50={:.2}ms max={:.2}ms n={} over budget={}",
                ms(s.p95),
                ms(s.p50),
                ms(s.max),
                s.count,
                self.frames.over_budget()
            ),
            None => "no frame samples".to_string(),
        };
        [
            gate_line(
                &format!(
                    "ui launch to first result painted <= {:.1} s",
                    STARTUP_BUDGET.as_secs_f64()
                ),
                self.startup_passes(),
                &startup_detail,
            ),
            gate_line(
                &format!(
                    "ui keystroke to repaint p95 <= {} ms",
                    KEYSTROKE_BUDGET.as_millis()
                ),
                self.keystrokes_pass(),
                &keystroke_detail,
            ),
            gate_line(
                &format!(
                    "ui autoscroll frame p95 <= {:.1} ms",
                    ms(self.frames.budget())
                ),
                self.frames.passes(),
                &frame_detail,
            ),
        ]
    }

    /// The report: a short header naming the index size, then one
    /// `GATE <criterion>: PASS|FAIL (<detail>)` line per criterion.
    pub fn report_text(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "fx-ui-macos gate probe: {} entries", self.entries);
        let _ = writeln!(out, "keystroke samples: {}", self.keystrokes.len());
        let _ = writeln!(out, "autoscroll frames: {}", self.frames.count());
        for line in self.gate_lines() {
            out.push_str(&line);
            out.push('\n');
        }
        for line in self.metric_lines() {
            out.push_str(&line);
            out.push('\n');
        }
        out
    }

    /// The measured values as `key=value` lines, after the GATE lines:
    /// `launch_ms` (process start to the first result painted),
    /// `keystroke_p95_ms`, `scroll_p95_ms` (the autoscroll frame p95) and
    /// `ui_entries`. A value without samples has no line, so a script never
    /// reads a made-up number.
    pub fn metric_lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(d) = self.first_result {
            out.push(format!("launch_ms={:.2}", ms(d)));
        }
        if let Some(s) = self.keystroke_stats() {
            out.push(format!("keystroke_p95_ms={:.2}", ms(s.p95)));
        }
        if let Some(s) = self.frames.stats() {
            out.push(format!("scroll_p95_ms={:.2}", ms(s.p95)));
        }
        out.push(format!("ui_entries={}", self.entries));
        out
    }

    /// Writes [`report_text`](Self::report_text) to `path`, a new file:
    /// only that file is written, and a file already at `path` is never
    /// replaced (the error is [`std::io::ErrorKind::AlreadyExists`]).
    pub fn write_report(&self, path: &Path) -> std::io::Result<()> {
        write_new_file(path, &self.report_text())
    }
}

/// Writes `text` to `path`, a new file: a probe only ever creates its own
/// report, so a file already at `path` is never replaced (the error is
/// [`std::io::ErrorKind::AlreadyExists`]) and the app stays read-only.
pub(crate) fn write_new_file(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(text.as_bytes())
}

/// A `GATE <criterion>: PASS|FAIL (<detail>)` line.
pub(crate) fn gate_line(criterion: &str, pass: bool, detail: &str) -> String {
    let verdict = if pass { "PASS" } else { "FAIL" };
    format!("GATE {criterion}: {verdict} ({detail})")
}

/// A duration in milliseconds.
pub(crate) fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1_000.0
}

/// What the window should do on this tick of a [`ProbeRun`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeAction {
    /// Nothing yet: wait for results or a repaint.
    Wait,
    /// Put this text in the search field and run it as the query.
    SetQuery(String),
    /// Scroll one frame, time it and pass the time to
    /// [`ProbeRun::record_frame`].
    ScrollFrame,
    /// Write the report and quit.
    Finish,
}

/// What the window observed so far, passed to every [`ProbeRun::tick`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Observed {
    /// When results with at least one row were first applied to the table.
    pub first_results: Option<Instant>,
    /// When a results page was last applied to the table.
    pub last_applied: Option<Instant>,
    /// When the table last drew.
    pub last_draw: Option<Instant>,
    /// Rows in the results shown.
    pub rows: usize,
}

impl Observed {
    /// Whether a page applied at or after `since` has been painted; returns
    /// the paint time.
    pub fn painted_since(&self, since: Instant) -> Option<Instant> {
        let applied = self.last_applied.filter(|&a| a >= since)?;
        self.last_draw.filter(|&d| d >= applied)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Phase {
    FirstResult,
    Typing {
        index: usize,
        typed_at: Option<Instant>,
    },
    Resetting {
        typed_at: Option<Instant>,
    },
    Scrolling {
        done: u64,
    },
    Done,
}

/// The order of one probe run: wait for the first painted result, type the
/// script one step at a time (each step waits for its repaint), clear the
/// query so every entry is listed, autoscroll, finish.
#[derive(Clone, Debug)]
pub struct ProbeRun {
    probe: GateProbe,
    script: Vec<String>,
    process_start: Instant,
    scroll_frames: u64,
    phase: Phase,
}

impl ProbeRun {
    /// A run typing `script` and scrolling `scroll_frames` frames, timing
    /// startup from `process_start`.
    pub fn new(process_start: Instant, script: Vec<String>, scroll_frames: u64) -> ProbeRun {
        ProbeRun {
            probe: GateProbe::new(0),
            script,
            process_start,
            scroll_frames,
            phase: Phase::FirstResult,
        }
    }

    /// A run with [`default_keystroke_script`] and [`PROBE_SCROLL_FRAMES`].
    pub fn with_defaults(process_start: Instant) -> ProbeRun {
        ProbeRun::new(
            process_start,
            default_keystroke_script(),
            PROBE_SCROLL_FRAMES,
        )
    }

    /// The samples so far.
    pub fn probe(&self) -> &GateProbe {
        &self.probe
    }

    /// Whether the run has finished.
    pub fn is_done(&self) -> bool {
        self.phase == Phase::Done
    }

    /// Records the time of a frame scrolled for [`ProbeAction::ScrollFrame`].
    pub fn record_frame(&mut self, frame: Duration) {
        self.probe.record_frame(frame);
        if let Phase::Scrolling { done } = &mut self.phase {
            *done += 1;
        }
    }

    /// Advances the run with what the window has observed at `now`, and
    /// says what to do next.
    pub fn tick(&mut self, now: Instant, seen: Observed) -> ProbeAction {
        loop {
            match self.phase.clone() {
                Phase::FirstResult => {
                    let painted = seen
                        .first_results
                        .and_then(|first| seen.last_draw.filter(|&d| d >= first));
                    if let Some(drawn) = painted {
                        self.probe
                            .set_first_result(drawn.saturating_duration_since(self.process_start));
                        self.probe.set_entries(seen.rows);
                    } else if now.saturating_duration_since(self.process_start)
                        < FIRST_RESULT_TIMEOUT
                    {
                        return ProbeAction::Wait;
                    }
                    self.phase = Phase::Typing {
                        index: 0,
                        typed_at: None,
                    };
                }
                Phase::Typing { index, typed_at } => {
                    let Some(query) = self.script.get(index) else {
                        self.phase = Phase::Resetting { typed_at: None };
                        continue;
                    };
                    let Some(t) = typed_at else {
                        self.phase = Phase::Typing {
                            index,
                            typed_at: Some(now),
                        };
                        return ProbeAction::SetQuery(query.clone());
                    };
                    let latency = match seen.painted_since(t) {
                        Some(drawn) => drawn.saturating_duration_since(t),
                        None if now.saturating_duration_since(t) >= STEP_TIMEOUT => {
                            now.saturating_duration_since(t)
                        }
                        None => return ProbeAction::Wait,
                    };
                    self.probe.record_keystroke(latency);
                    self.phase = Phase::Typing {
                        index: index + 1,
                        typed_at: None,
                    };
                }
                Phase::Resetting { typed_at } => {
                    let Some(t) = typed_at else {
                        self.phase = Phase::Resetting {
                            typed_at: Some(now),
                        };
                        return ProbeAction::SetQuery(String::new());
                    };
                    if seen.painted_since(t).is_some() {
                        self.probe.set_entries(seen.rows);
                    } else if now.saturating_duration_since(t) < STEP_TIMEOUT {
                        return ProbeAction::Wait;
                    }
                    self.phase = Phase::Scrolling { done: 0 };
                }
                Phase::Scrolling { done } => {
                    if done < self.scroll_frames && seen.rows > 0 {
                        return ProbeAction::ScrollFrame;
                    }
                    self.phase = Phase::Done;
                }
                Phase::Done => return ProbeAction::Finish,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn run_measures_startup_keystrokes_and_frames_in_order() {
        let t0 = Instant::now();
        let mut run = ProbeRun::new(t0, keystroke_script(&["ab"]), 2);
        let mut seen = Observed::default();
        assert_eq!(run.tick(at(t0, 10), seen), ProbeAction::Wait);

        seen.first_results = Some(at(t0, 100));
        seen.last_applied = Some(at(t0, 100));
        seen.last_draw = Some(at(t0, 120));
        seen.rows = 500;
        assert_eq!(
            run.tick(at(t0, 130), seen),
            ProbeAction::SetQuery("a".into())
        );
        assert_eq!(run.probe().first_result(), Some(Duration::from_millis(120)));
        assert_eq!(run.probe().entries(), 500);

        // Not painted yet.
        assert_eq!(run.tick(at(t0, 140), seen), ProbeAction::Wait);
        seen.last_applied = Some(at(t0, 150));
        seen.last_draw = Some(at(t0, 160));
        assert_eq!(
            run.tick(at(t0, 170), seen),
            ProbeAction::SetQuery("ab".into())
        );
        seen.last_applied = Some(at(t0, 180));
        seen.last_draw = Some(at(t0, 190));
        assert_eq!(
            run.tick(at(t0, 200), seen),
            ProbeAction::SetQuery(String::new())
        );
        let ks = run.probe().keystroke_stats().expect("two keystrokes");
        assert_eq!(ks.count, 2);
        assert_eq!(ks.max, Duration::from_millis(30));

        seen.last_applied = Some(at(t0, 210));
        seen.last_draw = Some(at(t0, 220));
        seen.rows = 1000;
        assert_eq!(run.tick(at(t0, 230), seen), ProbeAction::ScrollFrame);
        assert_eq!(run.probe().entries(), 1000);
        run.record_frame(Duration::from_millis(3));
        assert_eq!(run.tick(at(t0, 240), seen), ProbeAction::ScrollFrame);
        run.record_frame(Duration::from_millis(4));
        assert_eq!(run.tick(at(t0, 250), seen), ProbeAction::Finish);
        assert!(run.is_done());
        assert!(run.probe().passes());
    }

    #[test]
    fn a_stuck_keystroke_times_out_as_a_slow_sample() {
        let t0 = Instant::now();
        let mut run = ProbeRun::new(t0, keystroke_script(&["a"]), 0);
        let seen = Observed {
            first_results: Some(at(t0, 1)),
            last_applied: Some(at(t0, 1)),
            last_draw: Some(at(t0, 2)),
            rows: 1,
        };
        assert_eq!(run.tick(at(t0, 3), seen), ProbeAction::SetQuery("a".into()));
        assert_eq!(run.tick(at(t0, 1_000), seen), ProbeAction::Wait);
        let late = at(t0, 3) + STEP_TIMEOUT;
        assert_eq!(run.tick(late, seen), ProbeAction::SetQuery(String::new()));
        assert_eq!(
            run.probe().keystroke_stats().map(|s| s.max),
            Some(STEP_TIMEOUT)
        );
    }

    #[test]
    fn no_first_result_gives_up_after_the_timeout() {
        let t0 = Instant::now();
        let mut run = ProbeRun::new(t0, Vec::new(), 10);
        let seen = Observed::default();
        assert_eq!(run.tick(at(t0, 5), seen), ProbeAction::Wait);
        assert_eq!(
            run.tick(t0 + FIRST_RESULT_TIMEOUT, seen),
            ProbeAction::SetQuery(String::new())
        );
        assert_eq!(run.probe().first_result(), None);
        // Nothing to scroll in an empty index.
        assert_eq!(
            run.tick(t0 + FIRST_RESULT_TIMEOUT + STEP_TIMEOUT, seen),
            ProbeAction::Finish
        );
        assert!(!run.probe().passes());
    }
}
