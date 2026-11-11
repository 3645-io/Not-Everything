//! The results card: the raised `panel` card with its header of sortable
//! column titles, the rows in a plain `NSTableView` over the virtual row
//! model, and the empty states ("No matches" with clickable syntax
//! examples, and the invalid regular expression).
//!
//! Rows are [`ResultRowView`]s: a row's hover (`well`) and selection
//! (washed, rimmed, pressed) are one shared surface image shown by a
//! [`SurfaceView`] child, swapped only when the row's state changes, so
//! scrolling never draws a surface and the custom rows scroll as fast as
//! standard ones. Cells are [`ResultCell`]s holding a label (and in the
//! Name column a shared template glyph); their colours don't change with
//! the selection, so AppKit's emphasized background style is kept out.
//!
//! The column titles sit in a [`HeaderStrip`] that follows the rows when
//! they scroll sideways. Between the titles, a [`ColumnDivider`] after
//! Name, Path and Size resizes the column on its left
//! ([`crate::column_sizes`]): dragged, double-clicked for the default
//! layout, moved with Left and Right when it has the keyboard focus (Full
//! Keyboard Access, or VoiceOver running), and read and moved by VoiceOver
//! as a splitter. The table's own column resizing stays off: the card sets
//! the table columns' widths, and saves the user's in the settings.
//!
//! For VoiceOver ([`crate::accessibility`]) each cell names its column and
//! its text reads in words (set with the text, see `WinController`'s
//! `cell_view`), so a row reads its cells in order, and the table,
//! labelled "Results", reports the result count as its row count. The
//! glyphs, the column titles' texts and chevrons and the syntax examples'
//! tokens are hidden: the cells and the click targets say what they show.

use std::cell::{Cell, OnceCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{
    AnyThread, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    sel,
};
use objc2_app_kit::{
    NSAccessibility, NSAccessibilityOrientation, NSAccessibilitySplitterRole, NSApplication,
    NSAutoresizingMaskOptions, NSBackgroundStyle, NSBox, NSBoxType, NSClipView, NSColor,
    NSControlSize, NSCursor, NSEvent, NSEventModifierFlags, NSFocusRingType, NSImageScaling,
    NSImageView, NSResponder, NSScrollElasticity, NSScrollView, NSScroller, NSScrollerStyle,
    NSTableCellView, NSTableColumn, NSTableRowView, NSTableView, NSTableViewStyle, NSTextAlignment,
    NSTextField, NSTitlePosition, NSTrackingArea, NSTrackingAreaOptions,
    NSUserInterfaceItemIdentification, NSView, NSWorkspace,
};
use objc2_foundation::{
    NSEdgeInsets, NSNotification, NSNotificationCenter, NSNumber, NSPoint, NSRect, NSSize, NSString,
};

use super::classes::{ResultsTableView, with_controller};
use super::drag_operation;
use super::views::{
    FlippedView, Flow, HitButton, SurfaceView, clip_to_radius, color, glyph, hide_from_voiceover,
    label, place, place_line, place_text, set_label, set_text, text_height, text_width,
    wrapping_label,
};
use crate::accessibility::column_label;
use crate::column_sizes::{self, ColumnSizes, Drag, RESIZABLE, Step, divider_token};
use crate::columns::{Column, SortState};
use crate::main_layout::{CardLayout, Rect, card, empty_content_width, empty_state};
use crate::settings_view::layout::FOCUS_OUTLINE;
use crate::surface::Surface;
use crate::theme::{RADIUS_LG, RADIUS_SM, Token, metrics, styles};
use crate::ui_text::{
    NO_MATCHES_TEXT, REGEX_ERROR_TEXT, REGEX_ERROR_TITLE, RESIZE_HINT, RESULTS_LABEL,
    SYNTAX_EXAMPLES, SYNTAX_LABEL, no_matches_title, resize_label, sort_label,
};
use crate::window_view::{Body, glyph_tint, header_text, is_numeric, row_surface, sort_glyph};

/// Tags of the column headers' click targets: this plus the column's index.
pub(crate) const HEADER_TAG: isize = 200;
/// Tags of the syntax examples' click targets: this plus the example's
/// index.
pub(crate) const EXAMPLE_TAG: isize = 300;
/// The identifier of reused row views.
const ROW_IDENTIFIER: &str = "fx-row";

/// The rows' scroll view's inset at the top and bottom: the 8-point
/// padding round the rows, less the half of the 2-point gap the table puts
/// above the first row and below the last.
const ROWS_INSET_Y: f64 = metrics::ROWS_INSET - metrics::ROW_GAP / 2.0;

/// The width a legacy vertical scroller takes from the rows' scroll view
/// (macOS shows legacy scrollers when a mouse is connected or "Show scroll
/// bars" is Always), or 0 for overlay scrollers, which float over the rows.
///
/// The rows give up this width even while the scroller hides because every
/// row fits, so their columns don't move when the result count changes.
/// It follows the system's preferred style, which the scroll view follows
/// too; the window lays out again when that style changes.
fn legacy_scroller_width(scroll: &NSScrollView) -> f64 {
    let mtm = scroll.mtm();
    if NSScroller::preferredScrollerStyle(mtm) != NSScrollerStyle::Legacy {
        return 0.0;
    }
    NSScroller::scrollerWidthForControlSize_scrollerStyle(
        NSControlSize::Regular,
        NSScrollerStyle::Legacy,
        mtm,
    )
}

// ------------------------------------------------------------- row view

/// Instance variables of [`ResultRowView`].
#[derive(Default)]
pub(crate) struct RowIvars {
    surface: OnceCell<Retained<SurfaceView>>,
    hovered: Cell<bool>,
}

define_class!(
    // SAFETY: NSTableRowView may be subclassed; the overrides draw nothing
    // or call super first.
    #[unsafe(super(NSTableRowView, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FxResultRowView"]
    #[ivars = RowIvars]
    pub(crate) struct ResultRowView;

    impl ResultRowView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        /// The row draws nothing itself: its surface is a child view's
        /// layer, so the row has no backing store to fill.
        #[unsafe(method(wantsUpdateLayer))]
        fn wants_update_layer(&self) -> bool {
            true
        }

        #[unsafe(method(updateLayer))]
        fn update_layer(&self) {}

        #[unsafe(method(drawBackgroundInRect:))]
        fn draw_background(&self, _rect: NSRect) {}

        #[unsafe(method(drawSelectionInRect:))]
        fn draw_selection(&self, _rect: NSRect) {}

        #[unsafe(method(drawSeparatorInRect:))]
        fn draw_separator(&self, _rect: NSRect) {}

        #[unsafe(method(setSelected:))]
        fn set_selected(&self, selected: bool) {
            // SAFETY: `setSelected:` takes a BOOL and returns void.
            let _: () = unsafe { msg_send![super(self), setSelected: selected] };
            self.refresh();
        }

        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, size: NSSize) {
            // SAFETY: `setFrameSize:` takes an NSSize and returns void.
            let _: () = unsafe { msg_send![super(self), setFrameSize: size] };
            self.refresh();
        }
    }
);

impl ResultRowView {
    fn new(mtm: MainThreadMarker) -> Retained<ResultRowView> {
        let this = ResultRowView::alloc(mtm).set_ivars(RowIvars::default());
        // SAFETY: `initWithFrame:` is NSView's designated initializer.
        let row: Retained<ResultRowView> =
            unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] };
        row.setIdentifier(Some(&NSString::from_str(ROW_IDENTIFIER)));
        row.setFocusRingType(NSFocusRingType::None);
        let surface = SurfaceView::new(mtm);
        row.addSubview_positioned_relativeTo(
            &surface,
            objc2_app_kit::NSWindowOrderingMode::Below,
            None,
        );
        let _ = row.ivars().surface.set(surface);
        row
    }

    /// A reused row view of `table`, or a new one.
    pub(crate) fn make(table: &NSTableView) -> Retained<ResultRowView> {
        // SAFETY: a nil owner is allowed; the identifier is a valid NSString.
        let reused = unsafe {
            table.makeViewWithIdentifier_owner(&NSString::from_str(ROW_IDENTIFIER), None)
        };
        match reused.and_then(|v| v.downcast::<ResultRowView>().ok()) {
            Some(row) => row,
            None => ResultRowView::new(MainThreadMarker::from(table)),
        }
    }

    /// Shows the pointer over the row, or not.
    pub(crate) fn set_hovered(&self, hovered: bool) {
        if self.ivars().hovered.replace(hovered) != hovered {
            self.refresh();
        }
    }

    /// Shows the row's surface for its state, and tints the Name cell's
    /// glyph.
    pub(crate) fn refresh(&self) {
        let selected = self.isSelected();
        let Some(surface) = self.ivars().surface.get() else {
            return;
        };
        let b = self.bounds();
        let h = metrics::ROW_HEIGHT;
        surface.show(
            row_surface(selected, self.ivars().hovered.get()),
            Rect::new(0.0, (b.size.height - h) / 2.0, b.size.width, h),
        );
        // A row view not yet in the table has no columns, and asking it for
        // one raises.
        if self.numberOfColumns() == 0 {
            return;
        }
        if let Some(cell) = self
            .viewAtColumn(0)
            .and_then(|v| v.downcast::<ResultCell>().ok())
        {
            cell.set_selected(selected);
        }
    }
}

// ------------------------------------------------------------------ cells

/// Instance variables of [`ResultCell`].
#[derive(Default)]
pub(crate) struct CellIvars {
    glyph: OnceCell<Retained<NSImageView>>,
}

define_class!(
    // SAFETY: NSTableCellView may be subclassed; the override passes a
    // valid style to super.
    #[unsafe(super(NSTableCellView, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FxResultCell"]
    #[ivars = CellIvars]
    pub(crate) struct ResultCell;

    impl ResultCell {
        /// The design's colours stay on a selected row: the emphasized
        /// style AppKit sets would turn the text white.
        #[unsafe(method(setBackgroundStyle:))]
        fn set_background_style(&self, _style: NSBackgroundStyle) {
            // SAFETY: `setBackgroundStyle:` takes an NSBackgroundStyle and
            // returns void.
            let _: () =
                unsafe { msg_send![super(self), setBackgroundStyle: NSBackgroundStyle::Normal] };
        }
    }
);

impl ResultCell {
    /// A cell for `column` in a row `height` points tall: a label centred
    /// in the row's 20-point line, after an 18-point glyph in the Name
    /// column.
    fn new(mtm: MainThreadMarker, column: Column, height: f64) -> Retained<ResultCell> {
        let this = ResultCell::alloc(mtm).set_ivars(CellIvars::default());
        let frame = NSRect::new(
            objc2_foundation::NSPoint::new(0.0, 0.0),
            NSSize::new(200.0, height),
        );
        // SAFETY: `initWithFrame:` is NSView's designated initializer.
        let cell: Retained<ResultCell> = unsafe { msg_send![super(this), initWithFrame: frame] };
        cell.setIdentifier(Some(&NSString::from_str(column.identifier())));
        // Moving cell by cell inside a row, VoiceOver names the column. The
        // label goes on the cell, not its text, which keeps its value.
        set_label(&cell, column_label(column));
        let text = label(mtm);
        let (font, align) = match column {
            Column::Name => (&styles::ROW_NAME, NSTextAlignment::Left),
            Column::Path => (&styles::ROW_TEXT, NSTextAlignment::Left),
            Column::Size => (&styles::ROW_FIGURES, NSTextAlignment::Right),
            Column::Modified => (&styles::ROW_FIGURES, NSTextAlignment::Left),
        };
        text.setFont(Some(&super::paint::font(font)));
        text.setAlignment(align);
        let mut x = 0.0;
        if column == Column::Name {
            let g = metrics::GLYPH_SIZE;
            let image = NSImageView::new(mtm);
            image.setImageScaling(NSImageScaling::ScaleNone);
            image.setFrame(NSRect::new(
                objc2_foundation::NSPoint::new(0.0, ((height - g) / 2.0).round()),
                NSSize::new(g, g),
            ));
            // The name's text says "folder"; an unlabelled image would read
            // "image".
            hide_from_voiceover(&image);
            cell.addSubview(&image);
            let _ = cell.ivars().glyph.set(image);
            x = g + metrics::GLYPH_GAP;
        }
        place_line(&text, x, (height - 20.0) / 2.0, 200.0 - x, 20.0);
        text.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
        cell.addSubview(&text);
        // SAFETY: the outlet is set to a label that is also a subview of
        // the cell, so the cell keeps it alive.
        unsafe { cell.setTextField(Some(&text)) };
        cell
    }

    /// A reused (or new) cell of `table` for `column`.
    pub(crate) fn make(table: &NSTableView, column: Column) -> Retained<ResultCell> {
        let ident = NSString::from_str(column.identifier());
        // SAFETY: a nil owner is allowed; the identifier is a valid NSString.
        let reused = unsafe { table.makeViewWithIdentifier_owner(&ident, None) };
        match reused.and_then(|v| v.downcast::<ResultCell>().ok()) {
            Some(cell) => cell,
            None => ResultCell::new(MainThreadMarker::from(table), column, table.rowHeight()),
        }
    }

    /// The cell's label.
    pub(crate) fn text(&self) -> Option<Retained<NSTextField>> {
        // SAFETY: reading the outlet `new` set.
        unsafe { self.textField() }
    }

    /// Shows `image` as the Name cell's glyph.
    pub(crate) fn set_glyph(&self, image: &objc2_app_kit::NSImage) {
        if let Some(view) = self.ivars().glyph.get() {
            view.setImage(Some(image));
        }
    }

    /// Tints the Name cell's glyph for a selected row or not.
    pub(crate) fn set_selected(&self, selected: bool) {
        if let Some(view) = self.ivars().glyph.get() {
            view.setContentTintColor(Some(&color(glyph_tint(selected))));
        }
    }
}

// ----------------------------------------------------------------- header

/// One column title: its label, chevron and click target.
struct HeaderColumn {
    column: Column,
    label: Retained<NSTextField>,
    chevron: Retained<NSImageView>,
    button: Retained<HitButton>,
}

define_class!(
    // SAFETY: NSView may be subclassed; `isFlipped` keeps its contract and
    // the notification method only moves the view.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FxHeaderStrip"]
    pub(crate) struct HeaderStrip;

    impl HeaderStrip {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        /// The rows' clip view moved (`NSViewBoundsDidChangeNotification`):
        /// the titles follow it sideways.
        #[unsafe(method(rowsScrolled:))]
        fn rows_scrolled(&self, note: &NSNotification) {
            if let Some(clip) = note.object().and_then(|o| o.downcast::<NSClipView>().ok()) {
                self.follow(clip.bounds().origin.x);
            }
        }
    }
);

impl HeaderStrip {
    /// The header's content: the column titles and dividers, as wide as
    /// the rows' table, moved sideways with it.
    fn new(mtm: MainThreadMarker) -> Retained<HeaderStrip> {
        let this = HeaderStrip::alloc(mtm).set_ivars(());
        // SAFETY: `initWithFrame:` is NSView's designated initializer.
        let view: Retained<HeaderStrip> =
            unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] };
        view.setWantsLayer(true);
        view
    }

    /// Shows the titles over the rows scrolled `x` points sideways.
    fn follow(&self, x: f64) {
        let origin = NSPoint::new(-x, 0.0);
        if self.frame().origin != origin {
            self.setFrameOrigin(origin);
        }
    }
}

/// The cursor over a divider: macOS 15's column-resize cursor, or the
/// left-and-right one before it (deprecated since, so used only there).
fn resize_cursor() -> Retained<NSCursor> {
    // SAFETY: `respondsToSelector:` sent to the class object asks about
    // its class methods; it takes a selector and returns a BOOL.
    let new: bool =
        unsafe { msg_send![NSCursor::class(), respondsToSelector: sel!(columnResizeCursor)] };
    if new {
        NSCursor::columnResizeCursor()
    } else {
        left_right_cursor()
    }
}

/// The left-and-right cursor, for macOS 13 and 14, which lack the
/// column-resize one that replaced it.
#[allow(deprecated)]
fn left_right_cursor() -> Retained<NSCursor> {
    NSCursor::resizeLeftRightCursor()
}

/// Whether a divider takes the keyboard focus, as a button does: with
/// Full Keyboard Access on, or while VoiceOver runs.
fn keyboard_focusable(mtm: MainThreadMarker) -> bool {
    if NSApplication::sharedApplication(mtm).isFullKeyboardAccessEnabled() {
        return true;
    }
    let workspace = NSWorkspace::sharedWorkspace();
    // SAFETY: `-[NSWorkspace isVoiceOverEnabled]` (macOS 10.13, below the
    // app's 13.0) takes nothing and returns a BOOL.
    unsafe { msg_send![&*workspace, isVoiceOverEnabled] }
}

/// Runs `f` on the window's results card, if the window is built.
fn with_card(f: impl FnOnce(&ResultsCard)) {
    with_controller(|c| {
        if let Some(ui) = c.ui() {
            f(&ui.card);
        }
    });
}

/// Instance variables of [`ColumnDivider`].
#[derive(Default)]
pub(crate) struct DividerIvars {
    /// The column on its left: 0 Name, 1 Path, 2 Size.
    index: Cell<usize>,
    hovered: Cell<bool>,
    focused: Cell<bool>,
}

define_class!(
    // SAFETY: NSView may be subclassed; the responder overrides call super
    // (or handle the event themselves) and keep their return contracts.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FxColumnDivider"]
    #[ivars = DividerIvars]
    pub(crate) struct ColumnDivider;

    impl ColumnDivider {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            keyboard_focusable(self.mtm())
        }

        #[unsafe(method(becomeFirstResponder))]
        fn become_first_responder(&self) -> bool {
            // SAFETY: `becomeFirstResponder` takes nothing and returns BOOL.
            let ok: bool = unsafe { msg_send![super(self), becomeFirstResponder] };
            if ok {
                self.ivars().focused.set(true);
                with_card(|c| c.layout_dividers());
            }
            ok
        }

        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            // SAFETY: `resignFirstResponder` takes nothing and returns BOOL.
            let ok: bool = unsafe { msg_send![super(self), resignFirstResponder] };
            if ok {
                self.ivars().focused.set(false);
                with_card(|c| c.layout_dividers());
            }
            ok
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _event: &NSEvent) {
            self.ivars().hovered.set(true);
            with_card(|c| c.layout_dividers());
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            self.ivars().hovered.set(false);
            with_card(|c| c.layout_dividers());
        }

        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            self.addCursorRect_cursor(self.bounds(), &resize_cursor());
        }

        /// A press starts a drag; a double-click restores the default
        /// layout.
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let i = self.ivars().index.get();
            if event.clickCount() >= 2 {
                with_card(|c| c.reset_columns());
            } else {
                let x = event.locationInWindow().x;
                with_card(|c| c.begin_drag(i, x));
            }
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            // Kept while the pointer leaves the divider's cursor rect.
            resize_cursor().set();
            let x = event.locationInWindow().x;
            with_card(|c| c.drag_to(x));
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            // No enter or exit events come during a drag: look where the
            // pointer is now.
            let at = self.convertPoint_fromView(event.locationInWindow(), None);
            let b = self.bounds();
            let inside = at.x >= b.origin.x
                && at.x < b.origin.x + b.size.width
                && at.y >= b.origin.y
                && at.y < b.origin.y + b.size.height;
            self.ivars().hovered.set(inside);
            with_card(|c| c.end_drag());
        }

        /// Left and Right (with Shift, four times as far) move the divider;
        /// every other key goes on up the responder chain.
        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let flags = event.modifierFlags();
            let others = NSEventModifierFlags::Command
                | NSEventModifierFlags::Control
                | NSEventModifierFlags::Option;
            let step = match event.keyCode() {
                123 => Some(Step::Narrower),
                124 => Some(Step::Wider),
                _ => None,
            };
            match step.filter(|_| !flags.intersects(others)) {
                Some(step) => {
                    let i = self.ivars().index.get();
                    let large = flags.contains(NSEventModifierFlags::Shift);
                    with_card(|c| c.step_divider(i, step, large));
                }
                // SAFETY: `keyDown:` takes one NSEvent and returns void.
                None => unsafe { msg_send![super(self), keyDown: event] },
            }
        }

        /// VoiceOver's increment: as Right.
        #[unsafe(method(accessibilityPerformIncrement))]
        fn accessibility_increment(&self) -> bool {
            let i = self.ivars().index.get();
            with_card(|c| c.step_divider(i, Step::Wider, false));
            true
        }

        /// VoiceOver's decrement: as Left.
        #[unsafe(method(accessibilityPerformDecrement))]
        fn accessibility_decrement(&self) -> bool {
            let i = self.ivars().index.get();
            with_card(|c| c.step_divider(i, Step::Narrower, false));
            true
        }
    }
);

impl ColumnDivider {
    /// The divider after column `index`, titled `title`: a splitter for
    /// VoiceOver, with the board's label and tooltip.
    fn new(mtm: MainThreadMarker, index: usize, title: &str) -> Retained<ColumnDivider> {
        let this = ColumnDivider::alloc(mtm).set_ivars(DividerIvars {
            index: Cell::new(index),
            ..DividerIvars::default()
        });
        // SAFETY: `initWithFrame:` is NSView's designated initializer.
        let view: Retained<ColumnDivider> =
            unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] };
        let hint = NSString::from_str(RESIZE_HINT);
        view.setToolTip(Some(&hint));
        view.setAccessibilityElement(true);
        // SAFETY: the role is an immutable framework constant.
        view.setAccessibilityRole(Some(unsafe { NSAccessibilitySplitterRole }));
        view.setAccessibilityOrientation(NSAccessibilityOrientation::Vertical);
        set_label(&view, &resize_label(title));
        view.setAccessibilityHelp(Some(&hint));
        let options = NSTrackingAreaOptions::MouseEnteredAndExited
            | NSTrackingAreaOptions::ActiveInActiveApp
            | NSTrackingAreaOptions::InVisibleRect;
        // SAFETY: the owner is the divider, which keeps the area; no user
        // info is passed.
        let area = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(
                NSTrackingArea::alloc(),
                NSRect::ZERO,
                options,
                Some(&view),
                None,
            )
        };
        view.addTrackingArea(&area);
        view
    }

    /// Whether the pointer is over it or it has the keyboard focus.
    fn hovered_or_focused(&self) -> bool {
        self.ivars().hovered.get() || self.ivars().focused.get()
    }

    /// Tells VoiceOver the column's width in points.
    fn set_width(&self, width: f64) {
        let number = NSNumber::new_f64(width.round());
        let value: &AnyObject = &number;
        // SAFETY: an accessibility value may be any object; an NSNumber is
        // what a splitter's position is.
        unsafe { self.setAccessibilityValue(Some(value)) };
    }
}

/// A divider's views: what the pointer drags, its line and its focus
/// outline.
struct DividerViews {
    view: Retained<ColumnDivider>,
    line: Retained<NSBox>,
    ring: Retained<NSBox>,
}

/// A plain custom box (a line or an outline) for a divider.
fn divider_box(mtm: MainThreadMarker) -> Retained<NSBox> {
    let b = NSBox::initWithFrame(NSBox::alloc(mtm), NSRect::ZERO);
    b.setBoxType(NSBoxType::Custom);
    b.setBorderWidth(0.0);
    b.setTitlePosition(NSTitlePosition::NoTitle);
    b
}

/// `rect` moved so its left edge falls on a pixel of a `scale` screen, so
/// a one-point line is drawn sharp.
fn on_pixel(rect: Rect, scale: f64) -> Rect {
    Rect {
        x: (rect.x * scale).round() / scale,
        ..rect
    }
}

/// A divider drag in progress: the drag, the pointer's x in the window
/// where it started, and the sizes before it.
#[derive(Clone, Copy)]
struct Dragging {
    drag: Drag,
    start_x: f64,
    before: Option<ColumnSizes>,
}

// ----------------------------------------------------------- empty state

/// One syntax example: its token and what it finds.
struct Example {
    surface: Retained<SurfaceView>,
    token: Retained<NSTextField>,
    button: Retained<HitButton>,
    meaning: Retained<NSTextField>,
}

/// "No matches" and the invalid regular expression, in a scroll view of
/// their own over the rows.
struct EmptyState {
    scroll: Retained<NSScrollView>,
    doc: Retained<FlippedView>,
    title: Retained<NSTextField>,
    text: Retained<NSTextField>,
    label: Retained<NSTextField>,
    examples: Vec<Example>,
}

impl EmptyState {
    fn new(mtm: MainThreadMarker, target: &AnyObject) -> EmptyState {
        let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), NSRect::ZERO);
        scroll.setDrawsBackground(false);
        scroll.setHasVerticalScroller(true);
        scroll.setAutohidesScrollers(true);
        let doc = FlippedView::new(mtm);
        scroll.setDocumentView(Some(&doc));
        let title = wrapping_label(mtm);
        let text = wrapping_label(mtm);
        let syntax = label(mtm);
        doc.addSubview(&title);
        doc.addSubview(&text);
        doc.addSubview(&syntax);
        let examples = SYNTAX_EXAMPLES
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let surface = SurfaceView::new(mtm);
                let token = label(mtm);
                // Its button reads "Search for ext:pdf".
                hide_from_voiceover(&token);
                let meaning = label(mtm);
                let button = HitButton::new(
                    mtm,
                    target,
                    sel!(exampleClicked:),
                    EXAMPLE_TAG + i as isize,
                    &format!("Search for {}", e.query),
                    false,
                    true,
                );
                doc.addSubview(&surface);
                doc.addSubview(&token);
                doc.addSubview(&button);
                doc.addSubview(&meaning);
                Example {
                    surface,
                    token,
                    button,
                    meaning,
                }
            })
            .collect();
        scroll.setHidden(true);
        EmptyState {
            scroll,
            doc,
            title,
            text,
            label: syntax,
            examples,
        }
    }

    /// Shows `body` for `query` in a `width` × `height` area, or hides.
    fn show(&self, body: Body, query: &str, width: f64, height: f64) {
        let examples = match body {
            Body::NoMatches => true,
            Body::InvalidRegex => false,
            Body::Rows | Body::Blank => {
                self.scroll.setHidden(true);
                return;
            }
        };
        let (title, text, text_style, text_token) = if examples {
            (
                no_matches_title(query),
                NO_MATCHES_TEXT,
                &styles::EMPTY_TEXT,
                Token::Muted,
            )
        } else {
            (
                REGEX_ERROR_TITLE.to_owned(),
                REGEX_ERROR_TEXT,
                &styles::REGEX_ERROR,
                Token::DangerInk,
            )
        };
        set_text(
            &self.title,
            &title,
            &styles::EMPTY_TITLE,
            Token::InkStrong,
            Flow::Wrap,
        );
        set_text(&self.text, text, text_style, text_token, Flow::Wrap);
        let cw = empty_content_width(width);
        let title_h = text_height(&self.title, cw);
        let text_h = text_height(&self.text, cw);
        let widths: Vec<f64> = if examples {
            self.examples
                .iter()
                .map(|e| text_width(&e.token) + 2.0 * metrics::CODE_PADDING_X)
                .collect()
        } else {
            Vec::new()
        };
        let l = empty_state(width, title_h, text_h, &widths);
        place_text(&self.title, l.title);
        place_text(&self.text, l.text);
        self.label.setHidden(l.label.is_none());
        if let Some(r) = l.label {
            place_line(&self.label, r.x, r.y, r.width, r.height);
        }
        for (i, e) in self.examples.iter().enumerate() {
            let shown = examples;
            e.surface.setHidden(!shown);
            e.token.setHidden(!shown);
            e.button.setHidden(!shown);
            e.meaning.setHidden(!shown);
            if !shown {
                continue;
            }
            let t = l.tokens[i];
            e.surface.show(Some(Surface::Code), t);
            place(&e.button, t);
            place_line(&e.token, t.x, t.y, t.width, t.height);
            let m = l.meanings[i];
            place_line(&e.meaning, m.x, m.y, m.width, m.height);
        }
        place(&self.doc, Rect::new(0.0, 0.0, width, l.height.max(height)));
        self.scroll.setHidden(false);
    }

    /// The examples' fixed texts in the current theme.
    fn restyle(&self) {
        set_text(
            &self.label,
            SYNTAX_LABEL,
            &styles::SYNTAX_LABEL,
            Token::GreenInk,
            Flow::Line(NSTextAlignment::Left),
        );
        for (e, ex) in self.examples.iter().zip(SYNTAX_EXAMPLES) {
            set_text(
                &e.token,
                ex.query,
                &styles::SYNTAX_CODE,
                Token::InkStrong,
                Flow::Line(NSTextAlignment::Center),
            );
            set_text(
                &e.meaning,
                ex.meaning,
                &styles::SYNTAX_TEXT,
                Token::Text,
                Flow::Line(NSTextAlignment::Left),
            );
        }
    }
}

// ------------------------------------------------------------------- card

/// The results card's views. See the module documentation.
pub(crate) struct ResultsCard {
    surface: Retained<SurfaceView>,
    clip: Retained<FlippedView>,
    header: Retained<FlippedView>,
    /// The titles and dividers, moved sideways with the rows.
    strip: Retained<HeaderStrip>,
    line: Retained<NSBox>,
    columns: Vec<HeaderColumn>,
    dividers: Vec<DividerViews>,
    /// The rows' scroll view.
    pub(crate) scroll: Retained<NSScrollView>,
    /// The rows.
    pub(crate) table: Retained<ResultsTableView>,
    empty: EmptyState,
    layout: Cell<Option<CardLayout>>,
    body: Cell<Body>,
    /// The sort shown in the header.
    sort: Cell<SortState>,
    /// Where the card is in the window.
    rect: Cell<Rect>,
    /// The widths the user set, or `None` for the default layout.
    sizes: Cell<Option<ColumnSizes>>,
    /// The divider being dragged.
    dragging: Cell<Option<Dragging>>,
}

impl ResultsCard {
    /// Builds the card; `target` (the window controller) is the table's data
    /// source and delegate and receives the header's and examples' clicks.
    pub(crate) fn new(
        mtm: MainThreadMarker,
        controller: &super::classes::WinController,
    ) -> ResultsCard {
        use objc2::runtime::ProtocolObject;
        let target: &AnyObject = controller.as_ref();
        let surface = SurfaceView::new(mtm);
        let clip = FlippedView::new(mtm);
        clip_to_radius(&clip, RADIUS_LG);
        set_label(&clip, RESULTS_LABEL);

        let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), NSRect::ZERO);
        scroll.setDrawsBackground(false);
        scroll.contentView().setDrawsBackground(false);
        scroll.setHasVerticalScroller(true);
        // Turned on while resized columns don't fit (`lay_out_columns`).
        scroll.setHasHorizontalScroller(false);
        scroll.setAutohidesScrollers(true);
        scroll.setAutomaticallyAdjustsContentInsets(false);
        scroll.setContentInsets(NSEdgeInsets {
            top: ROWS_INSET_Y,
            left: 0.0,
            bottom: ROWS_INSET_Y,
            right: 0.0,
        });

        let table = ResultsTableView::new(mtm, NSRect::ZERO);
        table.setStyle(NSTableViewStyle::Plain);
        table.setUsesAutomaticRowHeights(false);
        table.setRowHeight(metrics::ROW_HEIGHT);
        table.setIntercellSpacing(NSSize::new(metrics::COLUMN_GAP, metrics::ROW_GAP));
        table.setUsesAlternatingRowBackgroundColors(false);
        table.setBackgroundColor(&NSColor::clearColor());
        table.setFocusRingType(NSFocusRingType::None);
        // The rows draw the design's selection; AppKit's (in the accent
        // colour) stays off. Row views still learn they are selected.
        table.setSelectionHighlightStyle(objc2_app_kit::NSTableViewSelectionHighlightStyle::None);
        // No system header: the card draws its own column titles.
        // SAFETY: `setHeaderView:` takes an NSTableHeaderView or nil.
        let _: () = unsafe { msg_send![&*table, setHeaderView: None::<&AnyObject>] };
        table.setAllowsMultipleSelection(true);
        table.setAllowsColumnReordering(false);
        table.setAllowsColumnResizing(false);
        table.setColumnAutoresizingStyle(
            objc2_app_kit::NSTableViewColumnAutoresizingStyle::NoColumnAutoresizing,
        );
        for col in Column::ALL {
            let ident = NSString::from_str(col.identifier());
            let column = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &ident);
            column.setTitle(&NSString::from_str(col.title()));
            column.setMinWidth(0.0);
            column.setEditable(false);
            column.setResizingMask(objc2_app_kit::NSTableColumnResizingOptions::empty());
            table.addTableColumn(&column);
        }
        // SAFETY: the controller implements both protocols and outlives the
        // table (it is kept alive until `run` returns).
        unsafe { table.setDataSource(Some(ProtocolObject::from_ref(controller))) };
        // SAFETY: as above.
        unsafe { table.setDelegate(Some(ProtocolObject::from_ref(controller))) };
        // SAFETY: `openClicked:` is implemented by WinController as
        // `-(void)openClicked:(id)sender`; the controller outlives the table.
        unsafe { table.setTarget(Some(target)) };
        // SAFETY: as above.
        unsafe { table.setDoubleAction(Some(sel!(openClicked:))) };
        // Drag-out offers copy and link only, to other apps and within this one.
        table.setDraggingSourceOperationMask_forLocal(drag_operation(), false);
        table.setDraggingSourceOperationMask_forLocal(drag_operation(), true);
        // VoiceOver: "Results, table"; AppKit reports the row count (the
        // result count) and each row's index.
        set_label(&table, RESULTS_LABEL);
        scroll.setDocumentView(Some(&table));

        let header = FlippedView::new(mtm);
        let line = divider_box(mtm);
        header.addSubview(&line);
        let strip = HeaderStrip::new(mtm);
        header.addSubview(&strip);
        // The dividers' lines and outlines go under the titles, and what the
        // pointer drags over them (`.ne-col-handle { z-index: 1 }`).
        let dividers: Vec<DividerViews> = Column::ALL
            .into_iter()
            .take(RESIZABLE)
            .enumerate()
            .map(|(i, column)| {
                let line = divider_box(mtm);
                let ring = divider_box(mtm);
                ring.setHidden(true);
                strip.addSubview(&line);
                strip.addSubview(&ring);
                DividerViews {
                    view: ColumnDivider::new(mtm, i, column.title()),
                    line,
                    ring,
                }
            })
            .collect();
        let columns = Column::ALL
            .into_iter()
            .enumerate()
            .map(|(i, column)| {
                let label = label(mtm);
                let chevron = NSImageView::new(mtm);
                chevron.setImageScaling(NSImageScaling::ScaleNone);
                // The click target reads "Sort by Name, ascending": the
                // title and the chevron say nothing more.
                hide_from_voiceover(&label);
                hide_from_voiceover(&chevron);
                let button = HitButton::new(
                    mtm,
                    target,
                    sel!(headerClicked:),
                    HEADER_TAG + i as isize,
                    &sort_label(column.title(), None),
                    false,
                    false,
                );
                strip.addSubview(&label);
                strip.addSubview(&chevron);
                strip.addSubview(&button);
                HeaderColumn {
                    column,
                    label,
                    chevron,
                    button,
                }
            })
            .collect();

        for d in &dividers {
            strip.addSubview(&d.view);
        }
        // The titles follow the rows sideways.
        let rows_clip = scroll.contentView();
        rows_clip.setPostsBoundsChangedNotifications(true);
        // SAFETY: `rowsScrolled:` is implemented by HeaderStrip as
        // `-(void)rowsScrolled:(NSNotification *)note`; the name is an
        // immutable framework constant; the strip and the clip view live as
        // long as the window, which lives until the app quits, so the
        // observer never outlives them.
        unsafe {
            NSNotificationCenter::defaultCenter().addObserver_selector_name_object(
                &strip,
                sel!(rowsScrolled:),
                Some(objc2_app_kit::NSViewBoundsDidChangeNotification),
                Some(&rows_clip),
            );
        }

        let empty = EmptyState::new(mtm, target);
        clip.addSubview(&scroll);
        clip.addSubview(&empty.scroll);
        clip.addSubview(&header);

        // The resident app starts with the widths it saved.
        let sizes = controller
            .with_resident(|r| r.settings.column_widths)
            .flatten();
        ResultsCard {
            surface,
            clip,
            header,
            strip,
            line,
            columns,
            dividers,
            scroll,
            table,
            empty,
            layout: Cell::new(None),
            body: Cell::new(Body::Blank),
            sort: Cell::new(SortState::default()),
            rect: Cell::new(Rect::default()),
            sizes: Cell::new(sizes),
            dragging: Cell::new(None),
        }
    }

    /// Adds the card to `root`.
    pub(crate) fn attach(&self, root: &NSView) {
        root.addSubview(&self.surface);
        root.addSubview(&self.clip);
    }

    /// Lays the card out at `rect` in the window.
    pub(crate) fn layout(&self, rect: Rect, query: &str) {
        self.surface.show(Some(Surface::Panel), rect);
        place(&self.clip, rect);
        self.rect.set(rect);
        let l = self.lay_out_columns();
        place(&self.header, l.header);
        place(
            &self.line,
            Rect::new(0.0, l.header.bottom() - 1.0, l.header.width, 1.0),
        );
        let inset = metrics::ROWS_INSET;
        place(
            &self.scroll,
            Rect::new(
                inset,
                l.rows.y,
                (l.rows.width - 2.0 * inset).max(0.0),
                l.rows.height,
            ),
        );
        place(&self.empty.scroll, l.rows);
        self.show_body(self.body.get(), query);
    }

    /// Lays the columns out for the card's size and the user's widths:
    /// the table columns, the titles and the dividers.
    fn lay_out_columns(&self) -> CardLayout {
        let rect = self.rect.get();
        let l = card(
            rect.width,
            rect.height,
            legacy_scroller_width(&self.scroll),
            self.sizes.get(),
        );
        self.layout.set(Some(l));
        let columns = self.table.tableColumns();
        for (i, column) in columns.iter().enumerate() {
            let w = l.columns.widths[i];
            if (column.width() - w).abs() > 0.01 {
                column.setWidth(w);
            }
        }
        // A horizontal scroller, and a bounce sideways, only when the
        // columns don't fit: the default layout's fractional widths can
        // make the table a hair wider than the rows, which must not scroll.
        let sideways = l.width > rect.width + 0.5;
        if self.scroll.hasHorizontalScroller() != sideways {
            self.scroll.setHasHorizontalScroller(sideways);
        }
        let elasticity = if sideways {
            NSScrollElasticity::Automatic
        } else {
            NSScrollElasticity::None
        };
        if self.scroll.horizontalScrollElasticity() != elasticity {
            self.scroll.setHorizontalScrollElasticity(elasticity);
        }
        place(
            &self.strip,
            Rect::new(
                -self.scroll.contentView().bounds().origin.x,
                0.0,
                l.width,
                l.header.height,
            ),
        );
        self.layout_header();
        self.layout_dividers();
        l
    }

    /// Places the dividers between the column titles and shows each one's
    /// state: its line green while hovered, focused or dragged, and its
    /// focus outline while it has the keyboard focus.
    fn layout_dividers(&self) {
        let Some(l) = self.layout.get() else { return };
        let scale = self.header.window().map_or(2.0, |w| w.backingScaleFactor());
        let dragged = self.dragging.get().map(|d| d.drag.divider());
        let (outline, offset) = FOCUS_OUTLINE;
        let reach = outline + offset;
        for (i, d) in self.dividers.iter().enumerate() {
            let at = l.dividers[i];
            let active = d.view.hovered_or_focused() || dragged == Some(i);
            place(&d.view, at.hit);
            let line = if active { at.active_line } else { at.line };
            place(&d.line, on_pixel(line, scale));
            d.line
                .setCornerRadius(if active { metrics::DIVIDER_RADIUS } else { 0.0 });
            d.line.setFillColor(&color(divider_token(active)));
            // `:focus-visible { outline: 2px solid var(--green-ink);
            // outline-offset: 2px }` round the 6-point-radius handle.
            let h = at.hit;
            place(
                &d.ring,
                Rect::new(
                    h.x - reach,
                    h.y - reach,
                    h.width + 2.0 * reach,
                    h.height + 2.0 * reach,
                ),
            );
            d.ring.setBorderWidth(outline);
            d.ring.setBorderColor(&color(Token::GreenInk));
            d.ring.setFillColor(&NSColor::clearColor());
            d.ring.setCornerRadius(RADIUS_SM + reach);
            d.ring.setHidden(!d.view.ivars().focused.get());
            d.view.set_width(l.columns.widths[i]);
        }
    }

    /// A press on divider `divider` at `x` in the window starts a drag.
    pub(crate) fn begin_drag(&self, divider: usize, x: f64) {
        let Some(l) = self.layout.get() else { return };
        self.dragging.set(Some(Dragging {
            drag: Drag::new(divider, l.columns.widths),
            start_x: x,
            before: self.sizes.get(),
        }));
        self.layout_dividers();
    }

    /// The pointer moved to `x` in the window during a drag: the column on
    /// the divider's left follows it, in the header and every row.
    pub(crate) fn drag_to(&self, x: f64) {
        let Some(mut d) = self.dragging.get() else {
            return;
        };
        let sizes = d.drag.to(x - d.start_x);
        self.dragging.set(Some(d));
        if sizes.is_some() && sizes != self.sizes.get() {
            self.sizes.set(sizes);
            self.lay_out_columns();
        }
    }

    /// The drag ended: the widths are saved if they changed.
    pub(crate) fn end_drag(&self) {
        let dragging = self.dragging.take();
        // Also after a double-click's second release, which ends no drag:
        // the divider shows whether the pointer is still over it.
        self.layout_dividers();
        if dragging.is_some_and(|d| self.sizes.get() != d.before) {
            self.save_sizes();
        }
    }

    /// Left or Right on divider `divider` (`large` with Shift).
    pub(crate) fn step_divider(&self, divider: usize, step: Step, large: bool) {
        let Some(l) = self.layout.get() else { return };
        let sizes = Some(column_sizes::step(l.columns.widths, divider, step, large));
        if sizes != self.sizes.get() {
            self.sizes.set(sizes);
            self.lay_out_columns();
            self.save_sizes();
        }
    }

    /// A double-click on a divider: the default layout again.
    pub(crate) fn reset_columns(&self) {
        self.dragging.set(None);
        if self.sizes.replace(None).is_some() {
            self.lay_out_columns();
            self.save_sizes();
        } else {
            self.layout_dividers();
        }
    }

    /// A debug script's divider step on divider `i`: mouse events sent
    /// to `window` as the hardware sends them, or an arrow key handed to
    /// the divider.
    #[cfg(debug_assertions)]
    pub(crate) fn debug_divider(
        &self,
        window: &objc2_app_kit::NSWindow,
        i: usize,
        step: super::debug_script::DividerStep,
    ) {
        use super::debug_script::DividerStep;
        use objc2_app_kit::NSEventType;
        let Some(d) = self.dividers.get(i) else {
            return;
        };
        let b = d.view.bounds();
        let mid = NSPoint::new(b.size.width / 2.0, b.size.height / 2.0);
        let at = d.view.convertPoint_toView(mid, None);
        let mouse = |kind: NSEventType, dx: f64, clicks: isize| {
            NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                kind,
                NSPoint::new(at.x + dx, at.y),
                NSEventModifierFlags::empty(),
                0.0,
                window.windowNumber(),
                None,
                0,
                clicks,
                if kind == NSEventType::LeftMouseUp { 0.0 } else { 1.0 },
            )
        };
        let events = match step {
            DividerStep::Drag(dx) => vec![
                mouse(NSEventType::LeftMouseDown, 0.0, 1),
                mouse(NSEventType::LeftMouseDragged, dx / 2.0, 1),
                mouse(NSEventType::LeftMouseDragged, dx, 1),
                mouse(NSEventType::LeftMouseUp, dx, 1),
            ],
            DividerStep::DoubleClick => vec![
                mouse(NSEventType::LeftMouseDown, 0.0, 1),
                mouse(NSEventType::LeftMouseUp, 0.0, 1),
                mouse(NSEventType::LeftMouseDown, 0.0, 2),
                mouse(NSEventType::LeftMouseUp, 0.0, 2),
            ],
            DividerStep::Key { right, shift } => {
                let (ch, code) = if right {
                    ('\u{F703}', 124)
                } else {
                    ('\u{F702}', 123)
                };
                let mut flags = NSEventModifierFlags::NumericPad | NSEventModifierFlags::Function;
                if shift {
                    flags |= NSEventModifierFlags::Shift;
                }
                let text = NSString::from_str(&ch.to_string());
                let key = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
                    NSEventType::KeyDown,
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
                if let Some(key) = key {
                    d.view.keyDown(&key);
                }
                return;
            }
        };
        for event in events.into_iter().flatten() {
            window.sendEvent(&event);
        }
    }

    /// A debug script's sideways scroll of the rows to `x`.
    #[cfg(debug_assertions)]
    pub(crate) fn debug_scroll_x(&self, x: f64) {
        let clip = self.scroll.contentView();
        let y = clip.bounds().origin.y;
        clip.scrollToPoint(NSPoint::new(x, y));
        self.scroll.reflectScrolledClipView(&clip);
    }

    /// Remembers the widths for the next launch (resident app only).
    fn save_sizes(&self) {
        let sizes = self.sizes.get();
        with_controller(|c| {
            c.with_resident(|r| {
                r.settings.column_widths = sizes;
                r.save_settings();
            });
        });
    }

    /// Shows the column titles for `sort`.
    pub(crate) fn set_sort(&self, sort: SortState) {
        self.sort.set(sort);
        self.restyle_header();
    }

    /// The header's colours for the sort and the pointer.
    pub(crate) fn restyle_header(&self) {
        let sort = self.sort.get();
        for h in &self.columns {
            let sorted = (h.column == sort.column).then_some(sort.direction);
            let token = header_text(sorted.is_some(), h.button.hovered());
            let align = if is_numeric(h.column) {
                NSTextAlignment::Right
            } else {
                NSTextAlignment::Left
            };
            set_text(
                &h.label,
                h.column.title(),
                &styles::COLUMN_HEADER,
                token,
                Flow::Line(align),
            );
            match sorted {
                Some(direction) => {
                    h.chevron.setImage(Some(&glyph(
                        sort_glyph(direction),
                        metrics::SORT_GLYPH_SIZE,
                    )));
                    h.chevron.setContentTintColor(Some(&color(token)));
                    h.chevron.setHidden(false);
                }
                None => h.chevron.setHidden(true),
            }
            set_label(&h.button, &sort_label(h.column.title(), sorted));
        }
        self.layout_header();
    }

    /// Places the column titles over their columns: left-aligned, Size
    /// set right; the chevron 4 points after the title.
    fn layout_header(&self) {
        let Some(l) = self.layout.get() else { return };
        let line_y = (metrics::HEADER_HEIGHT - 16.0) / 2.0;
        let g = metrics::SORT_GLYPH_SIZE;
        for (i, h) in self.columns.iter().enumerate() {
            let x = l.columns.x[i];
            let w = l.columns.widths[i];
            let text_w = text_width(&h.label).min(w);
            let chevron_w = if h.chevron.isHidden() { 0.0 } else { 4.0 + g };
            let group = text_w + chevron_w;
            let left = if is_numeric(h.column) {
                x + w - group
            } else {
                x
            };
            place_line(&h.label, left, line_y, text_w, 16.0);
            place(
                &h.chevron,
                Rect::new(
                    left + text_w + 4.0,
                    (metrics::HEADER_HEIGHT - g) / 2.0,
                    g,
                    g,
                ),
            );
            // `.ne-th { margin: 0 -6px; padding: 4px 6px }`: the click target
            // reaches 6 points past the title on each side.
            place(
                &h.button,
                Rect::new(left - 6.0, line_y - 4.0, group + 12.0, 24.0),
            );
        }
    }

    /// Shows `body` under the header: the rows, or an empty state for
    /// `query`.
    pub(crate) fn show_body(&self, body: Body, query: &str) {
        // The rows stay shown (with none, they draw nothing) under an empty
        // state, so a page without rows is still drawn by the table, as the
        // gate probe's keystroke timing expects.
        self.body.set(body);
        if let Some(l) = self.layout.get() {
            self.empty
                .show(body, query.trim(), l.rows.width, l.rows.height);
        }
    }

    /// Sets the card's fixed texts and colours for the current theme.
    pub(crate) fn restyle(&self) {
        self.line.setFillColor(&color(Token::Line));
        self.empty.restyle();
        self.restyle_header();
        self.layout_dividers();
    }

    /// The click target of `column`'s title, which the re-sort probe sends
    /// the action of, as a click does.
    #[cfg(feature = "measure")]
    pub(crate) fn header_target(&self, column: Column) -> Option<&objc2_app_kit::NSControl> {
        self.columns
            .iter()
            .find(|h| h.column == column)
            .map(|h| &***h.button)
    }

    /// The column of a header click target's `tag`.
    pub(crate) fn column_for_tag(tag: isize) -> Option<Column> {
        usize::try_from(tag - HEADER_TAG)
            .ok()
            .and_then(|i| Column::ALL.into_iter().nth(i))
    }

    /// The syntax example of a click target's `tag`.
    pub(crate) fn example_for_tag(tag: isize) -> Option<&'static str> {
        usize::try_from(tag - EXAMPLE_TAG)
            .ok()
            .and_then(|i| SYNTAX_EXAMPLES.get(i))
            .map(|e| e.query)
    }
}
