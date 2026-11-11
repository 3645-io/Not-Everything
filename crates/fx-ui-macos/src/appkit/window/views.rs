//! Shared pieces of the search window's AppKit layer: the current theme and
//! the shared surface images ([`theme`], [`surface_image`], [`glyph`]), the
//! view that shows a surface as its layer's contents ([`SurfaceView`]),
//! flipped containers ([`FlippedView`]), transparent click targets with
//! hover and press ([`HitButton`]), and text helpers.
//!
//! Every surface is a [`SurfaceView`]: a layer-backed view whose layer
//! shows the shared nine-part image (`layer.contents` with
//! `contentsCenter`), placed over the element's bounds grown by the outer
//! shadows' reach. It never draws in `drawRect:`; a change of surface,
//! size class or theme sets new contents, and nothing else does, so
//! neither drawing nor scrolling renders a shadow. It ignores clicks, so a shadow reaching
//! over a neighbour never takes its clicks.
//!
//! Main thread only.

use std::cell::Cell;
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua,
    NSAttributedStringNSStringDrawing, NSBaselineOffsetAttributeName, NSButton, NSColor, NSControl,
    NSCursor, NSEvent, NSFontAttributeName, NSForegroundColorAttributeName, NSImage,
    NSKernAttributeName, NSLineBreakMode, NSMutableParagraphStyle, NSParagraphStyleAttributeName,
    NSResponder, NSTextAlignment, NSTextField, NSTrackingArea, NSTrackingAreaOptions, NSView,
    NSViewLayerContentsRedrawPolicy,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSDictionary, NSNumber, NSPoint, NSRect, NSSize, NSString,
};

use super::classes::with_controller;
use super::paint;
use super::settings_controls::Layer;
use super::surfaces::{SurfaceCache, SurfaceImage};
use crate::file_glyph::Glyph;
use crate::main_layout::Rect;
use crate::surface::Surface;
use crate::theme::{Theme, Token, TypeStyle};

// ------------------------------------------------------------- the look

/// The theme the window shows and the images it shares.
struct Look {
    theme: Cell<Theme>,
    cache: SurfaceCache,
}

thread_local! {
    static LOOK: Look = Look {
        theme: Cell::new(Theme::Light),
        // The search window's screen scale is set when it shows
        // (`set_scale`); until then, a Retina screen's.
        cache: SurfaceCache::for_scale(2.0),
    };
}

/// Renders the shared images for a screen of `scale` from now on; returns
/// whether that changed (the images of the old scale are dropped).
pub(crate) fn set_scale(scale: f64) -> bool {
    LOOK.with(|l| l.cache.set_scale(scale))
}

/// The theme the window shows.
pub(crate) fn theme() -> Theme {
    LOOK.with(|l| l.theme.get())
}

/// Shows `theme` from now on; returns whether it changed. The other
/// theme's images are dropped (no view shows them any more).
pub(crate) fn set_theme(theme: Theme) -> bool {
    LOOK.with(|l| {
        let old = l.theme.replace(theme);
        if old != theme {
            l.cache.forget_theme(old);
        }
        old != theme
    })
}

/// `token`'s colour in the current theme.
pub(crate) fn color(token: Token) -> Retained<NSColor> {
    paint::token(theme(), token)
}

/// `surface` in the current theme for an element of `size`.
pub(crate) fn surface_image(surface: Surface, size: NSSize) -> Rc<SurfaceImage> {
    LOOK.with(|l| l.cache.surface(surface, l.theme.get(), size))
}

/// `glyph` as a shared template image `size` points square.
pub(crate) fn glyph(glyph: Glyph, size: f64) -> Retained<NSImage> {
    LOOK.with(|l| l.cache.glyph(glyph, size))
}

/// Whether `appearance` is a dark one.
pub(crate) fn is_dark(appearance: &NSAppearance) -> bool {
    // SAFETY: the appearance names are immutable framework constants.
    let (aqua, dark) = unsafe { (NSAppearanceNameAqua, NSAppearanceNameDarkAqua) };
    appearance
        .bestMatchFromAppearancesWithNames(&NSArray::from_slice(&[aqua, dark]))
        .is_some_and(|best| best.isEqualToString(dark))
}

/// `rect` as an `NSRect` (the views are flipped, so y runs down in both).
pub(crate) fn ns_rect(rect: Rect) -> NSRect {
    NSRect::new(
        NSPoint::new(rect.x, rect.y),
        NSSize::new(rect.width.max(0.0), rect.height.max(0.0)),
    )
}

/// Moves `view` to `rect` in its superview, when it isn't there already.
pub(crate) fn place(view: &NSView, rect: Rect) {
    let frame = ns_rect(rect);
    if view.frame() != frame {
        view.setFrame(frame);
    }
}

/// Rounds the corners of `view`'s layer to `radius` and clips what it
/// holds to them (the results card's content, `overflow: hidden`).
pub(crate) fn clip_to_radius(view: &NSView, radius: f64) {
    view.setWantsLayer(true);
    if let Some(layer) = Layer::of(view) {
        layer.clip_to_radius(radius);
    }
}

// ---------------------------------------------------------- surface view

/// Instance variables of [`SurfaceView`].
#[derive(Default)]
pub(crate) struct SurfaceIvars {
    /// What it shows: the surface, the element's size and the theme.
    shown: Cell<Option<(Surface, NSSize, Theme)>>,
}

define_class!(
    // SAFETY: NSView may be subclassed; the overrides keep their contracts.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FxSurfaceView"]
    #[ivars = SurfaceIvars]
    pub(crate) struct SurfaceView;

    impl SurfaceView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(wantsUpdateLayer))]
        fn wants_update_layer(&self) -> bool {
            true
        }

        #[unsafe(method(updateLayer))]
        fn update_layer(&self) {
            self.set_contents();
        }

        /// Clicks go to whatever is under the surface.
        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, _point: NSPoint) -> Option<Retained<NSView>> {
            None
        }

        /// Another screen scale picks the other representation.
        #[unsafe(method(viewDidChangeBackingProperties))]
        fn backing_changed(&self) {
            self.setNeedsDisplay(true);
        }
    }
);

impl SurfaceView {
    /// A hidden surface view.
    pub(crate) fn new(mtm: MainThreadMarker) -> Retained<SurfaceView> {
        let this = SurfaceView::alloc(mtm).set_ivars(SurfaceIvars::default());
        // SAFETY: `initWithFrame:` is NSView's designated initializer.
        let view: Retained<SurfaceView> =
            unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] };
        view.setWantsLayer(true);
        view.setLayerContentsRedrawPolicy(NSViewLayerContentsRedrawPolicy::OnSetNeedsDisplay);
        view.setHidden(true);
        view
    }

    /// Shows `surface` for an element at `rect` in the superview (flipped),
    /// or nothing for `None`. The view covers `rect` grown by the surface's
    /// outer shadows; new contents are set only when the surface, the
    /// element's size or the theme changed.
    pub(crate) fn show(&self, surface: Option<Surface>, rect: Rect) {
        let Some(surface) = surface else {
            self.setHidden(true);
            return;
        };
        let theme = theme();
        let o = surface.style(theme).outsets();
        place(
            self,
            Rect::new(
                rect.x - o.left,
                rect.y - o.top,
                rect.width + o.left + o.right,
                rect.height + o.top + o.bottom,
            ),
        );
        let size = NSSize::new(rect.width.round(), rect.height.round());
        let shown = Some((surface, size, theme));
        if self.ivars().shown.get() != shown {
            self.ivars().shown.set(shown);
            self.setNeedsDisplay(true);
        }
        self.setHidden(false);
    }

    /// Sets the layer's contents to the shared image.
    fn set_contents(&self) {
        let Some((surface, size, _)) = self.ivars().shown.get() else {
            return;
        };
        if size.width < 1.0 || size.height < 1.0 {
            return;
        }
        let Some(layer) = Layer::of(self) else { return };
        let scale = self.window().map_or(2.0, |w| w.backingScaleFactor());
        set_scale(scale);
        let image = surface_image(surface, size);
        let center = image.contents_center(layer.contents_are_flipped());
        layer.show_image(image.image(), scale, center);
    }
}

// ---------------------------------------------------------- flipped view

define_class!(
    // SAFETY: NSView may be subclassed; `isFlipped` keeps its contract.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FxFlippedView"]
    pub(crate) struct FlippedView;

    impl FlippedView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

impl FlippedView {
    /// An empty layer-backed container whose y runs down.
    pub(crate) fn new(mtm: MainThreadMarker) -> Retained<FlippedView> {
        let this = FlippedView::alloc(mtm).set_ivars(());
        // SAFETY: `initWithFrame:` is NSView's designated initializer.
        let view: Retained<FlippedView> =
            unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] };
        view.setWantsLayer(true);
        view
    }
}

// ------------------------------------------------------------ hit button

/// Instance variables of [`HitButton`].
#[derive(Default)]
pub(crate) struct HitIvars {
    hovered: Cell<bool>,
    pressed: Cell<bool>,
    hand: Cell<bool>,
}

define_class!(
    // SAFETY: NSButton may be subclassed; the overrides call super or only
    // add a cursor rect.
    #[unsafe(super(NSButton, NSControl, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FxHitButton"]
    #[ivars = HitIvars]
    pub(crate) struct HitButton;

    impl HitButton {
        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _event: &NSEvent) {
            self.ivars().hovered.set(true);
            with_controller(|c| c.controls_changed());
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            self.ivars().hovered.set(false);
            with_controller(|c| c.controls_changed());
        }

        /// Pressed while the button tracks the mouse (`:active`).
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.ivars().pressed.set(true);
            with_controller(|c| c.controls_changed());
            // SAFETY: `mouseDown:` takes one NSEvent and returns void; it
            // tracks the mouse until it is released and sends the action.
            let _: () = unsafe { msg_send![super(self), mouseDown: event] };
            self.ivars().pressed.set(false);
            with_controller(|c| c.controls_changed());
        }

        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            if self.ivars().hand.get() {
                self.addCursorRect_cursor(self.bounds(), &NSCursor::pointingHandCursor());
            }
        }
    }
);

impl HitButton {
    /// A transparent button sending `action` to `target` with `tag`,
    /// spoken as `label`; it never takes the keyboard focus. `toggle`
    /// makes it an on/off button for accessibility; `hand` shows the
    /// pointing hand over it.
    pub(crate) fn new(
        mtm: MainThreadMarker,
        target: &AnyObject,
        action: objc2::runtime::Sel,
        tag: isize,
        label: &str,
        toggle: bool,
        hand: bool,
    ) -> Retained<HitButton> {
        let this = HitButton::alloc(mtm).set_ivars(HitIvars {
            hand: Cell::new(hand),
            ..HitIvars::default()
        });
        // SAFETY: `initWithFrame:` is NSView's designated initializer.
        let button: Retained<HitButton> =
            unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] };
        button.setTitle(&NSString::from_str(""));
        button.setBordered(false);
        button.setTransparent(true);
        if toggle {
            button.setButtonType(objc2_app_kit::NSButtonType::PushOnPushOff);
        }
        button.setTag(tag);
        button.setRefusesFirstResponder(true);
        button.setToolTip(None);
        // SAFETY: every action passed is implemented by WinController as
        // `-(void)name:(id)sender`; the controller outlives the window.
        unsafe {
            button.setTarget(Some(target));
            button.setAction(Some(action));
        }
        set_label(&button, label);
        let options = NSTrackingAreaOptions::MouseEnteredAndExited
            | NSTrackingAreaOptions::ActiveInActiveApp
            | NSTrackingAreaOptions::InVisibleRect;
        // SAFETY: the owner is the button, which keeps the area; no user
        // info is passed.
        let area = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(
                NSTrackingArea::alloc(),
                NSRect::ZERO,
                options,
                Some(&button),
                None,
            )
        };
        button.addTrackingArea(&area);
        button
    }

    /// Whether the pointer is over the button.
    pub(crate) fn hovered(&self) -> bool {
        self.ivars().hovered.get()
    }

    /// Whether the button is being pressed.
    pub(crate) fn pressed(&self) -> bool {
        self.ivars().pressed.get()
    }
}

/// Sets the accessibility label of `view`.
pub(crate) fn set_label(view: &NSView, label: &str) {
    use objc2_app_kit::NSAccessibility;
    view.setAccessibilityLabel(Some(&NSString::from_str(label)));
}

/// Keeps `view` out of what VoiceOver reads: a glyph, or a text whose
/// control or row reads it out already. A control (a label, an image view)
/// answers VoiceOver through its cell, so the cell is hidden too.
pub(crate) fn hide_from_voiceover(view: &NSView) {
    use objc2_app_kit::NSAccessibility;
    view.setAccessibilityElement(false);
    if let Some(cell) = view.downcast_ref::<NSControl>().and_then(|c| c.cell()) {
        cell.setAccessibilityElement(false);
    }
}

/// Makes VoiceOver read `label` as `spoken` (its accessibility value)
/// rather than its text: the status bar's " · " separators as commas, key
/// caps in words ([`crate::accessibility`]). The text drawn is unchanged.
/// The label answers VoiceOver through its cell, so the value goes there.
pub(crate) fn set_spoken(label: &NSTextField, spoken: &str) {
    use objc2_app_kit::NSAccessibility;
    let value = NSString::from_str(spoken);
    if let Some(cell) = label.cell() {
        // SAFETY: the value is an NSString, as a text's value is.
        unsafe { cell.setAccessibilityValue(Some(&value)) };
    }
}

/// A result cell's text as VoiceOver reads it: `spoken`, or, for `None`,
/// nothing at all (a folder's "—" size). Set every time a reused cell
/// shows another row, so no row keeps another's reading.
pub(crate) fn set_speech(label: &NSTextField, spoken: Option<&str>) {
    use objc2_app_kit::NSAccessibility;
    let Some(cell) = label.cell() else { return };
    cell.setAccessibilityElement(spoken.is_some());
    if let Some(spoken) = spoken {
        let value = NSString::from_str(spoken);
        // SAFETY: the value is an NSString, as a text's value is.
        unsafe { cell.setAccessibilityValue(Some(&value)) };
    }
}

// ------------------------------------------------------------------ text

/// How a text wraps and aligns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Flow {
    /// One line, cut with "…" at the end.
    Line(NSTextAlignment),
    /// Lines of the style's line height, wrapped at words.
    Wrap,
}

/// The attributes of `style` in `token`'s colour, flowing as `flow`.
pub(crate) fn attributes(
    style: &TypeStyle,
    token: Token,
    flow: Flow,
) -> Retained<NSDictionary<NSString, AnyObject>> {
    let font = paint::font(style);
    let paragraph = NSMutableParagraphStyle::new();
    let mut baseline = 0.0;
    match flow {
        Flow::Line(alignment) => {
            paragraph.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
            paragraph.setAlignment(alignment);
        }
        Flow::Wrap => {
            paragraph.setLineBreakMode(NSLineBreakMode::ByWordWrapping);
            paragraph.setMinimumLineHeight(style.line_height);
            paragraph.setMaximumLineHeight(style.line_height);
            // CSS puts half the leading above the glyphs and half below;
            // AppKit puts all of it above, so lift the glyphs by half.
            let natural = font.ascender() - font.descender();
            baseline = ((style.line_height - natural) / 2.0).max(0.0);
        }
    }
    let color = color(token);
    let kern = NSNumber::new_f64(style.kern());
    let baseline = NSNumber::new_f64(baseline);
    // SAFETY: the attribute names are immutable framework constants.
    let keys = unsafe {
        [
            NSFontAttributeName,
            NSForegroundColorAttributeName,
            NSParagraphStyleAttributeName,
            NSKernAttributeName,
            NSBaselineOffsetAttributeName,
        ]
    };
    let values: [&AnyObject; 5] = [
        font.as_ref(),
        color.as_ref(),
        paragraph.as_ref(),
        kern.as_ref(),
        baseline.as_ref(),
    ];
    NSDictionary::from_slices(&keys, &values)
}

/// `text` in `style` (in capitals when the style says so) and `token`'s
/// colour.
pub(crate) fn styled(
    text: &str,
    style: &TypeStyle,
    token: Token,
    flow: Flow,
) -> Retained<NSAttributedString> {
    let text = if style.caps {
        text.to_uppercase()
    } else {
        text.to_owned()
    };
    // SAFETY: the attributes are valid AppKit text attributes.
    unsafe {
        NSAttributedString::new_with_attributes(
            &NSString::from_str(&text),
            &attributes(style, token, flow),
        )
    }
}

/// A non-editable, non-selectable label without a background.
pub(crate) fn label(mtm: MainThreadMarker) -> Retained<NSTextField> {
    let label = NSTextField::labelWithString(&NSString::from_str(""), mtm);
    label.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    label.setUsesSingleLineMode(true);
    label.setRefusesFirstResponder(true);
    label
}

/// A label that wraps at words.
pub(crate) fn wrapping_label(mtm: MainThreadMarker) -> Retained<NSTextField> {
    let label = NSTextField::wrappingLabelWithString(&NSString::from_str(""), mtm);
    label.setSelectable(false);
    label.setRefusesFirstResponder(true);
    label
}

/// Sets `label` to `text` in `style` and `token`'s colour.
pub(crate) fn set_text(
    label: &NSTextField,
    text: &str,
    style: &TypeStyle,
    token: Token,
    flow: Flow,
) {
    label.setAttributedStringValue(&styled(text, style, token, flow));
}

/// The width of `label`'s text on one line, rounded up.
pub(crate) fn text_width(label: &NSTextField) -> f64 {
    label.attributedStringValue().size().width.ceil()
}

/// How far a label draws its text inside its frame, on each side: the
/// cell's text inset (the frame is placed this much outside the text, so
/// the glyphs sit where the design puts them).
fn label_inset(label: &NSTextField) -> f64 {
    let Some(cell) = label.cell() else { return 0.0 };
    let text = label.attributedStringValue().size().width;
    ((cell.cellSize().width - text) / 2.0).max(0.0)
}

/// The height of `label`'s text wrapped at `width`.
pub(crate) fn text_height(label: &NSTextField, width: f64) -> f64 {
    let Some(cell) = label.cell() else { return 0.0 };
    let w = width + 2.0 * label_inset(label);
    let bounds = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, 1.0e5));
    cell.cellSizeForBounds(bounds).height.ceil()
}

/// Places a wrapping `label` so its text fills `rect`.
pub(crate) fn place_text(label: &NSTextField, rect: Rect) {
    let inset = label_inset(label);
    let h = text_height(label, rect.width);
    place(
        label,
        Rect::new(rect.x - inset, rect.y, rect.width + 2.0 * inset, h),
    );
}

/// Places a one-line `label` so its text starts at `x` and runs `width`
/// points, centred in the line box `line_y .. line_y + line_height` (CSS
/// centres the glyphs in the line box; the label is its own glyph
/// height).
pub(crate) fn place_line(label: &NSTextField, x: f64, line_y: f64, width: f64, line_height: f64) {
    let h = label
        .cell()
        .map_or(line_height, |c| c.cellSize().height.ceil());
    let inset = label_inset(label);
    place(
        label,
        Rect::new(
            x - inset,
            line_y + ((line_height - h) / 2.0).round(),
            width + 2.0 * inset,
            h,
        ),
    );
}

/// The tag of the control that sent an action (a chip, a column title or
/// a syntax example: each kind has its own range of tags).
pub(crate) fn tag_of(sender: Option<&AnyObject>) -> Option<isize> {
    let sender = sender?.downcast_ref::<NSControl>()?;
    Some(sender.tag())
}
