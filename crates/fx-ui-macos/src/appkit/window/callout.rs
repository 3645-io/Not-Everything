//! The Full Disk Access callout above the results (the canvas's
//! AccessBanner): a 3645 caution Callout with "Grant Full Disk Access…"
//! (primary) and "Not Now". When it shows is the onboarding's call.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, sel};
use objc2_app_kit::{NSTextAlignment, NSTextField, NSView};
use objc2_foundation::NSMutableAttributedString;

use super::views::{
    FlippedView, Flow, HitButton, SurfaceView, label, place, place_line, place_text, set_text,
    styled, text_height, text_width, wrapping_label,
};
use crate::main_layout::{BannerLayout, Rect, banner};
use crate::surface::Surface;
use crate::theme::{Token, metrics, styles};
use crate::ui_text::{BANNER_GRANT, BANNER_LEAD, BANNER_NOT_NOW, BANNER_TEXT};
use crate::window_view::button_surface;

/// One of the callout's buttons.
struct Button {
    primary: bool,
    surface: Retained<SurfaceView>,
    label: Retained<NSTextField>,
    hit: Retained<HitButton>,
}

/// The callout's views. See the module documentation.
pub(crate) struct Callout {
    view: Retained<FlippedView>,
    surface: Retained<SurfaceView>,
    text: Retained<NSTextField>,
    buttons: [Button; 2],
    layout: std::cell::Cell<Option<BannerLayout>>,
}

impl Callout {
    /// Builds the (hidden) callout; its buttons send `grantAccess:` and
    /// `dismissBanner:` to `target`.
    pub(crate) fn new(mtm: MainThreadMarker, target: &AnyObject) -> Callout {
        let view = FlippedView::new(mtm);
        let surface = SurfaceView::new(mtm);
        let text = wrapping_label(mtm);
        view.addSubview(&surface);
        view.addSubview(&text);
        let make = |primary: bool, title: &str, action| {
            let surface = SurfaceView::new(mtm);
            let label = label(mtm);
            label.setAlignment(NSTextAlignment::Center);
            let hit = HitButton::new(mtm, target, action, 0, title, false, false);
            view.addSubview(&surface);
            view.addSubview(&label);
            view.addSubview(&hit);
            Button {
                primary,
                surface,
                label,
                hit,
            }
        };
        let buttons = [
            make(true, BANNER_GRANT, sel!(grantAccess:)),
            make(false, BANNER_NOT_NOW, sel!(dismissBanner:)),
        ];
        view.setHidden(true);
        Callout {
            view,
            surface,
            text,
            buttons,
            layout: std::cell::Cell::new(None),
        }
    }

    /// Adds the callout to `root`.
    pub(crate) fn attach(&self, root: &NSView) {
        root.addSubview(&self.view);
    }

    /// Whether the callout shows.
    pub(crate) fn is_shown(&self) -> bool {
        !self.view.isHidden()
    }

    /// Shows or hides the callout.
    pub(crate) fn set_shown(&self, shown: bool) {
        self.view.setHidden(!shown);
    }

    /// Sets the text and the buttons' labels for the current theme.
    pub(crate) fn restyle(&self) {
        let text = NSMutableAttributedString::from_attributed_nsstring(&styled(
            BANNER_LEAD,
            &styles::BANNER_LEAD,
            Token::CautionInk,
            Flow::Wrap,
        ));
        text.appendAttributedString(&styled(
            &format!(" {BANNER_TEXT}"),
            &styles::BANNER_TEXT,
            Token::CautionInk,
            Flow::Wrap,
        ));
        self.text.setAttributedStringValue(&text);
        for b in &self.buttons {
            let (title, token) = if b.primary {
                (BANNER_GRANT, Token::OnGreen)
            } else {
                (BANNER_NOT_NOW, Token::InkStrong)
            };
            set_text(
                &b.label,
                title,
                &styles::BUTTON,
                token,
                Flow::Line(NSTextAlignment::Center),
            );
        }
    }

    /// The callout's height in a window `width` points wide, laid out at
    /// `rect`'s origin with that width.
    pub(crate) fn layout(&self, rect: Rect) -> f64 {
        let pad = 2.0 * (metrics::BUTTON_PADDING[1] + metrics::SELECTED_BORDER);
        let widths = [0, 1].map(|i| text_width(&self.buttons[i].label) + pad);
        let text = &self.text;
        let l = banner(rect.width, |w| text_height(text, w), widths);
        self.layout.set(Some(l));
        let frame = Rect::new(rect.x, rect.y, rect.width, l.height);
        place(&self.view, frame);
        self.surface.show(
            Some(Surface::Callout),
            Rect::new(0.0, 0.0, rect.width, l.height),
        );
        place_text(&self.text, l.text);
        for (b, r) in self.buttons.iter().zip(l.buttons) {
            place(&b.hit, r);
            place_line(&b.label, r.x, r.y + (r.height - 20.0) / 2.0, r.width, 20.0);
        }
        self.refresh_buttons();
        l.height
    }

    /// The buttons' surfaces, pressed while held (`.btn:active`).
    pub(crate) fn refresh_buttons(&self) {
        let Some(l) = self.layout.get() else { return };
        for (b, r) in self.buttons.iter().zip(l.buttons) {
            b.surface
                .show(Some(button_surface(b.primary, b.hit.pressed())), r);
        }
    }
}
