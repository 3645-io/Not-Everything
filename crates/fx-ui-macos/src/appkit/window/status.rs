//! The status bar: what [`crate::status_view::status_view`] decides, drawn
//! as the canvas's StatusBar. The counts on the left (the figure in bold), the selected
//! row's key hints or a note in the middle, and the index on the right: a
//! green dot with "Up to date · 9 ms", the progress bar while indexing, or
//! an error in `danger-ink`.
//!
//! The views are made once; an update sets the texts that changed and
//! moves the slots, and nothing redraws while the window is idle.
//!
//! VoiceOver reads each text in words ([`crate::accessibility`]): "8
//! results, 2,014,377 objects", "Up to date, 9 ms", and each hint as one
//! element, "Reveal in Finder, Command Return", with its caps hidden.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBezierPath, NSColor, NSGradient, NSResponder, NSTextAlignment, NSTextField, NSView,
};
use objc2_foundation::{NSArray, NSMutableAttributedString, NSPoint, NSRect, NSSize};

use super::views::{
    FlippedView, Flow, SurfaceView, color, hide_from_voiceover, label, place, place_line,
    set_spoken, set_text, styled, surface_image, text_width,
};
use crate::accessibility::{hint_label, spoken};
use crate::key_caps::RESULT_HINTS;
use crate::main_layout::{Rect, STATUS_LINE, hints, key_width, status_bar};
use crate::status_view::{IndexSlot, Middle, StatusView};
use crate::surface::Surface;
use crate::theme::{Token, metrics, styles};

/// One key cap.
struct KeyCap {
    surface: Retained<SurfaceView>,
    label: Retained<NSTextField>,
}

/// One hint: its key caps and what they do.
struct Hint {
    keys: Vec<KeyCap>,
    label: Retained<NSTextField>,
}

// --------------------------------------------------------- progress view

/// Instance variables of [`ProgressView`].
#[derive(Default)]
pub(crate) struct ProgressIvars {
    fraction: Cell<f64>,
}

define_class!(
    // SAFETY: NSView may be subclassed; `drawRect:` only draws.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FxProgressView"]
    #[ivars = ProgressIvars]
    pub(crate) struct ProgressView;

    impl ProgressView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        /// The 3645 ProgressBar: the pressed track, then the fill's
        /// gradient from `green-light` through `green` to `green-dark`,
        /// both pills. Drawn once a second at most, while indexing.
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let b = self.bounds();
            surface_image(Surface::ProgressTrack, b.size).draw(b, true);
            let fraction = self.ivars().fraction.get().clamp(0.0, 1.0);
            if fraction <= 0.0 {
                return;
            }
            let h = b.size.height;
            let fill = NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new((b.size.width * fraction).max(h), h),
            );
            let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(fill, h / 2.0, h / 2.0);
            let colors: Retained<NSArray<NSColor>> = NSArray::from_retained_slice(&[
                color(Token::GreenLight),
                color(Token::Green),
                color(Token::GreenDark),
            ]);
            if let Some(gradient) = NSGradient::initWithColors(NSGradient::alloc(), &colors) {
                gradient.drawInBezierPath_angle(&path, 0.0);
            }
        }
    }
);

impl ProgressView {
    fn new(mtm: MainThreadMarker) -> Retained<ProgressView> {
        let this = ProgressView::alloc(mtm).set_ivars(ProgressIvars::default());
        // SAFETY: `initWithFrame:` is NSView's designated initializer.
        unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] }
    }

    fn set_fraction(&self, fraction: f64) {
        if (self.ivars().fraction.replace(fraction) - fraction).abs() > 1e-4 {
            self.setNeedsDisplay(true);
        }
    }
}

// ------------------------------------------------------------- status bar

/// The status bar's views. See the module documentation.
pub(crate) struct StatusBar {
    view: Retained<FlippedView>,
    counts: Retained<NSTextField>,
    note: Retained<NSTextField>,
    hints: Retained<FlippedView>,
    hint_views: Vec<Hint>,
    hints_width: Cell<f64>,
    dot: Retained<SurfaceView>,
    index: Retained<NSTextField>,
    progress: Retained<ProgressView>,
    /// What is shown, to skip unchanged updates.
    shown: RefCell<Option<StatusView>>,
    height: Cell<f64>,
}

impl StatusBar {
    /// Builds the (empty) status bar.
    pub(crate) fn new(mtm: MainThreadMarker) -> StatusBar {
        let view = FlippedView::new(mtm);
        let counts = label(mtm);
        let note = label(mtm);
        let hints_view = FlippedView::new(mtm);
        let hint_views = RESULT_HINTS
            .iter()
            .map(|h| {
                let keys = h
                    .keys
                    .iter()
                    .map(|_| {
                        let surface = SurfaceView::new(mtm);
                        let label = label(mtm);
                        label.setAlignment(NSTextAlignment::Center);
                        // The hint's label reads its keys in words.
                        hide_from_voiceover(&label);
                        hints_view.addSubview(&surface);
                        hints_view.addSubview(&label);
                        KeyCap { surface, label }
                    })
                    .collect();
                let label = label(mtm);
                set_spoken(&label, &hint_label(h));
                hints_view.addSubview(&label);
                Hint { keys, label }
            })
            .collect();
        let dot = SurfaceView::new(mtm);
        let index = label(mtm);
        let progress = ProgressView::new(mtm);
        for v in [&*counts, &*note] {
            view.addSubview(v);
        }
        view.addSubview(&hints_view);
        view.addSubview(&dot);
        view.addSubview(&progress);
        view.addSubview(&index);
        StatusBar {
            view,
            counts,
            note,
            hints: hints_view,
            hint_views,
            hints_width: Cell::new(0.0),
            dot,
            index,
            progress,
            shown: RefCell::new(None),
            height: Cell::new(
                metrics::STATUS_PADDING[0] + STATUS_LINE + metrics::STATUS_PADDING[2],
            ),
        }
    }

    /// Adds the bar to `root`.
    pub(crate) fn attach(&self, root: &NSView) {
        root.addSubview(&self.view);
    }

    /// The bar's height for its current content.
    pub(crate) fn height(&self) -> f64 {
        self.height.get()
    }

    /// Sets the key caps' and hints' texts for the current theme.
    pub(crate) fn restyle(&self) {
        let center = Flow::Line(NSTextAlignment::Center);
        let left = Flow::Line(NSTextAlignment::Left);
        let mut items = Vec::new();
        for (hint, spec) in self.hint_views.iter().zip(RESULT_HINTS) {
            let mut widths = Vec::new();
            for (key, cap) in hint.keys.iter().zip(spec.keys) {
                set_text(&key.label, cap, &styles::KEY_CAP, Token::InkStrong, center);
                widths.push(key_width(text_width(&key.label)));
            }
            set_text(
                &hint.label,
                spec.label,
                &styles::STATUS_TEXT,
                Token::Muted,
                left,
            );
            items.push((widths, text_width(&hint.label)));
        }
        let l = hints(&items);
        for ((hint, (widths, label_w)), (keys_x, label_x)) in self
            .hint_views
            .iter()
            .zip(&items)
            .zip(l.keys.iter().zip(&l.labels))
        {
            for ((key, w), x) in hint.keys.iter().zip(widths).zip(keys_x) {
                let r = Rect::new(*x, 0.0, *w, metrics::KEY_HEIGHT);
                key.surface.show(Some(Surface::Key), r);
                place_line(&key.label, r.x, 3.0, r.width, 16.0);
            }
            place_line(&hint.label, *label_x, 2.0, *label_w, 18.0);
        }
        self.hints_width.set(l.width);
        self.shown.replace(None);
    }

    /// Shows `status` in a bar `width` points wide at `y` in the window;
    /// returns whether its height changed.
    pub(crate) fn update(&self, status: &StatusView, width: f64, y: f64) -> bool {
        let unchanged = self.shown.borrow().as_ref() == Some(status)
            && (self.view.frame().size.width - width).abs() < 0.5;
        if unchanged {
            place(&self.view, Rect::new(0.0, y, width, self.height.get()));
            return false;
        }
        let left = Flow::Line(NSTextAlignment::Left);

        // Counts: the figure in bold `ink-strong`, the rest `muted`.
        let counts = NSMutableAttributedString::from_attributed_nsstring(&styled(
            &status.counts.figure,
            &styles::STATUS_FIGURE,
            Token::InkStrong,
            left,
        ));
        counts.appendAttributedString(&styled(
            &status.counts.rest,
            &styles::STATUS_TEXT,
            Token::Muted,
            left,
        ));
        self.counts.setAttributedStringValue(&counts);
        set_spoken(&self.counts, &spoken(&status.counts.text()));
        let counts_w = text_width(&self.counts);

        let middle_w = match &status.middle {
            Middle::Empty => {
                self.note.setHidden(true);
                self.hints.setHidden(true);
                None
            }
            Middle::Hints(_) => {
                self.note.setHidden(true);
                self.hints.setHidden(false);
                Some(self.hints_width.get())
            }
            Middle::Note(text) => {
                set_text(&self.note, text, &styles::STATUS_TEXT, Token::Muted, left);
                set_spoken(&self.note, &spoken(text));
                self.note.setHidden(false);
                self.hints.setHidden(true);
                Some(text_width(&self.note))
            }
        };

        // The index slot: a lead (dot or bar), its gap, then the text.
        let (text, token, lead) = match &status.index {
            IndexSlot::UpToDate(text) => (text, Token::Muted, Some((metrics::DOT_SIZE, 8.0))),
            IndexSlot::Indexing { text, progress } => (
                text,
                Token::Muted,
                progress.map(|_| (metrics::PROGRESS_WIDTH, metrics::PROGRESS_GAP)),
            ),
            IndexSlot::Error(text) => (text, Token::DangerInk, None),
        };
        set_text(&self.index, text, &styles::STATUS_INDEX, token, left);
        set_spoken(&self.index, &spoken(text));
        let text_w = text_width(&self.index);
        let index_w = lead.map_or(0.0, |(w, gap)| w + gap) + text_w;

        let l = status_bar(width, counts_w, middle_w, index_w);
        let grew = (self.height.replace(l.height) - l.height).abs() > 0.5;
        place(&self.view, Rect::new(0.0, y, width, l.height));
        place_line(&self.counts, l.counts.x, l.counts.y, counts_w, STATUS_LINE);
        if let Some(m) = l.middle {
            place_line(&self.note, m.x, m.y, m.width, STATUS_LINE);
            place(
                &self.hints,
                Rect::new(m.x, m.y, self.hints_width.get(), metrics::KEY_HEIGHT),
            );
        }
        let i = l.index;
        let mut x = i.x;
        self.dot.setHidden(true);
        self.progress.setHidden(true);
        match &status.index {
            IndexSlot::UpToDate(_) => {
                let d = metrics::DOT_SIZE;
                self.dot.show(
                    Some(Surface::Dot),
                    Rect::new(x, i.y + (i.height - d) / 2.0, d, d),
                );
                x += d + 8.0;
            }
            IndexSlot::Indexing {
                progress: Some(p), ..
            } => {
                let h = metrics::PROGRESS_HEIGHT;
                place(
                    &self.progress,
                    Rect::new(x, i.y + (i.height - h) / 2.0, metrics::PROGRESS_WIDTH, h),
                );
                self.progress.set_fraction(*p);
                self.progress.setHidden(false);
                x += metrics::PROGRESS_WIDTH + metrics::PROGRESS_GAP;
            }
            _ => {}
        }
        place_line(&self.index, x, i.y, text_w, STATUS_LINE);
        self.shown.replace(Some(status.clone()));
        grew
    }
}
