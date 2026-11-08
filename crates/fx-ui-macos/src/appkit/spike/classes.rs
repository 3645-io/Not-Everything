//! The Objective-C classes the spike declares with `define_class!`.
//!
//! - [`SpikeTableView`]: an `NSTableView` subclass that records the instant
//!   of its first `drawRect:`, for the launch probe.
//! - [`Controller`]: the table's data source and delegate, the search
//!   field's target, the application delegate and the display-rate timer's
//!   target. It owns the row model and the instrumentation state.

use std::cell::{Cell, OnceCell, RefCell};
use std::io::Write as _;
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationDelegate, NSAutoresizingMaskOptions, NSControl,
    NSControlTextEditingDelegate, NSResponder, NSScrollView, NSSearchField, NSTableCellView,
    NSTableColumn, NSTableView, NSTableViewDataSource, NSTableViewDelegate, NSTextAlignment,
    NSTextField, NSUserInterfaceItemIdentification, NSView, NSWindow,
};
use objc2_foundation::{NSNotification, NSPoint, NSRect, NSString, NSTimer};

use super::{column_for_identifier, next_scroll_y, status_text};
use crate::frame_stats::FrameStats;
use crate::model::{Column, RowModel};
use crate::spike_args::SpikeArgs;

/// Instance variables of [`SpikeTableView`].
#[derive(Default)]
pub(crate) struct TableIvars {
    /// When `drawRect:` first returned.
    first_draw: Cell<Option<Instant>>,
}

define_class!(
    // SAFETY: NSTableView may be subclassed; the override below calls
    // super and adds no state that AppKit could observe.
    #[unsafe(super(NSTableView, NSControl, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FxSpikeTableView"]
    #[ivars = TableIvars]
    pub(crate) struct SpikeTableView;

    impl SpikeTableView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, rect: NSRect) {
            // SAFETY: `drawRect:` takes one NSRect and returns void.
            let _: () = unsafe { msg_send![super(self), drawRect: rect] };
            if self.ivars().first_draw.get().is_none() {
                self.ivars().first_draw.set(Some(Instant::now()));
            }
        }
    }
);

impl SpikeTableView {
    /// A new, empty table view with `frame`.
    pub(crate) fn new(mtm: objc2::MainThreadMarker, frame: NSRect) -> Retained<SpikeTableView> {
        let this = SpikeTableView::alloc(mtm).set_ivars(TableIvars::default());
        // SAFETY: `initWithFrame:` is NSView's designated initializer.
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    /// When the table was first drawn, if it has been.
    pub(crate) fn first_draw(&self) -> Option<Instant> {
        self.ivars().first_draw.get()
    }
}

/// The views the controller drives, set once the window is built.
pub(crate) struct Ui {
    pub(crate) window: Retained<NSWindow>,
    pub(crate) search: Retained<NSSearchField>,
    pub(crate) scroll: Retained<NSScrollView>,
    pub(crate) table: Retained<SpikeTableView>,
    pub(crate) status: Retained<NSTextField>,
}

/// Instance variables of [`Controller`].
pub(crate) struct ControllerIvars {
    model: RowModel,
    /// Rows matching the search field, or `None` for every row (so the
    /// unfiltered 2M rows need no index vector).
    visible: RefCell<Option<Vec<usize>>>,
    /// Reused buffer for cell text.
    buf: RefCell<String>,
    args: SpikeArgs,
    process_start: Instant,
    model_time: Duration,
    ui: OnceCell<Ui>,
    stats: RefCell<FrameStats>,
    /// Frames scrolled so far in `--autoscroll` mode.
    frames_done: Cell<u64>,
    /// Set once the probe or autoscroll has reported, so later ticks do
    /// nothing while the app terminates.
    finished: Cell<bool>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; Controller does not
    // implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FxSpikeController"]
    #[ivars = ControllerIvars]
    pub(crate) struct Controller;

    // SAFETY: NSObject itself conforms to NSObjectProtocol; no method is
    // overridden.
    unsafe impl NSObjectProtocol for Controller {}

    // SAFETY: the methods below have the signatures the protocol declares;
    // every other method of it is optional.
    unsafe impl NSApplicationDelegate for Controller {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            let app = NSApplication::sharedApplication(self.mtm());
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window_closed(&self, _app: &NSApplication) -> bool {
            true
        }
    }

    // SAFETY: every method of the protocol is optional and none is
    // implemented.
    unsafe impl NSControlTextEditingDelegate for Controller {}

    // SAFETY: `numberOfRowsInTableView:` has the signature the protocol
    // declares; every other method of it is optional.
    unsafe impl NSTableViewDataSource for Controller {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> isize {
            self.visible_count() as isize
        }
    }

    // SAFETY: `tableView:viewForTableColumn:row:` has the signature the
    // protocol declares and returns a retained view; every other method of
    // it is optional.
    unsafe impl NSTableViewDelegate for Controller {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for_row(
            &self,
            table: &NSTableView,
            column: Option<&NSTableColumn>,
            row: isize,
        ) -> Option<Retained<NSView>> {
            column.and_then(|column| self.cell_view(table, column, row))
        }
    }

    impl Controller {
        /// Action of the search field: refilter and reload.
        #[unsafe(method(search:))]
        fn search_action(&self, _sender: Option<&AnyObject>) {
            self.refilter();
        }

        /// Fired by the display-rate timer.
        #[unsafe(method(tick:))]
        fn tick(&self, _timer: &NSTimer) {
            self.on_tick();
        }
    }
);

impl Controller {
    /// A controller over `model`.
    pub(crate) fn new(
        mtm: objc2::MainThreadMarker,
        model: RowModel,
        args: SpikeArgs,
        process_start: Instant,
        model_time: Duration,
    ) -> Retained<Controller> {
        let this = Controller::alloc(mtm).set_ivars(ControllerIvars {
            model,
            visible: RefCell::new(None),
            buf: RefCell::new(String::new()),
            args,
            process_start,
            model_time,
            ui: OnceCell::new(),
            stats: RefCell::new(FrameStats::new()),
            frames_done: Cell::new(0),
            finished: Cell::new(false),
        });
        // SAFETY: `init` is NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }

    /// Hands the built views to the controller. Later calls are ignored.
    pub(crate) fn set_ui(&self, ui: Ui) {
        let _ = self.ivars().ui.set(ui);
        self.update_status();
    }

    fn ui(&self) -> Option<&Ui> {
        self.ivars().ui.get()
    }

    /// Number of rows the table shows.
    fn visible_count(&self) -> usize {
        match &*self.ivars().visible.borrow() {
            Some(rows) => rows.len(),
            None => self.ivars().model.row_count(),
        }
    }

    /// The model row behind table row `row`.
    fn model_row(&self, row: isize) -> Option<usize> {
        let row = usize::try_from(row).ok()?;
        match &*self.ivars().visible.borrow() {
            Some(rows) => rows.get(row).copied(),
            None => (row < self.ivars().model.row_count()).then_some(row),
        }
    }

    /// A reused (or new) cell view showing one cell of `row`.
    fn cell_view(
        &self,
        table: &NSTableView,
        column: &NSTableColumn,
        row: isize,
    ) -> Option<Retained<NSView>> {
        let ident = column.identifier();
        let col = column_for_identifier(&ident.to_string())?;
        // SAFETY: a nil owner is allowed; the identifier is a valid NSString.
        let reused = unsafe { table.makeViewWithIdentifier_owner(&ident, None) };
        let cell = match reused.and_then(|v| v.downcast::<NSTableCellView>().ok()) {
            Some(cell) => cell,
            None => new_cell_view(self.mtm(), table, &ident, col),
        };
        let mut buf = self.ivars().buf.borrow_mut();
        let found = self
            .model_row(row)
            .is_some_and(|r| self.ivars().model.write_cell(r, col, &mut buf));
        if !found {
            buf.clear();
        }
        // SAFETY: reading the cell's `textField` outlet, which is either
        // nil or the label `new_cell_view` installed.
        if let Some(label) = unsafe { cell.textField() } {
            label.setStringValue(&NSString::from_str(&buf));
        }
        Some(Retained::into_super(cell))
    }

    /// Re-runs the filter for the search field's text and reloads.
    fn refilter(&self) {
        let Some(ui) = self.ui() else { return };
        let needle = ui.search.stringValue().to_string();
        let rows = if needle.is_empty() {
            None
        } else {
            Some(self.ivars().model.filter(&needle))
        };
        *self.ivars().visible.borrow_mut() = rows;
        ui.table.reloadData();
        if self.visible_count() > 0 {
            ui.table.scrollRowToVisible(0);
        }
        self.update_status();
    }

    fn update_status(&self) {
        if let Some(ui) = self.ui() {
            ui.status
                .setStringValue(&NSString::from_str(&status_text(self.visible_count())));
        }
    }

    /// Drives `--launch-probe` and `--autoscroll` once the table has been
    /// painted. Does nothing in interactive mode.
    fn on_tick(&self) {
        let iv = self.ivars();
        if iv.finished.get() || !(iv.args.launch_probe || iv.args.autoscroll > 0) {
            return;
        }
        let Some(ui) = self.ui() else { return };
        let Some(first_draw) = ui.table.first_draw() else {
            return;
        };
        if iv.args.launch_probe {
            self.report_launch(first_draw);
            return self.finish();
        }
        if iv.frames_done.get() < iv.args.autoscroll {
            let frame = self.scroll_one_frame(ui);
            iv.stats.borrow_mut().record(frame);
            iv.frames_done.set(iv.frames_done.get() + 1);
        }
        if iv.frames_done.get() >= iv.args.autoscroll {
            self.report_autoscroll();
            self.finish();
        }
    }

    /// Scrolls the clip view by `--rows-per-frame` rows, then lays out and
    /// displays synchronously. Returns the time all of it took.
    fn scroll_one_frame(&self, ui: &Ui) -> Duration {
        let start = Instant::now();
        let clip = ui.scroll.contentView();
        let row_step = ui.table.rowHeight() + ui.table.intercellSpacing().height;
        let step = row_step * self.ivars().args.rows_per_frame as f64;
        let bounds = clip.bounds();
        let max_y = ui.table.frame().size.height - bounds.size.height;
        let y = next_scroll_y(bounds.origin.y, step, max_y);
        clip.scrollToPoint(NSPoint::new(bounds.origin.x, y));
        ui.scroll.reflectScrolledClipView(&clip);
        if let Some(content) = ui.window.contentView() {
            content.layoutSubtreeIfNeeded();
            content.displayIfNeeded();
        }
        start.elapsed()
    }

    fn report_launch(&self, first_draw: Instant) {
        let iv = self.ivars();
        let first_paint = first_draw.duration_since(iv.process_start);
        let ms = |d: Duration| d.as_secs_f64() * 1_000.0;
        let text = if iv.args.json {
            format!(
                "{{\"rows\":{},\"launch_to_first_paint_ms\":{:.3},\"model_generation_ms\":{:.3}}}",
                iv.model.row_count(),
                ms(first_paint),
                ms(iv.model_time)
            )
        } else {
            format!(
                "rows: {}\nmodel generation: {:.2} ms\nlaunch to first table paint: {:.2} ms \
                 (includes model generation)",
                iv.model.row_count(),
                ms(iv.model_time),
                ms(first_paint)
            )
        };
        print_flush(&text);
    }

    fn report_autoscroll(&self) {
        let iv = self.ivars();
        let stats = iv.stats.borrow();
        let text = if iv.args.json {
            stats.report_json()
        } else {
            format!(
                "autoscroll: {} rows, {} rows per frame\n{}",
                iv.model.row_count(),
                iv.args.rows_per_frame,
                stats.report_text().trim_end()
            )
        };
        print_flush(&text);
    }

    /// Ends the app after a probe or autoscroll run.
    fn finish(&self) {
        self.ivars().finished.set(true);
        NSApplication::sharedApplication(self.mtm()).terminate(None);
    }
}

/// Prints one line (or block) to stdout and flushes it, since `terminate:`
/// exits the process without running Rust destructors.
fn print_flush(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{text}");
    let _ = out.flush();
}

/// A new cell view for column `col`: an `NSTableCellView` holding one
/// non-editable label that tracks the cell's width.
fn new_cell_view(
    mtm: objc2::MainThreadMarker,
    table: &NSTableView,
    ident: &NSString,
    col: Column,
) -> Retained<NSTableCellView> {
    let height = table.rowHeight();
    let frame = NSRect::new(
        NSPoint::new(0.0, 0.0),
        objc2_foundation::NSSize::new(100.0, height),
    );
    let cell = NSTableCellView::initWithFrame(NSTableCellView::alloc(mtm), frame);
    cell.setIdentifier(Some(ident));
    let label = NSTextField::labelWithString(&NSString::from_str(""), mtm);
    let label_height = (height - 4.0).max(1.0);
    label.setFrame(NSRect::new(
        NSPoint::new(2.0, (height - label_height) / 2.0),
        objc2_foundation::NSSize::new(96.0, label_height),
    ));
    label.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
    if col == Column::Size {
        label.setAlignment(NSTextAlignment::Right);
    }
    cell.addSubview(&label);
    // SAFETY: the outlet is set to a label that is also a subview of the
    // cell, so the cell keeps it alive.
    unsafe { cell.setTextField(Some(&label)) };
    cell
}
