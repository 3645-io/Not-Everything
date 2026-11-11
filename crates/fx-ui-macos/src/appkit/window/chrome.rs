//! The search window's frame and top bar: the window with a transparent
//! title bar over full-size content ([`SearchWindow`]), its flipped root
//! view on the `ground` that follows the appearance ([`RootView`]), and the
//! top bar ([`TopBar`]): the centred name with a Preview build's channel
//! tag, the search field in its pressed well with the search glyph, × and
//! the design's focus ring, and the Aa / Path / .* chips.
//!
//! The search field stays an `NSSearchField`, so typing, Escape and the
//! field editor behave as before; only its look changes. Its cell
//! ([`SearchFieldCell`]) hides the system's search and cancel buttons
//! (the window draws its own glyph and ×) and gives the text the whole
//! field; the bezel, background and system focus ring are off.

use std::cell::Cell;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject, ProtocolObject};
use objc2::{ClassType, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSActionCell, NSAttributedStringNSExtendedStringDrawing, NSCell, NSControl, NSFocusRingType,
    NSImageScaling, NSImageView, NSResponder, NSSearchField, NSSearchFieldCell,
    NSStringDrawingOptions, NSTextAlignment, NSTextField, NSTextFieldCell, NSView, NSWindow,
    NSWindowButton,
};
use objc2_foundation::{NSRect, NSSize, NSString};

use super::classes::{WinController, with_controller};
use super::settings_controls::TextView;
use super::views::{
    FlippedView, Flow, HitButton, SurfaceView, color, glyph, hide_from_voiceover, place,
    place_line, set_label, set_text, styled, text_width, theme,
};
use crate::build_info::BuildInfo;
use crate::chips::{Chip, QueryOptions, chip_surface, chip_text, chip_width};
use crate::file_glyph::Glyph;
use crate::main_layout::{Rect, TopBarLayout, clear_button, title_row, top_bar};
use crate::settings_view::layout::{Frame, css_baseline};
use crate::surface::Surface;
use crate::theme::{Token, metrics, styles};
use crate::ui_text::{CLEAR_GLYPH, CLEAR_LABEL, OPTIONS_LABEL, SEARCH_LABEL, SEARCH_PLACEHOLDER};

/// Tags of the chips' click targets: this plus the chip's index.
pub(crate) const CHIP_TAG: isize = 100;

/// How far the field's text starts inside the field's frame: the field
/// editor's line padding. The frame starts this much before the design's
/// 42-point text inset.
const FIELD_TEXT_INSET: f64 = 2.0;

/// The clear button's mark (`.search .clear { font-size: 22px; line-height:
/// 1 }`).
const CLEAR_MARK: crate::theme::TypeStyle = crate::theme::TypeStyle {
    size: 22.0,
    line_height: 22.0,
    weight: 400,
    tracking_em: 0.0,
    caps: false,
    tabular: false,
    mono: false,
};

/// The × as the window draws it: [`CLEAR_MARK`] in a line box as tall as
/// its button. TextKit puts the baseline where the text view expects only
/// in a line at least as tall as the font's ascent plus descent (26 points
/// for SF at 22), and drew the × 2.5 points high in the design's 22-point
/// line. The mark is centred by its ink ([`clear_button`]), so the line's
/// height doesn't move it.
const CLEAR_MARK_LINE: crate::theme::TypeStyle = crate::theme::TypeStyle {
    line_height: metrics::CLEAR_SIZE,
    ..CLEAR_MARK
};

/// Where CSS puts `text`'s baseline in a `style` line box
/// ([`css_baseline`]), and the bottom and top of its ink above that
/// baseline as the font draws it: what [`clear_button`] needs to centre a
/// mark by its ink.
fn mark_metrics(text: &str, style: &crate::theme::TypeStyle) -> (f64, (f64, f64)) {
    let font = super::paint::font(style);
    let baseline = css_baseline(style.line_height, font.ascender(), -font.descender());
    // Without the line-fragment option the rectangle is the glyphs' ink,
    // measured from the baseline with y running up.
    let ink = styled(
        text,
        style,
        Token::Muted,
        Flow::Line(NSTextAlignment::Center),
    )
    .boundingRectWithSize_options_context(
        NSSize::new(1.0e6, 1.0e6),
        NSStringDrawingOptions::UsesDeviceMetrics,
        None,
    );
    (baseline, (ink.origin.y, ink.origin.y + ink.size.height))
}

// ---------------------------------------------------------------- window

define_class!(
    // SAFETY: NSWindow may be subclassed; the overrides call super first.
    #[unsafe(super(NSWindow, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FxSearchWindow"]
    pub(crate) struct SearchWindow;

    impl SearchWindow {
        /// Every focus change goes through here: the window redraws the
        /// field's focus ring and the status bar's hints.
        #[unsafe(method(makeFirstResponder:))]
        fn make_first_responder(&self, responder: Option<&NSResponder>) -> bool {
            // SAFETY: `makeFirstResponder:` takes a responder or nil and
            // returns BOOL.
            let made: bool = unsafe { msg_send![super(self), makeFirstResponder: responder] };
            with_controller(|c| c.focus_changed());
            made
        }

        #[unsafe(method(becomeKeyWindow))]
        fn become_key(&self) {
            // SAFETY: `becomeKeyWindow` takes no arguments and returns void.
            let _: () = unsafe { msg_send![super(self), becomeKeyWindow] };
            with_controller(|c| c.focus_changed());
        }

        #[unsafe(method(resignKeyWindow))]
        fn resign_key(&self) {
            // SAFETY: `resignKeyWindow` takes no arguments and returns void.
            let _: () = unsafe { msg_send![super(self), resignKeyWindow] };
            with_controller(|c| c.focus_changed());
        }

        // The Quick Look panel's controller (`QLPreviewPanelController`, an
        // informal protocol the panel looks for in the responder chain).
        // The window answers wherever the focus is in it, so the panel keeps
        // showing the selection while the search field has the focus.

        #[unsafe(method(acceptsPreviewPanelControl:))]
        fn accepts_preview_panel_control(&self, _panel: Option<&AnyObject>) -> bool {
            true
        }

        #[unsafe(method(beginPreviewPanelControl:))]
        fn begin_preview_panel_control(&self, panel: &NSWindow) {
            with_controller(|c| c.begin_preview(panel));
        }

        #[unsafe(method(endPreviewPanelControl:))]
        fn end_preview_panel_control(&self, panel: &NSWindow) {
            with_controller(|c| c.end_preview(panel));
        }
    }
);

impl SearchWindow {
    /// A titled, closable, resizable window with full-size content under a
    /// transparent title bar whose title text is hidden (the title stays
    /// for the Window menu).
    pub(crate) fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<SearchWindow> {
        use objc2_app_kit::{NSBackingStoreType, NSWindowStyleMask, NSWindowTitleVisibility};
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable
            | NSWindowStyleMask::Resizable
            | NSWindowStyleMask::FullSizeContentView;
        let this = SearchWindow::alloc(mtm).set_ivars(());
        // SAFETY: valid frame, style and backing; the window is released
        // only through its Retained handle (see setReleasedWhenClosed).
        let window: Retained<SearchWindow> = unsafe {
            msg_send![
                super(this),
                initWithContentRect: frame,
                styleMask: style,
                backing: NSBackingStoreType::Buffered,
                defer: false
            ]
        };
        // SAFETY: we own the window through a Retained, so AppKit must not
        // also release it on close.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitlebarAppearsTransparent(true);
        window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
        window
    }

    /// The traffic lights' centre, from the content's top, when the window
    /// shows them.
    pub(crate) fn lights_center(&self) -> Option<f64> {
        let close = self.standardWindowButton(NSWindowButton::CloseButton)?;
        if close.isHidden() {
            return None;
        }
        let content = self.contentView()?;
        let in_window = close.convertRect_toView(close.bounds(), None);
        let in_content = content.convertRect_fromView(in_window, None);
        let flipped = content.isFlipped();
        let mid = in_content.origin.y + in_content.size.height / 2.0;
        Some(if flipped {
            mid
        } else {
            content.bounds().size.height - mid
        })
    }
}

// ------------------------------------------------------------- root view

define_class!(
    // SAFETY: NSView may be subclassed; the overrides call super first.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FxRootView"]
    pub(crate) struct RootView;

    impl RootView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        /// The window's parts are laid out from its size.
        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, size: NSSize) {
            // SAFETY: `setFrameSize:` takes an NSSize and returns void.
            let _: () = unsafe { msg_send![super(self), setFrameSize: size] };
            with_controller(|c| c.layout_window());
        }

        /// Light or Dark changed (the system's, or the app's setting).
        #[unsafe(method(viewDidChangeEffectiveAppearance))]
        fn appearance_changed(&self) {
            with_controller(|c| c.theme_changed());
        }

        /// The window moved to a screen of another scale: the shared
        /// images are rendered again for it.
        #[unsafe(method(viewDidChangeBackingProperties))]
        fn backing_changed(&self) {
            with_controller(|c| c.scale_changed());
        }
    }
);

impl RootView {
    /// An empty, layer-backed root view.
    pub(crate) fn new(mtm: MainThreadMarker) -> Retained<RootView> {
        let this = RootView::alloc(mtm).set_ivars(());
        // SAFETY: `initWithFrame:` is NSView's designated initializer.
        let view: Retained<RootView> =
            unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] };
        view.setWantsLayer(true);
        view
    }

    /// Whether the view shows the dark theme.
    pub(crate) fn is_dark(&self) -> bool {
        use objc2_app_kit::NSAppearanceCustomization;
        super::views::is_dark(&self.effectiveAppearance())
    }
}

// ---------------------------------------------------------- search field

define_class!(
    // SAFETY: NSSearchFieldCell may be subclassed; the overrides return
    // rectangles inside the bounds given.
    #[unsafe(super(NSSearchFieldCell, NSTextFieldCell, NSActionCell, NSCell, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FxSearchFieldCell"]
    pub(crate) struct SearchFieldCell;

    impl SearchFieldCell {
        /// No system magnifier: the window draws the design's glyph.
        #[unsafe(method(searchButtonRectForBounds:))]
        fn search_button_rect(&self, _rect: NSRect) -> NSRect {
            NSRect::ZERO
        }

        /// No system cancel button: the window draws the design's ×. The
        /// cell keeps its cancel button, so Escape still clears the text.
        #[unsafe(method(cancelButtonRectForBounds:))]
        fn cancel_button_rect(&self, _rect: NSRect) -> NSRect {
            NSRect::ZERO
        }

        /// The text takes the whole field.
        #[unsafe(method(searchTextRectForBounds:))]
        fn search_text_rect(&self, rect: NSRect) -> NSRect {
            rect
        }

        /// The typed text is drawn and edited where the placeholder is:
        /// the whole field, with no room kept for the system's buttons.
        #[unsafe(method(drawingRectForBounds:))]
        fn drawing_rect(&self, rect: NSRect) -> NSRect {
            rect
        }
    }
);

define_class!(
    // SAFETY: NSSearchField may be subclassed; `cellClass` returns a
    // subclass of NSSearchFieldCell.
    #[unsafe(super(NSSearchField, NSTextField, NSControl, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FxSearchField"]
    pub(crate) struct SearchField;

    impl SearchField {
        #[unsafe(method(cellClass))]
        fn cell_class() -> &'static AnyClass {
            SearchFieldCell::class()
        }
    }
);

impl SearchField {
    fn new(mtm: MainThreadMarker) -> Retained<SearchField> {
        let this = SearchField::alloc(mtm).set_ivars(());
        // SAFETY: `initWithFrame:` is NSView's designated initializer; it
        // makes the cell from `cellClass`.
        let field: Retained<SearchField> =
            unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] };
        // Search on every keystroke, as the owner asked: AppKit's default
        // waits about 200 ms after the last key before it sends the action.
        if let Some(cell) = field
            .cell()
            .and_then(|c| c.downcast::<NSSearchFieldCell>().ok())
        {
            cell.setSendsSearchStringImmediately(true);
        }
        field
    }
}

// ------------------------------------------------------------------ chips

/// One option chip: its surface, label and click target.
struct ChipViews {
    chip: Chip,
    surface: Retained<SurfaceView>,
    label: Retained<NSTextField>,
    button: Retained<HitButton>,
}

/// The channel tag after a Preview build's name. Its text is drawn in a CSS
/// line box, as Settings draws its text: an `NSTextField` label puts the
/// baseline lower than CSS does, which left the tag's text 2 points low.
struct TagViews {
    surface: Retained<SurfaceView>,
    label: Retained<TextView>,
    fill: Surface,
    text: Token,
    title: String,
}

// ---------------------------------------------------------------- top bar

/// The top bar's views. See the module documentation.
pub(crate) struct TopBar {
    surface: Retained<SurfaceView>,
    content: Retained<FlippedView>,
    title: Retained<NSTextField>,
    tag: Option<TagViews>,
    well: Retained<SurfaceView>,
    glyph: Retained<NSImageView>,
    /// The search field.
    pub(crate) search: Retained<SearchField>,
    /// The ×, drawn with a CSS baseline so its ink can be centred (an
    /// `NSTextField` label put it about 6 points high).
    clear_mark: Retained<TextView>,
    clear: Retained<HitButton>,
    chips: Vec<ChipViews>,
    /// The chips' state.
    options: Cell<QueryOptions>,
    /// Whether the field shows its focus ring.
    focused: Cell<bool>,
    /// Where everything went last.
    layout: Cell<Option<TopBarLayout>>,
}

impl TopBar {
    /// Builds the top bar into `root`: the surface first (under the rest of
    /// the window's parts added later) is added by [`TopBar::attach`].
    pub(crate) fn new(
        mtm: MainThreadMarker,
        controller: &WinController,
        options: QueryOptions,
    ) -> TopBar {
        let target: &AnyObject = controller.as_ref();
        let content = FlippedView::new(mtm);
        let title = super::views::label(mtm);
        title.setAlignment(NSTextAlignment::Center);
        content.addSubview(&title);
        let tag = BuildInfo::CURRENT.channel_tag().map(|t| {
            let surface = SurfaceView::new(mtm);
            let label = TextView::new(mtm, true);
            content.addSubview(&surface);
            content.addSubview(&label);
            TagViews {
                surface,
                label,
                fill: if t.fill == Token::Green {
                    Surface::ChannelRc
                } else {
                    Surface::ChannelBeta
                },
                text: t.text,
                title: t.label,
            }
        });

        let well = SurfaceView::new(mtm);
        content.addSubview(&well);
        let glyph =
            NSImageView::imageViewWithImage(&glyph(Glyph::Search, metrics::GLYPH_SIZE), mtm);
        glyph.setImageScaling(NSImageScaling::ScaleNone);
        // The field reads "Search file and folder names"; the magnifier
        // would read "image".
        hide_from_voiceover(&glyph);
        content.addSubview(&glyph);

        let search = SearchField::new(mtm);
        search.setBezeled(false);
        search.setBordered(false);
        search.setDrawsBackground(false);
        search.setFocusRingType(NSFocusRingType::None);
        if let Some(cell) = search.cell() {
            cell.setScrollable(true);
            cell.setWraps(false);
        }
        search.setUsesSingleLineMode(true);
        set_label(&search, SEARCH_LABEL);
        // SAFETY: `search:` is implemented by WinController as
        // `-(void)search:(id)sender`; the controller outlives the window.
        unsafe {
            search.setTarget(Some(target));
            search.setAction(Some(sel!(search:)));
        }
        // SAFETY: the controller implements NSSearchFieldDelegate's text
        // editing methods and outlives the field.
        unsafe { search.setDelegate(Some(ProtocolObject::from_ref(controller))) };
        content.addSubview(&search);

        // The button reads out its label; the mark itself is ignored.
        let clear_mark = TextView::new(mtm, false);
        content.addSubview(&clear_mark);
        let clear = HitButton::new(
            mtm,
            target,
            sel!(clearClicked:),
            0,
            CLEAR_LABEL,
            false,
            false,
        );
        content.addSubview(&clear);

        let chips = Chip::ALL
            .into_iter()
            .map(|chip| {
                let surface = SurfaceView::new(mtm);
                let label = super::views::label(mtm);
                label.setAlignment(NSTextAlignment::Center);
                // The button reads "Match case" and its state; "Aa" would
                // only repeat it, less clearly.
                hide_from_voiceover(&label);
                let button = HitButton::new(
                    mtm,
                    target,
                    sel!(chipClicked:),
                    CHIP_TAG + chip.index() as isize,
                    chip.text().title,
                    true,
                    false,
                );
                button.setToolTip(Some(&NSString::from_str(chip.text().title)));
                content.addSubview(&surface);
                content.addSubview(&label);
                content.addSubview(&button);
                ChipViews {
                    chip,
                    surface,
                    label,
                    button,
                }
            })
            .collect();
        set_label(&content, OPTIONS_LABEL);

        TopBar {
            surface: SurfaceView::new(mtm),
            content,
            title,
            tag,
            well,
            glyph,
            search,
            clear_mark,
            clear,
            chips,
            options: Cell::new(options),
            focused: Cell::new(false),
            layout: Cell::new(None),
        }
    }

    /// Adds the bar's surface and its content to `root`, on top of what
    /// is there: the bar floats over the page (`z-index: 2`).
    pub(crate) fn attach(&self, root: &NSView) {
        root.addSubview(&self.surface);
        root.addSubview(&self.content);
    }

    /// Sets the texts and colours for the current theme and state.
    pub(crate) fn restyle(&self) {
        let center = crate::appkit::window::views::Flow::Line(NSTextAlignment::Center);
        set_text(
            &self.title,
            BuildInfo::CURRENT.app_name(),
            &styles::WINDOW_TITLE,
            Token::InkStrong,
            center,
        );
        if let Some(tag) = &self.tag {
            tag.label.set(
                &tag.title,
                &styles::CHANNEL_TAG,
                theme().color(tag.text),
                NSTextAlignment::Center,
            );
        }
        self.glyph
            .setImage(Some(&glyph(Glyph::Search, metrics::GLYPH_SIZE)));
        self.glyph.setContentTintColor(Some(&color(Token::Muted)));
        self.search
            .setFont(Some(&super::paint::font(&styles::SEARCH_TEXT)));
        self.search.setTextColor(Some(&color(Token::InkStrong)));
        self.search.setPlaceholderAttributedString(Some(&styled(
            SEARCH_PLACEHOLDER,
            &styles::SEARCH_TEXT,
            Token::Muted,
            Flow::Line(NSTextAlignment::Left),
        )));
        self.clear_mark.set(
            CLEAR_GLYPH,
            &CLEAR_MARK_LINE,
            theme().color(Token::Muted),
            NSTextAlignment::Center,
        );
        self.restyle_chips();
    }

    /// The chips' labels for their state and the pointer.
    pub(crate) fn restyle_chips(&self) {
        let options = self.options.get();
        for c in &self.chips {
            let on = c.chip.is_on(options);
            let style = if c.chip.text().mono {
                &styles::TOGGLE_MONO
            } else {
                &styles::TOGGLE
            };
            set_text(
                &c.label,
                c.chip.text().label,
                style,
                chip_text(on, c.button.hovered()),
                Flow::Line(NSTextAlignment::Center),
            );
            c.button.setState(if on { 1 } else { 0 });
        }
    }

    /// Shows the chips' new state.
    pub(crate) fn set_options(&self, options: QueryOptions) {
        self.options.set(options);
        self.restyle_chips();
        self.place_surfaces();
    }

    /// Shows or hides the field's focus ring.
    pub(crate) fn set_focused(&self, focused: bool) {
        if self.focused.replace(focused) != focused {
            self.place_surfaces();
        }
    }

    /// Shows × only while the field holds text.
    pub(crate) fn set_clear_visible(&self, visible: bool) {
        self.clear.setHidden(!visible);
        self.clear_mark.setHidden(!visible);
    }

    /// Lays the bar out across a window `width` points wide, its title row
    /// centred on the traffic lights at `lights` points from the top.
    /// Returns its height.
    pub(crate) fn layout(&self, width: f64, lights: Option<f64>) -> f64 {
        let widths = [0, 1, 2].map(|i| chip_width(text_width(&self.chips[i].label)));
        let l = top_bar(width, lights, widths);
        self.layout.set(Some(l));
        place(&self.content, Rect::new(0.0, 0.0, width, l.height));

        // The title row: the name and the tag, centred together.
        let row = title_row(
            width,
            l.title_row,
            text_width(&self.title),
            self.tag.as_ref().map(|t| t.label.width()),
        );
        let t = row.title;
        place_line(&self.title, t.x, t.y, t.width, t.height);
        if let (Some(tag), Some(tag_rect), Some(text)) = (&self.tag, row.tag, row.tag_text) {
            tag.surface.show(Some(tag.fill), tag_rect);
            tag.label
                .place(Frame::new(text.x, text.y, text.width, text.height));
        }

        // The field: the glyph, the text and ×.
        let s = l.search;
        let g = metrics::GLYPH_SIZE;
        place(
            &self.glyph,
            Rect::new(
                s.x + metrics::SEARCH_GLYPH_X,
                s.y + (s.height - g) / 2.0,
                g,
                g,
            ),
        );
        let text_x = s.x + metrics::SEARCH_TEXT_X - FIELD_TEXT_INSET;
        let text_w = (s.right() - 44.0 - text_x).max(0.0);
        let field_h = self
            .search
            .cell()
            .map_or(22.0, |c| c.cellSize().height.ceil());
        place(
            &self.search,
            Rect::new(
                text_x,
                s.y + ((s.height - field_h) / 2.0).round(),
                text_w,
                field_h,
            ),
        );
        let (baseline, ink) = mark_metrics(CLEAR_GLYPH, &CLEAR_MARK_LINE);
        let clear = clear_button(s, CLEAR_MARK_LINE.line_height, baseline, ink);
        place(&self.clear, clear.button);
        let m = clear.mark;
        self.clear_mark
            .place(Frame::new(m.x, m.y, m.width, m.height));

        for (c, r) in self.chips.iter().zip(l.chips) {
            place(&c.button, r);
            place_line(&c.label, r.x, r.y + (r.height - 20.0) / 2.0, r.width, 20.0);
        }
        self.place_surfaces();
        l.height
    }

    /// Puts the bar's, the field's and the chips' surfaces in place.
    fn place_surfaces(&self) {
        let Some(l) = self.layout.get() else { return };
        let width = self.content.frame().size.width;
        self.surface
            .show(Some(Surface::TopBar), Rect::new(0.0, 0.0, width, l.height));
        let field = if self.focused.get() {
            Surface::FieldFocused
        } else {
            Surface::Field
        };
        self.well.show(Some(field), l.search);
        let options = self.options.get();
        for (c, r) in self.chips.iter().zip(l.chips) {
            c.surface.show(Some(chip_surface(c.chip.is_on(options))), r);
        }
    }

    /// The chip whose click target sent `tag`.
    pub(crate) fn chip_for_tag(tag: isize) -> Option<Chip> {
        usize::try_from(tag - CHIP_TAG)
            .ok()
            .and_then(Chip::from_index)
    }

    /// The field's text.
    pub(crate) fn text(&self) -> String {
        self.search.stringValue().to_string()
    }

    /// Whether the field is being edited (it holds the focus).
    pub(crate) fn editing(&self) -> bool {
        self.search.currentEditor().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::super::settings_controls::laid_out;
    use super::super::surfaces::tests::draw_offscreen;
    use super::*;
    use crate::settings_view::layout::snap_down;
    use crate::theme::Rgb;

    /// The rows of `pixmap` that hold ink (any pixel more than half opaque),
    /// first and last, from the top.
    fn ink_rows(pixmap: &crate::surface::Pixmap) -> Option<(usize, usize)> {
        let row_has_ink =
            |y: usize| (0..pixmap.width).any(|x| pixmap.data[(y * pixmap.width + x) * 4 + 3] > 127);
        let first = (0..pixmap.height).find(|&y| row_has_ink(y))?;
        let last = (0..pixmap.height).rev().find(|&y| row_has_ink(y))?;
        Some((first, last))
    }

    /// The × as the window draws it (laid out as a TextView lays it out,
    /// placed as `TopBar::layout` places it) at 2 and at 1 pixels a point:
    /// its ink's middle is within half a point of the field's middle. It
    /// was once drawn about 6 points high.
    #[test]
    fn the_clear_mark_is_drawn_centred_in_the_field() {
        let field = Rect::new(0.0, 0.0, 120.0, metrics::SEARCH_HEIGHT);
        let (baseline, ink) = mark_metrics(CLEAR_GLYPH, &CLEAR_MARK_LINE);
        let mark = clear_button(field, CLEAR_MARK_LINE.line_height, baseline, ink).mark;
        for scale in [2.0, 1.0] {
            // TextView::place moves the view's top up onto the pixel grid.
            let top = snap_down(mark.y, scale);
            let shift = mark.y - top;
            let text = laid_out(CLEAR_GLYPH, &CLEAR_MARK_LINE, Rgb(0x000000), shift, scale);
            let size = NSSize::new(field.width, field.height);
            let pixmap = draw_offscreen(size, scale, || {
                // The bitmap's y runs up; the text runs down from the
                // rectangle's top, as in the flipped view.
                let height = mark.height + shift;
                let rect = NSRect::new(
                    objc2_foundation::NSPoint::new(mark.x, field.height - top - height),
                    NSSize::new(mark.width, height),
                );
                text.drawWithRect_options_context(
                    rect,
                    NSStringDrawingOptions::UsesLineFragmentOrigin,
                    None,
                );
            });
            let (first, last) = ink_rows(&pixmap).expect("the × draws");
            let middle = (first + last + 1) as f64 / 2.0 / scale;
            let want = field.height / 2.0;
            assert!(
                (middle - want).abs() <= 0.5,
                "at {scale}x the ×'s ink spans rows {first} to {last}: middle {middle}, field middle {want}"
            );
        }
    }
}
