//! The Settings window: the canvas's Settings board in Light and Dark,
//! drawn with the 3645 surfaces.
//!
//! A top bar with only the title, under the traffic lights, over a
//! scrolling sheet of raised cards (General, Search, Appearance, App Icon,
//! Full Disk Access) and a footnote with the version. What each row says is
//! [`crate::settings_view`]; where everything goes is
//! [`crate::settings_view::layout`]; the views and controls are in
//! `settings_controls.rs`. The window is 640 points wide, as tall as its
//! contents up to the screen's height (then the sheet scrolls), and can be
//! made shorter.
//!
//! Every change applies and is saved at once; there is no OK button. The
//! window is built the first time it is shown and only hidden when closed;
//! closing it releases the six icon images it decoded (only the current
//! theme's). It follows the app's effective appearance, observed with
//! key-value observing, which also sets the Dock icon
//! (`NSApp.applicationIconImage`, [`dock_tile`]: the chosen design and, in
//! a release candidate or beta, its corner ribbon, drawn by `ribbon.rs`).
//! While it shows, it follows the login item and Full Disk Access too:
//! the menu-bar menu's Launch at Login item and the Settings switch change
//! the same login item, and each shows the other's change.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;

use fx_channel::Channel;
use fx_fs::AccessState;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{AnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAccessibility, NSAccessibilityImageRole, NSAccessibilityRadioGroupRole, NSAppearance,
    NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication, NSBackingStoreType, NSImage,
    NSScreen, NSScrollView, NSTextAlignment, NSTitlebarSeparatorStyle, NSView, NSWindow,
    NSWindowButton, NSWindowCollectionBehavior, NSWindowStyleMask, NSWindowTitleVisibility,
};
use objc2_foundation::{
    NSArray, NSData, NSEdgeInsets, NSKeyValueObservingOptions,
    NSObjectNSKeyValueObserverRegistration, NSPoint, NSRect, NSSize, NSString, ns_string,
};

use super::classes::WinController;
use super::paint;
use super::resident::activate;
use super::ribbon::with_ribbon;
use super::settings_controls::{
    ChoiceControl, ChoiceLooks, FlippedView, Look, PushControl, PushLooks, SettingsTarget,
    SurfaceView, SwitchControl, SwitchLooks, TextView, keep_unclipped, ns_color, ns_rect,
};
use super::surfaces::SurfaceCache;
use crate::APP_NAME;
use crate::app_icon::{AppIcon, DockIcon};
use crate::appearance::AppearanceMode;
use crate::appkit::login_item::{self, LoginItemStatus};
use crate::build_info::BuildInfo;
use crate::dock_ribbon::{DockTile, dock_tile};
use crate::key_caps::{key_caps, spoken};
use crate::settings::Settings;
use crate::settings_view::layout::{self, Frame, Sheet, TopBar};
use crate::settings_view::{
    APPEARANCE_NOTE, HOTKEY_CAPTION, HOTKEY_LABEL, ICON_LABEL, LOGIN_LABEL, LoginState, SECTIONS,
    TITLE, YOUR_FILES_FIRST_LABEL, access_row, appearance_segments, footnote, icon_note,
    icon_tiles, login_row, your_files_first_row,
};
use crate::surface::Surface;
use crate::theme::{RADIUS_SM, Rgba, Theme, Token, metrics, styles};

/// The window's width: the sheet's widest.
const WIDTH: f64 = metrics::SETTINGS_WIDTH;
/// The shortest the user can make the sheet's visible part.
const MIN_SHEET: f64 = 240.0;
/// Where the traffic lights' middle is when the window can't say: the
/// board's (10 + 24 / 2).
const BOARD_LIGHTS_CENTER: f64 = 22.0;
/// The Dock icon image size: the Dock's largest tile.
const DOCK_IMAGE: f64 = 128.0;

/// The key path the window controller observes on `NSApp`.
fn appearance_key() -> &'static NSString {
    ns_string!("effectiveAppearance")
}

/// What the window shows: the settings and the states it follows.
struct ViewState {
    settings: Settings,
    login: LoginState,
    login_error: Option<String>,
    access: AccessState,
    theme: Theme,
}

/// A row's name and caption.
struct RowText {
    label: Retained<TextView>,
    caption: Retained<TextView>,
}

impl RowText {
    fn new(mtm: MainThreadMarker, sheet: &NSView) -> RowText {
        let row = RowText {
            label: TextView::new(mtm, true),
            caption: TextView::new(mtm, true),
        };
        sheet.addSubview(&row.label);
        sheet.addSubview(&row.caption);
        row
    }

    /// Shows the name and caption (an error in `danger-ink`).
    fn set(&self, theme: Theme, label: &str, caption: &str, error: bool) {
        let left = NSTextAlignment::Left;
        self.label.set(
            label,
            &styles::SETTING_LABEL,
            theme.color(Token::InkStrong),
            left,
        );
        let tone = if error {
            Token::DangerInk
        } else {
            Token::Muted
        };
        self.caption
            .set(caption, &styles::SETTING_CAPTION, theme.color(tone), left);
    }

    /// The caption's height beside a control `control_width` wide.
    fn caption_height(&self, content_width: f64, control_width: f64) -> f64 {
        self.caption
            .height(layout::row_text_width(content_width, control_width))
    }

    /// Places the name at `text`'s top (indented by `indent`) and the
    /// caption under it.
    fn place(&self, text: Frame, caption_height: f64, indent: f64) {
        let line = styles::SETTING_LABEL.line_height;
        self.label.place(Frame::new(
            text.x + indent,
            text.y,
            text.width - indent,
            line,
        ));
        self.caption.place(Frame::new(
            text.x,
            text.y + line + layout::CAPTION_GAP,
            text.width,
            caption_height,
        ));
    }
}

/// An icon pick and the views inside it.
struct Pick {
    icon: AppIcon,
    button: Retained<ChoiceControl>,
    image: Retained<SurfaceView>,
    name: Retained<TextView>,
    tag: Option<(Retained<SurfaceView>, Retained<TextView>, &'static str)>,
}

/// The six icons of one theme, in [`AppIcon::ALL`]'s order (`None` for
/// one that would not decode).
type Icons = (Theme, Vec<Option<Retained<NSImage>>>);

/// A key cap: its surface and label.
struct Cap {
    surface: Retained<SurfaceView>,
    label: Retained<TextView>,
}

/// The Settings window's views.
#[derive(Clone)]
pub(crate) struct SettingsUi(Rc<Ui>);

struct Ui {
    mtm: MainThreadMarker,
    window: Retained<NSWindow>,
    _target: Retained<SettingsTarget>,
    cache: SurfaceCache,
    root: Retained<FlippedView>,
    top_bar: Retained<SurfaceView>,
    title: Retained<TextView>,
    scroll: Retained<NSScrollView>,
    sheet: Retained<FlippedView>,
    cards: Vec<Retained<SurfaceView>>,
    card_titles: Vec<Retained<TextView>>,
    hotkey: RowText,
    keys: Retained<FlippedView>,
    caps: RefCell<Vec<Cap>>,
    login: RowText,
    login_divider: Retained<SurfaceView>,
    login_switch: Retained<SwitchControl>,
    files: RowText,
    files_switch: Retained<SwitchControl>,
    segments_surface: Retained<SurfaceView>,
    segments_group: Retained<FlippedView>,
    segments: Vec<(AppearanceMode, Retained<ChoiceControl>, Retained<TextView>)>,
    appearance_note: Retained<TextView>,
    icons_group: Retained<FlippedView>,
    picks: Vec<Pick>,
    icon_note: Retained<TextView>,
    access_dot: Retained<SurfaceView>,
    access: RowText,
    access_button: Retained<PushControl>,
    access_button_label: Retained<TextView>,
    footnote: Retained<TextView>,
    /// The six icons of one theme, decoded while the window shows.
    icons: RefCell<Option<Icons>>,
    bar: Cell<TopBar>,
    sheet_height: Cell<f64>,
    /// The tallest the window was last allowed to be.
    max_height: Cell<f64>,
}

impl SettingsUi {
    /// Builds the (hidden) window for `controller`.
    fn new(mtm: MainThreadMarker, controller: &WinController) -> SettingsUi {
        let target = SettingsTarget::new(mtm, controller);
        let t: &AnyObject = target.as_ref();
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable
            | NSWindowStyleMask::Resizable
            | NSWindowStyleMask::FullSizeContentView;
        // SAFETY: valid frame, style and backing; the window is released
        // only through its Retained handle (see setReleasedWhenClosed).
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::ZERO, NSSize::new(WIDTH, 600.0)),
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: we own `window` through a Retained, so AppKit must not
        // also release it on close.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str(TITLE));
        window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
        window.setTitlebarAppearsTransparent(true);
        window.setTitlebarSeparatorStyle(NSTitlebarSeparatorStyle::None);
        window.setCollectionBehavior(NSWindowCollectionBehavior::FullScreenNone);
        window.setAutorecalculatesKeyViewLoop(true);
        window.setDelegate(Some(ProtocolObject::from_ref(&*target)));

        let root = FlippedView::new(mtm);
        window.setContentView(Some(&root));
        let scroll = NSScrollView::new(mtm);
        scroll.setDrawsBackground(false);
        scroll.setHasVerticalScroller(true);
        scroll.setAutohidesScrollers(true);
        scroll.setAutomaticallyAdjustsContentInsets(false);
        scroll.setContentInsets(NSEdgeInsets {
            top: 0.0,
            left: 0.0,
            bottom: 0.0,
            right: 0.0,
        });
        let sheet = FlippedView::new(mtm);
        scroll.setDocumentView(Some(&sheet));
        root.addSubview(&scroll);
        let top_bar = SurfaceView::new(mtm);
        root.addSubview(&top_bar);
        let title = TextView::new(mtm, false);
        root.addSubview(&title);

        let cards: Vec<Retained<SurfaceView>> = SECTIONS
            .iter()
            .map(|_| {
                let card = SurfaceView::new(mtm);
                sheet.addSubview(&card);
                card
            })
            .collect();
        let card_titles: Vec<Retained<TextView>> = SECTIONS
            .iter()
            .map(|_| {
                let title = TextView::new(mtm, true);
                sheet.addSubview(&title);
                title
            })
            .collect();

        // General.
        let hotkey = RowText::new(mtm, &sheet);
        let keys = FlippedView::new(mtm);
        keys.setAccessibilityElement(true);
        // SAFETY: the role is an immutable framework constant.
        keys.setAccessibilityRole(Some(unsafe { NSAccessibilityImageRole }));
        sheet.addSubview(&keys);
        let login_divider = SurfaceView::new(mtm);
        sheet.addSubview(&login_divider);
        let login = RowText::new(mtm, &sheet);
        let login_switch = SwitchControl::new(mtm, LOGIN_LABEL, t, SettingsTarget::login_action());
        sheet.addSubview(&login_switch);

        // Search.
        let files = RowText::new(mtm, &sheet);
        let files_switch = SwitchControl::new(
            mtm,
            YOUR_FILES_FIRST_LABEL,
            t,
            SettingsTarget::your_files_first_action(),
        );
        sheet.addSubview(&files_switch);

        // Appearance.
        let segments_surface = SurfaceView::new(mtm);
        sheet.addSubview(&segments_surface);
        let segments_group = FlippedView::new(mtm);
        segments_group.setAccessibilityElement(true);
        // SAFETY: the role is an immutable framework constant.
        segments_group.setAccessibilityRole(Some(unsafe { NSAccessibilityRadioGroupRole }));
        segments_group.setAccessibilityLabel(Some(&NSString::from_str(SECTIONS[2])));
        sheet.addSubview(&segments_group);
        let segments = appearance_segments()
            .enumerate()
            .map(|(n, (mode, title))| {
                let button = ChoiceControl::new(
                    mtm,
                    title,
                    isize::try_from(n).unwrap_or(-1),
                    false,
                    t,
                    SettingsTarget::appearance_action(),
                );
                let label = TextView::new(mtm, false);
                button.addSubview(&label);
                segments_group.addSubview(&button);
                (mode, button, label)
            })
            .collect();
        let appearance_note = TextView::new(mtm, true);
        sheet.addSubview(&appearance_note);

        // App Icon.
        let icons_group = FlippedView::new(mtm);
        icons_group.setAccessibilityElement(true);
        // SAFETY: the role is an immutable framework constant.
        icons_group.setAccessibilityRole(Some(unsafe { NSAccessibilityRadioGroupRole }));
        icons_group.setAccessibilityLabel(Some(&NSString::from_str(ICON_LABEL)));
        sheet.addSubview(&icons_group);
        let picks = icon_tiles()
            .enumerate()
            .map(|(n, tile)| {
                let spoken = match tile.badge {
                    Some(badge) => format!("{}, {badge}", tile.title),
                    None => tile.title.to_owned(),
                };
                let button = ChoiceControl::new(
                    mtm,
                    &spoken,
                    isize::try_from(n).unwrap_or(-1),
                    true,
                    t,
                    SettingsTarget::icon_action(),
                );
                let image = SurfaceView::new(mtm);
                button.addSubview(&image);
                let name = TextView::new(mtm, false);
                button.addSubview(&name);
                let tag = tile.badge.map(|badge| {
                    let surface = SurfaceView::new(mtm);
                    button.addSubview(&surface);
                    let label = TextView::new(mtm, false);
                    button.addSubview(&label);
                    (surface, label, badge)
                });
                icons_group.addSubview(&button);
                Pick {
                    icon: tile.icon,
                    button,
                    image,
                    name,
                    tag,
                }
            })
            .collect();
        let icon_note = TextView::new(mtm, true);
        sheet.addSubview(&icon_note);

        // Full Disk Access.
        let access_dot = SurfaceView::new(mtm);
        sheet.addSubview(&access_dot);
        let access = RowText::new(mtm, &sheet);
        let button_surface = SurfaceView::new(mtm);
        sheet.addSubview(&button_surface);
        let access_button = PushControl::new(
            mtm,
            crate::settings_view::ACCESS_SETTINGS_BUTTON,
            button_surface,
            t,
            SettingsTarget::access_action(),
        );
        let access_button_label = TextView::new(mtm, false);
        access_button.addSubview(&access_button_label);
        sheet.addSubview(&access_button);

        let footnote = TextView::new(mtm, true);
        sheet.addSubview(&footnote);

        for view in [&*root, &*sheet, &*keys, &*segments_group, &*icons_group] {
            keep_unclipped(view);
        }

        SettingsUi(Rc::new(Ui {
            mtm,
            window,
            _target: target,
            cache: SurfaceCache::new(),
            root,
            top_bar,
            title,
            scroll,
            sheet,
            cards,
            card_titles,
            hotkey,
            keys,
            caps: RefCell::new(Vec::new()),
            login,
            login_divider,
            login_switch,
            files,
            files_switch,
            segments_surface,
            segments_group,
            segments,
            appearance_note,
            icons_group,
            picks,
            icon_note,
            access_dot,
            access,
            access_button,
            access_button_label,
            footnote,
            icons: RefCell::new(None),
            bar: Cell::new(layout::top_bar(BOARD_LIGHTS_CENTER)),
            sheet_height: Cell::new(0.0),
            max_height: Cell::new(0.0),
        }))
    }

    /// Whether the window is on screen (shown and not closed).
    pub(super) fn is_visible(&self) -> bool {
        self.0.window.isVisible()
    }

    /// Shows `state`.
    fn update(&self, state: &ViewState) {
        let ui = &*self.0;
        let theme = state.theme;
        ui.window
            .setBackgroundColor(Some(&ns_color(theme.color(Token::Ground))));
        ui.ensure_icons(theme);
        ui.set_texts(state);
        ui.layout(state);
        ui.layout_root();
        for other in Theme::ALL.into_iter().filter(|&t| t != theme) {
            ui.cache.forget_theme(other);
        }
    }

    /// Sizes the window for its contents, at most the screen's height,
    /// keeping its top edge; the `first` time, as tall as allowed.
    fn fit_window(&self, first: bool) {
        self.0.fit_window(first);
    }

    /// Lets the icon images go (the window closed).
    fn release_icons(&self) {
        let ui = &*self.0;
        *ui.icons.borrow_mut() = None;
        for pick in &ui.picks {
            pick.image.set_look(Look::None);
            pick.button.reset_hover();
        }
    }
}

impl Ui {
    /// Decodes the six icons for `theme`, dropping the other theme's.
    fn ensure_icons(&self, theme: Theme) {
        let mut icons = self.icons.borrow_mut();
        if icons.as_ref().is_some_and(|(t, _)| *t == theme) {
            return;
        }
        let images = AppIcon::ALL
            .iter()
            .map(|icon| decode(icon.png(theme.is_dark())))
            .collect();
        *icons = Some((theme, images));
    }

    /// `surface` in `theme` for an element `size` points large: a shared
    /// image, or layer properties for a style without shadows.
    fn look(&self, surface: Surface, theme: Theme, size: (f64, f64)) -> Look {
        let style = surface.style(theme);
        if style.needs_image() {
            Look::Surface(
                self.cache
                    .surface(surface, theme, NSSize::new(size.0, size.1)),
            )
        } else {
            Look::Flat {
                fill: style.fill,
                border: style.border.map(|b| (b.width, b.color)),
                radius: style.corners.clamped(size.0, size.1).max(),
            }
        }
    }

    fn set_texts(&self, state: &ViewState) {
        let theme = state.theme;
        let c = |t: Token| theme.color(t);
        let (left, center) = (NSTextAlignment::Left, NSTextAlignment::Center);
        self.title
            .set(TITLE, &styles::WINDOW_TITLE, c(Token::InkStrong), center);
        for (title, text) in self.card_titles.iter().zip(SECTIONS) {
            title.set(text, &styles::SECTION_TITLE, c(Token::Muted), left);
        }
        self.hotkey.set(theme, HOTKEY_LABEL, HOTKEY_CAPTION, false);
        let login = login_row(APP_NAME, state.login, state.login_error.as_deref());
        self.login
            .set(theme, LOGIN_LABEL, &login.caption, login.error);
        let files = your_files_first_row(state.settings.your_files_first);
        self.files
            .set(theme, YOUR_FILES_FIRST_LABEL, &files.caption, files.error);
        self.appearance_note.set(
            APPEARANCE_NOTE,
            &styles::SETTING_NOTE,
            c(Token::Muted),
            left,
        );
        for (mode, _, label) in &self.segments {
            let on = *mode == state.settings.appearance;
            let tone = if on { Token::OnGreen } else { Token::Muted };
            label.set(mode.title(), &styles::SEGMENT, c(tone), center);
        }
        for pick in &self.picks {
            pick.name.set(
                pick.icon.title(),
                &styles::ICON_NAME,
                c(Token::InkStrong),
                center,
            );
            if let Some((_, label, text)) = &pick.tag {
                label.set(text, &styles::TAG, c(Token::GreenInk), center);
            }
        }
        self.icon_note.set(
            &icon_note(APP_NAME),
            &styles::SETTING_NOTE,
            c(Token::Muted),
            left,
        );
        let access = access_row(state.access);
        self.access.set(theme, access.label, access.caption, false);
        let tone = if access.primary {
            Token::OnGreen
        } else {
            Token::InkStrong
        };
        self.access_button_label
            .set(access.button, &styles::BUTTON, c(tone), center);
        self.footnote.set(
            &footnote(BuildInfo::CURRENT),
            &styles::FOOTNOTE,
            c(Token::Muted),
            center,
        );
        let caps = key_caps(state.settings.hotkey);
        self.keys
            .setAccessibilityLabel(Some(&NSString::from_str(&spoken(state.settings.hotkey))));
        let mut views = self.caps.borrow_mut();
        if views.len() != caps.len() {
            for cap in views.drain(..) {
                cap.surface.removeFromSuperview();
                cap.label.removeFromSuperview();
            }
            for _ in &caps {
                let surface = SurfaceView::new(self.mtm);
                self.keys.addSubview(&surface);
                views.push(Cap {
                    surface,
                    label: TextView::new(self.mtm, false),
                });
            }
            for cap in views.iter() {
                self.keys.addSubview(&cap.label);
            }
        }
        for (cap, text) in views.iter().zip(&caps) {
            cap.label
                .set(text, &styles::KEY_CAP, c(Token::InkStrong), center);
        }
    }

    /// Places every view for `state` (its text is set).
    fn layout(&self, state: &ViewState) {
        let theme = state.theme;
        let focus = Some(Rgba::opaque(theme.color(Token::GreenInk)));
        let bar = layout::top_bar(self.lights_center());
        self.bar.set(bar);
        let mut sheet = Sheet::new(WIDTH);

        // General: the hotkey's caps and Launch at Login.
        let mut card = sheet.card();
        self.card_titles[0].place(card.title());
        let (cx, cw) = card.content();
        let caps = self.caps.borrow();
        let widths: Vec<f64> = caps.iter().map(|c| c.label.width()).collect();
        let keys_width = layout::key_caps_width(&widths);
        let caption = self.hotkey.caption_height(cw, keys_width);
        let row = card.row(
            layout::text_block_height(caption),
            (keys_width, metrics::KEY_HEIGHT),
        );
        self.hotkey.place(row.text, caption, 0.0);
        let (group, frames) = layout::key_caps(row.control.right(), row.control.y, &widths);
        self.keys.setFrame(ns_rect(group));
        for (cap, frame) in caps.iter().zip(frames) {
            let local = relative(frame, group);
            cap.surface.set(
                local,
                self.look(Surface::Key, theme, (frame.width, frame.height)),
            );
            let line = styles::KEY_CAP.line_height;
            cap.label.place(Frame::new(
                local.x,
                local.y + (local.height - line) / 2.0,
                local.width,
                line,
            ));
        }
        drop(caps);
        let login = login_row(APP_NAME, state.login, state.login_error.as_deref());
        let (sw, sh) = layout::SWITCH_SIZE;
        let caption = self.login.caption_height(cw, sw);
        let row = card.row(layout::text_block_height(caption), (sw, sh));
        if let Some(divider) = row.divider {
            self.login_divider
                .set(divider, flat(theme, Token::Line, 0.0));
        }
        self.login.place(row.text, caption, 0.0);
        self.login_switch.setFrame(ns_rect(row.control));
        self.login_switch
            .set(login.on, login.enabled, self.switch_looks(theme));
        self.card(0, sheet.end(card), theme);

        // Search: Your files first.
        let mut card = sheet.card();
        self.card_titles[1].place(card.title());
        let files = your_files_first_row(state.settings.your_files_first);
        let caption = self.files.caption_height(cw, sw);
        let row = card.row(layout::text_block_height(caption), (sw, sh));
        self.files.place(row.text, caption, 0.0);
        self.files_switch.setFrame(ns_rect(row.control));
        self.files_switch
            .set(files.on, files.enabled, self.switch_looks(theme));
        self.card(1, sheet.end(card), theme);

        // Appearance: the segmented control and its note.
        let mut card = sheet.card();
        self.card_titles[2].place(card.title());
        let widths: Vec<f64> = self.segments.iter().map(|(_, _, l)| l.width()).collect();
        let (size, _) = layout::segments(0.0, 0.0, &widths);
        let top = card.block(size.height);
        let (container, buttons) = layout::segments(cx, top, &widths);
        self.segments_surface.set(
            container,
            self.look(
                Surface::Segmented,
                theme,
                (container.width, container.height),
            ),
        );
        self.segments_group.setFrame(ns_rect(container));
        for ((mode, button, label), frame) in self.segments.iter().zip(buttons) {
            let local = relative(frame, container);
            button.setFrame(ns_rect(local));
            label.place(relative(layout::segment_text(frame), frame));
            button.set(
                *mode == state.settings.appearance,
                ChoiceLooks {
                    off: Look::None,
                    hover: Look::None,
                    on: self.look(Surface::SegmentOn, theme, (frame.width, frame.height)),
                    radius: metrics::SEGMENT_RADIUS,
                    focus,
                },
            );
        }
        let note = self.appearance_note.height(cw);
        let top = card.note(note);
        self.appearance_note.place(Frame::new(cx, top, cw, note));
        self.card(2, sheet.end(card), theme);

        // App Icon: the picks and their note.
        let mut card = sheet.card();
        self.card_titles[3].place(card.title());
        let tagged: Vec<bool> = self.picks.iter().map(|p| p.tag.is_some()).collect();
        let (_, height) = layout::icon_grid(0.0, 0.0, cw, &tagged);
        let top = card.block(height);
        let grid = Frame::new(cx, top, cw, height);
        let (frames, _) = layout::icon_grid(cx, top, cw, &tagged);
        self.icons_group.setFrame(ns_rect(grid));
        let icons = self.icons.borrow();
        for (n, (pick, frame)) in self.picks.iter().zip(frames).enumerate() {
            let on = pick.icon == state.settings.app_icon;
            let local = relative(frame, grid);
            pick.button.setFrame(ns_rect(local));
            let inside = layout::icon_pick_content(Frame::new(0.0, 0.0, frame.width, frame.height));
            let picture = icons
                .as_ref()
                .and_then(|(_, images)| images.get(n)?.as_ref())
                .map_or(Look::None, |image| Look::Picture(image.clone()));
            pick.image.set(inside.icon, picture);
            pick.name.place(inside.name);
            if let Some((surface, label, _)) = &pick.tag {
                let (tw, th) = layout::tag_size(label.width());
                let tag = Frame::new((frame.width - tw) / 2.0, inside.tag_top, tw, th);
                // A tag on the chosen pick sits on `panel`, else on its wash.
                let fill = if on { Token::Panel } else { Token::GreenWash };
                surface.set(tag, flat(theme, fill, RADIUS_SM));
                let [py, px] = layout::TAG_PADDING;
                label.place(Frame::new(
                    tag.x + px,
                    tag.y + py,
                    tag.width - 2.0 * px,
                    styles::TAG.line_height,
                ));
            }
            let size = (frame.width, frame.height);
            pick.button.set(
                on,
                ChoiceLooks {
                    off: Look::None,
                    hover: self.look(Surface::IconPickHover, theme, size),
                    on: self.look(Surface::IconPickSelected, theme, size),
                    radius: crate::theme::RADIUS_LG,
                    focus,
                },
            );
        }
        drop(icons);
        let note = self.icon_note.height(cw);
        let top = card.note(note);
        self.icon_note.place(Frame::new(cx, top, cw, note));
        self.card(3, sheet.end(card), theme);

        // Full Disk Access: the dot, the state and the button.
        let mut card = sheet.card();
        self.card_titles[4].place(card.title());
        let access = access_row(state.access);
        let (bw, bh) = layout::button_size(self.access_button_label.width());
        let caption = self.access.caption_height(cw, bw);
        let row = card.row(layout::text_block_height(caption), (bw, bh));
        self.access
            .place(row.text, caption, layout::ACCESS_LABEL_INDENT);
        let font = paint::font(&styles::SETTING_LABEL);
        let dot = layout::access_dot(row.text.x, row.text.y, font.ascender(), -font.descender());
        let ring = metrics::DOT_RING;
        let (fill, rim) = if access.granted {
            (Token::Green, Token::GreenWash)
        } else {
            (Token::Edge, Token::Well)
        };
        let side = dot.width + 2.0 * ring;
        self.access_dot.set(
            Frame::new(dot.x - ring, dot.y - ring, side, side),
            Look::Flat {
                fill: Some(Rgba::opaque(theme.color(fill))),
                border: Some((ring, Rgba::opaque(theme.color(rim)))),
                radius: side / 2.0,
            },
        );
        let (rest, down) = if access.primary {
            (Surface::PrimaryButton, Surface::PrimaryButtonDown)
        } else {
            (Surface::Button, Surface::ButtonDown)
        };
        self.access_button.set(
            row.control,
            PushLooks {
                rest: self.look(rest, theme, (bw, bh)),
                down: self.look(down, theme, (bw, bh)),
                focus,
            },
        );
        self.access_button_label
            .place(relative(layout::button_text(row.control), row.control));
        self.card(4, sheet.end(card), theme);

        self.footnote.place(sheet.footnote());
        let height = sheet.height();
        self.sheet_height.set(height);
        self.sheet
            .setFrame(NSRect::new(NSPoint::ZERO, NSSize::new(WIDTH, height)));
        self.top_bar.set(
            Frame::new(0.0, 0.0, WIDTH, bar.height),
            self.look(Surface::TopBar, theme, (WIDTH, bar.height)),
        );
        let title = self.title.width();
        self.title.place(Frame::new(
            (WIDTH - title) / 2.0,
            bar.title_line_top(),
            title,
            styles::WINDOW_TITLE.line_height,
        ));
    }

    /// Shows card `n` at `frame`.
    fn card(&self, n: usize, frame: Frame, theme: Theme) {
        self.cards[n].set(
            frame,
            self.look(Surface::Panel, theme, (frame.width, frame.height)),
        );
    }

    fn switch_looks(&self, theme: Theme) -> SwitchLooks {
        let (w, h) = layout::SWITCH_SIZE;
        let k = metrics::KNOB_SIZE;
        let image = |s: Surface, size: f64, height: f64| {
            self.cache.surface(s, theme, NSSize::new(size, height))
        };
        SwitchLooks {
            track: [
                image(Surface::SwitchOff, w, h),
                image(Surface::SwitchOn, w, h),
            ],
            knob: [image(Surface::KnobOff, k, k), image(Surface::KnobOn, k, k)],
            focus: Rgba::opaque(theme.color(Token::GreenInk)),
        }
    }

    /// Where the traffic lights' middle is, down from the window's top.
    fn lights_center(&self) -> f64 {
        let Some(close) = self
            .window
            .standardWindowButton(NSWindowButton::CloseButton)
        else {
            return BOARD_LIGHTS_CENTER;
        };
        let r = close.convertRect_toView(close.bounds(), None);
        let center = self.window.frame().size.height - (r.origin.y + r.size.height / 2.0);
        if center.is_finite() && center > 0.0 {
            center
        } else {
            BOARD_LIGHTS_CENTER
        }
    }

    /// Lays out the top bar and the scroll view in the window's content.
    fn layout_root(&self) {
        let bounds = self.root.bounds();
        let bar = self.bar.get().height;
        self.scroll.setFrame(NSRect::new(
            NSPoint::new(0.0, bar),
            NSSize::new(bounds.size.width, (bounds.size.height - bar).max(0.0)),
        ));
    }

    fn fit_window(&self, first: bool) {
        let bar = self.bar.get().height;
        let screen = self
            .window
            .screen()
            .or_else(|| NSScreen::mainScreen(self.mtm))
            .map_or(f64::MAX, |s| s.visibleFrame().size.height);
        let max = layout::window_height(bar, self.sheet_height.get(), screen);
        let frame = self.window.frame();
        let at_max = first || frame.size.height >= self.max_height.get() - 0.5;
        self.max_height.set(max);
        let min = (bar + MIN_SHEET).min(max);
        self.window.setMinSize(NSSize::new(WIDTH, min));
        self.window.setMaxSize(NSSize::new(WIDTH, max));
        let height = if at_max {
            max
        } else {
            frame.size.height.clamp(min, max)
        };
        if (height - frame.size.height).abs() > 0.5 || frame.size.width != WIDTH {
            let top = frame.origin.y + frame.size.height;
            self.window.setFrame_display(
                NSRect::new(
                    NSPoint::new(frame.origin.x, top - height),
                    NSSize::new(WIDTH, height),
                ),
                true,
            );
        }
        self.layout_root();
    }
}

/// `frame` in a view whose frame is `parent` (both in the same space).
fn relative(frame: Frame, parent: Frame) -> Frame {
    Frame::new(
        frame.x - parent.x,
        frame.y - parent.y,
        frame.width,
        frame.height,
    )
}

/// A flat fill of `token`, `radius` round.
fn flat(theme: Theme, token: Token, radius: f64) -> Look {
    Look::Flat {
        fill: Some(Rgba::opaque(theme.color(token))),
        border: None,
        radius,
    }
}

/// An embedded PNG as an image.
fn decode(png: &[u8]) -> Option<Retained<NSImage>> {
    NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(png))
}

/// The login item's status as the Settings row shows it.
fn login_state(status: LoginItemStatus) -> LoginState {
    match status {
        LoginItemStatus::Enabled => LoginState::Enabled,
        LoginItemStatus::RequiresApproval => LoginState::NeedsApproval,
        LoginItemStatus::NotRegistered | LoginItemStatus::NotFound => LoginState::Off,
        LoginItemStatus::Unavailable => LoginState::Unavailable,
    }
}

impl WinController {
    /// Applies the saved appearance and Dock icon and starts observing the
    /// effective appearance. Called once by `start_resident`.
    pub(crate) fn start_look(&self) {
        let mtm = MainThreadMarker::from(self);
        let Some(mode) = self.with_resident(|r| r.settings.appearance) else {
            return;
        };
        apply_appearance(mtm, mode);
        let app = NSApplication::sharedApplication(mtm);
        // SAFETY: the controller lives as long as the app and implements
        // `observeValueForKeyPath:ofObject:change:context:`; it is removed
        // in `stop_look` before the app terminates. The context is unused.
        unsafe {
            app.addObserver_forKeyPath_options_context(
                self,
                appearance_key(),
                NSKeyValueObservingOptions::empty(),
                std::ptr::null_mut::<c_void>(),
            );
        }
        self.with_resident(|r| r.observing_appearance = true);
        self.refresh_look();
    }

    /// Stops observing the effective appearance (at termination).
    pub(crate) fn stop_look(&self) {
        let observing = self
            .with_resident(|r| std::mem::take(&mut r.observing_appearance))
            .unwrap_or(false);
        if observing {
            let app = NSApplication::sharedApplication(MainThreadMarker::from(self));
            // SAFETY: the controller was added for this key path in
            // `start_look` and not removed since.
            unsafe { app.removeObserver_forKeyPath(self, appearance_key()) };
        }
    }

    /// Sets the Dock icon (with a Preview build's ribbon) for the current
    /// settings and effective appearance, and the Settings window if it
    /// shows.
    pub(crate) fn refresh_look(&self) {
        let mtm = MainThreadMarker::from(self);
        let Some(settings) = self.with_resident(|r| r.settings) else {
            return;
        };
        set_dock_tile(
            mtm,
            dock_tile(
                Channel::CURRENT,
                settings.app_icon,
                settings.appearance,
                is_dark(mtm),
            ),
        );
        self.refresh_settings();
    }

    /// Shows the current settings, login item and Full Disk Access in the
    /// Settings window, if it is on screen (a hidden one is brought up to
    /// date when shown).
    pub(crate) fn refresh_settings(&self) {
        let Some(Some(ui)) = self.with_resident(|r| r.settings_ui.clone()) else {
            return;
        };
        if !ui.is_visible() {
            return;
        }
        self.sync_login_status();
        if let Some(state) = self.settings_state() {
            ui.update(&state);
            ui.fit_window(false);
        }
    }

    /// Reads the login item's status again (the user may have allowed or
    /// removed it in System Settings) and updates the menu if it changed.
    fn sync_login_status(&self) {
        let fresh = login_item::status();
        let changed = self
            .with_resident(|r| {
                let changed = r.login != fresh;
                if changed {
                    r.login = fresh;
                    r.login_error = None;
                }
                changed
            })
            .unwrap_or(false);
        if changed {
            self.rebuild_menu();
        }
    }

    fn settings_state(&self) -> Option<ViewState> {
        let dark = is_dark(MainThreadMarker::from(self));
        self.with_resident(|r| ViewState {
            settings: r.settings,
            login: login_state(r.login),
            login_error: r.login_error.clone(),
            access: r.access(),
            theme: Theme::for_dark(dark),
        })
    }

    /// Shows the Settings window, building it the first time.
    pub(crate) fn show_settings(&self) {
        let mtm = MainThreadMarker::from(self);
        let Some(existing) = self.with_resident(|r| r.settings_ui.clone()) else {
            return;
        };
        let (ui, built) = match existing {
            Some(ui) => (ui, false),
            None => {
                let ui = SettingsUi::new(mtm, self);
                self.with_resident(|r| r.settings_ui = Some(ui.clone()));
                (ui, true)
            }
        };
        self.sync_login_status();
        if let Some(state) = self.settings_state() {
            ui.update(&state);
        }
        ui.fit_window(built);
        if built {
            ui.0.window.center();
        }
        activate(&NSApplication::sharedApplication(mtm));
        ui.0.window.makeKeyAndOrderFront(None);
    }

    /// The Settings window closed: release the icons it decoded.
    pub(super) fn settings_closed(&self) {
        if let Some(Some(ui)) = self.with_resident(|r| r.settings_ui.clone()) {
            ui.release_icons();
        }
    }

    /// The Settings window was resized: the sheet's view follows.
    pub(super) fn settings_resized(&self) {
        if let Some(Some(ui)) = self.with_resident(|r| r.settings_ui.clone()) {
            ui.0.layout_root();
        }
    }

    /// The Launch at Login switch was set to `on`: registers or
    /// unregisters the login item, as the menu-bar menu's item does.
    pub(super) fn login_switched(&self, on: bool) {
        let is_on = self.with_resident(|r| r.login.is_on()).unwrap_or(false);
        if on == is_on {
            self.refresh_settings();
        } else {
            self.toggle_launch_at_login();
        }
    }

    /// The appearance control chose `segment`.
    pub(crate) fn appearance_changed(&self, segment: isize) {
        let Some(mode) = usize::try_from(segment)
            .ok()
            .and_then(|i| AppearanceMode::ALL.into_iter().nth(i))
        else {
            return;
        };
        self.with_resident(|r| {
            r.settings.appearance = mode;
            r.save_settings();
        });
        apply_appearance(MainThreadMarker::from(self), mode);
        self.refresh_look();
    }

    /// The "Your files first" switch was set to `on`: saves it and re-runs
    /// the search in the new order.
    pub(crate) fn your_files_first_changed(&self, on: bool) {
        self.with_resident(|r| {
            r.settings.your_files_first = on;
            r.save_settings();
        });
        self.set_your_files_first(on);
        self.refresh_settings();
    }

    /// The icon pick with `tag` (its index in [`AppIcon::ALL`]) was chosen.
    pub(crate) fn icon_chosen(&self, tag: isize) {
        let Some(icon) = usize::try_from(tag)
            .ok()
            .and_then(|i| AppIcon::ALL.into_iter().nth(i))
        else {
            return;
        };
        self.with_resident(|r| {
            r.settings.app_icon = icon;
            r.save_settings();
        });
        self.refresh_look();
    }
}

/// Sets `NSApp.appearance` for `mode`: none (follow the system), Aqua or
/// Dark Aqua.
fn apply_appearance(mtm: MainThreadMarker, mode: AppearanceMode) {
    let name = match mode {
        AppearanceMode::System => None,
        // SAFETY: the appearance name is an immutable framework constant.
        AppearanceMode::Light => Some(unsafe { NSAppearanceNameAqua }),
        // SAFETY: as above.
        AppearanceMode::Dark => Some(unsafe { NSAppearanceNameDarkAqua }),
    };
    let appearance = name.and_then(NSAppearance::appearanceNamed);
    NSApplication::sharedApplication(mtm).setAppearance(appearance.as_deref());
}

/// Whether the app's effective appearance is dark.
fn is_dark(mtm: MainThreadMarker) -> bool {
    // SAFETY: the appearance names are immutable framework constants.
    let (aqua, dark) = unsafe { (NSAppearanceNameAqua, NSAppearanceNameDarkAqua) };
    NSApplication::sharedApplication(mtm)
        .effectiveAppearance()
        .bestMatchFromAppearancesWithNames(&NSArray::from_slice(&[aqua, dark]))
        .is_some_and(|best| best.isEqualToString(dark))
}

/// Sets the Dock icon: the bundle's own, or an embedded image, with the
/// ribbon drawn over it in a release candidate or beta.
fn set_dock_tile(mtm: MainThreadMarker, tile: DockTile) {
    let image = match tile.icon {
        DockIcon::Bundle => None,
        DockIcon::Image { icon, dark } => icon_image(icon, dark, DOCK_IMAGE),
    };
    let image = match (image, tile.ribbon) {
        (Some(image), Some(ribbon)) => with_ribbon(&image, &ribbon, DOCK_IMAGE).or(Some(image)),
        (image, _) => image,
    };
    // SAFETY: AppKit copies what it needs from the image; nothing else
    // mutates it.
    unsafe {
        NSApplication::sharedApplication(mtm).setApplicationIconImage(image.as_deref());
    }
}

/// The embedded image of `icon` (light or `dark` artwork), `size` points
/// square.
fn icon_image(icon: AppIcon, dark: bool, size: f64) -> Option<Retained<NSImage>> {
    let image = decode(icon.png(dark))?;
    image.setSize(NSSize::new(size, size));
    Some(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_states_follow_the_menu_s_check_mark() {
        for status in [
            LoginItemStatus::Enabled,
            LoginItemStatus::RequiresApproval,
            LoginItemStatus::NotRegistered,
            LoginItemStatus::NotFound,
            LoginItemStatus::Unavailable,
        ] {
            let row = login_row("Not-Everything", login_state(status), None);
            assert_eq!(row.on, status.is_on(), "{status:?}");
            assert_eq!(
                row.enabled,
                status != LoginItemStatus::Unavailable,
                "{status:?}"
            );
        }
    }

    #[test]
    fn frames_relative_to_a_parent() {
        let parent = Frame::new(32.0, 100.0, 300.0, 200.0);
        assert_eq!(
            relative(Frame::new(40.0, 130.0, 10.0, 20.0), parent),
            Frame::new(8.0, 30.0, 10.0, 20.0)
        );
    }

    #[test]
    fn the_embedded_icons_decode() {
        for icon in AppIcon::ALL {
            for dark in [false, true] {
                let image = decode(icon.png(dark)).expect("a PNG");
                assert!(image.isValid(), "{icon} dark={dark}");
            }
        }
    }
}
