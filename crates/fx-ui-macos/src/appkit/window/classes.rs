//! The Objective-C classes of the product window.
//!
//! - [`ResultsTableView`]: an `NSTableView` subclass that maps keys through
//!   [`action_for_key`] and Escape through [`escape_action`], leaves
//!   `copy:` unanswered so Edit > Copy is off, limits drag-out to copy and
//!   link, tracks the row under the pointer for the rows' hover look and,
//!   in `measure` builds, records its draw times for the launch and gate
//!   probes.
//! - [`WinController`]: data source and delegate of the table, target and
//!   delegate of the search field, application delegate and timer target.
//!   It owns the [`ResultsController`]. It is also the status menu's target
//!   and runs the summon hotkey and onboarding timers (see `resident.rs`),
//!   answers About, Settings… and the Settings window's controls, and
//!   observes the app's effective appearance (see `settings_window.rs`). It
//!   reports every user selection change to the controller and, after a
//!   refresh, selects the rows the controller gives, so the selection stays
//!   on the same files. It drives the custom-drawn window: it lays out the
//!   top bar, card, callout and status bar, follows the appearance and the
//!   focus, answers the chips, column titles, syntax examples and ×,
//!   highlights matches in the visible rows, and times each search for the
//!   status bar.
//! - [`Waker`]: a stateless object the background threads message with
//!   `performSelectorOnMainThread:` to have the main thread drain the inbox.

use std::cell::{Cell, OnceCell, RefCell};
use std::ffi::c_void;
#[cfg(feature = "measure")]
use std::io::Write as _;
use std::ops::Range;
#[cfg(feature = "measure")]
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fx_engine::{Generation, SearchError, qos};
use fx_query::{Highlighter, Highlights};
use global_hotkey::GlobalHotKeyEvent;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSApplication, NSApplicationDelegate, NSControl, NSControlTextEditingDelegate, NSDragOperation,
    NSDraggingContext, NSEvent, NSEventModifierFlags, NSEventType, NSFontAttributeName,
    NSForegroundColorAttributeName, NSMenuItem, NSPasteboard, NSPasteboardTypeString,
    NSPasteboardWriting, NSResponder, NSSearchFieldDelegate, NSTableColumn, NSTableRowView,
    NSTableView, NSTableViewDataSource, NSTableViewDelegate, NSTextAlignment, NSTextFieldDelegate,
    NSTrackingArea, NSTrackingAreaOptions, NSView, NSWindowOcclusionState, NSWorkspace,
};
use objc2_foundation::{
    NSArray, NSDictionary, NSIndexSet, NSMutableAttributedString, NSMutableIndexSet, NSNotFound,
    NSNotification, NSNotificationCenter, NSPoint, NSRange, NSRect, NSString, NSTimer, NSURL,
};
#[cfg(debug_assertions)]
use objc2_foundation::{NSDate, NSRunLoop};

use super::callout::Callout;
use super::card::{ResultCell, ResultRowView, ResultsCard};
use super::chrome::{RootView, SearchField, SearchWindow, TopBar};
use super::resident::ResidentState;
use super::status::StatusBar;
use super::views::{self, Flow, attributes, color, tag_of};
#[cfg(feature = "measure")]
use super::{AUTOSCROLL_FRAMES, AUTOSCROLL_ROWS_PER_FRAME};
use super::{MAX_OPEN, WindowConfig};
use crate::APP_NAME;
use crate::actions::{
    Action, EscapeAction, Focus, Key, Modifiers, SearchFieldKey, action_for_key, copy_text,
    escape_action, key_from_code, search_field_key,
};
#[cfg(feature = "measure")]
use crate::appkit::spike::next_scroll_y;
use crate::appkit::time_zone::SystemTimeZone;
use crate::chips::QueryOptions;
use crate::columns::{Column, SortState};
use crate::controller::{
    DEFAULT_THROTTLE, MAX_COPY_PATHS, MAX_KEPT_SELECTION, RequestId, ResultsController,
    SearchRequest,
};
use crate::daily_save::daily_save_due;
use crate::file_glyph::FileKind;
use crate::formats::{write_date, write_folder, write_size_cell};
#[cfg(feature = "measure")]
use crate::frame_stats::FrameStats;
#[cfg(feature = "measure")]
use crate::gate_probe::{Observed, ProbeAction, ProbeRun};
use crate::main_layout::{Rect, window};
use crate::onboarding::{IndexScope, OnboardingEvent, ScopeNote};
use crate::quit::{QuitStep, quit_steps};
use crate::source::{ResultPage, ResultsSource};
use crate::status_view::{StatusInput, status_view};
use crate::theme::{Theme, Token, TypeStyle, metrics, styles};
use crate::window_view::{body, folder_highlights, with_example};

type Source = Arc<dyn ResultsSource>;

/// How often the status bar is refreshed while the source is indexing.
const STATUS_REFRESH: Duration = Duration::from_secs(1);

thread_local! {
    /// The window's controller, reachable from the table view and the
    /// waker. Only ever set and read on the main thread.
    static CONTROLLER: RefCell<Option<Retained<WinController>>> = const { RefCell::new(None) };
}

/// Runs `f` on the window's controller, if there is one. The controller is
/// cloned out first, so `f` may re-enter AppKit freely.
pub(super) fn with_controller(f: impl FnOnce(&WinController)) {
    let controller = CONTROLLER.with(|c| c.borrow().clone());
    if let Some(controller) = controller {
        f(&controller);
    }
}

// ------------------------------------------------------------ table view

/// Instance variables of [`ResultsTableView`]: its draw times, for the
/// measurement modes.
#[derive(Default)]
pub(crate) struct TableIvars {
    #[cfg(feature = "measure")]
    first_draw: Cell<Option<Instant>>,
    #[cfg(feature = "measure")]
    last_draw: Cell<Option<Instant>>,
}

define_class!(
    // SAFETY: NSTableView may be subclassed; the overrides call super or
    // replace methods whose contracts they keep.
    #[unsafe(super(NSTableView, NSControl, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FxResultsTableView"]
    #[ivars = TableIvars]
    pub(crate) struct ResultsTableView;

    impl ResultsTableView {
        #[cfg(feature = "measure")]
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, rect: NSRect) {
            // SAFETY: `drawRect:` takes one NSRect and returns void.
            let _: () = unsafe { msg_send![super(self), drawRect: rect] };
            let now = Instant::now();
            if self.ivars().first_draw.get().is_none() {
                self.ivars().first_draw.set(Some(now));
            }
            self.ivars().last_draw.set(Some(now));
        }

        /// A click on a row moves the keyboard focus to the results, so the
        /// arrow keys move from the clicked row at once. Clicks land on a
        /// row's labels, which refuse the focus, so the window would leave
        /// it in the search field.
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            if let Some(window) = self.window() {
                window.makeFirstResponder(Some(self));
            }
            // SAFETY: `mouseDown:` takes one NSEvent and returns void; it
            // selects, starts a drag or sends the double action.
            let _: () = unsafe { msg_send![super(self), mouseDown: event] };
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let (key, mods) = key_of(event);
            if let Some(action) = action_for_key(key, mods) {
                with_controller(|c| c.perform(action, None));
                return;
            }
            // Escape reaches the table as a plain key-down (key code 53).
            let mut handled = false;
            with_controller(|c| handled = c.escape(Focus::Results, key, mods));
            if handled {
                return;
            }
            // SAFETY: `keyDown:` takes one NSEvent and returns void.
            let _: () = unsafe { msg_send![super(self), keyDown: event] };
        }

        // No `copy:` here, on purpose: with nothing in the responder chain
        // answering it, AppKit greys out Edit > Copy while the table has
        // the focus. Cmd+C is kept for copying the files themselves, as in
        // Finder, which is not built yet; Option+Cmd+C copies the paths
        // through `keyDown:` and `action_for_key`.

        /// Drag-out offers copy and link only, whatever the destination.
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn source_operation_mask(
            &self,
            _session: &AnyObject,
            _context: NSDraggingContext,
        ) -> NSDragOperation {
            super::drag_operation()
        }

        /// The row under the pointer shows its hover look.
        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            self.hover_at(point);
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            with_controller(|c| c.set_hover_row(None));
        }

        /// Scrolling moves the rows under a still pointer.
        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            // SAFETY: `scrollWheel:` takes one NSEvent and returns void.
            let _: () = unsafe { msg_send![super(self), scrollWheel: event] };
            if let Some(window) = self.window() {
                let point = self.convertPoint_fromView(window.mouseLocationOutsideOfEventStream(), None);
                let v = self.visibleRect();
                let inside = point.x >= v.origin.x
                    && point.y >= v.origin.y
                    && point.x < v.origin.x + v.size.width
                    && point.y < v.origin.y + v.size.height;
                if inside {
                    self.hover_at(point);
                }
            }
        }
    }
);

impl ResultsTableView {
    pub(crate) fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<ResultsTableView> {
        let this = ResultsTableView::alloc(mtm).set_ivars(TableIvars::default());
        // SAFETY: `initWithFrame:` is NSView's designated initializer.
        let table: Retained<ResultsTableView> =
            unsafe { msg_send![super(this), initWithFrame: frame] };
        let options = NSTrackingAreaOptions::MouseMoved
            | NSTrackingAreaOptions::MouseEnteredAndExited
            | NSTrackingAreaOptions::ActiveInKeyWindow
            | NSTrackingAreaOptions::InVisibleRect;
        // SAFETY: the owner is the table, which keeps the area; no user
        // info is passed.
        let area = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(
                NSTrackingArea::alloc(),
                NSRect::ZERO,
                options,
                Some(&table),
                None,
            )
        };
        table.addTrackingArea(&area);
        table
    }

    fn hover_at(&self, point: NSPoint) {
        let row = usize::try_from(self.rowAtPoint(point)).ok();
        with_controller(|c| c.set_hover_row(row));
    }
}

/// A debug script's mouse event of `kind` at `at` in `window`'s
/// coordinates, as the hardware sends it (a single click).
#[cfg(debug_assertions)]
fn debug_mouse(window: &SearchWindow, kind: NSEventType, at: NSPoint) -> Option<Retained<NSEvent>> {
    NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        kind,
        at,
        NSEventModifierFlags::empty(),
        0.0,
        window.windowNumber(),
        None,
        0,
        1,
        if kind == NSEventType::LeftMouseDown { 1.0 } else { 0.0 },
    )
}

/// A debug script's key press of `ch` (key code `code`) sent to `window`:
/// a key-down then a key-up, as the hardware sends them.
#[cfg(debug_assertions)]
fn debug_key(window: &SearchWindow, ch: char, code: u16, flags: NSEventModifierFlags) {
    let text = NSString::from_str(&ch.to_string());
    for kind in [NSEventType::KeyDown, NSEventType::KeyUp] {
        let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            kind,
            NSPoint::new(0.0, 0.0),
            flags,
            0.0,
            window.windowNumber(),
            None,
            &text,
            &text,
            false,
            code,
        );
        if let Some(event) = event {
            window.sendEvent(&event);
        }
    }
}

/// The range holding just `row`.
fn one_row(row: usize) -> Range<usize> {
    row..row + 1
}

/// The first `limit` rows of `ranges`, in order.
fn take_rows(ranges: Vec<Range<usize>>, limit: usize) -> Vec<Range<usize>> {
    let mut left = limit;
    let mut out = Vec::new();
    for r in ranges {
        if left == 0 {
            break;
        }
        let len = r.end.saturating_sub(r.start).min(left);
        if len > 0 {
            out.push(r.start..r.start + len);
            left -= len;
        }
    }
    out
}

/// The action-map key and modifiers of a key event.
fn key_of(event: &NSEvent) -> (Key, Modifiers) {
    let flags = event.modifierFlags();
    let mods = Modifiers {
        command: flags.contains(NSEventModifierFlags::Command),
        shift: flags.contains(NSEventModifierFlags::Shift),
        option: flags.contains(NSEventModifierFlags::Option),
        control: flags.contains(NSEventModifierFlags::Control),
    };
    let chars = event.charactersIgnoringModifiers().map(|s| s.to_string());
    (key_from_code(event.keyCode(), chars.as_deref()), mods)
}

// ---------------------------------------------------------------- waker

/// A finished search: its request, its answer and how long it took.
type Answer = (RequestId, Result<ResultPage, SearchError>, Duration);

/// Pages, generations and hotkey events posted by the background threads
/// and the `global-hotkey` handler.
#[derive(Default)]
struct Inbox {
    pages: Vec<Answer>,
    generation: Option<Generation>,
    hotkey: Vec<GlobalHotKeyEvent>,
}

/// State shared with the background threads.
#[derive(Default)]
pub(crate) struct Shared {
    inbox: Mutex<Inbox>,
    /// Set while a wake-up is queued, so bursts post one message.
    wake_pending: AtomicBool,
    /// The window is off screen: a new generation is only recorded in the
    /// inbox, without waking the main thread, until it shows again.
    hidden: AtomicBool,
}

impl Shared {
    fn inbox(&self) -> std::sync::MutexGuard<'_, Inbox> {
        self.inbox.lock().unwrap_or_else(|p| p.into_inner())
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; Waker has no
    // instance variables.
    #[unsafe(super(NSObject))]
    #[name = "FxMainThreadWaker"]
    pub(crate) struct Waker;

    impl Waker {
        /// Runs on the main thread (posted with
        /// `performSelectorOnMainThread:`).
        #[unsafe(method(wake:))]
        fn wake(&self, _arg: Option<&AnyObject>) {
            with_controller(|c| c.drain());
        }
    }
);

/// A [`Waker`] the background threads can hold.
struct MainWaker {
    waker: Retained<Waker>,
    shared: Arc<Shared>,
}

// SAFETY: Waker is a plain NSObject subclass without state; retain,
// release and `performSelectorOnMainThread:withObject:waitUntilDone:` are
// thread-safe, and its only method runs on the main thread.
unsafe impl Send for MainWaker {}
// SAFETY: as above; `wake` only reads through `&self`.
unsafe impl Sync for MainWaker {}

impl MainWaker {
    fn wake(&self) {
        if self.shared.wake_pending.swap(true, Ordering::AcqRel) {
            return;
        }
        // SAFETY: `wake:` takes one object (nil here) and returns void;
        // the message is queued on the main run loop, which retains the
        // receiver until it has run.
        let _: () = unsafe {
            msg_send![
                &*self.waker,
                performSelectorOnMainThread: sel!(wake:),
                withObject: None::<&AnyObject>,
                waitUntilDone: false
            ]
        };
    }
}

// ----------------------------------------------------------- controller

/// The views the controller drives, set once the window is built.
pub(crate) struct WinUi {
    pub(crate) window: Retained<SearchWindow>,
    pub(crate) root: Retained<RootView>,
    /// The search field (the top bar's).
    pub(crate) search: Retained<SearchField>,
    /// The rows (the card's).
    pub(crate) table: Retained<ResultsTableView>,
    pub(crate) top: TopBar,
    pub(crate) card: ResultsCard,
    pub(crate) status: StatusBar,
    /// The Full Disk Access callout (resident app only).
    pub(crate) callout: Option<Callout>,
}

/// The running gate probe and where its report goes.
#[cfg(feature = "measure")]
pub(crate) struct GateState {
    run: ProbeRun,
    out: PathBuf,
}

/// The highlighter of the query shown and the highlights of the last row
/// asked about.
#[derive(Default)]
struct Highlighting {
    /// The query and options it was prepared for, and the highlighter.
    prepared: Option<(String, QueryOptions, Highlighter)>,
    /// The page shown when `row` was highlighted, the row, and its
    /// highlights in the name and in the Path column's text.
    last: Option<(u64, usize, Highlights)>,
}

/// Instance variables of [`WinController`].
pub(crate) struct WinIvars {
    results: RefCell<ResultsController<Source>>,
    shared: Arc<Shared>,
    requests: RefCell<Option<Sender<SearchRequest>>>,
    ui: OnceCell<WinUi>,
    /// A deferred generation re-run has a timer scheduled.
    tick_scheduled: Cell<bool>,
    /// A status-bar refresh timer is scheduled (while indexing).
    status_scheduled: Cell<bool>,
    /// The next applied page answers a new query or sort: clear the
    /// selection and scroll to the top.
    reset_on_apply: Cell<bool>,
    /// Set while the controller itself sets the table's selection after a
    /// refresh, so the change is not remembered as the user's.
    restoring_selection: Cell<bool>,
    buf: RefCell<String>,
    /// The local time zone for the Date Modified column.
    zone: SystemTimeZone,
    /// The home folder, shown as "~" in the Path column; `None` when it is
    /// unknown or not UTF-8.
    home: Option<String>,
    /// How long the search shown took ("Up to date · 9 ms").
    search_time: Cell<Option<Duration>>,
    /// Counts the pages shown, so cached highlights belong to one.
    page_serial: Cell<u64>,
    /// Match highlighting.
    highlighting: RefCell<Highlighting>,
    /// The row under the pointer.
    hover_row: Cell<Option<usize>>,
    /// Where the keyboard focus was at the last focus change.
    focus: Cell<Focus>,
    /// A copy just stopped at the copy limit after this many paths; shown
    /// until the status bar is next refreshed.
    copy_note: Cell<Option<usize>>,
    /// Whether the source was indexing at the last status refresh.
    was_indexing: Cell<Option<bool>>,
    #[cfg(feature = "measure")]
    autoscroll: bool,
    #[cfg(feature = "measure")]
    launch_probe: bool,
    #[cfg(feature = "measure")]
    process_start: Instant,
    #[cfg(feature = "measure")]
    first_results: Cell<Option<Instant>>,
    /// When a results page was last applied to the table (for the gate
    /// probe's keystroke-to-repaint timing).
    #[cfg(feature = "measure")]
    last_applied: Cell<Option<Instant>>,
    /// The Phase 1 gate probe (`--gate-probe`), when running.
    #[cfg(feature = "measure")]
    gate: RefCell<Option<GateState>>,
    #[cfg(feature = "measure")]
    stats: RefCell<FrameStats>,
    #[cfg(feature = "measure")]
    frames_done: Cell<u64>,
    #[cfg(feature = "measure")]
    finished: Cell<bool>,
    /// The menu-bar side; `None` in measurement modes.
    pub(super) resident: RefCell<Option<ResidentState>>,
    /// A debug build's scripted states (`NE_DEBUG_SCRIPT`), still to run.
    #[cfg(debug_assertions)]
    debug_steps: RefCell<std::collections::VecDeque<super::debug_script::Step>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; WinController does
    // not implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FxWindowController"]
    #[ivars = WinIvars]
    pub(crate) struct WinController;

    // SAFETY: NSObject itself conforms to NSObjectProtocol; no method is
    // overridden.
    unsafe impl NSObjectProtocol for WinController {}

    // SAFETY: the methods below have the signatures the protocol declares;
    // every other method of it is optional.
    unsafe impl NSApplicationDelegate for WinController {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            let app = NSApplication::sharedApplication(self.mtm());
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);
        }

        /// The app quits (status menu, Cmd+Q, logout or the end of a
        /// measurement run): the windows and the menu-bar item disappear at
        /// once, then the index is saved if the next launch needs it
        /// ([`quit_steps`]; usually nothing is written).
        #[unsafe(method(applicationWillTerminate:))]
        fn will_terminate(&self, _notification: &NSNotification) {
            self.stop_look();
            for step in quit_steps(self.has_status_item()) {
                match step {
                    QuitStep::HideWindows => self.hide_windows(),
                    QuitStep::RemoveStatusItem => self.remove_status_item(),
                    QuitStep::SaveIndex => {
                        let source = Arc::clone(self.ivars().results.borrow().source());
                        if let Err(e) = source.save_for_quit() {
                            eprintln!("{APP_NAME}: cannot save the index: {e}");
                        }
                    }
                }
            }
        }

        /// A resident app keeps running with its window closed; Quit comes
        /// only from the status menu or Cmd+Q.
        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window_closed(&self, _app: &NSApplication) -> bool {
            !self.is_resident()
        }

        /// Re-probe Full Disk Access when the user comes back (for example
        /// from System Settings).
        #[unsafe(method(applicationDidBecomeActive:))]
        fn did_become_active(&self, _notification: &NSNotification) {
            self.onboarding_event(OnboardingEvent::BecameActive);
        }

        /// Opening the app again (Finder, Spotlight) shows the window.
        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn should_handle_reopen(&self, _app: &NSApplication, _visible: bool) -> bool {
            self.show_window();
            false
        }
    }

    // SAFETY: the methods below have the signatures the protocol declares;
    // every other method of it is optional.
    unsafe impl NSControlTextEditingDelegate for WinController {
        /// Return / Enter and the Down arrow in the search field move into
        /// the results; they never open a file. Cmd+Return reveals the
        /// selection, or the first row, in Finder.
        /// Escape (`cancelOperation:`) in the empty field hides the window
        /// (resident app); with text, the field clears itself as before.
        #[unsafe(method(control:textView:doCommandBySelector:))]
        fn do_command(&self, _control: &NSControl, _text_view: &AnyObject, command: Sel) -> bool {
            self.search_field_command(command)
        }

        /// × shows while the field holds text.
        #[unsafe(method(controlTextDidChange:))]
        fn text_did_change(&self, _notification: &NSNotification) {
            self.sync_clear_button();
        }
    }

    // SAFETY: every method of the protocol is optional; the inherited
    // NSControlTextEditingDelegate methods are implemented above.
    unsafe impl NSTextFieldDelegate for WinController {}
    // SAFETY: every method of the protocol is optional and none is
    // implemented.
    unsafe impl NSSearchFieldDelegate for WinController {}

    // SAFETY: the methods below have the signatures the protocol declares
    // (`tableView:pasteboardWriterForRow:` returns a retained writer);
    // every other method of it is optional.
    unsafe impl NSTableViewDataSource for WinController {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> isize {
            isize::try_from(self.ivars().results.borrow().row_count()).unwrap_or(isize::MAX)
        }

        #[unsafe(method_id(tableView:pasteboardWriterForRow:))]
        fn pasteboard_writer(
            &self,
            _table: &NSTableView,
            row: isize,
        ) -> Option<Retained<ProtocolObject<dyn NSPasteboardWriting>>> {
            self.drag_writer(row)
        }
    }

    // SAFETY: the methods below have the signatures the protocol declares
    // (the `method_id` ones return retained views); every other method of
    // it is optional.
    unsafe impl NSTableViewDelegate for WinController {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for_row(
            &self,
            table: &NSTableView,
            column: Option<&NSTableColumn>,
            row: isize,
        ) -> Option<Retained<NSView>> {
            column.and_then(|column| self.cell_view(table, column, row))
        }

        /// The design's rows: hover and selection are one shared surface
        /// each.
        #[unsafe(method_id(tableView:rowViewForRow:))]
        fn row_view_for_row(
            &self,
            table: &NSTableView,
            _row: isize,
        ) -> Option<Retained<NSTableRowView>> {
            Some(Retained::into_super(ResultRowView::make(table)))
        }

        /// A row view (new or reused) shows its row's hover and the theme.
        #[unsafe(method(tableView:didAddRowView:forRow:))]
        fn did_add_row_view(&self, _table: &NSTableView, row_view: &NSTableRowView, row: isize) {
            if let Some(view) = row_view.downcast_ref::<ResultRowView>() {
                let hovered = usize::try_from(row).ok() == self.ivars().hover_row.get();
                view.set_hovered(hovered);
                view.refresh();
            }
        }

        #[unsafe(method(tableViewSelectionDidChange:))]
        fn selection_did_change(&self, _notification: &NSNotification) {
            self.selection_changed();
            self.update_status();
        }
    }

    impl WinController {
        /// Action of the search field: run the new query.
        #[unsafe(method(search:))]
        fn search_action(&self, _sender: Option<&AnyObject>) {
            self.query_changed();
        }

        /// Double action of the table: open the clicked row (with the rest
        /// of the selection when it is part of it).
        #[unsafe(method(openClicked:))]
        fn open_clicked(&self, _sender: Option<&AnyObject>) {
            let Some(ui) = self.ui() else { return };
            let Ok(clicked) = usize::try_from(ui.table.clickedRow()) else {
                return;
            };
            let rows = if ui.table.selectedRowIndexes().containsIndex(clicked) {
                None
            } else {
                Some(vec![one_row(clicked)])
            };
            self.perform(Action::Open, rows);
        }

        /// A search option chip (Aa, Path, .*) was clicked.
        #[unsafe(method(chipClicked:))]
        fn chip_clicked(&self, sender: Option<&AnyObject>) {
            if let Some(chip) = tag_of(sender).and_then(TopBar::chip_for_tag) {
                let options = chip.toggle(self.ivars().results.borrow().options());
                self.set_options(options);
            }
        }

        /// A column title was clicked: sort by it, or flip the direction.
        #[unsafe(method(headerClicked:))]
        fn header_clicked(&self, sender: Option<&AnyObject>) {
            if let Some(column) = tag_of(sender).and_then(ResultsCard::column_for_tag) {
                self.sort_by(column);
            }
        }

        /// A syntax example in "No matches" was clicked: type it into the
        /// search.
        #[unsafe(method(exampleClicked:))]
        fn example_clicked(&self, sender: Option<&AnyObject>) {
            if let Some(example) = tag_of(sender).and_then(ResultsCard::example_for_tag) {
                self.type_example(example);
            }
        }

        /// The search field's ×.
        #[unsafe(method(clearClicked:))]
        fn clear_clicked(&self, _sender: Option<&AnyObject>) {
            self.clear_search();
        }

        /// The system switched between legacy and overlay scrollers (a
        /// mouse connected or removed, or the "Show scroll bars" setting):
        /// the rows' columns make room for a legacy scroller, or take it
        /// back.
        #[unsafe(method(scrollerStyleChanged:))]
        fn scroller_style_changed(&self, _note: &NSNotification) {
            self.layout_window();
        }

        /// One-shot timer: fire a deferred generation re-run.
        #[unsafe(method(rerunTick:))]
        fn rerun_tick(&self, _timer: &NSTimer) {
            self.ivars().tick_scheduled.set(false);
            let req = self.ivars().results.borrow_mut().tick(Instant::now());
            if let Some(req) = req {
                self.submit(req);
            }
            self.schedule_tick_if_pending();
        }

        /// One-shot timer: refresh the status bar while indexing.
        #[unsafe(method(statusTick:))]
        fn status_tick(&self, _timer: &NSTimer) {
            self.ivars().status_scheduled.set(false);
            self.update_status();
        }

        /// A debug build's scripted step (`NE_DEBUG_SCRIPT`).
        #[cfg(debug_assertions)]
        #[unsafe(method(debugStep:))]
        fn debug_step_sel(&self, timer: &NSTimer) {
            if !self.debug_step() {
                timer.invalidate();
            }
        }

        /// Display-rate timer of the measurement modes.
        #[cfg(feature = "measure")]
        #[unsafe(method(measureTick:))]
        fn measure_tick(&self, _timer: &NSTimer) {
            self.on_measure_tick();
        }

        /// A status menu item was chosen; its tag names the action.
        #[unsafe(method(menuAction:))]
        fn menu_action_sel(&self, sender: &NSMenuItem) {
            self.menu_action(sender.tag());
        }

        /// The callout's "Grant Full Disk Access…" button.
        #[unsafe(method(grantAccess:))]
        fn grant_access(&self, _sender: Option<&AnyObject>) {
            self.onboarding_event(OnboardingEvent::GrantClicked);
        }

        /// The callout's "Not Now" button.
        #[unsafe(method(dismissBanner:))]
        fn dismiss_banner(&self, _sender: Option<&AnyObject>) {
            self.onboarding_event(OnboardingEvent::Dismissed);
        }

        /// Repeating timer: the daily save, when it is due.
        #[unsafe(method(dailySaveTick:))]
        fn daily_save_tick_sel(&self, _timer: &NSTimer) {
            self.daily_save_tick();
        }

        /// The results window went on or off screen.
        #[unsafe(method(occlusionChanged:))]
        fn occlusion_changed(&self, _note: &NSNotification) {
            self.sync_visibility();
        }

        /// One-shot timer: re-probe Full Disk Access while the banner shows.
        #[unsafe(method(onboardingTick:))]
        fn onboarding_tick_sel(&self, _timer: &NSTimer) {
            self.onboarding_tick();
        }

        /// About in the app menu (sent down the responder chain to the app
        /// delegate): the standard About panel with the version label and
        /// build number.
        #[unsafe(method(showAbout:))]
        fn show_about_sel(&self, _sender: Option<&AnyObject>) {
            super::show_about_panel(self.mtm());
        }

        /// Settings… in the app menu (sent down the responder chain to the
        /// app delegate).
        #[unsafe(method(showSettings:))]
        fn show_settings_sel(&self, _sender: Option<&AnyObject>) {
            self.show_settings();
        }

        /// Key-value observing: `NSApp.effectiveAppearance` changed (the
        /// system or the app switched between light and dark).
        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        fn observe_value(
            &self,
            _key_path: Option<&NSString>,
            _object: Option<&AnyObject>,
            _change: Option<&AnyObject>,
            _context: *mut c_void,
        ) {
            self.refresh_look();
        }
    }
);

impl WinController {
    pub(crate) fn new(mtm: MainThreadMarker, config: WindowConfig) -> Retained<WinController> {
        // The resident app starts from the sort and chips saved in the
        // settings; the measurement modes (no resident state) keep Name
        // ascending and every chip off.
        let (sort, options) = config.resident.as_ref().map_or_else(
            || (SortState::default(), QueryOptions::default()),
            |r| (r.settings.sort, r.settings.search_options),
        );
        let this = WinController::alloc(mtm).set_ivars(WinIvars {
            results: RefCell::new(
                ResultsController::new(config.source)
                    .with_sort(sort)
                    .with_options(options)
                    .with_your_files_first(config.your_files_first),
            ),
            shared: Arc::new(Shared::default()),
            requests: RefCell::new(None),
            ui: OnceCell::new(),
            tick_scheduled: Cell::new(false),
            status_scheduled: Cell::new(false),
            reset_on_apply: Cell::new(false),
            restoring_selection: Cell::new(false),
            buf: RefCell::new(String::new()),
            zone: SystemTimeZone::new(),
            home: fx_fs_macos::fda::user_home().and_then(|h| h.into_os_string().into_string().ok()),
            search_time: Cell::new(None),
            page_serial: Cell::new(0),
            highlighting: RefCell::new(Highlighting::default()),
            hover_row: Cell::new(None),
            focus: Cell::new(Focus::SearchField),
            copy_note: Cell::new(None),
            was_indexing: Cell::new(None),
            #[cfg(feature = "measure")]
            autoscroll: config.autoscroll,
            #[cfg(feature = "measure")]
            launch_probe: config.launch_probe,
            #[cfg(feature = "measure")]
            process_start: config.process_start,
            #[cfg(feature = "measure")]
            first_results: Cell::new(None),
            #[cfg(feature = "measure")]
            last_applied: Cell::new(None),
            #[cfg(feature = "measure")]
            gate: RefCell::new(config.gate_probe.map(|out| GateState {
                run: ProbeRun::with_defaults(config.process_start),
                out,
            })),
            #[cfg(feature = "measure")]
            stats: RefCell::new(FrameStats::new()),
            #[cfg(feature = "measure")]
            frames_done: Cell::new(0),
            #[cfg(feature = "measure")]
            finished: Cell::new(false),
            resident: RefCell::new(config.resident.map(ResidentState::new)),
            #[cfg(debug_assertions)]
            debug_steps: RefCell::new(super::debug_script::script().into()),
        });
        // SAFETY: `init` is NSObject's designated initializer.
        let this: Retained<WinController> = unsafe { msg_send![super(this), init] };
        CONTROLLER.with(|c| *c.borrow_mut() = Some(this.clone()));
        this
    }

    /// The chips the window starts with.
    pub(crate) fn options(&self) -> QueryOptions {
        self.ivars().results.borrow().options()
    }

    pub(crate) fn set_ui(&self, ui: WinUi) {
        views::set_scale(ui.window.backingScaleFactor());
        let _ = self.ivars().ui.set(ui);
        views::set_theme(self.window_theme());
        self.restyle();
        self.sync_sort_indicator();
        self.layout_window();
        self.update_status();
        // SAFETY: `scrollerStyleChanged:` is implemented above as
        // `-(void)scrollerStyleChanged:(NSNotification *)note`; the name is
        // an immutable framework constant; the controller lives until the
        // app quits, so the observer never outlives it.
        unsafe {
            NSNotificationCenter::defaultCenter().addObserver_selector_name_object(
                self,
                sel!(scrollerStyleChanged:),
                Some(objc2_app_kit::NSPreferredScrollerStyleDidChangeNotification),
                None,
            );
        }
        if let Some(ui) = self.ui() {
            // SAFETY: `occlusionChanged:` is implemented above as
            // `-(void)occlusionChanged:(NSNotification *)note`; the name is
            // an immutable framework constant; the controller and the window
            // live until the app quits, so the observer never outlives them.
            unsafe {
                NSNotificationCenter::defaultCenter().addObserver_selector_name_object(
                    self,
                    sel!(occlusionChanged:),
                    Some(objc2_app_kit::NSWindowDidChangeOcclusionStateNotification),
                    Some(&ui.window),
                );
            }
        }
        // A window that has never been shown posts no change: read its state.
        self.sync_visibility();
    }

    /// Tells the results controller whether the window is on screen: no
    /// search re-runs while it is not, and one when it shows again.
    fn sync_visibility(&self) {
        let Some(ui) = self.ui() else { return };
        let visible = ui
            .window
            .occlusionState()
            .contains(NSWindowOcclusionState::Visible);
        self.ivars()
            .shared
            .hidden
            .store(!visible, Ordering::Release);
        if visible {
            // Take in the generations recorded while hidden.
            self.drain();
        }
        let req = self
            .ivars()
            .results
            .borrow_mut()
            .set_visible(visible, Instant::now());
        if let Some(req) = req {
            self.submit(req);
        }
        self.schedule_tick_if_pending();
    }

    /// The daily-save check: once a day, at a moment nobody uses the
    /// keyboard or mouse, the engine saves the index in the background.
    fn daily_save_tick(&self) {
        let results = self.ivars().results.borrow();
        let source = results.source();
        if daily_save_due(source.saved_ago(), super::resident::input_idle()) {
            source.request_save();
        }
    }

    /// A `global-hotkey` event handler: queues each event for the main
    /// thread and wakes it, so no timer polls for events.
    pub(super) fn hotkey_sender(&self) -> impl Fn(GlobalHotKeyEvent) + Send + Sync + 'static {
        let waker = MainWaker {
            waker: Waker::new(),
            shared: Arc::clone(&self.ivars().shared),
        };
        move |event| {
            waker.shared.inbox().hotkey.push(event);
            waker.wake();
        }
    }

    pub(super) fn ui(&self) -> Option<&WinUi> {
        self.ivars().ui.get()
    }

    /// Spawns the search worker and the generation listener, then runs
    /// the initial (empty) query.
    pub(crate) fn start(&self) {
        let iv = self.ivars();
        let source = Arc::clone(iv.results.borrow().source());
        let waker = Arc::new(MainWaker {
            waker: Waker::new(),
            shared: Arc::clone(&iv.shared),
        });

        let (tx, rx) = mpsc::channel::<SearchRequest>();
        let worker_source = Arc::clone(&source);
        let worker_waker = Arc::clone(&waker);
        std::thread::Builder::new()
            .name("fx-ui-search".into())
            .spawn(move || {
                // Someone waits for these searches; the engine's own threads
                // run at utility.
                qos::set_current_thread_class(qos::ThreadClass::UserInitiated);
                while let Ok(mut request) = rx.recv() {
                    // Only the newest request matters.
                    while let Ok(newer) = rx.try_recv() {
                        request = newer;
                    }
                    let started = Instant::now();
                    let answer = request.execute(&*worker_source);
                    if matches!(answer, Err(SearchError::Cancelled)) {
                        continue;
                    }
                    let took = started.elapsed();
                    worker_waker
                        .shared
                        .inbox()
                        .pages
                        .push((request.id, answer, took));
                    worker_waker.wake();
                }
            })
            .expect("spawn the search worker");
        *iv.requests.borrow_mut() = Some(tx);

        let generations = source.subscribe();
        std::thread::Builder::new()
            .name("fx-ui-generations".into())
            .spawn(move || {
                for g in generations {
                    {
                        let mut inbox = waker.shared.inbox();
                        inbox.generation = Some(inbox.generation.map_or(g, |old| old.max(g)));
                    }
                    if !waker.shared.hidden.load(Ordering::Acquire) {
                        waker.wake();
                    }
                }
            })
            .expect("spawn the generation listener");

        let req = iv.results.borrow_mut().set_query("");
        self.prepare_highlighter();
        self.submit(req);

        #[cfg(debug_assertions)]
        if !iv.debug_steps.borrow().is_empty() {
            let target: &AnyObject = self.as_ref();
            // SAFETY: `debugStep:` is implemented as `-(void)debugStep:
            // (NSTimer *)timer`; the run loop retains the repeating timer,
            // which retains its target until it is invalidated.
            let _timer = unsafe {
                NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                    0.7,
                    target,
                    sel!(debugStep:),
                    None,
                    true,
                )
            };
        }
    }

    /// Runs the next scripted step; `false` when none is left.
    #[cfg(debug_assertions)]
    fn debug_step(&self) -> bool {
        use super::debug_script::{Arrow, Step};
        let Some(step) = self.ivars().debug_steps.borrow_mut().pop_front() else {
            return false;
        };
        let Some(ui) = self.ui() else { return false };
        let shown = format!("{step:?}");
        match step {
            Step::Query(text) => {
                ui.search.setStringValue(&NSString::from_str(&text));
                self.query_changed();
            }
            Step::Chips { case, path, regex } => self.set_options(QueryOptions {
                match_case: case,
                match_path: path,
                regex,
            }),
            Step::Select(row) => {
                ui.table.selectRowIndexes_byExtendingSelection(
                    &NSIndexSet::indexSetWithIndex(row),
                    false,
                );
            }
            Step::FocusResults => {
                ui.window.makeFirstResponder(Some(&ui.table));
            }
            Step::FocusField => {
                ui.window.makeFirstResponder(Some(&ui.search));
            }
            Step::Hover(row) => self.set_hover_row(Some(row)),
            Step::Sort(id) => {
                if let Some(column) = Column::from_identifier(&id) {
                    self.sort_by(column);
                }
            }
            Step::Size(w, h) => {
                ui.window
                    .setContentSize(objc2_foundation::NSSize::new(w, h));
            }
            Step::Dark(dark) => {
                // SAFETY: the appearance names are immutable framework
                // constants.
                let name = unsafe {
                    if dark {
                        objc2_app_kit::NSAppearanceNameDarkAqua
                    } else {
                        objc2_app_kit::NSAppearanceNameAqua
                    }
                };
                let appearance = objc2_app_kit::NSAppearance::appearanceNamed(name);
                NSApplication::sharedApplication(self.mtm()).setAppearance(appearance.as_deref());
            }
            Step::Escape => {
                ui.window.makeFirstResponder(Some(&ui.search));
                if let Some(editor) = ui.search.currentEditor() {
                    use objc2_app_kit::NSStandardKeyBindingResponding;
                    // SAFETY: `cancelOperation:` is a standard responder
                    // action taking a sender, as a key binding sends it.
                    unsafe { editor.doCommandBySelector(sel!(cancelOperation:)) };
                }
            }
            Step::Press(tag) => {
                if let Some(control) = ui
                    .root
                    .viewWithTag(tag)
                    .and_then(|v| v.downcast::<NSControl>().ok())
                {
                    // SAFETY: `performClick:` takes an optional sender.
                    unsafe { control.performClick(None) };
                }
            }
            Step::Wait => {}
            Step::Click(row) => {
                let r = ui.table.rectOfRow(row as isize);
                let point = NSPoint::new(r.origin.x + 40.0, r.origin.y + r.size.height / 2.0);
                let at = ui.table.convertPoint_toView(point, None);
                let down = debug_mouse(&ui.window, NSEventType::LeftMouseDown, at);
                let up = debug_mouse(&ui.window, NSEventType::LeftMouseUp, at);
                if let (Some(down), Some(up)) = (down, up) {
                    // The table's mouse-down tracks the click until it
                    // takes this mouse-up from the queue.
                    NSApplication::sharedApplication(self.mtm()).postEvent_atStart(&up, false);
                    ui.window.sendEvent(&down);
                }
            }
            Step::Arrow(arrow) => {
                let (ch, code) = match arrow {
                    Arrow::Up => ('\u{F700}', 126),
                    Arrow::Down => ('\u{F701}', 125),
                };
                let flags = NSEventModifierFlags::NumericPad | NSEventModifierFlags::Function;
                debug_key(&ui.window, ch, code, flags);
            }
            Step::Type(text) => {
                // 80 ms apart, a fast typist, with the run loop turning in
                // between as it does for real key presses.
                for ch in text.chars() {
                    let code = if ch == ' ' { 49 } else { 0 };
                    debug_key(&ui.window, ch, code, NSEventModifierFlags::empty());
                    NSRunLoop::currentRunLoop()
                        .runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.08));
                }
            }
        }
        let focus = ui.window.firstResponder().map_or_else(
            || "nothing".to_owned(),
            |r| format!("{:?}", r.class().name()),
        );
        eprintln!(
            "debug step {shown}: focus {focus}, key window {}, selected row {}, query {:?}, {:?}",
            ui.window.isKeyWindow(),
            ui.table.selectedRow(),
            ui.search.stringValue().to_string(),
            self.ivars().results.borrow().current_request(),
        );
        true
    }

    /// The pasteboard writer for dragging `row` out: its file URL.
    fn drag_writer(&self, row: isize) -> Option<Retained<ProtocolObject<dyn NSPasteboardWriting>>> {
        let row = usize::try_from(row).ok()?;
        let path = self
            .ivars()
            .results
            .borrow_mut()
            .paths_for_ranges(&[one_row(row)])
            .paths
            .pop();
        self.schedule_tick_if_pending();
        let path = path?;
        Some(ProtocolObject::from_retained(file_url(&path)))
    }

    /// Sends `req` (with the current query, options and sort) to the
    /// worker.
    fn submit(&self, req: RequestId) {
        let search = self.ivars().results.borrow().search_request(req);
        if let Some(tx) = &*self.ivars().requests.borrow() {
            let _ = tx.send(search);
        }
    }

    /// On the main thread: applies posted pages, errors and generations.
    fn drain(&self) {
        let iv = self.ivars();
        iv.shared.wake_pending.store(false, Ordering::Release);
        let (pages, generation, hotkey) = {
            let mut inbox = iv.shared.inbox();
            (
                std::mem::take(&mut inbox.pages),
                inbox.generation.take(),
                std::mem::take(&mut inbox.hotkey),
            )
        };
        if !hotkey.is_empty() {
            self.hotkey_events(&hotkey);
        }
        // Read before the rows change under the table.
        let first_in_view = !pages.is_empty() && self.first_selected_in_view();
        let mut changed = false;
        let mut rerun = None;
        {
            let mut results = iv.results.borrow_mut();
            for (id, answer, took) in pages {
                let shown = match &answer {
                    Ok(_) => answer.map(|page| results.apply(id, page)).unwrap_or(false),
                    Err(e) => results.apply_error(id, e),
                };
                if shown {
                    iv.search_time.set(Some(took));
                }
                changed |= shown;
            }
            if let Some(g) = generation {
                rerun = results.on_generation(g, Instant::now());
            }
        }
        if let Some(req) = rerun {
            self.submit(req);
        }
        if changed {
            self.reload(first_in_view);
        }
        // After the reload: re-selecting may ask for another re-run.
        self.schedule_tick_if_pending();
    }

    /// Shows the page just applied. A new query or sort clears the
    /// selection and scrolls to the top; a refresh of the same results
    /// selects the same files again where the controller says they now are
    /// (see `ResultsController::reselect`). `first_in_view` says whether
    /// the first selected row was visible before the page was applied.
    fn reload(&self, first_in_view: bool) {
        let Some(ui) = self.ui() else { return };
        let iv = self.ivars();
        iv.page_serial.set(iv.page_serial.get() + 1);
        // reloadData and the selection calls below may post selection
        // changes; they are the controller's, not the user's.
        iv.restoring_selection.set(true);
        ui.table.reloadData();
        let rows = iv.results.borrow().row_count();
        if iv.reset_on_apply.replace(false) {
            iv.results.borrow_mut().forget_selection();
            ui.table
                .selectRowIndexes_byExtendingSelection(&NSIndexSet::new(), false);
            if rows > 0 {
                ui.table.scrollRowToVisible(0);
            }
        } else {
            let reselection = iv.results.borrow_mut().reselect(first_in_view);
            if let Some(r) = reselection {
                let set = NSMutableIndexSet::new();
                for &row in &r.rows {
                    set.addIndex(row);
                }
                ui.table.selectRowIndexes_byExtendingSelection(&set, false);
                if let Some(row) = r.scroll_to.and_then(|row| isize::try_from(row).ok()) {
                    ui.table.scrollRowToVisible(row);
                }
            }
        }
        iv.restoring_selection.set(false);
        #[cfg(feature = "measure")]
        self.note_page_applied(ui, rows);
        self.show_body();
        self.update_status();
    }

    /// Shows the rows, "No matches" or the invalid regular expression.
    fn show_body(&self) {
        let Some(ui) = self.ui() else { return };
        let results = self.ivars().results.borrow();
        let query = results.query().to_owned();
        let b = body(
            !query.trim().is_empty(),
            results.row_count(),
            results.invalid_regex(),
        );
        drop(results);
        ui.card.show_body(b, &query);
    }

    fn schedule_tick_if_pending(&self) {
        let iv = self.ivars();
        if iv.tick_scheduled.get() || !iv.results.borrow().wants_tick() {
            return;
        }
        iv.tick_scheduled.set(true);
        let target: &AnyObject = self.as_ref();
        // SAFETY: `rerunTick:` is implemented as `-(void)rerunTick:(NSTimer
        // *)timer`; the run loop retains the one-shot timer and the timer
        // retains its target.
        let _timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                DEFAULT_THROTTLE.as_secs_f64(),
                target,
                sel!(rerunTick:),
                None,
                false,
            )
        };
    }

    /// "Your files first" changed to `on`: re-runs the current query and
    /// sort in the new order, from the top.
    pub(super) fn set_your_files_first(&self, on: bool) {
        let req = self.ivars().results.borrow_mut().set_your_files_first(on);
        if let Some(req) = req {
            self.ivars().reset_on_apply.set(true);
            self.submit(req);
        }
    }

    fn query_changed(&self) {
        let Some(ui) = self.ui() else { return };
        let text = ui.search.stringValue().to_string();
        let req = {
            let mut results = self.ivars().results.borrow_mut();
            if results.query() == text {
                return;
            }
            results.set_query(&text)
        };
        self.ivars().reset_on_apply.set(true);
        self.ivars().copy_note.set(None);
        self.prepare_highlighter();
        self.sync_clear_button();
        self.submit(req);
    }

    /// A chip set the search field's options to `options`: re-runs the
    /// search under them and remembers them (resident app).
    fn set_options(&self, options: QueryOptions) {
        let req = self.ivars().results.borrow_mut().set_options(options);
        let Some(req) = req else { return };
        self.ivars().reset_on_apply.set(true);
        self.prepare_highlighter();
        if let Some(ui) = self.ui() {
            ui.top.set_options(options);
        }
        self.submit(req);
        self.with_resident(|r| {
            r.settings.search_options = options;
            r.save_settings();
        });
    }

    /// Prepares the highlighter for the current query and options.
    fn prepare_highlighter(&self) {
        let results = self.ivars().results.borrow();
        let (query, options) = (results.query(), results.options());
        let mut h = self.ivars().highlighting.borrow_mut();
        let current = h
            .prepared
            .as_ref()
            .is_some_and(|(q, o, _)| q == query && *o == options);
        if !current {
            h.prepared = Some((
                query.to_owned(),
                options,
                Highlighter::for_query(query, options),
            ));
            h.last = None;
        }
    }

    /// A click on `column`'s title: sort by it, or flip the direction.
    fn sort_by(&self, column: Column) {
        let iv = self.ivars();
        let (req, sort) = {
            let mut results = iv.results.borrow_mut();
            let req = results.set_sort(column);
            (req, results.sort())
        };
        iv.reset_on_apply.set(true);
        self.sync_sort_indicator();
        self.submit(req);
        // Remembered for the next launch (resident app only).
        self.with_resident(|r| {
            r.settings.sort = sort;
            r.save_settings();
        });
    }

    /// Makes the column titles show the controller's sort.
    fn sync_sort_indicator(&self) {
        let Some(ui) = self.ui() else { return };
        let sort = self.ivars().results.borrow().sort();
        ui.card.set_sort(sort);
    }

    /// Types `example` at the end of the search and runs it.
    fn type_example(&self, example: &str) {
        let Some(ui) = self.ui() else { return };
        let text = with_example(&ui.search.stringValue().to_string(), example);
        ui.search.setStringValue(&NSString::from_str(&text));
        ui.window.makeFirstResponder(Some(&ui.search));
        if let Some(editor) = ui.search.currentEditor() {
            let end = text.encode_utf16().count();
            editor.setSelectedRange(NSRange::new(end, 0));
        }
        self.query_changed();
    }

    /// Shows × only while the field holds text.
    fn sync_clear_button(&self) {
        if let Some(ui) = self.ui() {
            ui.top.set_clear_visible(!ui.top.text().is_empty());
        }
    }

    fn search_field_command(&self, command: Sel) -> bool {
        if command == sel!(moveDown:) {
            self.focus_results();
            return true;
        }
        if command == sel!(cancelOperation:) {
            // Returning false leaves Escape to the field, which clears its
            // text itself (and Cmd+. to AppKit).
            return self
                .current_key()
                .is_some_and(|(key, mods)| self.escape(Focus::SearchField, key, mods));
        }
        if command != sel!(insertNewline:) && command != sel!(noop:) {
            return false;
        }
        let Some((key, mods)) = self.current_key() else {
            return false;
        };
        match search_field_key(key, mods) {
            Some(SearchFieldKey::FocusResults) => {
                self.focus_results();
                true
            }
            Some(SearchFieldKey::Act(action)) => {
                self.select_first_if_none();
                self.perform(action, None);
                true
            }
            None => false,
        }
    }

    /// The action-map key and modifiers of the key-down event being
    /// handled, if the current event is one.
    fn current_key(&self) -> Option<(Key, Modifiers)> {
        let event = NSApplication::sharedApplication(self.mtm()).currentEvent()?;
        (event.r#type() == NSEventType::KeyDown).then(|| key_of(&event))
    }

    /// Escape with the focus in `focus`, as [`escape_action`] decides from
    /// the search field's text: clear the search, hide the window, or
    /// nothing. Returns whether the key was handled.
    fn escape(&self, focus: Focus, key: Key, mods: Modifiers) -> bool {
        let Some(ui) = self.ui() else { return false };
        let text = ui.search.stringValue().to_string();
        match escape_action(key, mods, focus, &text, self.is_resident()) {
            Some(EscapeAction::ClearSearch) => {
                self.clear_search();
                true
            }
            Some(EscapeAction::HideWindow) => {
                self.hide_window();
                true
            }
            None => false,
        }
    }

    /// Empties the search field, runs the empty query and moves the focus
    /// to the field, as Escape in a search field with text does.
    fn clear_search(&self) {
        let Some(ui) = self.ui() else { return };
        ui.search.setStringValue(&NSString::from_str(""));
        self.query_changed();
        ui.window.makeFirstResponder(Some(&ui.search));
    }

    /// Moves the focus from the search field to the results, selecting the
    /// first row when none is selected. With no results the focus stays.
    fn focus_results(&self) {
        let Some(ui) = self.ui() else { return };
        if self.ivars().results.borrow().row_count() == 0 {
            return;
        }
        self.select_first_if_none();
        if let Some(window) = ui.table.window() {
            window.makeFirstResponder(Some(&ui.table));
        }
    }

    fn select_first_if_none(&self) {
        let Some(ui) = self.ui() else { return };
        if ui.table.selectedRow() < 0 && self.ivars().results.borrow().row_count() > 0 {
            ui.table
                .selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(0), false);
        }
    }

    /// The user changed the selection: the controller remembers its files
    /// (up to `MAX_KEPT_SELECTION` rows) to keep them selected when the
    /// results refresh.
    fn selection_changed(&self) {
        if self.ivars().restoring_selection.get() {
            return;
        }
        self.ivars().copy_note.set(None);
        // One row past the cap, so the controller sees a selection too large
        // to keep without reading it all.
        let ranges = self.selected_ranges(MAX_KEPT_SELECTION + 1);
        self.ivars()
            .results
            .borrow_mut()
            .remember_selection(&ranges);
        self.schedule_tick_if_pending();
    }

    /// Whether the first selected row is (at least partly) visible.
    fn first_selected_in_view(&self) -> bool {
        let Some(ui) = self.ui() else { return false };
        let first = ui.table.selectedRowIndexes().firstIndex();
        if first == NSNotFound as usize {
            // Nothing is selected.
            return false;
        }
        let Ok(row) = isize::try_from(first) else {
            return false;
        };
        let rect = ui.table.rectOfRow(row);
        let visible = ui.table.visibleRect();
        let (top, bottom) = (rect.origin.y, rect.origin.y + rect.size.height);
        let (view_top, view_bottom) = (visible.origin.y, visible.origin.y + visible.size.height);
        rect.size.height > 0.0 && top < view_bottom && bottom > view_top
    }

    /// The first `limit` selected rows as contiguous ranges, in row order.
    /// A contiguous selection (such as Select All) costs O(1) calls; any
    /// other walks at most `limit` indexes.
    fn selected_ranges(&self, limit: usize) -> Vec<Range<usize>> {
        let Some(ui) = self.ui() else {
            return Vec::new();
        };
        let set = ui.table.selectedRowIndexes();
        let count = set.count();
        let first = set.firstIndex();
        if count == 0 || first == NSNotFound as usize {
            return Vec::new();
        }
        if set.lastIndex() - first + 1 == count {
            return vec![Range {
                start: first,
                end: first + count.min(limit),
            }];
        }
        let mut ranges: Vec<Range<usize>> = Vec::new();
        let mut taken = 0;
        let mut i = first;
        while i != NSNotFound as usize && taken < limit {
            match ranges.last_mut() {
                Some(last) if last.end == i => last.end += 1,
                _ => ranges.push(i..i + 1),
            }
            taken += 1;
            i = set.indexGreaterThanIndex(i);
        }
        ranges
    }

    /// Performs a read-only `action` on `rows` (default: the selection).
    /// Open and Reveal act on at most [`MAX_OPEN`] rows, and only those
    /// are resolved; Copy resolves at most [`MAX_COPY_PATHS`] and says so
    /// in the status bar when the selection held more. The selection is
    /// resolved to the files the controller remembers for it, and rows are
    /// never read from newer results than the table shows: while a refresh
    /// is on its way such an action does nothing (see
    /// `CopiedPaths::outdated`).
    fn perform(&self, action: Action, rows: Option<Vec<Range<usize>>>) {
        let limit = match action {
            Action::Open | Action::RevealInFinder => MAX_OPEN,
            // One row past the cap, so the controller reports truncation.
            Action::CopyPath => MAX_COPY_PATHS + 1,
        };
        let (ranges, selected) = match rows {
            Some(rows) => (take_rows(rows, limit), None),
            None => {
                let selected = self.ui().map_or(0, |ui| ui.table.numberOfSelectedRows());
                (self.selected_ranges(limit), usize::try_from(selected).ok())
            }
        };
        if ranges.is_empty() {
            return;
        }
        let copied = {
            let mut results = self.ivars().results.borrow_mut();
            match selected {
                Some(selected) => results.paths_of_selection(&ranges, selected),
                None => results.paths_for_ranges(&ranges),
            }
        };
        self.schedule_tick_if_pending();
        let paths = copied.paths;
        if paths.is_empty() {
            return;
        }
        let workspace = NSWorkspace::sharedWorkspace();
        match action {
            Action::Open => {
                for path in &paths {
                    workspace.openURL(&file_url(path));
                }
            }
            Action::RevealInFinder => {
                let urls: Vec<Retained<NSURL>> = paths.iter().map(|p| file_url(p)).collect();
                workspace.activateFileViewerSelectingURLs(&NSArray::from_retained_slice(&urls));
            }
            Action::CopyPath => {
                let pasteboard = NSPasteboard::generalPasteboard();
                pasteboard.clearContents();
                // SAFETY: reading an immutable AppKit string constant.
                let kind = unsafe { NSPasteboardTypeString };
                pasteboard.setString_forType(&NSString::from_str(&copy_text(&paths)), kind);
                if copied.truncated {
                    self.ivars().copy_note.set(Some(paths.len()));
                    self.update_status();
                }
            }
        }
    }

    // ----------------------------------------------------- the window's look

    /// The theme of the window's appearance.
    fn window_theme(&self) -> Theme {
        self.ui()
            .map_or(Theme::Light, |ui| Theme::for_dark(ui.root.is_dark()))
    }

    /// Light or Dark changed: everything takes the new theme's colours and
    /// surfaces (the old theme's images are dropped).
    pub(crate) fn theme_changed(&self) {
        if self.ui().is_none() {
            return;
        }
        if views::set_theme(self.window_theme()) {
            self.restyle();
            self.layout_window();
            self.update_status();
        }
    }

    /// The window's screen scale changed: the glyphs and surfaces are
    /// shown again at the new scale.
    pub(crate) fn scale_changed(&self) {
        let Some(ui) = self.ui() else { return };
        if views::set_scale(ui.window.backingScaleFactor()) {
            self.restyle();
            self.layout_window();
        }
    }

    /// Sets every text and colour for the current theme.
    fn restyle(&self) {
        let Some(ui) = self.ui() else { return };
        ui.window.setBackgroundColor(Some(&color(Token::Ground)));
        ui.top.restyle();
        ui.card.restyle();
        ui.status.restyle();
        if let Some(callout) = &ui.callout {
            callout.restyle();
        }
        self.sync_clear_button();
        // The cells take their colours when they are next shown.
        self.ivars().highlighting.borrow_mut().last = None;
        ui.table.reloadData();
        self.refresh_visible_rows();
    }

    /// The visible rows show their state in the current theme.
    fn refresh_visible_rows(&self) {
        let Some(ui) = self.ui() else { return };
        let range = ui.table.rowsInRect(ui.table.visibleRect());
        for row in range.location..range.location + range.length {
            let Ok(row) = isize::try_from(row) else { break };
            if let Some(view) = ui
                .table
                .rowViewAtRow_makeIfNecessary(row, false)
                .and_then(|v| v.downcast::<ResultRowView>().ok())
            {
                view.refresh();
            }
        }
    }

    /// Lays the window's parts out for its size.
    pub(crate) fn layout_window(&self) {
        let Some(ui) = self.ui() else { return };
        let size = ui.root.bounds().size;
        let (width, height) = (size.width, size.height);
        let top_h = ui.top.layout(width, ui.window.lights_center());
        let m = metrics::CARD_MARGIN;
        let banner_h = ui
            .callout
            .as_ref()
            .filter(|c| c.is_shown())
            .map(|c| c.layout(Rect::new(m, top_h + m, width - 2.0 * m, 0.0)));
        let status_h = ui.status.height();
        let l = window(width, height, top_h, banner_h, status_h);
        let query = self.ivars().results.borrow().query().to_owned();
        ui.card.layout(l.card, &query);
        self.update_status_at(l.status.y);
    }

    /// The keyboard focus moved, or the window became or stopped being
    /// key: the field's focus ring and the status bar's hints follow.
    pub(crate) fn focus_changed(&self) {
        let Some(ui) = self.ui() else { return };
        let key = ui.window.isKeyWindow();
        let field = key && ui.top.editing();
        ui.top.set_focused(field);
        let focus = match ui.window.firstResponder() {
            Some(r) if std::ptr::eq(&*r, (**ui.table).as_ref()) => Focus::Results,
            _ => Focus::SearchField,
        };
        if self.ivars().focus.replace(focus) != focus {
            self.update_status();
        }
    }

    /// A chip, column title or button was hovered or pressed.
    pub(crate) fn controls_changed(&self) {
        let Some(ui) = self.ui() else { return };
        ui.top.restyle_chips();
        ui.card.restyle_header();
        if let Some(callout) = &ui.callout {
            callout.refresh_buttons();
        }
    }

    /// The pointer is over `row` (or no row).
    pub(crate) fn set_hover_row(&self, row: Option<usize>) {
        let old = self.ivars().hover_row.replace(row);
        if old == row {
            return;
        }
        let Some(ui) = self.ui() else { return };
        for (r, hovered) in [(old, false), (row, true)] {
            let Some(r) = r.and_then(|r| isize::try_from(r).ok()) else {
                continue;
            };
            if let Some(view) = ui
                .table
                .rowViewAtRow_makeIfNecessary(r, false)
                .and_then(|v| v.downcast::<ResultRowView>().ok())
            {
                view.set_hovered(hovered);
            }
        }
    }

    // --------------------------------------------------------- status bar

    /// The status bar for the window's state ([`status_view`]).
    pub(super) fn update_status(&self) {
        let Some(ui) = self.ui() else { return };
        let y = ui.root.bounds().size.height - ui.status.height();
        self.update_status_at(y);
    }

    fn update_status_at(&self, y: f64) {
        let Some(ui) = self.ui() else { return };
        let iv = self.ivars();
        let selected = usize::try_from(ui.table.numberOfSelectedRows()).unwrap_or(0);
        // Limited (non-FDA) mode, the switch to the full index and other
        // resident notes.
        let resident = self.with_resident(|r| {
            r.poll_full_index();
            (
                r.scope_note(),
                r.index_scope() == IndexScope::Upgrading,
                r.note.clone(),
                r.error.clone(),
                r.settings.index_size,
            )
        });
        let (scope, upgrading, note, error, previous): (
            Option<ScopeNote>,
            bool,
            Option<String>,
            Option<String>,
            Option<usize>,
        ) = resident.unwrap_or((None, false, None, None, None));
        let mut input = StatusInput::from_controller(&iv.results.borrow());
        let indexing = input.indexing;
        input.previous_objects = previous;
        input.search_time = iv.search_time.get();
        input.focus = iv.focus.get();
        input.selected = selected;
        input.copy_limit = iv.copy_note.get();
        input.scope = scope;
        input.banner_shown = ui.callout.as_ref().is_some_and(|c| c.is_shown());
        input.note = note.as_deref();
        input.error = error.as_deref();
        let view = status_view(&input);
        let width = ui.root.bounds().size.width;
        if ui.status.update(&view, width, y) {
            // The bar wrapped to another height: the card follows.
            self.layout_window();
        }
        self.note_index_size(indexing, input.objects);

        // The indexing state, and the switch to the full index, can end
        // without a new generation; look again in a second while they last.
        if (indexing || upgrading) && !iv.status_scheduled.replace(true) {
            let target: &AnyObject = self.as_ref();
            // SAFETY: `statusTick:` is implemented as `-(void)statusTick:
            // (NSTimer *)timer`; the run loop retains the one-shot timer and
            // the timer retains its target.
            let _timer = unsafe {
                NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                    STATUS_REFRESH.as_secs_f64(),
                    target,
                    sel!(statusTick:),
                    None,
                    false,
                )
            };
        }
    }

    /// When a scan finishes, remembers the index's size for the next
    /// scan's "of about N" (resident app only; written once per scan, never
    /// at a launch that finds the index complete).
    fn note_index_size(&self, indexing: bool, entries: usize) {
        let was = self.ivars().was_indexing.replace(Some(indexing));
        if was != Some(true) || indexing || entries == 0 {
            return;
        }
        self.with_resident(|r| {
            if r.settings.index_size != Some(entries) {
                r.settings.index_size = Some(entries);
                r.save_settings();
            }
        });
    }

    // -------------------------------------------------------------- cells

    /// A reused (or new) cell view showing one cell of `row`.
    fn cell_view(
        &self,
        table: &NSTableView,
        column: &NSTableColumn,
        row: isize,
    ) -> Option<Retained<NSView>> {
        let col = Column::from_identifier(&column.identifier().to_string())?;
        let cell = ResultCell::make(table, col);
        let label = cell.text()?;
        let iv = self.ivars();
        let mut buf = iv.buf.borrow_mut();
        buf.clear();
        let mut kind = None;
        let mut ranges: Vec<Range<usize>> = Vec::new();
        if let Ok(i) = usize::try_from(row) {
            let mut results = iv.results.borrow_mut();
            if let Some(r) = results.row(i) {
                // The interface's formats: "~" for the home folder (copied
                // paths stay full), "182 KB" and "—" for folders, local dates.
                match col {
                    Column::Name => {
                        buf.push_str(&r.name);
                        kind = Some(FileKind::of(&r.name, r.is_folder));
                    }
                    Column::Path => write_folder(r.folder(), iv.home.as_deref(), &mut buf),
                    Column::Size => write_size_cell(r.size, r.is_folder, &mut buf),
                    Column::Modified => {
                        if let Some(t) = r.modified {
                            write_date(t, &iv.zone, &mut buf);
                        }
                    }
                }
                if matches!(col, Column::Name | Column::Path) {
                    let h = self.highlights(i, r);
                    ranges = if col == Column::Name {
                        h.name
                    } else {
                        folder_highlights(r.folder(), iv.home.as_deref(), &h.folder)
                    };
                }
            }
        }
        // A window from a newer generation was dropped: re-run soon.
        self.schedule_tick_if_pending();
        let (style, token) = match col {
            Column::Name => (&styles::ROW_NAME, Token::InkStrong),
            Column::Path => (&styles::ROW_TEXT, Token::Muted),
            Column::Size | Column::Modified => (&styles::ROW_FIGURES, Token::Text),
        };
        if ranges.is_empty() {
            label.setStringValue(&NSString::from_str(&buf));
            label.setTextColor(Some(&color(token)));
        } else {
            label.setAttributedStringValue(&highlighted(&buf, style, token, &ranges));
        }
        if let Some(kind) = kind {
            cell.set_glyph(&views::glyph(kind.glyph(), metrics::GLYPH_SIZE));
            cell.set_selected(table.isRowSelected(row));
        }
        Some(Retained::into_super(Retained::into_super(cell)))
    }

    /// The matches in row `i` (`row`): the name's, and the folder's in the
    /// folder's own text. Cached for the row last asked about, since its
    /// Name and Path cells ask in turn.
    fn highlights(&self, i: usize, row: &crate::source::Row) -> Highlights {
        let serial = self.ivars().page_serial.get();
        let mut h = self.ivars().highlighting.borrow_mut();
        if let Some((s, r, found)) = &h.last {
            if *s == serial && *r == i {
                return found.clone();
            }
        }
        let found = match &h.prepared {
            Some((_, _, hl)) if !hl.is_empty() => {
                hl.highlight(row.folder(), &row.name, row.is_folder)
            }
            _ => Highlights::default(),
        };
        h.last = Some((serial, i, found.clone()));
        found
    }

    // ------------------------------------------------- instrumentation

    /// Records when a page with `rows` rows was applied, for the launch and
    /// gate probes.
    #[cfg(feature = "measure")]
    fn note_page_applied(&self, ui: &WinUi, rows: usize) {
        let iv = self.ivars();
        let now = Instant::now();
        if rows > 0 && iv.first_results.get().is_none() {
            iv.first_results.set(Some(now));
        }
        iv.last_applied.set(Some(now));
        if iv.gate.borrow().is_some() {
            // The gate probe times each page to its repaint: make sure even
            // a page with no rows is drawn.
            let view: &NSView = &ui.table;
            view.setNeedsDisplay(true);
        }
    }

    #[cfg(feature = "measure")]
    fn on_measure_tick(&self) {
        let iv = self.ivars();
        if iv.finished.get() {
            return;
        }
        if iv.gate.borrow().is_some() {
            return self.on_gate_tick();
        }
        if !(iv.launch_probe || iv.autoscroll) {
            return;
        }
        let Some(ui) = self.ui() else { return };
        let (Some(first_draw), Some(last_draw), Some(first_results)) = (
            ui.table.ivars().first_draw.get(),
            ui.table.ivars().last_draw.get(),
            iv.first_results.get(),
        ) else {
            return;
        };
        if last_draw < first_results {
            return; // the results have not been painted yet
        }
        if iv.launch_probe {
            let ms = |t: Instant| t.duration_since(iv.process_start).as_secs_f64() * 1_000.0;
            print_flush(&format!(
                "rows: {}\nlaunch to first table paint: {:.2} ms\nlaunch to first results: {:.2} ms\nlaunch to results painted: {:.2} ms",
                iv.results.borrow().row_count(),
                ms(first_draw),
                ms(first_results),
                ms(last_draw)
            ));
            return self.finish();
        }
        if iv.frames_done.get() < AUTOSCROLL_FRAMES {
            let frame = self.scroll_one_frame(ui);
            iv.stats.borrow_mut().record(frame);
            iv.frames_done.set(iv.frames_done.get() + 1);
        }
        if iv.frames_done.get() >= AUTOSCROLL_FRAMES {
            print_flush(&format!(
                "autoscroll: {} rows, {} rows per frame\n{}",
                iv.results.borrow().row_count(),
                AUTOSCROLL_ROWS_PER_FRAME,
                iv.stats.borrow().report_text().trim_end()
            ));
            self.finish();
        }
    }

    /// One display-rate tick of the gate probe: report what the window has
    /// seen to the [`ProbeRun`] and do what it asks. The scripted keystrokes
    /// set the search field's text and run it as the query, as the field's
    /// action does, so each sample runs from the query change to the next
    /// table draw after its page is applied.
    #[cfg(feature = "measure")]
    fn on_gate_tick(&self) {
        let iv = self.ivars();
        let Some(ui) = self.ui() else { return };
        let seen = self.observed();
        let action = match iv.gate.borrow_mut().as_mut() {
            Some(gate) => gate.run.tick(Instant::now(), seen),
            None => return,
        };
        match action {
            ProbeAction::Wait => {}
            ProbeAction::SetQuery(query) => {
                ui.search.setStringValue(&NSString::from_str(&query));
                self.query_changed();
            }
            ProbeAction::ScrollFrame => {
                let frame = self.scroll_one_frame(ui);
                if let Some(gate) = iv.gate.borrow_mut().as_mut() {
                    gate.run.record_frame(frame);
                }
            }
            ProbeAction::Finish => self.finish_gate_probe(),
        }
    }

    /// What the probes see of the results: when the first results and the
    /// last page were applied, when the table last drew, and the rows.
    #[cfg(feature = "measure")]
    pub(super) fn observed(&self) -> Observed {
        let iv = self.ivars();
        Observed {
            first_results: iv.first_results.get(),
            last_applied: iv.last_applied.get(),
            last_draw: self.ui().and_then(|ui| ui.table.ivars().last_draw.get()),
            rows: iv.results.borrow().row_count(),
        }
    }

    /// Prints the gate probe's report, writes it to its output file and
    /// quits (exit code 1 when the file cannot be written, for example
    /// because one is already there).
    #[cfg(feature = "measure")]
    fn finish_gate_probe(&self) {
        let written = {
            let gate = self.ivars().gate.borrow();
            let Some(gate) = gate.as_ref() else { return };
            let probe = gate.run.probe();
            print_flush(probe.report_text().trim_end());
            probe
                .write_report(&gate.out)
                .map_err(|e| format!("cannot write {}: {e}", gate.out.display()))
        };
        if let Err(e) = written {
            eprintln!("{APP_NAME}: gate probe: {e}");
            std::process::exit(1);
        }
        self.finish();
    }

    /// Scrolls by [`AUTOSCROLL_ROWS_PER_FRAME`] rows, then lays out and
    /// displays synchronously, timing all of it as one frame.
    #[cfg(feature = "measure")]
    fn scroll_one_frame(&self, ui: &WinUi) -> Duration {
        let start = Instant::now();
        let clip = ui.card.scroll.contentView();
        let row_step = ui.table.rowHeight() + ui.table.intercellSpacing().height;
        let step = row_step * AUTOSCROLL_ROWS_PER_FRAME as f64;
        let bounds = clip.bounds();
        let max_y = ui.table.frame().size.height - bounds.size.height;
        let y = next_scroll_y(bounds.origin.y, step, max_y);
        clip.scrollToPoint(NSPoint::new(bounds.origin.x, y));
        ui.card.scroll.reflectScrolledClipView(&clip);
        if let Some(content) = ui.window.contentView() {
            content.layoutSubtreeIfNeeded();
            content.displayIfNeeded();
        }
        start.elapsed()
    }

    #[cfg(feature = "measure")]
    fn finish(&self) {
        self.ivars().finished.set(true);
        NSApplication::sharedApplication(self.mtm()).terminate(None);
    }
}

impl Waker {
    fn new() -> Retained<Waker> {
        let this = Waker::alloc().set_ivars(());
        // SAFETY: `init` is NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

/// `text` in `style` and `token`'s colour, with `ranges` (UTF-16 units)
/// in the match style: `green-ink`, weight 800 (`.ne-hit`).
fn highlighted(
    text: &str,
    style: &TypeStyle,
    token: Token,
    ranges: &[Range<usize>],
) -> Retained<objc2_foundation::NSAttributedString> {
    let base = attributes(style, token, Flow::Line(NSTextAlignment::Left));
    // SAFETY: the attributes are valid AppKit text attributes.
    let s = unsafe {
        NSMutableAttributedString::initWithString_attributes(
            NSMutableAttributedString::alloc(),
            &NSString::from_str(text),
            Some(&base),
        )
    };
    let len = s.length();
    let font = super::paint::font(&styles::ROW_HIT);
    let ink = color(Token::GreenInk);
    // SAFETY: the attribute names are immutable framework constants.
    let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    let values: [&AnyObject; 2] = [font.as_ref(), ink.as_ref()];
    let hit = NSDictionary::from_slices(&keys, &values);
    for r in ranges {
        let start = r.start.min(len);
        let end = r.end.min(len);
        if end > start {
            // SAFETY: the range lies inside the string; the attributes are
            // valid AppKit text attributes.
            unsafe { s.addAttributes_range(&hit, NSRange::new(start, end - start)) };
        }
    }
    Retained::into_super(s)
}

/// A file URL for `path`.
fn file_url(path: &str) -> Retained<NSURL> {
    NSURL::fileURLWithPath(&NSString::from_str(path))
}

/// Prints and flushes, since `terminate:` exits without running Rust
/// destructors.
#[cfg(feature = "measure")]
fn print_flush(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{text}");
    let _ = out.flush();
}
