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

use std::cell::{Cell, OnceCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSBackgroundStyle, NSBox, NSBoxType, NSColor, NSControlSize,
    NSFocusRingType, NSImageScaling, NSImageView, NSResponder, NSScrollView, NSScroller,
    NSScrollerStyle, NSTableCellView, NSTableColumn, NSTableRowView, NSTableView, NSTableViewStyle,
    NSTextAlignment, NSTextField, NSTitlePosition, NSUserInterfaceItemIdentification, NSView,
};
use objc2_foundation::{NSEdgeInsets, NSRect, NSSize, NSString};

use super::classes::ResultsTableView;
use super::drag_operation;
use super::views::{
    FlippedView, Flow, HitButton, SurfaceView, clip_to_radius, color, glyph, label, place,
    place_line, place_text, set_label, set_text, text_height, text_width, wrapping_label,
};
use crate::columns::{Column, SortState};
use crate::main_layout::{CardLayout, Rect, card, empty_content_width, empty_state};
use crate::surface::Surface;
use crate::theme::{RADIUS_LG, Token, metrics, styles};
use crate::ui_text::{
    NO_MATCHES_TEXT, REGEX_ERROR_TEXT, REGEX_ERROR_TITLE, RESULTS_LABEL, SYNTAX_EXAMPLES,
    SYNTAX_LABEL, no_matches_title, sort_label,
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
    line: Retained<NSBox>,
    columns: Vec<HeaderColumn>,
    /// The rows' scroll view.
    pub(crate) scroll: Retained<NSScrollView>,
    /// The rows.
    pub(crate) table: Retained<ResultsTableView>,
    empty: EmptyState,
    layout: Cell<Option<CardLayout>>,
    body: Cell<Body>,
    /// The sort shown in the header.
    sort: Cell<SortState>,
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
        scroll.setDocumentView(Some(&table));

        let header = FlippedView::new(mtm);
        let line = NSBox::initWithFrame(NSBox::alloc(mtm), NSRect::ZERO);
        line.setBoxType(NSBoxType::Custom);
        line.setBorderWidth(0.0);
        line.setTitlePosition(NSTitlePosition::NoTitle);
        header.addSubview(&line);
        let columns = Column::ALL
            .into_iter()
            .enumerate()
            .map(|(i, column)| {
                let label = label(mtm);
                let chevron = NSImageView::new(mtm);
                chevron.setImageScaling(NSImageScaling::ScaleNone);
                let button = HitButton::new(
                    mtm,
                    target,
                    sel!(headerClicked:),
                    HEADER_TAG + i as isize,
                    &sort_label(column.title(), None),
                    false,
                    false,
                );
                header.addSubview(&label);
                header.addSubview(&chevron);
                header.addSubview(&button);
                HeaderColumn {
                    column,
                    label,
                    chevron,
                    button,
                }
            })
            .collect();

        let empty = EmptyState::new(mtm, target);
        clip.addSubview(&scroll);
        clip.addSubview(&empty.scroll);
        clip.addSubview(&header);

        ResultsCard {
            surface,
            clip,
            header,
            line,
            columns,
            scroll,
            table,
            empty,
            layout: Cell::new(None),
            body: Cell::new(Body::Blank),
            sort: Cell::new(SortState::default()),
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
        let l = card(rect.width, rect.height, legacy_scroller_width(&self.scroll));
        self.layout.set(Some(l));
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
        let columns = self.table.tableColumns();
        for (i, column) in columns.iter().enumerate() {
            let w = l.columns.widths[i];
            if (column.width() - w).abs() > 0.01 {
                column.setWidth(w);
            }
        }
        place(&self.empty.scroll, l.rows);
        self.layout_header();
        self.show_body(self.body.get(), query);
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
