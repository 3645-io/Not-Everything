//! The measurements that drive the window from a timer of their own
//! (`measure` builds only): the shortcut probe (`--summon-probe`), the
//! re-sort probe (`--sort-probe`) and the start states of a `--window` run.
//!
//! The probes' order and timing live in [`crate::summon_probe`] and
//! [`crate::sort_probe`]; this file only does what their `tick` asks, on a
//! repeating timer whose target is a small [`ProbeTicker`] object:
//!
//! - The shortcut probe hides the window with the controller's
//!   `hide_window` (what Escape in the empty search field calls) and shows
//!   it with `toggle_window` (what the global hotkey's handler calls). It
//!   looks at the window every [`SUMMON_POLL`]; once the window is on
//!   screen, key in the active app, with the search field holding the
//!   focus, it runs the window's display pass and commits it (`draw_now`),
//!   and the sample ends when that returns.
//! - The re-sort probe sends a column title's action, as a click on it
//!   does, so the sample covers `headerClicked:`, the search worker and the
//!   repaint; it reads the times the window records (`observed`) at the
//!   display rate.
//! - `--window search` puts the text in the search field and sends the
//!   field's action, as typing does, before the run loop starts.
//!   `--window hidden` waits until the launched app is active (at most
//!   [`ACTIVE_WAIT`]), hides the window with `hide_window`, as Escape does,
//!   checks for [`HIDDEN_CHECK`] that it stays hidden, then stops its timer,
//!   so the run that is sampled has no probe timer.
//!
//! A probe prints its report, writes it to its output file (a new file;
//! exit code 1 when it cannot be written), and quits: exit code 0, or 1
//! without samples.

use std::cell::RefCell;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSScreen};
use objc2_foundation::{NSString, NSTimer};

use super::WindowConfig;
use super::classes::{WinController, WinUi, with_controller};
use crate::APP_NAME;
use crate::app_args::WindowStart;
use crate::latency_probe::LatencyProbe;
use crate::sort_probe::{SortAction, SortRun};
use crate::source::ResultsSource;
use crate::summon_probe::{SummonAction, SummonRun, SummonSeen, WindowState};

/// How often the shortcut probe looks at the window. Its samples end at
/// the first look that finds the window ready, so this bounds how late
/// that look can be.
const SUMMON_POLL: Duration = Duration::from_millis(1);

/// How often the hidden start looks at the window.
const START_POLL: Duration = Duration::from_millis(50);

/// How long the hidden start waits for the launched app to become active
/// before it hides the window anyway. Hiding first would let the launch's
/// own activation, arriving later, bring the window back.
const ACTIVE_WAIT: Duration = Duration::from_secs(3);

/// How long the hidden start checks that the window stays hidden, hiding it
/// again if it came back, before it stops its timer.
const HIDDEN_CHECK: Duration = Duration::from_secs(2);

/// One measurement of this file and the source it reads the index state of.
pub(super) struct Probe {
    job: Job,
    source: Arc<dyn ResultsSource>,
}

enum Job {
    Summon { run: SummonRun, out: PathBuf },
    Sort { run: SortRun, out: PathBuf },
    Hidden(Hiding),
    Search(String),
}

#[derive(Clone, Copy)]
enum Hiding {
    WaitActive { since: Instant },
    Checking { until: Instant },
}

impl Probe {
    /// Takes this file's measurement out of `config`, if it asks for one.
    pub(super) fn take(config: &mut WindowConfig) -> Option<Probe> {
        let now = Instant::now();
        let job = if let Some(out) = config.summon_probe.take() {
            Job::Summon {
                run: SummonRun::with_defaults(now),
                out,
            }
        } else if let Some(out) = config.sort_probe.take() {
            Job::Sort {
                run: SortRun::with_defaults(now),
                out,
            }
        } else {
            match config.window_start.take()? {
                WindowStart::Hidden => Job::Hidden(Hiding::WaitActive { since: now }),
                WindowStart::Search(text) => Job::Search(text),
            }
        };
        Some(Probe {
            job,
            source: Arc::clone(&config.source),
        })
    }

    /// Starts the measurement on the window `controller` drives: the search
    /// at once, the rest on a repeating timer.
    pub(super) fn start(self, mtm: MainThreadMarker, controller: &WinController) {
        let interval = match &self.job {
            Job::Search(text) => {
                search_for(controller, text);
                return;
            }
            Job::Summon { .. } => SUMMON_POLL,
            Job::Sort { .. } => display_interval(mtm),
            Job::Hidden(_) => START_POLL,
        };
        let ticker = ProbeTicker::new(mtm, self);
        let target: &AnyObject = ticker.as_ref();
        // SAFETY: `probeTick:` is implemented by ProbeTicker as
        // `-(void)probeTick:(NSTimer *)timer`; the run loop retains the
        // repeating timer, which retains its target until it is invalidated.
        let _timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                interval.as_secs_f64(),
                target,
                sel!(probeTick:),
                None,
                true,
            )
        };
    }
}

/// Instance variables of [`ProbeTicker`]: the measurement it drives.
struct TickerIvars {
    probe: RefCell<Probe>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; ProbeTicker does not
    // implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FxProbeTicker"]
    #[ivars = TickerIvars]
    struct ProbeTicker;

    impl ProbeTicker {
        /// The measurement's repeating timer; stopped when it is done.
        #[unsafe(method(probeTick:))]
        fn probe_tick(&self, timer: &NSTimer) {
            let mut running = true;
            with_controller(|c| running = self.tick(c));
            if !running {
                timer.invalidate();
            }
        }
    }
);

impl ProbeTicker {
    fn new(mtm: MainThreadMarker, probe: Probe) -> Retained<ProbeTicker> {
        let this = ProbeTicker::alloc(mtm).set_ivars(TickerIvars {
            probe: RefCell::new(probe),
        });
        // SAFETY: `init` is NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }

    /// One tick of the measurement; `false` once it needs no more.
    fn tick(&self, c: &WinController) -> bool {
        // A tick that AppKit re-enters (it never should) is skipped.
        let Ok(mut probe) = self.ivars().probe.try_borrow_mut() else {
            return true;
        };
        let Some(ui) = c.ui() else { return true };
        let app = NSApplication::sharedApplication(self.mtm());
        let Probe { job, source } = &mut *probe;
        match job {
            Job::Summon { run, out } => {
                let seen = SummonSeen {
                    results: c.observed(),
                    indexing: source.status().indexing,
                    window: WindowState {
                        on_screen: ui.window.isVisible(),
                        key: ui.window.isKeyWindow(),
                        app_active: app.isActive(),
                        field_focused: ui.top.editing(),
                    },
                };
                match run.tick(Instant::now(), seen) {
                    SummonAction::Wait => {}
                    SummonAction::Hide => c.hide_window(),
                    SummonAction::Show => {
                        let at = Instant::now();
                        c.toggle_window();
                        run.shown(at);
                    }
                    SummonAction::Draw => {
                        draw_now(ui);
                        run.drawn(Instant::now());
                    }
                    SummonAction::Finish => {
                        if run.skipped() > 0 {
                            eprintln!(
                                "{APP_NAME}: summon probe: {} cycles skipped, the window \
                                 did not go off screen",
                                run.skipped()
                            );
                        }
                        finish(self.mtm(), run.probe(), out);
                        return false;
                    }
                }
            }
            Job::Sort { run, out } => {
                match run.tick(Instant::now(), c.observed(), source.status().indexing) {
                    SortAction::Wait => {}
                    SortAction::Click(column) => {
                        let Some(title) = ui.card.header_target(column) else {
                            return true;
                        };
                        let at = Instant::now();
                        let (action, target) = (title.action(), title.target());
                        // SAFETY: the title's action is the controller's
                        // `headerClicked:`, `-(void)headerClicked:(id)sender`,
                        // and its target the controller; the title is the
                        // sender, as when it is clicked.
                        unsafe { title.sendAction_to(action, target.as_deref()) };
                        run.clicked(at);
                    }
                    SortAction::Finish => {
                        finish(self.mtm(), run.probe(), out);
                        return false;
                    }
                }
            }
            Job::Hidden(hiding) => {
                let now = Instant::now();
                match *hiding {
                    Hiding::WaitActive { since } => {
                        if app.isActive() || now.saturating_duration_since(since) >= ACTIVE_WAIT {
                            c.hide_window();
                            *hiding = Hiding::Checking {
                                until: now + HIDDEN_CHECK,
                            };
                        }
                    }
                    Hiding::Checking { until } => {
                        if ui.window.isVisible() {
                            c.hide_window();
                        }
                        if now >= until {
                            return false;
                        }
                    }
                }
            }
            Job::Search(_) => return false,
        }
        true
    }
}

/// `--window search`: puts `text` in the search field and sends the field's
/// action, as typing it does.
fn search_for(c: &WinController, text: &str) {
    let Some(ui) = c.ui() else { return };
    ui.search.setStringValue(&NSString::from_str(text));
    let (action, target) = (ui.search.action(), ui.search.target());
    // SAFETY: the field's action is the controller's `search:`,
    // `-(void)search:(id)sender`, and its target the controller; the field
    // is the sender, as when the user types.
    unsafe { ui.search.sendAction_to(action, target.as_deref()) };
}

/// Runs the window's display pass now (layout, then drawing what needs it)
/// and commits it to the window server: what AppKit does at the end of the
/// run-loop turn, run here so the shortcut probe can time its end.
fn draw_now(ui: &WinUi) {
    if let Some(content) = ui.window.contentView() {
        content.layoutSubtreeIfNeeded();
    }
    ui.window.displayIfNeeded();
    if let Some(transaction) = AnyClass::get(c"CATransaction") {
        // SAFETY: `+[CATransaction flush]` takes no arguments and returns
        // void; it is called on the main thread.
        let _: () = unsafe { msg_send![transaction, flush] };
    }
}

/// One frame of the main screen, as the gate probe's timer uses.
fn display_interval(mtm: MainThreadMarker) -> Duration {
    let fps = NSScreen::mainScreen(mtm)
        .map(|s| s.maximumFramesPerSecond())
        .filter(|&f| f > 0)
        .unwrap_or(60);
    Duration::from_secs_f64(1.0 / fps as f64)
}

/// Prints `probe`'s report, writes it to `out`, a new file, and quits:
/// exit code 1 when the file cannot be written or there are no samples,
/// otherwise through `terminate:`, as the gate probe does.
fn finish(mtm: MainThreadMarker, probe: &LatencyProbe, out: &Path) {
    let name = probe.kind().name;
    {
        // Flushed, since `terminate:` exits without running Rust destructors.
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(probe.report_text().as_bytes());
        let _ = stdout.flush();
    }
    if let Err(e) = probe.write_report(out) {
        eprintln!(
            "{APP_NAME}: {name} probe: cannot write {}: {e}",
            out.display()
        );
        std::process::exit(1);
    }
    if probe.samples().is_empty() {
        eprintln!("{APP_NAME}: {name} probe: no samples");
        std::process::exit(1);
    }
    NSApplication::sharedApplication(mtm).terminate(None);
}
