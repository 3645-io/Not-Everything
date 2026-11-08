//! The Phase 0 UI spike's window (macOS, `measure` builds only), which the
//! `table_spike` example measures: whether a view-based `NSTableView`
//! through `objc2` scrolls millions of rows smoothly enough to build the
//! app on.
//!
//! [`run`] builds one window matching `docs/img/mock-ui.png`: an
//! `NSSearchField` at the top, a view-based `NSTableView` (columns Name,
//! Path, Size and Date Modified, fixed row height) in an `NSScrollView`,
//! and a status bar label reading "N objects". Only standard controls are
//! used, so light and dark appearance work without extra code.
//!
//! The table asks its data source for the row count and its delegate for
//! the cells on screen only; cell views are reused through
//! `makeViewWithIdentifier:owner:`, so nothing is materialized per row.
//! Typing in the search field filters through
//! [`RowModel::filter`](crate::model::RowModel::filter) and reloads.
//!
//! Instrumentation, chosen by [`SpikeArgs`]:
//!
//! - `--autoscroll F`: after the first paint, a timer at display rate
//!   scrolls the clip view by `--rows-per-frame` rows per tick, then runs
//!   `layoutSubtreeIfNeeded` and `displayIfNeeded` synchronously, timing
//!   all three as one frame. After `F` frames it prints the
//!   [`FrameStats`](crate::frame_stats::FrameStats) report (text or JSON)
//!   and terminates. The times are main-thread CPU time for scroll, layout
//!   and drawing; window-server compositing is not included.
//! - `--launch-probe`: prints milliseconds from the process-start instant
//!   the caller recorded to the table's first `drawRect:`, and the model
//!   generation time separately, then terminates.
//! - Otherwise the app runs interactively until its window is closed.
//!
//! The spike is read-only: there are no open, reveal, delete, rename, move
//! or drag actions, and the cells are non-editable labels.

mod classes;

use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{MainThreadMarker, MainThreadOnly, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSAutoresizingMaskOptions, NSBackingStoreType,
    NSMenu, NSMenuItem, NSScreen, NSScrollView, NSSearchField, NSTableColumn, NSTextField,
    NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, NSTimer};

use crate::model::{Column, RowModel};
use crate::spike_args::SpikeArgs;
use classes::{Controller, SpikeTableView, Ui};

/// Fixed table row height in points.
pub const ROW_HEIGHT: f64 = 20.0;

/// Initial window content size in points.
const WINDOW_SIZE: (f64, f64) = (1100.0, 680.0);

/// What the spike runs on: the parsed options, the pre-generated model and
/// the timestamps the launch probe reports.
pub struct SpikeConfig {
    /// Parsed command-line options.
    pub args: SpikeArgs,
    /// The row model, generated before the app starts.
    pub model: RowModel,
    /// Recorded as early as possible in `main`.
    pub process_start: Instant,
    /// How long generating `model` took.
    pub model_time: Duration,
}

/// Why the spike could not start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpikeError {
    /// [`run`] was called off the main thread.
    NotMainThread,
}

impl std::fmt::Display for SpikeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpikeError::NotMainThread => {
                f.write_str("the AppKit spike must run on the main thread")
            }
        }
    }
}

impl std::error::Error for SpikeError {}

/// Builds the window and runs the application. In `--launch-probe` and
/// `--autoscroll` modes the app prints its report and terminates the
/// process; otherwise this returns when the app stops.
pub fn run(config: SpikeConfig) -> Result<(), SpikeError> {
    let mtm = MainThreadMarker::new().ok_or(SpikeError::NotMainThread)?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    app.setMainMenu(Some(&main_menu(mtm)));

    let SpikeConfig {
        args,
        model,
        process_start,
        model_time,
    } = config;
    let measuring = args.launch_probe || args.autoscroll > 0;
    let controller = Controller::new(mtm, model, args, process_start, model_time);
    let ui = build_window(mtm, &controller);
    ui.window.center();
    ui.window.makeKeyAndOrderFront(None);
    ui.window.makeFirstResponder(Some(&ui.search));
    controller.set_ui(ui);

    app.setDelegate(Some(ProtocolObject::from_ref(&*controller)));

    // The display-rate timer drives the probe and autoscroll only; the
    // interactive app has no timer, so it stays idle between events.
    if measuring {
        let fps = NSScreen::mainScreen(mtm)
            .map(|s| s.maximumFramesPerSecond())
            .filter(|&f| f > 0)
            .unwrap_or(60);
        let target: &AnyObject = controller.as_ref();
        // SAFETY: `tick:` is implemented by Controller with the signature
        // `-(void)tick:(NSTimer *)timer`; the run loop retains the timer and
        // the timer retains the target.
        let _timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                1.0 / fps as f64,
                target,
                sel!(tick:),
                None,
                true,
            )
        };
    }

    app.run();
    // Keep the delegate and data source alive for the app's lifetime;
    // AppKit holds them weakly.
    drop(controller);
    Ok(())
}

/// A main menu with the application menu's Quit item (Cmd+Q).
fn main_menu(mtm: MainThreadMarker) -> Retained<NSMenu> {
    let menu_bar = NSMenu::new(mtm);
    let app_item = NSMenuItem::new(mtm);
    let app_menu = NSMenu::new(mtm);
    // SAFETY: `terminate:` is NSApplication's action; a nil target sends it
    // along the responder chain to the application.
    let quit = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("Quit Table Spike"),
            Some(sel!(terminate:)),
            &NSString::from_str("q"),
        )
    };
    app_menu.addItem(&quit);
    app_item.setSubmenu(Some(&app_menu));
    menu_bar.addItem(&app_item);
    menu_bar
}

/// Builds the window, search field, table and status bar, wired to
/// `controller`. Views are laid out with autoresizing masks.
fn build_window(mtm: MainThreadMarker, controller: &Controller) -> Ui {
    let (w, h) = WINDOW_SIZE;
    let frame = rect(0.0, 0.0, w, h);
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
    // SAFETY: valid frame, style and backing; the window is released only
    // through its Retained handle (see setReleasedWhenClosed below).
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // SAFETY: we own `window` through a Retained, so AppKit must not also
    // release it on close.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str("Table Spike"));
    window.setMinSize(NSSize::new(480.0, 240.0));

    let content = window
        .contentView()
        .expect("a new window has a content view");
    let target: &AnyObject = controller.as_ref();

    // Search field across the top.
    let search = NSSearchField::initWithFrame(
        NSSearchField::alloc(mtm),
        rect(10.0, h - 36.0, w - 20.0, 26.0),
    );
    search.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMinYMargin,
    );
    search.setPlaceholderString(Some(&NSString::from_str("Search")));
    // SAFETY: `search:` is implemented by Controller with the signature
    // `-(void)search:(id)sender`; the controller outlives the window.
    unsafe { search.setTarget(Some(target)) };
    // SAFETY: as above.
    unsafe { search.setAction(Some(sel!(search:))) };
    content.addSubview(&search);

    // Status bar along the bottom.
    let status = NSTextField::labelWithString(&NSString::from_str(""), mtm);
    status.setFrame(rect(10.0, 4.0, w - 20.0, 18.0));
    status.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMaxYMargin,
    );
    content.addSubview(&status);

    // The table in a scroll view, filling the space between.
    let scroll_frame = rect(0.0, 26.0, w, h - 26.0 - 46.0);
    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), scroll_frame);
    scroll.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    scroll.setHasVerticalScroller(true);
    scroll.setHasHorizontalScroller(true);
    scroll.setAutohidesScrollers(true);

    let table = SpikeTableView::new(mtm, rect(0.0, 0.0, w, scroll_frame.size.height));
    table.setUsesAutomaticRowHeights(false);
    table.setRowHeight(ROW_HEIGHT);
    table.setUsesAlternatingRowBackgroundColors(true);
    table.setAllowsMultipleSelection(true);
    for col in Column::ALL {
        let ident = NSString::from_str(column_identifier(col));
        let column = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &ident);
        column.setTitle(&NSString::from_str(col.title()));
        column.setWidth(column_width(col));
        column.setMinWidth(40.0);
        column.setEditable(false);
        table.addTableColumn(&column);
    }
    // SAFETY: the controller implements both protocols and outlives the
    // table (it is kept alive until `run` returns).
    unsafe { table.setDataSource(Some(ProtocolObject::from_ref(controller))) };
    // SAFETY: as above.
    unsafe { table.setDelegate(Some(ProtocolObject::from_ref(controller))) };
    scroll.setDocumentView(Some(&table));
    content.addSubview(&scroll);

    Ui {
        window,
        search,
        scroll,
        table,
        status,
    }
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

/// The table column identifier of `col`.
pub fn column_identifier(col: Column) -> &'static str {
    match col {
        Column::Name => "name",
        Column::Path => "path",
        Column::Size => "size",
        Column::Modified => "modified",
    }
}

/// The column a table column identifier names, or `None` if unknown.
pub fn column_for_identifier(ident: &str) -> Option<Column> {
    Column::ALL
        .into_iter()
        .find(|&c| column_identifier(c) == ident)
}

/// Initial width of a column in points.
fn column_width(col: Column) -> f64 {
    match col {
        Column::Name => 300.0,
        Column::Path => 480.0,
        Column::Size => 90.0,
        Column::Modified => 140.0,
    }
}

/// The next scroll offset for autoscroll: `y + step`, wrapping to the top
/// once it would pass `max_y` (the last offset that still fills the view).
/// A table shorter than its view (`max_y <= 0`) stays at 0.
pub fn next_scroll_y(y: f64, step: f64, max_y: f64) -> f64 {
    if max_y <= 0.0 {
        return 0.0;
    }
    let next = y + step;
    if next > max_y { 0.0 } else { next }
}

/// The status bar text: `"1 object"`, `"2,000,000 objects"`.
pub fn status_text(count: usize) -> String {
    let digits = count.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    let noun = if count == 1 { "object" } else { "objects" };
    format!("{grouped} {noun}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_identifiers_round_trip() {
        for col in Column::ALL {
            assert_eq!(column_for_identifier(column_identifier(col)), Some(col));
        }
        assert_eq!(column_for_identifier("nope"), None);
    }

    #[test]
    fn scroll_offsets_advance_and_wrap() {
        assert_eq!(next_scroll_y(0.0, 100.0, 1_000.0), 100.0);
        assert_eq!(next_scroll_y(950.0, 100.0, 1_000.0), 0.0);
        assert_eq!(next_scroll_y(900.0, 100.0, 1_000.0), 1_000.0);
        assert_eq!(next_scroll_y(10.0, 100.0, -5.0), 0.0);
    }

    #[test]
    fn status_text_groups_thousands() {
        assert_eq!(status_text(0), "0 objects");
        assert_eq!(status_text(1), "1 object");
        assert_eq!(status_text(999), "999 objects");
        assert_eq!(status_text(1_000), "1,000 objects");
        assert_eq!(status_text(2_000_000), "2,000,000 objects");
    }

    #[test]
    fn run_off_the_main_thread_is_an_error() {
        // libtest runs tests on spawned threads, so this needs no window
        // server. Skip rather than open a window if that ever changes.
        if MainThreadMarker::new().is_some() {
            return;
        }
        let config = SpikeConfig {
            args: SpikeArgs::default(),
            model: RowModel::new(10, 1),
            process_start: Instant::now(),
            model_time: Duration::ZERO,
        };
        assert_eq!(run(config), Err(SpikeError::NotMainThread));
    }
}
