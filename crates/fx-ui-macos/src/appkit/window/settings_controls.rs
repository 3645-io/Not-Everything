//! The Settings window's drawn views and controls: the canvas's Switch,
//! segmented control, IconPick, Button and KeyCap, on layer-backed views
//! that show the shared surface images ([`super::surfaces::SurfaceCache`])
//! as layer contents and flat surfaces as layer properties. Nothing here draws a shadow per view or runs a
//! timer; a view changes its layer only when its state or the theme does.
//!
//! - [`SurfaceView`]: a decorative surface (a card, the top bar, a key cap,
//!   a tag, a divider, an icon), a layer whose frame is the element grown by
//!   the surface's outer shadows. It ignores the mouse.
//! - [`TextView`]: a run of text drawn in CSS line boxes: the line height
//!   is exact and each baseline sits where Chrome puts it
//!   ([`layout::css_baseline`]), on the pixel grid; it ignores the mouse and
//!   reads as static text.
//! - [`SwitchControl`]: an on/off `NSButton` (Space presses it with the
//!   keyboard focus) read as a checkbox with the `AXSwitch` subrole, drawn
//!   as the board's pressed track and sliding knob (0.15 s, Core
//!   Animation's default timing, which is CSS's `ease`).
//! - [`ChoiceControl`]: an on/off `NSButton` read as a radio button: a
//!   segment of the appearance control or an icon pick. Its own layer shows
//!   its state; its labels are subviews, and a pick takes every click
//!   inside it and shows a hover look.
//! - [`PushControl`]: an `NSButton` (`.btn`), raised by a surface view
//!   behind it and pressed in while the mouse is down.
//!
//! The buttons are borderless and untitled and their cells draw no state,
//! so AppKit draws nothing of its own (no checkbox or radio glyph); each
//! sets its role, label and value for VoiceOver itself.
//! - [`SettingsTarget`]: the controls' target and the window's delegate; it
//!   forwards to the window controller.
//!
//! The controls draw no focus ring of the system's: when one is the first
//! responder (Full Keyboard Access) it shows the design's 2-point
//! `green-ink` outline 2 points outside it.
//!
//! The crate binds no QuartzCore, so [`Layer`] sends the few `CALayer` and
//! `CATransaction` messages it needs through the Objective-C runtime.

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

use objc2::encode::{Encoding, RefEncode};
use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyClass, AnyObject, NSObject, NSObjectProtocol};
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSAccessibility, NSAccessibilityButtonRole, NSAccessibilityCheckBoxRole,
    NSAccessibilityRadioButtonRole, NSAccessibilitySwitchSubrole,
    NSAttributedStringNSExtendedStringDrawing, NSBaselineOffsetAttributeName, NSButton,
    NSButtonCell, NSButtonType, NSCellStyleMask, NSColor, NSControl, NSControlStateValueOff,
    NSControlStateValueOn, NSEvent, NSFocusRingType, NSFontAttributeName,
    NSForegroundColorAttributeName, NSImage, NSKernAttributeName, NSLineBreakMode,
    NSMutableParagraphStyle, NSParagraphStyleAttributeName, NSResponder, NSStringDrawingOptions,
    NSTextAlignment, NSTrackingArea, NSTrackingAreaOptions, NSView, NSWindowDelegate,
};
use objc2_foundation::{
    NSAttributedString, NSAttributedStringKey, NSDictionary, NSNotification, NSNumber, NSPoint,
    NSRect, NSSize, NSString,
};

use super::classes::WinController;
use super::paint;
use super::surfaces::SurfaceImage;
use crate::onboarding::OnboardingEvent;
use crate::settings_view::layout::{self, Frame};
use crate::theme::{Rgb, Rgba, TypeStyle};

// ------------------------------------------------------------------ layers

/// Core Graphics' colour type, only passed through to `CALayer`.
#[repr(C)]
struct CGColor {
    _private: [u8; 0],
}

// SAFETY: `CGColorRef` is a pointer to the opaque struct `CGColor`, which
// the Objective-C runtime encodes as `^{CGColor=}`.
unsafe impl RefEncode for CGColor {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("CGColor", &[]));
}

/// A `CALayer`. See the module documentation.
pub(super) struct Layer(Retained<AnyObject>);

impl Layer {
    /// The backing layer of a layer-backed `view`.
    pub(super) fn of(view: &NSView) -> Option<Layer> {
        // SAFETY: `-[NSView layer]` returns the view's CALayer or nil.
        let layer: Option<Retained<AnyObject>> = unsafe { msg_send![view, layer] };
        layer.map(Layer)
    }

    /// A new layer, without implicit animations until it is in a tree.
    fn new() -> Option<Layer> {
        let class = AnyClass::get(c"CALayer")?;
        // SAFETY: `+[CALayer layer]` returns a new autoreleased layer.
        let layer: Retained<AnyObject> = unsafe { msg_send![class, layer] };
        Some(Layer(layer))
    }

    fn add(&self, sublayer: &Layer) {
        // SAFETY: both are CALayers; the receiver retains the sublayer.
        let _: () = unsafe { msg_send![&*self.0, addSublayer: &*sublayer.0] };
    }

    fn set_frame(&self, frame: NSRect) {
        // SAFETY: `setFrame:` takes a CGRect, which NSRect is.
        let _: () = unsafe { msg_send![&*self.0, setFrame: frame] };
    }

    fn set_hidden(&self, hidden: bool) {
        // SAFETY: `setHidden:` takes a BOOL.
        let _: () = unsafe { msg_send![&*self.0, setHidden: hidden] };
    }

    fn set_opacity(&self, opacity: f32) {
        // SAFETY: `setOpacity:` takes a float.
        let _: () = unsafe { msg_send![&*self.0, setOpacity: opacity] };
    }

    /// Whether the layer's contents are drawn flipped (y down), which
    /// decides how `contentsCenter` is measured.
    pub(super) fn contents_are_flipped(&self) -> bool {
        // SAFETY: `contentsAreFlipped` takes nothing and returns a BOOL.
        unsafe { msg_send![&*self.0, contentsAreFlipped] }
    }

    fn set_color(&self, key: Color, color: Option<Rgba>) {
        let ns = color.map(paint::rgba);
        let cg: *const CGColor = match &ns {
            // SAFETY: `-[NSColor CGColor]` returns a colour the NSColor owns;
            // the layer retains it before `ns` goes away.
            Some(ns) => unsafe { msg_send![&**ns, CGColor] },
            None => std::ptr::null(),
        };
        // SAFETY: both setters take a CGColorRef (or NULL).
        let _: () = unsafe {
            match key {
                Color::Background => msg_send![&*self.0, setBackgroundColor: cg],
                Color::Border => msg_send![&*self.0, setBorderColor: cg],
            }
        };
    }

    fn set_number(&self, key: Number, value: f64) {
        // SAFETY: each setter takes a CGFloat.
        let _: () = unsafe {
            match key {
                Number::CornerRadius => msg_send![&*self.0, setCornerRadius: value],
                Number::BorderWidth => msg_send![&*self.0, setBorderWidth: value],
                Number::ContentsScale => msg_send![&*self.0, setContentsScale: value],
            }
        };
    }

    fn set_contents(&self, contents: Option<&AnyObject>) {
        // SAFETY: `setContents:` takes an object (here a CGImage from
        // `-[NSImage layerContentsForContentsScale:]`) or nil.
        let _: () = unsafe { msg_send![&*self.0, setContents: contents] };
    }

    fn set_contents_center(&self, center: NSRect) {
        // SAFETY: `setContentsCenter:` takes a CGRect.
        let _: () = unsafe { msg_send![&*self.0, setContentsCenter: center] };
    }

    /// Rounds the layer's corners to `radius` and clips its sublayers to
    /// them (the search window's results card, `overflow: hidden`).
    pub(super) fn clip_to_radius(&self, radius: f64) {
        self.set_number(Number::CornerRadius, radius);
        // SAFETY: `setMasksToBounds:` takes a BOOL.
        let _: () = unsafe { msg_send![&*self.0, setMasksToBounds: true] };
    }

    /// Uses mipmaps when the contents are drawn smaller than their pixels
    /// (the 256-pixel icons at 72 points), as a browser's image scaling
    /// does.
    fn set_trilinear(&self) {
        let filter = NSString::from_str("trilinear");
        // SAFETY: `setMinificationFilter:` takes an NSString filter name.
        let _: () = unsafe { msg_send![&*self.0, setMinificationFilter: &*filter] };
    }

    /// Shows `look` for an element filling the layer's bounds.
    fn show(&self, look: &Look, scale: f64) {
        match look {
            Look::None => {
                self.set_contents(None);
                self.set_color(Color::Background, None);
                self.set_number(Number::BorderWidth, 0.0);
            }
            Look::Surface(surface) => {
                let flipped = self.contents_are_flipped();
                self.show_image(surface.image(), scale, surface.contents_center(flipped));
            }
            Look::Picture(image) => self.show_image(image, scale, unit_rect()),
            Look::Flat {
                fill,
                border,
                radius,
            } => {
                self.set_contents(None);
                self.set_color(Color::Background, *fill);
                self.set_number(Number::CornerRadius, *radius);
                match border {
                    Some((width, color)) => {
                        self.set_number(Number::BorderWidth, *width);
                        self.set_color(Color::Border, Some(*color));
                    }
                    None => self.set_number(Number::BorderWidth, 0.0),
                }
            }
        }
    }

    pub(super) fn show_image(&self, image: &NSImage, scale: f64, center: NSRect) {
        let scale = image.recommendedLayerContentsScale(scale);
        let contents = image.layerContentsForContentsScale(scale);
        self.set_color(Color::Background, None);
        self.set_number(Number::BorderWidth, 0.0);
        self.set_number(Number::CornerRadius, 0.0);
        self.set_contents(Some(&contents));
        self.set_number(Number::ContentsScale, scale);
        self.set_contents_center(center);
    }
}

#[derive(Clone, Copy)]
enum Color {
    Background,
    Border,
}

#[derive(Clone, Copy)]
enum Number {
    CornerRadius,
    BorderWidth,
    ContentsScale,
}

/// Runs `f` in a Core Animation transaction: animated over `duration`
/// seconds, or with implicit animations off.
fn transaction(duration: Option<f64>, f: impl FnOnce()) {
    let Some(class) = AnyClass::get(c"CATransaction") else {
        f();
        return;
    };
    // SAFETY: `+begin`, `+commit`, `+setAnimationDuration:` (a
    // CFTimeInterval) and `+setDisableActions:` (a BOOL) are class methods
    // of CATransaction, called on the main thread.
    unsafe {
        let _: () = msg_send![class, begin];
        match duration {
            Some(seconds) => {
                let _: () = msg_send![class, setAnimationDuration: seconds];
            }
            None => {
                let _: () = msg_send![class, setDisableActions: true];
            }
        }
    }
    f();
    // SAFETY: as above.
    let _: () = unsafe { msg_send![class, commit] };
}

fn unit_rect() -> NSRect {
    NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0))
}

/// `frame` as an `NSRect`.
pub(super) fn ns_rect(frame: Frame) -> NSRect {
    NSRect::new(
        NSPoint::new(frame.x, frame.y),
        NSSize::new(frame.width, frame.height),
    )
}

/// What a surface view or control shows.
#[derive(Clone, Default)]
pub(super) enum Look {
    /// Nothing.
    #[default]
    None,
    /// A shared surface image (a style with shadows).
    Surface(Rc<SurfaceImage>),
    /// A style without shadows, as layer properties.
    Flat {
        /// The fill.
        fill: Option<Rgba>,
        /// The border's width and colour.
        border: Option<(f64, Rgba)>,
        /// The corner radius.
        radius: f64,
    },
    /// An image filling the element (an app icon).
    Picture(Retained<NSImage>),
}

impl Look {
    /// How far the look reaches outside its element: the outer shadows.
    fn outsets(&self) -> [f64; 4] {
        match self {
            Look::Surface(s) => {
                let o = &s.layout().outsets;
                [o.top, o.left, o.bottom, o.right]
            }
            _ => [0.0; 4],
        }
    }
}

/// The scale of the screen `view` is on.
fn backing_scale(view: &NSView) -> f64 {
    view.window().map_or(2.0, |w| w.backingScaleFactor())
}

/// A focus outline layer for a control `radius` round, in `color`.
fn focus_ring(view: &NSView, ring: &OnceCell<Layer>, focused: bool, radius: f64, color: Rgba) {
    let Some(root) = Layer::of(view) else { return };
    let Some(layer) = ring.get().or_else(|| {
        let layer = Layer::new()?;
        root.add(&layer);
        let _ = ring.set(layer);
        ring.get()
    }) else {
        return;
    };
    let (width, offset) = layout::FOCUS_OUTLINE;
    let reach = width + offset;
    let b = view.bounds();
    layer.set_frame(NSRect::new(
        NSPoint::new(b.origin.x - reach, b.origin.y - reach),
        NSSize::new(b.size.width + 2.0 * reach, b.size.height + 2.0 * reach),
    ));
    layer.show(
        &Look::Flat {
            fill: None,
            border: Some((width, color)),
            radius: radius + reach,
        },
        1.0,
    );
    layer.set_hidden(!focused);
}

// ------------------------------------------------------------ surface view

/// Instance variables of [`SurfaceView`].
#[derive(Default)]
pub(super) struct SurfaceIvars {
    look: RefCell<Look>,
    element: Cell<NSRect>,
}

define_class!(
    // SAFETY: NSView may be subclassed; the overrides keep their contracts.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FxSettingsSurfaceView"]
    #[ivars = SurfaceIvars]
    pub(super) struct SurfaceView;

    impl SurfaceView {
        #[unsafe(method(wantsUpdateLayer))]
        fn wants_update_layer(&self) -> bool {
            true
        }

        #[unsafe(method(updateLayer))]
        fn update_layer(&self) {
            self.apply();
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        /// Decoration: clicks and scrolls go to the view behind.
        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, _point: NSPoint) -> Option<Retained<NSView>> {
            None
        }

        #[unsafe(method(viewDidChangeBackingProperties))]
        fn backing_changed(&self) {
            self.apply();
        }
    }
);

impl SurfaceView {
    pub(super) fn new(mtm: MainThreadMarker) -> Retained<SurfaceView> {
        let this = SurfaceView::alloc(mtm).set_ivars(SurfaceIvars::default());
        // SAFETY: `initWithFrame:` is NSView's designated initializer.
        let view: Retained<SurfaceView> =
            unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] };
        view.setWantsLayer(true);
        view
    }

    /// Shows `look` for an element at `element` (in the superview, which is
    /// flipped), at once.
    pub(super) fn set(&self, element: Frame, look: Look) {
        self.ivars().element.set(ns_rect(element));
        *self.ivars().look.borrow_mut() = look;
        self.place();
        self.apply();
    }

    /// Shows `look` at the same element.
    pub(super) fn set_look(&self, look: Look) {
        *self.ivars().look.borrow_mut() = look;
        self.place();
        self.apply();
    }

    fn place(&self) {
        let e = self.ivars().element.get();
        let [top, left, bottom, right] = self.ivars().look.borrow().outsets();
        self.setFrame(NSRect::new(
            NSPoint::new(e.origin.x - left, e.origin.y - top),
            NSSize::new(e.size.width + left + right, e.size.height + top + bottom),
        ));
    }

    fn apply(&self) {
        let Some(layer) = Layer::of(self) else { return };
        let scale = backing_scale(self);
        transaction(None, || layer.show(&self.ivars().look.borrow(), scale));
        if matches!(*self.ivars().look.borrow(), Look::Picture(_)) {
            layer.set_trilinear();
        }
    }
}

// --------------------------------------------------------------- text view

/// What a [`TextView`] shows.
struct TextContent {
    string: Retained<NSString>,
    style: TypeStyle,
    color: Rgb,
    align: NSTextAlignment,
}

/// Instance variables of [`TextView`].
#[derive(Default)]
pub(super) struct TextIvars {
    content: RefCell<Option<TextContent>>,
    /// How far the view's top was moved up onto the pixel grid.
    shift: Cell<f64>,
    /// The string as last laid out, with the screen scale it was for.
    drawn: RefCell<Option<(f64, Retained<NSAttributedString>)>>,
}

define_class!(
    // SAFETY: NSView may be subclassed; the overrides keep their contracts.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FxSettingsTextView"]
    #[ivars = TextIvars]
    pub(super) struct TextView;

    impl TextView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            if let Some(text) = self.attributed(backing_scale(self)) {
                text.drawWithRect_options_context(
                    self.bounds(),
                    NSStringDrawingOptions::UsesLineFragmentOrigin,
                    None,
                );
            }
        }

        /// Text ignores the mouse: clicks go to the control or sheet
        /// behind it.
        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, _point: NSPoint) -> Option<Retained<NSView>> {
            None
        }

        #[unsafe(method(viewDidChangeBackingProperties))]
        fn backing_changed(&self) {
            redraw(self);
        }
    }
);

impl TextView {
    /// A text view read as static text (`accessible`) or ignored (a
    /// control's label, which the control reads out).
    pub(super) fn new(mtm: MainThreadMarker, accessible: bool) -> Retained<TextView> {
        let this = TextView::alloc(mtm).set_ivars(TextIvars::default());
        // SAFETY: `initWithFrame:` is NSView's designated initializer.
        let view: Retained<TextView> =
            unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] };
        view.setWantsLayer(true);
        view.setAccessibilityElement(accessible);
        if accessible {
            // SAFETY: the role is an immutable framework constant.
            view.setAccessibilityRole(Some(unsafe {
                objc2_app_kit::NSAccessibilityStaticTextRole
            }));
        }
        view
    }

    /// Shows `text` in `style` and `color`, aligned `align`.
    pub(super) fn set(&self, text: &str, style: &TypeStyle, color: Rgb, align: NSTextAlignment) {
        let shown = if style.caps {
            text.to_uppercase()
        } else {
            text.to_owned()
        };
        let string = NSString::from_str(&shown);
        // SAFETY: the value is an NSString, as static text's is.
        unsafe { self.setAccessibilityValue(Some(&string)) };
        *self.ivars().content.borrow_mut() = Some(TextContent {
            string,
            style: *style,
            color,
            align,
        });
        *self.ivars().drawn.borrow_mut() = None;
        redraw(self);
    }

    /// The text's height when it wraps at `width`.
    pub(super) fn height(&self, width: f64) -> f64 {
        self.measure(NSSize::new(width, 1.0e6)).1
    }

    /// The width of the text on one line.
    pub(super) fn width(&self) -> f64 {
        self.measure(NSSize::new(1.0e6, 1.0e6)).0
    }

    fn measure(&self, size: NSSize) -> (f64, f64) {
        match self.attributed(backing_scale(self)) {
            Some(text) => {
                let r = text.boundingRectWithSize_options_context(
                    size,
                    NSStringDrawingOptions::UsesLineFragmentOrigin,
                    None,
                );
                (r.size.width.ceil(), r.size.height.ceil())
            }
            None => (0.0, 0.0),
        }
    }

    /// Places the text's line boxes at `frame` (in the flipped superview),
    /// the view's top moved up onto the pixel grid.
    pub(super) fn place(&self, frame: Frame) {
        let scale = backing_scale(self);
        let x = (frame.x * scale).round() / scale;
        let y = layout::snap_down(frame.y, scale);
        let shift = frame.y - y;
        if shift != self.ivars().shift.get() {
            self.ivars().shift.set(shift);
            *self.ivars().drawn.borrow_mut() = None;
            redraw(self);
        }
        self.setFrame(NSRect::new(
            NSPoint::new(x, y),
            NSSize::new(frame.width, frame.height + shift),
        ));
    }

    /// The string laid out for a screen of `scale` pixels per point.
    fn attributed(&self, scale: f64) -> Option<Retained<NSAttributedString>> {
        if let Some((s, text)) = &*self.ivars().drawn.borrow() {
            if *s == scale {
                return Some(text.clone());
            }
        }
        let content = self.ivars().content.borrow();
        let text = attributed(content.as_ref()?, self.ivars().shift.get(), scale);
        *self.ivars().drawn.borrow_mut() = Some((scale, text.clone()));
        Some(text)
    }
}

/// `text` as a [`TextView`] lays it out, centred, for tests that draw it
/// offscreen: see [`attributed`].
#[cfg(test)]
pub(super) fn laid_out(
    text: &str,
    style: &TypeStyle,
    color: Rgb,
    shift: f64,
    scale: f64,
) -> Retained<NSAttributedString> {
    let content = TextContent {
        string: NSString::from_str(text),
        style: *style,
        color,
        align: NSTextAlignment::Center,
    };
    attributed(&content, shift, scale)
}

/// `content`'s string in its style and colour: the font, the letter
/// spacing, and lines exactly the style's line height whose baselines sit
/// where CSS puts them ([`layout::css_baseline`]) on the pixel grid of a
/// `scale` screen, in a view whose top is `shift` above the first line box.
/// TextKit only puts a baseline there in a line at least as tall as the
/// font's ascent plus descent: in a shorter one it draws the text higher.
fn attributed(content: &TextContent, shift: f64, scale: f64) -> Retained<NSAttributedString> {
    let style = &content.style;
    let font = paint::font(style);
    let line = style.line_height;
    let paragraph = NSMutableParagraphStyle::new();
    paragraph.setMinimumLineHeight(line);
    paragraph.setMaximumLineHeight(line);
    paragraph.setAlignment(content.align);
    paragraph.setLineBreakMode(NSLineBreakMode::ByWordWrapping);
    let (ascent, descent) = (font.ascender(), -font.descender());
    let want = layout::snap_baseline(shift + layout::css_baseline(line, ascent, descent), scale);
    // TextKit lays the first line from the view's top; raise its baseline
    // to `want`.
    let offset = layout::textkit_baseline(line, descent) - want;
    let color = paint::rgb(content.color);
    let kern = NSNumber::numberWithDouble(style.kern());
    let offset = NSNumber::numberWithDouble(offset);
    let values: [&AnyObject; 5] = [
        font.as_ref(),
        color.as_ref(),
        paragraph.as_ref(),
        kern.as_ref(),
        offset.as_ref(),
    ];
    // SAFETY: the attribute names are immutable framework constants.
    let keys: [&NSAttributedStringKey; 5] = unsafe {
        [
            NSFontAttributeName,
            NSForegroundColorAttributeName,
            NSParagraphStyleAttributeName,
            NSKernAttributeName,
            NSBaselineOffsetAttributeName,
        ]
    };
    let attributes = NSDictionary::<NSAttributedStringKey, AnyObject>::from_slices(&keys, &values);
    // SAFETY: each attribute holds the class AppKit expects for it (NSFont,
    // NSColor, NSParagraphStyle, NSNumber, NSNumber).
    unsafe { NSAttributedString::new_with_attributes(&content.string, &attributes) }
}

// ----------------------------------------------------------- flipped view

define_class!(
    // SAFETY: NSView may be subclassed; the override keeps its contract.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FxSettingsFlippedView"]
    pub(super) struct FlippedView;

    impl FlippedView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

impl FlippedView {
    pub(super) fn new(mtm: MainThreadMarker) -> Retained<FlippedView> {
        let this = FlippedView::alloc(mtm).set_ivars(());
        // SAFETY: `initWithFrame:` is NSView's designated initializer.
        let view: Retained<FlippedView> =
            unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] };
        view.setWantsLayer(true);
        view
    }
}

// ------------------------------------------------------------------ switch

/// The images of a switch in one theme.
pub(super) struct SwitchLooks {
    /// The track off and on.
    pub track: [Rc<SurfaceImage>; 2],
    /// The knob off and on.
    pub knob: [Rc<SurfaceImage>; 2],
    /// The focus outline's colour.
    pub focus: Rgba,
}

/// Instance variables of [`SwitchControl`].
#[derive(Default)]
pub(super) struct SwitchIvars {
    looks: RefCell<Option<SwitchLooks>>,
    track: OnceCell<Layer>,
    knob: OnceCell<Layer>,
    ring: OnceCell<Layer>,
    drawn: Cell<Option<bool>>,
    focused: Cell<bool>,
}

define_class!(
    // SAFETY: NSButton may be subclassed; the overrides call super or
    // replace drawing, which the button no longer does.
    #[unsafe(super(NSButton, NSControl, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FxSettingsSwitch"]
    #[ivars = SwitchIvars]
    pub(super) struct SwitchControl;

    impl SwitchControl {
        #[unsafe(method(wantsUpdateLayer))]
        fn wants_update_layer(&self) -> bool {
            true
        }

        /// The window draws the control; the button draws nothing.
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {}

        /// On (1) or off (0), for VoiceOver.
        #[unsafe(method_id(accessibilityValue))]
        fn accessibility_value(&self) -> Option<Retained<AnyObject>> {
            let on = self.state() == NSControlStateValueOn;
            Some(Retained::into_super(Retained::into_super(Retained::into_super(
                NSNumber::numberWithBool(on),
            ))))
        }

        #[unsafe(method(updateLayer))]
        fn update_layer(&self) {
            self.apply();
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(becomeFirstResponder))]
        fn become_first_responder(&self) -> bool {
            // SAFETY: `becomeFirstResponder` takes nothing and returns BOOL.
            let ok: bool = unsafe { msg_send![super(self), becomeFirstResponder] };
            if ok {
                self.ivars().focused.set(true);
                reveal(self);
                redraw(self);
            }
            ok
        }

        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            // SAFETY: `resignFirstResponder` takes nothing and returns BOOL.
            let ok: bool = unsafe { msg_send![super(self), resignFirstResponder] };
            if ok {
                self.ivars().focused.set(false);
                redraw(self);
            }
            ok
        }
    }
);

impl SwitchControl {
    /// A switch named `label` for VoiceOver, sending `action` to `target`;
    /// its tag tells the target which setting it is.
    pub(super) fn new(
        mtm: MainThreadMarker,
        label: &str,
        target: &AnyObject,
        action: objc2::runtime::Sel,
    ) -> Retained<SwitchControl> {
        let this = SwitchControl::alloc(mtm).set_ivars(SwitchIvars::default());
        let (w, h) = layout::SWITCH_SIZE;
        // SAFETY: `initWithFrame:` is NSView's designated initializer.
        let button: Retained<SwitchControl> = unsafe {
            msg_send![super(this), initWithFrame: NSRect::new(NSPoint::ZERO, NSSize::new(w, h))]
        };
        plain_toggle(&button);
        button.setAccessibilityLabel(Some(&NSString::from_str(label)));
        // SAFETY: the role and subrole are immutable framework constants.
        unsafe {
            button.setAccessibilityRole(Some(NSAccessibilityCheckBoxRole));
            button.setAccessibilitySubrole(Some(NSAccessibilitySwitchSubrole));
        }
        // SAFETY: the target outlives the window (the Settings UI keeps it)
        // and implements `action` as `-(void)name:(NSButton *)sender`.
        unsafe {
            button.setTarget(Some(target));
            button.setAction(Some(action));
        }
        button
    }

    /// Shows the switch `on` or off, usable or not, with `looks`; a switch
    /// already drawn the other way slides to it.
    pub(super) fn set(&self, on: bool, enabled: bool, looks: SwitchLooks) {
        *self.ivars().looks.borrow_mut() = Some(looks);
        self.setState(if on {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
        self.setEnabled(enabled);
        redraw(self);
    }

    /// Whether the switch is on.
    pub(super) fn is_on(&self) -> bool {
        self.state() == NSControlStateValueOn
    }

    fn apply(&self) {
        let iv = self.ivars();
        let Some(root) = Layer::of(self) else { return };
        let looks = iv.looks.borrow();
        let Some(looks) = &*looks else { return };
        let (Some(track), Some(knob)) = (sublayer(&root, &iv.track), sublayer(&root, &iv.knob))
        else {
            return;
        };
        let on = self.is_on();
        let animate = iv.drawn.get().is_some_and(|was| was != on);
        iv.drawn.set(Some(on));
        let scale = backing_scale(self);
        let bounds = self.bounds();
        let i = usize::from(on);
        transaction(animate.then_some(SWITCH_SECONDS), || {
            let t = &looks.track[i];
            track.set_frame(t.frame_for(bounds, true));
            track.show(&Look::Surface(Rc::clone(t)), scale);
            let k = &looks.knob[i];
            knob.set_frame(k.frame_for(ns_rect(layout::knob(on)), true));
            knob.show(&Look::Surface(Rc::clone(k)), scale);
        });
        root.set_opacity(if self.isEnabled() {
            1.0
        } else {
            DISABLED_OPACITY
        });
        let radius = bounds.size.height / 2.0;
        transaction(None, || {
            focus_ring(self, &iv.ring, iv.focused.get(), radius, looks.focus);
        });
    }
}

/// Makes `button` an on/off button that draws nothing of its own: no
/// checkbox or radio glyph (the window draws the control's look), with its
/// role set by the caller.
fn plain_toggle(button: &NSButton) {
    plain(button, NSButtonType::Toggle);
}

/// Makes `button` a borderless button of `kind` with no title, image,
/// highlight or state drawing, layer-backed and without the system's focus
/// ring.
fn plain(button: &NSButton, kind: NSButtonType) {
    button.setButtonType(kind);
    button.setBordered(false);
    button.setTitle(&NSString::from_str(""));
    button.setFocusRingType(NSFocusRingType::None);
    button.setWantsLayer(true);
    button.setAccessibilityElement(true);
    keep_unclipped(button);
    if let Some(cell) = button
        .cell()
        .and_then(|c| c.downcast::<NSButtonCell>().ok())
    {
        cell.setHighlightsBy(NSCellStyleMask::empty());
        cell.setShowsStateBy(NSCellStyleMask::empty());
    }
}

/// Lets `view` show its layers outside its bounds (shadows, focus
/// outlines); macOS 14 and later do by default for most views.
pub(super) fn keep_unclipped(view: &NSView) {
    if view.respondsToSelector(sel!(setClipsToBounds:)) {
        view.setClipsToBounds(false);
    }
    if let Some(layer) = Layer::of(view) {
        // SAFETY: `setMasksToBounds:` takes a BOOL.
        let _: () = unsafe { msg_send![&*layer.0, setMasksToBounds: false] };
    }
}

/// The switch's knob and colour transition (`.ne-switch`, `.ne-knob`).
const SWITCH_SECONDS: f64 = 0.15;

/// A control that can't be used (`.btn:disabled`).
const DISABLED_OPACITY: f32 = 0.5;

/// Marks `view` for display (`needsDisplay`; `NSControl`'s method of the
/// same name takes no argument).
fn redraw(view: &NSView) {
    view.setNeedsDisplay(true);
}

/// The sublayer in `cell`, added to `root` the first time.
fn sublayer<'a>(root: &Layer, cell: &'a OnceCell<Layer>) -> Option<&'a Layer> {
    if cell.get().is_none() {
        let layer = Layer::new()?;
        root.add(&layer);
        let _ = cell.set(layer);
    }
    cell.get()
}

/// Scrolls the sheet so a control that took the keyboard focus shows with
/// its focus outline.
fn reveal(view: &NSView) {
    let (width, offset) = layout::FOCUS_OUTLINE;
    let reach = width + offset + 8.0;
    let b = view.bounds();
    view.scrollRectToVisible(NSRect::new(
        NSPoint::new(b.origin.x - reach, b.origin.y - reach),
        NSSize::new(b.size.width + 2.0 * reach, b.size.height + 2.0 * reach),
    ));
}

// ------------------------------------------------------------------ choice

/// How a [`ChoiceControl`] looks in each state.
#[derive(Clone, Default)]
pub(super) struct ChoiceLooks {
    /// Not chosen.
    pub off: Look,
    /// Not chosen, under the pointer.
    pub hover: Look,
    /// Chosen.
    pub on: Look,
    /// The control's corner radius, for the focus outline.
    pub radius: f64,
    /// The focus outline's colour.
    pub focus: Option<Rgba>,
}

/// Instance variables of [`ChoiceControl`].
#[derive(Default)]
pub(super) struct ChoiceIvars {
    looks: RefCell<ChoiceLooks>,
    hovered: Cell<bool>,
    focused: Cell<bool>,
    ring: OnceCell<Layer>,
    /// A pick takes every click inside it, over its icon and labels.
    whole: Cell<bool>,
}

define_class!(
    // SAFETY: NSButton may be subclassed; the overrides call super or
    // replace drawing, which the button no longer does.
    #[unsafe(super(NSButton, NSControl, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FxSettingsChoice"]
    #[ivars = ChoiceIvars]
    pub(super) struct ChoiceControl;

    impl ChoiceControl {
        #[unsafe(method(wantsUpdateLayer))]
        fn wants_update_layer(&self) -> bool {
            true
        }

        /// The window draws the control; the button draws nothing.
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {}

        /// On (1) or off (0), for VoiceOver.
        #[unsafe(method_id(accessibilityValue))]
        fn accessibility_value(&self) -> Option<Retained<AnyObject>> {
            let on = self.state() == NSControlStateValueOn;
            Some(Retained::into_super(Retained::into_super(Retained::into_super(
                NSNumber::numberWithBool(on),
            ))))
        }

        #[unsafe(method(updateLayer))]
        fn update_layer(&self) {
            self.apply();
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> Option<Retained<NSView>> {
            // SAFETY: `hitTest:` takes an NSPoint and returns a view or nil.
            let hit: Option<Retained<NSView>> = unsafe { msg_send![super(self), hitTest: point] };
            match hit {
                Some(_) if self.ivars().whole.get() => Some(self.retain().into_super().into_super().into_super()),
                other => other,
            }
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _event: &NSEvent) {
            self.ivars().hovered.set(true);
            redraw(self);
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            self.ivars().hovered.set(false);
            redraw(self);
        }

        #[unsafe(method(becomeFirstResponder))]
        fn become_first_responder(&self) -> bool {
            // SAFETY: `becomeFirstResponder` takes nothing and returns BOOL.
            let ok: bool = unsafe { msg_send![super(self), becomeFirstResponder] };
            if ok {
                self.ivars().focused.set(true);
                reveal(self);
                redraw(self);
            }
            ok
        }

        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            // SAFETY: `resignFirstResponder` takes nothing and returns BOOL.
            let ok: bool = unsafe { msg_send![super(self), resignFirstResponder] };
            if ok {
                self.ivars().focused.set(false);
                redraw(self);
            }
            ok
        }
    }
);

impl ChoiceControl {
    /// A radio button named `label` for VoiceOver with `tag`, sending
    /// `action` to `target`. A `pick` takes every click inside it and
    /// tracks the pointer for its hover look.
    pub(super) fn new(
        mtm: MainThreadMarker,
        label: &str,
        tag: isize,
        pick: bool,
        target: &AnyObject,
        action: objc2::runtime::Sel,
    ) -> Retained<ChoiceControl> {
        let ivars = ChoiceIvars::default();
        ivars.whole.set(pick);
        let this = ChoiceControl::alloc(mtm).set_ivars(ivars);
        // SAFETY: `initWithFrame:` is NSView's designated initializer.
        let button: Retained<ChoiceControl> =
            unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] };
        plain_toggle(&button);
        button.setTag(tag);
        button.setAccessibilityLabel(Some(&NSString::from_str(label)));
        // SAFETY: the role is an immutable framework constant.
        button.setAccessibilityRole(Some(unsafe { NSAccessibilityRadioButtonRole }));
        // SAFETY: as for `SwitchControl::new`.
        unsafe {
            button.setTarget(Some(target));
            button.setAction(Some(action));
        }
        if pick {
            // SAFETY: the area's owner is the button itself, which keeps it;
            // no user info.
            let area = unsafe {
                NSTrackingArea::initWithRect_options_owner_userInfo(
                    NSTrackingArea::alloc(),
                    NSRect::ZERO,
                    NSTrackingAreaOptions::MouseEnteredAndExited
                        | NSTrackingAreaOptions::ActiveInActiveApp
                        | NSTrackingAreaOptions::InVisibleRect,
                    Some(&button),
                    None,
                )
            };
            button.addTrackingArea(&area);
        }
        button
    }

    /// Shows the control chosen (`on`) or not, with `looks`.
    pub(super) fn set(&self, on: bool, looks: ChoiceLooks) {
        *self.ivars().looks.borrow_mut() = looks;
        self.setState(if on {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
        redraw(self);
    }

    /// Forgets the hover look (the window closed under the pointer).
    pub(super) fn reset_hover(&self) {
        self.ivars().hovered.set(false);
    }

    fn apply(&self) {
        let iv = self.ivars();
        let Some(root) = Layer::of(self) else { return };
        let looks = iv.looks.borrow();
        let on = self.state() == NSControlStateValueOn;
        let look = if on {
            &looks.on
        } else if iv.hovered.get() {
            &looks.hover
        } else {
            &looks.off
        };
        let scale = backing_scale(self);
        transaction(None, || {
            root.show(look, scale);
            // A corner radius on the backing layer makes AppKit clip the
            // view to its bounds, which would hide the focus outline.
            keep_unclipped(self);
            if let Some(focus) = looks.focus {
                focus_ring(self, &iv.ring, iv.focused.get(), looks.radius, focus);
            }
        });
    }
}

// -------------------------------------------------------------- push button

/// How a [`PushControl`] looks at rest and pressed.
#[derive(Clone, Default)]
pub(super) struct PushLooks {
    /// At rest: shown by the surface view behind the button.
    pub rest: Look,
    /// Pressed: shown by the button's own layer (pressed in, so nothing
    /// reaches outside it).
    pub down: Look,
    /// The focus outline's colour.
    pub focus: Option<Rgba>,
}

/// Instance variables of [`PushControl`].
#[derive(Default)]
pub(super) struct PushIvars {
    looks: RefCell<PushLooks>,
    surface: OnceCell<Retained<SurfaceView>>,
    pressed: Cell<bool>,
    focused: Cell<bool>,
    ring: OnceCell<Layer>,
}

define_class!(
    // SAFETY: NSButton may be subclassed; the overrides call super or
    // replace drawing, which the button no longer does.
    #[unsafe(super(NSButton, NSControl, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FxSettingsPush"]
    #[ivars = PushIvars]
    pub(super) struct PushControl;

    impl PushControl {
        #[unsafe(method(wantsUpdateLayer))]
        fn wants_update_layer(&self) -> bool {
            true
        }

        /// The window draws the control; the button draws nothing.
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {}

        #[unsafe(method(updateLayer))]
        fn update_layer(&self) {
            self.apply();
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> Option<Retained<NSView>> {
            // SAFETY: `hitTest:` takes an NSPoint and returns a view or nil.
            let hit: Option<Retained<NSView>> = unsafe { msg_send![super(self), hitTest: point] };
            hit.map(|_| self.retain().into_super().into_super().into_super())
        }

        /// Pressed in while the mouse is down (`.btn:active`); the button's
        /// own tracking runs in between.
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.set_pressed(true);
            // SAFETY: `mouseDown:` takes one NSEvent and returns void.
            let _: () = unsafe { msg_send![super(self), mouseDown: event] };
            self.set_pressed(false);
        }

        #[unsafe(method(becomeFirstResponder))]
        fn become_first_responder(&self) -> bool {
            // SAFETY: `becomeFirstResponder` takes nothing and returns BOOL.
            let ok: bool = unsafe { msg_send![super(self), becomeFirstResponder] };
            if ok {
                self.ivars().focused.set(true);
                reveal(self);
                redraw(self);
            }
            ok
        }

        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            // SAFETY: `resignFirstResponder` takes nothing and returns BOOL.
            let ok: bool = unsafe { msg_send![super(self), resignFirstResponder] };
            if ok {
                self.ivars().focused.set(false);
                redraw(self);
            }
            ok
        }
    }
);

impl PushControl {
    /// A push button named `label`, raised by `surface` (a view the caller
    /// puts behind it), sending `action` to `target`.
    pub(super) fn new(
        mtm: MainThreadMarker,
        label: &str,
        surface: Retained<SurfaceView>,
        target: &AnyObject,
        action: objc2::runtime::Sel,
    ) -> Retained<PushControl> {
        let ivars = PushIvars::default();
        let _ = ivars.surface.set(surface);
        let this = PushControl::alloc(mtm).set_ivars(ivars);
        // SAFETY: `initWithFrame:` is NSView's designated initializer.
        let button: Retained<PushControl> =
            unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] };
        plain(&button, NSButtonType::MomentaryPushIn);
        button.setAccessibilityLabel(Some(&NSString::from_str(label)));
        // SAFETY: the role is an immutable framework constant.
        button.setAccessibilityRole(Some(unsafe { NSAccessibilityButtonRole }));
        // SAFETY: as for `SwitchControl::new`.
        unsafe {
            button.setTarget(Some(target));
            button.setAction(Some(action));
        }
        button
    }

    /// Places the button at `frame` with `looks`.
    pub(super) fn set(&self, frame: Frame, looks: PushLooks) {
        self.setFrame(ns_rect(frame));
        if let Some(surface) = self.ivars().surface.get() {
            surface.set(frame, looks.rest.clone());
        }
        *self.ivars().looks.borrow_mut() = looks;
        redraw(self);
    }

    fn set_pressed(&self, pressed: bool) {
        self.ivars().pressed.set(pressed);
        if let Some(surface) = self.ivars().surface.get() {
            surface.setHidden(pressed);
        }
        redraw(self);
        self.displayIfNeeded();
    }

    fn apply(&self) {
        let iv = self.ivars();
        let Some(root) = Layer::of(self) else { return };
        let looks = iv.looks.borrow();
        let scale = backing_scale(self);
        let radius = self.bounds().size.height / 2.0;
        transaction(None, || {
            let none = Look::None;
            root.show(if iv.pressed.get() { &looks.down } else { &none }, scale);
            if let Some(focus) = looks.focus {
                focus_ring(self, &iv.ring, iv.focused.get(), radius, focus);
            }
        });
    }
}

// ------------------------------------------------------------------ target

/// Instance variables of [`SettingsTarget`].
pub(super) struct TargetIvars {
    controller: Weak<WinController>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; SettingsTarget does
    // not implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FxSettingsTarget"]
    #[ivars = TargetIvars]
    pub(super) struct SettingsTarget;

    // SAFETY: NSObject itself conforms to NSObjectProtocol; no method is
    // overridden.
    unsafe impl NSObjectProtocol for SettingsTarget {}

    // SAFETY: the methods below have the signatures the protocol declares;
    // every other method of it is optional.
    unsafe impl NSWindowDelegate for SettingsTarget {
        /// The window closed: release what only it shows.
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            self.with(|c| c.settings_closed());
        }

        #[unsafe(method(windowDidResize:))]
        fn window_did_resize(&self, _notification: &NSNotification) {
            self.with(|c| c.settings_resized());
        }
    }

    impl SettingsTarget {
        /// The Launch at Login switch.
        #[unsafe(method(loginSwitched:))]
        fn login_switched(&self, sender: &NSButton) {
            let on = sender.state() == NSControlStateValueOn;
            self.with(|c| c.login_switched(on));
        }

        /// The "Your files first" switch.
        #[unsafe(method(yourFilesFirstSwitched:))]
        fn your_files_first_switched(&self, sender: &NSButton) {
            let on = sender.state() == NSControlStateValueOn;
            self.with(|c| c.your_files_first_changed(on));
        }

        /// A segment of the appearance control; its tag is the mode's
        /// index.
        #[unsafe(method(appearancePicked:))]
        fn appearance_picked(&self, sender: &NSButton) {
            let tag = sender.tag();
            self.with(|c| c.appearance_changed(tag));
        }

        /// An icon pick; its tag is the icon's index.
        #[unsafe(method(iconPicked:))]
        fn icon_picked(&self, sender: &NSButton) {
            let tag = sender.tag();
            self.with(|c| c.icon_chosen(tag));
        }

        /// Open System Settings: the Full Disk Access page, as the access
        /// callout and the menu's Grant item open it.
        #[unsafe(method(openAccessSettings:))]
        fn open_access_settings(&self, _sender: Option<&AnyObject>) {
            self.with(|c| c.onboarding_event(OnboardingEvent::GrantClicked));
        }
    }
);

impl SettingsTarget {
    pub(super) fn new(
        mtm: MainThreadMarker,
        controller: &WinController,
    ) -> Retained<SettingsTarget> {
        let this = SettingsTarget::alloc(mtm).set_ivars(TargetIvars {
            controller: Weak::new(controller),
        });
        // SAFETY: `init` is NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }

    fn with(&self, f: impl FnOnce(&WinController)) {
        if let Some(controller) = self.ivars().controller.load() {
            f(&controller);
        }
    }

    /// The selector the Launch at Login switch sends.
    pub(super) fn login_action() -> objc2::runtime::Sel {
        sel!(loginSwitched:)
    }

    /// The selector the "Your files first" switch sends.
    pub(super) fn your_files_first_action() -> objc2::runtime::Sel {
        sel!(yourFilesFirstSwitched:)
    }

    /// The selector the appearance segments send.
    pub(super) fn appearance_action() -> objc2::runtime::Sel {
        sel!(appearancePicked:)
    }

    /// The selector the icon picks send.
    pub(super) fn icon_action() -> objc2::runtime::Sel {
        sel!(iconPicked:)
    }

    /// The selector the Open System Settings button sends.
    pub(super) fn access_action() -> objc2::runtime::Sel {
        sel!(openAccessSettings:)
    }
}

/// A plain `NSColor` for code that needs one (the window's background).
pub(super) fn ns_color(color: Rgb) -> Retained<NSColor> {
    paint::rgb(color)
}
