//! The Settings window's copy, contents and geometry: the canvas's Settings
//! board.
//!
//! The window is a top bar with only its title, over a scrolling sheet of
//! raised cards: General (the summon hotkey as key caps that record a new
//! one when clicked, Launch at Login),
//! Search (Your files first), Appearance (System / Light / Dark), App Icon
//! (the six approved icons, Match Row tagged "Default") and Full Disk
//! Access, then a footnote with the version. This module holds what the
//! rows say in each state ([`hotkey_row`], [`login_row`],
//! [`your_files_first_row`], [`access_row`]) and where every box goes
//! ([`layout`]); the AppKit layer
//! (`appkit::window::settings_window`) measures the text, places the views
//! and saves each change at once.

#![forbid(unsafe_code)]

use fx_fs::AccessState;

use crate::app_icon::AppIcon;
use crate::appearance::AppearanceMode;
use crate::build_info::BuildInfo;
use crate::hotkey::{HotkeySpec, Modifiers, Refusal};
use crate::key_caps::{key_caps, modifier_caps, spoken};

/// The window title, the only text in the top bar.
pub const TITLE: &str = "Settings";

/// The Appearance card's title.
pub const APPEARANCE_LABEL: &str = "Appearance";

/// The note under the appearance control.
pub const APPEARANCE_NOTE: &str = "System follows the light or dark setting of your Mac.";

/// The App Icon card's title.
pub const ICON_LABEL: &str = "App Icon";

/// The tag under the default icon's name (set in capitals by
/// `theme::styles::TAG`).
pub const DEFAULT_BADGE: &str = "Default";

/// The "Your files first" row's name.
pub const YOUR_FILES_FIRST_LABEL: &str = "Your files first";

/// The note under the icon picks for the app named `app_name`.
pub fn icon_note(app_name: &str) -> String {
    format!(
        "The Dock shows your choice while {app_name} is running. \
         Finder and Launchpad show the default icon."
    )
}

/// The cards' titles, in order (set in capitals by
/// `theme::styles::SECTION_TITLE`).
pub const SECTIONS: [&str; 5] = [
    "General",
    "Search",
    APPEARANCE_LABEL,
    ICON_LABEL,
    "Full Disk Access",
];

/// The summon hotkey row's name.
pub const HOTKEY_LABEL: &str = "Show the search window";

/// The summon hotkey row's caption; its control is the hotkey's key caps
/// (`crate::key_caps::key_caps`), which record a new one when clicked.
pub const HOTKEY_CAPTION: &str = "Press this shortcut in any app.";

/// The summon hotkey row's caption while it records (the shortcut
/// recorder board, states 4 and 5).
pub const HOTKEY_RECORDING_CAPTION: &str = "Type the new shortcut. Esc cancels.";

/// What the recording box shows before a modifier is held.
pub const HOTKEY_RECORDING_HINT: &str = "Type shortcut";

/// The reset link's title, naming the app's `default` shortcut (the
/// board's state 9).
pub fn hotkey_reset_title(default: HotkeySpec) -> String {
    format!("Use Default ({})", default.symbols())
}

/// What the summon hotkey row shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HotkeyRow {
    /// The caption under the row's name.
    pub caption: String,
    /// Whether the caption is a refusal (`danger-ink`, not `muted`).
    pub error: bool,
    /// Whether the caps record: pressed and rimmed in green.
    pub recording: bool,
    /// The caps: the shortcut's, or while recording the modifiers held so
    /// far (none: the box shows [`HOTKEY_RECORDING_HINT`]).
    pub caps: Vec<&'static str>,
    /// The reset link's title, while the shortcut isn't the default and
    /// the caps don't record.
    pub reset: Option<String>,
    /// What VoiceOver reads for the caps.
    pub spoken: String,
}

/// The summon hotkey row for the saved shortcut `spec`, the app's
/// `default`, the modifiers held while `recording` (`None` at rest) and the
/// reason the last change was refused, if any (shown in place of the
/// caption until a change succeeds, and hidden while recording).
pub fn hotkey_row(
    spec: HotkeySpec,
    default: HotkeySpec,
    recording: Option<Modifiers>,
    error: Option<&Refusal>,
) -> HotkeyRow {
    match recording {
        Some(held) => HotkeyRow {
            caption: HOTKEY_RECORDING_CAPTION.to_string(),
            error: false,
            recording: true,
            caps: modifier_caps(held),
            reset: None,
            spoken: format!("{HOTKEY_LABEL}: recording"),
        },
        None => HotkeyRow {
            caption: error.map_or_else(|| HOTKEY_CAPTION.to_string(), Refusal::to_string),
            error: error.is_some(),
            recording: false,
            caps: key_caps(spec),
            reset: (spec != default).then(|| hotkey_reset_title(default)),
            spoken: format!("{HOTKEY_LABEL}: {}", spoken(spec)),
        },
    }
}

/// The Launch at Login row's name.
pub const LOGIN_LABEL: &str = "Launch at Login";

/// The Launch at Login row's caption for the app named `app_name`.
pub fn login_caption(app_name: &str) -> String {
    format!("Starts {app_name} in the menu bar when you log in.")
}

/// The Launch at Login row's caption while macOS waits for the user to
/// allow the login item. Not on the board: derived from the status bar's
/// note, for the owner's review.
pub const LOGIN_APPROVAL_CAPTION: &str =
    "Requires approval in System Settings \u{203a} Login Items.";

/// The Launch at Login row's caption when the app does not run from its
/// bundle (no login item to manage): the menu item's tool tip. Derived.
pub const LOGIN_UNAVAILABLE_CAPTION: &str = "Available when run from the .app bundle.";

/// The "Your files first" row's caption: the canvas's, keeping the earlier
/// caption's last sentence (the owner's choice).
pub const YOUR_FILES_FIRST_CAPTION: &str = "Lists matches in your home folder, iCloud Drive and \
     cloud storage above system and app files, in every sort. Hidden files and Library are not \
     counted.";

/// The Full Disk Access row's name when access is granted (after a green
/// dot).
pub const ACCESS_GRANTED_LABEL: &str = "Granted";

/// The Full Disk Access row's caption when access is granted.
pub const ACCESS_GRANTED_CAPTION: &str =
    "Every folder is indexed, including Desktop, Documents, Downloads and iCloud Drive.";

/// The Full Disk Access row's button.
pub const ACCESS_SETTINGS_BUTTON: &str = "Open System Settings";

/// The Full Disk Access row's name when access is off. Not drawn on the
/// canvas: derived from the access callout, for the owner's review.
pub const ACCESS_OFF_LABEL: &str = "Off";

/// The Full Disk Access row's caption when access is off (derived, as
/// [`ACCESS_OFF_LABEL`]).
pub const ACCESS_OFF_CAPTION: &str = "Desktop, Documents, Downloads, iCloud Drive, Mail and \
     other protected folders are left out of the index.";

/// The footnote under the cards: "Not-Everything 0.2.0 (200) · 3645", with
/// the label and build number the About panel shows.
pub fn footnote(build: BuildInfo) -> String {
    let about = build.about();
    let number = if about.build.is_empty() {
        String::new()
    } else {
        format!(" ({})", about.build)
    };
    format!(
        "{} {}{number} \u{b7} 3645",
        build.app_name(),
        about.application_version
    )
}

/// The appearance segments, in order: each mode and its label.
pub fn appearance_segments() -> impl Iterator<Item = (AppearanceMode, &'static str)> {
    AppearanceMode::ALL.into_iter().map(|m| (m, m.title()))
}

/// One icon pick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IconTile {
    /// The design the pick chooses.
    pub icon: AppIcon,
    /// The name under the icon.
    pub title: &'static str,
    /// The tag under the name ([`DEFAULT_BADGE`] for the default icon).
    pub badge: Option<&'static str>,
}

/// The icon picks, in the approved order.
pub fn icon_tiles() -> impl Iterator<Item = IconTile> {
    AppIcon::ALL.into_iter().map(|icon| IconTile {
        icon,
        title: icon.title(),
        badge: icon.is_default().then_some(DEFAULT_BADGE),
    })
}

/// The login item's state as the Launch at Login row shows it (the AppKit
/// layer maps `SMAppService`'s status onto it).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LoginState {
    /// Registered and allowed.
    Enabled,
    /// Registered, waiting for the user to allow it in System Settings.
    NeedsApproval,
    /// Not registered (or not found).
    Off,
    /// Not running from the bundle: there is no login item to manage.
    Unavailable,
}

/// What a setting row with a switch shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwitchRow {
    /// Whether the switch is on.
    pub on: bool,
    /// Whether the switch can be used.
    pub enabled: bool,
    /// The caption under the row's name.
    pub caption: String,
    /// Whether the caption is an error (`danger-ink`, not `muted`).
    pub error: bool,
}

/// The Launch at Login row for the app named `app_name`: on while the login
/// item is enabled or waiting for approval, as the menu-bar menu's check
/// mark is; `error` (the reason the last change was refused) replaces the
/// caption.
pub fn login_row(app_name: &str, state: LoginState, error: Option<&str>) -> SwitchRow {
    let (on, enabled, caption) = match state {
        LoginState::Enabled => (true, true, login_caption(app_name)),
        LoginState::NeedsApproval => (true, true, LOGIN_APPROVAL_CAPTION.to_string()),
        LoginState::Off => (false, true, login_caption(app_name)),
        LoginState::Unavailable => (false, false, LOGIN_UNAVAILABLE_CAPTION.to_string()),
    };
    match error {
        Some(error) => SwitchRow {
            on,
            enabled,
            caption: error.to_string(),
            error: true,
        },
        None => SwitchRow {
            on,
            enabled,
            caption,
            error: false,
        },
    }
}

/// The "Your files first" row.
pub fn your_files_first_row(on: bool) -> SwitchRow {
    SwitchRow {
        on,
        enabled: true,
        caption: YOUR_FILES_FIRST_CAPTION.to_string(),
        error: false,
    }
}

/// What the Full Disk Access row shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccessRow {
    /// Granted (the board's green dot), or off (the derived state: a grey
    /// dot).
    pub granted: bool,
    /// The row's name after the dot.
    pub label: &'static str,
    /// The caption under it.
    pub caption: &'static str,
    /// The button, which opens the Full Disk Access page of System
    /// Settings.
    pub button: &'static str,
    /// Whether the button is the primary (green) one.
    pub primary: bool,
}

/// The Full Disk Access row for `access`. The board draws only the granted
/// row; while access is off (denied, or not known yet) the row says so and
/// its button becomes the primary one, as the access callout's is
/// (derived, for the owner's review).
pub fn access_row(access: AccessState) -> AccessRow {
    if access == AccessState::Granted {
        AccessRow {
            granted: true,
            label: ACCESS_GRANTED_LABEL,
            caption: ACCESS_GRANTED_CAPTION,
            button: ACCESS_SETTINGS_BUTTON,
            primary: false,
        }
    } else {
        AccessRow {
            granted: false,
            label: ACCESS_OFF_LABEL,
            caption: ACCESS_OFF_CAPTION,
            button: ACCESS_SETTINGS_BUTTON,
            primary: true,
        }
    }
}

/// Where the Settings window's boxes go, in points, from the board's CSS.
///
/// Frames are in the sheet's flipped space (y runs down from the sheet's
/// top), except the top bar's, which are in the window's. The text styles
/// are `theme::styles`; this module only adds the board's paddings and gaps
/// that the 3645 tokens don't name. Text heights come from the AppKit
/// layer, which measures them at the widths this module gives
/// ([`row_text_width`](layout::row_text_width),
/// [`card_content_width`](layout::card_content_width)).
pub mod layout {
    use crate::theme::{SPACE_1, SPACE_2, SPACE_3, SPACE_4, metrics, styles};

    /// A rectangle; y runs down.
    #[derive(Clone, Copy, Debug, Default, PartialEq)]
    pub struct Frame {
        /// Left edge.
        pub x: f64,
        /// Top edge.
        pub y: f64,
        /// Width.
        pub width: f64,
        /// Height.
        pub height: f64,
    }

    impl Frame {
        /// A frame at (`x`, `y`), `width` × `height`.
        pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Frame {
            Frame {
                x,
                y,
                width,
                height,
            }
        }

        /// The right edge.
        pub fn right(&self) -> f64 {
            self.x + self.width
        }

        /// The bottom edge.
        pub fn bottom(&self) -> f64 {
            self.y + self.height
        }
    }

    /// Below a card's title (`.sec-title` margin-bottom, `space-2`).
    pub const TITLE_GAP: f64 = SPACE_2;
    /// A first row's top padding (`.sec-title + .ne-setting`, `space-1`).
    pub const FIRST_ROW_TOP: f64 = SPACE_1;
    /// Between a row's name and caption (`.ne-setting-caption` margin-top).
    pub const CAPTION_GAP: f64 = 2.0;
    /// The divider over every row but a card's first.
    pub const DIVIDER: f64 = 1.0;
    /// Between a row's text and its control (`.ne-setting` gap, `space-4`).
    pub const ROW_GAP: f64 = SPACE_4;
    /// Above a note (`.ne-note` margin-top, `space-3`).
    pub const NOTE_GAP: f64 = SPACE_3;
    /// Between icon picks (`.ne-icons` gap, `space-3`).
    pub const ICON_GAP: f64 = SPACE_3;
    /// An icon pick's padding: top, sides, bottom (`.ne-icon-pick`).
    pub const ICON_PICK_PADDING: [f64; 3] = [14.0, 8.0, 12.0];
    /// Between an icon, its name and its tag.
    pub const ICON_PICK_GAP: f64 = 6.0;
    /// A tag's padding: vertical, horizontal (`.tag`).
    pub const TAG_PADDING: [f64; 2] = [2.0, 7.0];
    /// A segment's padding: vertical, horizontal (`.seg button`).
    pub const SEGMENT_PADDING: [f64; 2] = [4.0, 10.0];
    /// The Full Disk Access dot's side (`.ne-dot`)...
    pub const ACCESS_DOT: f64 = metrics::DOT_SIZE;
    /// ...where the row's name starts after it (its margin-right is 10).
    pub const ACCESS_LABEL_INDENT: f64 = metrics::DOT_SIZE + 10.0;
    /// A button's transparent border (`.btn`).
    pub const BUTTON_BORDER: f64 = 2.0;
    /// The focus outline's width and its distance from the control
    /// (`:focus-visible`).
    pub const FOCUS_OUTLINE: (f64, f64) = (2.0, 2.0);
    /// The switch's size (`.ne-switch`), its border and the knob's side.
    pub const SWITCH_SIZE: (f64, f64) = (metrics::SWITCH_WIDTH, metrics::SWITCH_HEIGHT);
    /// The switch's edge.
    pub const SWITCH_BORDER: f64 = 1.0;
    /// The top bar's padding above and below the title row (the board sets
    /// the bottom one to 10).
    pub const TOP_BAR_PADDING_Y: f64 = 10.0;

    /// The sheet's left edge and width in a window `window_width` wide:
    /// at most [`metrics::SETTINGS_WIDTH`], centred.
    pub fn sheet_frame(window_width: f64) -> (f64, f64) {
        let width = window_width.min(metrics::SETTINGS_WIDTH);
        ((window_width - width) / 2.0, width)
    }

    /// The width inside a card on a sheet `sheet_width` wide.
    pub fn card_content_width(sheet_width: f64) -> f64 {
        sheet_width - 2.0 * metrics::SETTINGS_PADDING[1] - 2.0 * metrics::CARD_PADDING
    }

    /// The width a row's name and caption wrap at, beside a control
    /// `control_width` wide.
    pub fn row_text_width(content_width: f64, control_width: f64) -> f64 {
        content_width - ROW_GAP - control_width
    }

    /// The height of a row's text: the name's line, the gap and a caption
    /// `caption_height` tall.
    pub fn text_block_height(caption_height: f64) -> f64 {
        styles::SETTING_LABEL.line_height + CAPTION_GAP + caption_height
    }

    /// The sheet: cards [`metrics::SETTINGS_GAP`] apart, then the
    /// footnote, inside the sheet's padding.
    #[derive(Clone, Debug)]
    pub struct Sheet {
        x: f64,
        width: f64,
        y: f64,
        items: usize,
    }

    impl Sheet {
        /// An empty sheet in a window `window_width` wide.
        pub fn new(window_width: f64) -> Sheet {
            let (x, width) = sheet_frame(window_width);
            Sheet {
                x: x + metrics::SETTINGS_PADDING[1],
                width: width - 2.0 * metrics::SETTINGS_PADDING[1],
                y: metrics::SETTINGS_PADDING[0],
                items: 0,
            }
        }

        fn next_top(&mut self) -> f64 {
            if self.items > 0 {
                self.y += metrics::SETTINGS_GAP;
            }
            self.items += 1;
            self.y
        }

        /// Starts the next card.
        pub fn card(&mut self) -> Card {
            let top = self.next_top();
            Card::new(self.x, top, self.width)
        }

        /// Ends `card`; returns its frame.
        pub fn end(&mut self, card: Card) -> Frame {
            let frame = card.frame();
            self.y = frame.bottom();
            frame
        }

        /// The footnote's line, after the cards.
        pub fn footnote(&mut self) -> Frame {
            let top = self.next_top();
            let frame = Frame::new(self.x, top, self.width, styles::FOOTNOTE.line_height);
            self.y = frame.bottom();
            frame
        }

        /// The sheet's height so far, with its bottom padding.
        pub fn height(&self) -> f64 {
            self.y + metrics::SETTINGS_PADDING[2]
        }
    }

    /// A card (`.card`): its padding, a title, then rows, blocks and notes.
    #[derive(Clone, Debug)]
    pub struct Card {
        x: f64,
        top: f64,
        width: f64,
        y: f64,
        rows: usize,
    }

    impl Card {
        /// A card at (`x`, `top`), `width` wide.
        pub fn new(x: f64, top: f64, width: f64) -> Card {
            Card {
                x,
                top,
                width,
                y: top + metrics::CARD_PADDING,
                rows: 0,
            }
        }

        /// The card's top edge.
        pub fn top(&self) -> f64 {
            self.top
        }

        /// The left edge and width of the card's content.
        pub fn content(&self) -> (f64, f64) {
            (
                self.x + metrics::CARD_PADDING,
                self.width - 2.0 * metrics::CARD_PADDING,
            )
        }

        /// The title's line (`.sec-title`).
        pub fn title(&mut self) -> Frame {
            let (x, width) = self.content();
            let frame = Frame::new(x, self.y, width, styles::SECTION_TITLE.line_height);
            self.y = frame.bottom() + TITLE_GAP;
            frame
        }

        /// A setting row (`.ne-setting`) with text `text_height` tall and a
        /// control `control` (width, height), each centred on the other.
        pub fn row(&mut self, text_height: f64, control: (f64, f64)) -> Row {
            let (x, width) = self.content();
            let first = self.rows == 0;
            self.rows += 1;
            let divider = (!first).then(|| Frame::new(x, self.y, width, DIVIDER));
            let pad_top = if first {
                FIRST_ROW_TOP
            } else {
                DIVIDER + metrics::SETTING_ROW_PADDING
            };
            let top = self.y + pad_top;
            let tall = text_height.max(control.1);
            let text = Frame::new(
                x,
                top + (tall - text_height) / 2.0,
                row_text_width(width, control.0),
                text_height,
            );
            let control = Frame::new(
                x + width - control.0,
                top + (tall - control.1) / 2.0,
                control.0,
                control.1,
            );
            self.y = top + tall + metrics::SETTING_ROW_PADDING;
            Row {
                divider,
                text,
                control,
            }
        }

        /// A block `height` tall right after the title (the segmented
        /// control, the icon grid); returns its top.
        pub fn block(&mut self, height: f64) -> f64 {
            let top = self.y;
            self.y += height;
            top
        }

        /// A note (`.ne-note`) `height` tall; returns its top.
        pub fn note(&mut self, height: f64) -> f64 {
            let top = self.y + NOTE_GAP;
            self.y = top + height;
            top
        }

        /// The card's frame, with its bottom padding.
        pub fn frame(&self) -> Frame {
            Frame::new(
                self.x,
                self.top,
                self.width,
                self.y + metrics::CARD_PADDING - self.top,
            )
        }
    }

    /// A setting row's boxes.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Row {
        /// The divider over the row (all rows but a card's first).
        pub divider: Option<Frame>,
        /// The name and caption.
        pub text: Frame,
        /// The control on the right.
        pub control: Frame,
    }

    /// The segmented control at (`x`, `top`) with segments whose labels are
    /// `text_widths` wide: the container (`.seg`, padding and gaps 2) and
    /// each segment (padding 4 10 inside a 1.5 border, a 20-point line).
    pub fn segments(x: f64, top: f64, text_widths: &[f64]) -> (Frame, Vec<Frame>) {
        let pad = metrics::SEGMENT_PADDING;
        let border = metrics::SEGMENT_BORDER;
        let height = styles::SEGMENT.line_height + 2.0 * SEGMENT_PADDING[0] + 2.0 * border;
        let mut left = x + pad;
        let mut frames = Vec::with_capacity(text_widths.len());
        for (n, w) in text_widths.iter().enumerate() {
            if n > 0 {
                left += pad;
            }
            let width = w + 2.0 * SEGMENT_PADDING[1] + 2.0 * border;
            frames.push(Frame::new(left, top + pad, width, height));
            left += width;
        }
        let container = Frame::new(x, top, left + pad - x, height + 2.0 * pad);
        (container, frames)
    }

    /// A segment's label line inside its frame.
    pub fn segment_text(segment: Frame) -> Frame {
        let border = metrics::SEGMENT_BORDER;
        Frame::new(
            segment.x + border + SEGMENT_PADDING[1],
            segment.y + border + SEGMENT_PADDING[0],
            segment.width - 2.0 * (border + SEGMENT_PADDING[1]),
            styles::SEGMENT.line_height,
        )
    }

    /// The icon picks' grid at (`x`, `top`), `width` wide, for picks with
    /// or without a tag (`tagged`, in order): [`metrics::ICON_COLUMNS`]
    /// columns [`ICON_GAP`] apart; each row as tall as its tallest pick,
    /// and at least [`metrics::ICON_PICK_MIN_HEIGHT`]. Returns the frames
    /// and the grid's height.
    pub fn icon_grid(x: f64, top: f64, width: f64, tagged: &[bool]) -> (Vec<Frame>, f64) {
        let columns = metrics::ICON_COLUMNS;
        let gaps = (columns - 1) as f64 * ICON_GAP;
        let column = (width - gaps) / columns as f64;
        let mut frames = Vec::with_capacity(tagged.len());
        let mut y = top;
        for (n, row) in tagged.chunks(columns).enumerate() {
            if n > 0 {
                y += ICON_GAP;
            }
            let height = row
                .iter()
                .map(|&t| icon_pick_height(t))
                .fold(metrics::ICON_PICK_MIN_HEIGHT, f64::max);
            for c in 0..row.len() {
                frames.push(Frame::new(
                    x + c as f64 * (column + ICON_GAP),
                    y,
                    column,
                    height,
                ));
            }
            y += height;
        }
        (frames, y - top)
    }

    /// A pick's content height with or without a tag, before the minimum.
    fn icon_pick_height(tagged: bool) -> f64 {
        let border = 2.0 * metrics::SELECTED_BORDER;
        let mut h = border + ICON_PICK_PADDING[0] + metrics::ICON_SIZE;
        h += ICON_PICK_GAP + styles::ICON_NAME.line_height;
        if tagged {
            h += ICON_PICK_GAP + tag_size(0.0).1;
        }
        h + ICON_PICK_PADDING[2]
    }

    /// The boxes inside an icon pick.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct IconPickContent {
        /// The icon, [`metrics::ICON_SIZE`] square.
        pub icon: Frame,
        /// The name's line, the pick's inner width.
        pub name: Frame,
        /// The top of the tag, if the pick has one (it is centred).
        pub tag_top: f64,
    }

    /// The boxes inside the icon pick at `pick`.
    pub fn icon_pick_content(pick: Frame) -> IconPickContent {
        let border = metrics::SELECTED_BORDER;
        let size = metrics::ICON_SIZE;
        let icon_top = pick.y + border + ICON_PICK_PADDING[0];
        let name_top = icon_top + size + ICON_PICK_GAP;
        let inset = border + ICON_PICK_PADDING[1];
        IconPickContent {
            icon: Frame::new(pick.x + (pick.width - size) / 2.0, icon_top, size, size),
            name: Frame::new(
                pick.x + inset,
                name_top,
                pick.width - 2.0 * inset,
                styles::ICON_NAME.line_height,
            ),
            tag_top: name_top + styles::ICON_NAME.line_height + ICON_PICK_GAP,
        }
    }

    /// A tag's size around a label `text_width` wide (`.tag`).
    pub fn tag_size(text_width: f64) -> (f64, f64) {
        (
            text_width + 2.0 * TAG_PADDING[1],
            styles::TAG.line_height + 2.0 * TAG_PADDING[0],
        )
    }

    /// A button's size around a label `text_width` wide (`.btn`).
    pub fn button_size(text_width: f64) -> (f64, f64) {
        let [py, px] = metrics::BUTTON_PADDING;
        (
            text_width + 2.0 * (px + BUTTON_BORDER),
            styles::BUTTON.line_height + 2.0 * (py + BUTTON_BORDER),
        )
    }

    /// A button's label line inside its frame.
    pub fn button_text(button: Frame) -> Frame {
        let [py, px] = metrics::BUTTON_PADDING;
        Frame::new(
            button.x + BUTTON_BORDER + px,
            button.y + BUTTON_BORDER + py,
            button.width - 2.0 * (BUTTON_BORDER + px),
            styles::BUTTON.line_height,
        )
    }

    /// The knob inside the switch, off or `on` (`.ne-knob`: 3 inside the
    /// 1-point edge, sliding [`metrics::KNOB_TRAVEL`]).
    pub fn knob(on: bool) -> Frame {
        let inset = SWITCH_BORDER + metrics::KNOB_INSET;
        let x = inset + if on { metrics::KNOB_TRAVEL } else { 0.0 };
        Frame::new(x, inset, metrics::KNOB_SIZE, metrics::KNOB_SIZE)
    }

    /// A key cap's width for a label `text_width` wide (`.ne-key`).
    pub fn key_cap_width(text_width: f64) -> f64 {
        (text_width + 2.0 * metrics::KEY_PADDING_X).max(metrics::KEY_MIN_WIDTH)
    }

    /// The width of key caps for labels `text_widths` wide, 3 apart.
    pub fn key_caps_width(text_widths: &[f64]) -> f64 {
        let gaps = text_widths.len().saturating_sub(1) as f64 * metrics::KEY_GAP;
        text_widths.iter().map(|&w| key_cap_width(w)).sum::<f64>() + gaps
    }

    /// Key caps for labels `text_widths` wide ending at `right`, at `top`:
    /// the whole group and each cap.
    pub fn key_caps(right: f64, top: f64, text_widths: &[f64]) -> (Frame, Vec<Frame>) {
        let width = key_caps_width(text_widths);
        let mut x = right - width;
        let mut caps = Vec::with_capacity(text_widths.len());
        for &w in text_widths {
            let cap = key_cap_width(w);
            caps.push(Frame::new(x, top, cap, metrics::KEY_HEIGHT));
            x += cap + metrics::KEY_GAP;
        }
        (
            Frame::new(right - width, top, width, metrics::KEY_HEIGHT),
            caps,
        )
    }

    /// How far the shortcut recorder's box (`.rec`) reaches past the caps:
    /// above and below, and on each side (its negative margin). At rest the
    /// row lays the caps out as before; the box only shows on hover, focus
    /// and while recording.
    pub const RECORDER_REACH: [f64; 2] = [5.0, 4.0];
    /// The recorder box's height: the caps, 3 of padding and a 2-point rim
    /// above and below.
    pub const RECORDER_HEIGHT: f64 = metrics::KEY_HEIGHT + 2.0 * (3.0 + RECORDER_RIM);
    /// The recorder box's rim (transparent at rest; `green-ink` while
    /// recording).
    pub const RECORDER_RIM: f64 = metrics::SELECTED_BORDER;
    /// The recorder box's side padding while recording (2 at rest).
    pub const RECORDING_PADDING_X: f64 = 6.0;
    /// The recording box's narrowest.
    pub const RECORDING_MIN_WIDTH: f64 = 144.0;
    /// The hint's side padding in the recording box (`.rec-hint`).
    pub const RECORDER_HINT_PADDING_X: f64 = 4.0;
    /// Above the reset link (`.rec-reset` margin-top).
    pub const RESET_GAP: f64 = 2.0;
    /// The line the reset link sits on: its 18-point box and 2-point margin
    /// on the baseline of the row's 20-point line (measured on the board).
    pub const RESET_LINE: f64 = 21.0;

    /// The recording box's hint, `text_width` wide, with its padding.
    pub fn recorder_hint_width(text_width: f64) -> f64 {
        text_width + 2.0 * RECORDER_HINT_PADDING_X
    }

    /// The recorder's footprint in its row (the control size for
    /// [`Card::row`]) around content `content_width` wide: the caps at
    /// rest; while `recording`, the box (at least
    /// [`RECORDING_MIN_WIDTH`]) less what it reaches past the row.
    pub fn recorder_size(content_width: f64, recording: bool) -> (f64, f64) {
        let width = if recording {
            let inset = RECORDER_RIM + RECORDING_PADDING_X;
            (content_width + 2.0 * inset).max(RECORDING_MIN_WIDTH) - 2.0 * RECORDER_REACH[1]
        } else {
            content_width
        };
        (width, metrics::KEY_HEIGHT)
    }

    /// The recorder's box around its row control `control`.
    pub fn recorder_box(control: Frame) -> Frame {
        let [y, x] = RECORDER_REACH;
        Frame::new(
            control.x - x,
            control.y - y,
            control.width + 2.0 * x,
            control.height + 2.0 * y,
        )
    }

    /// Where content `size` (width, height) goes in the recorder box `bx`:
    /// right-aligned inside the rim and padding, centred top to bottom.
    pub fn recorder_content(bx: Frame, size: (f64, f64), recording: bool) -> Frame {
        let padding = if recording {
            RECORDING_PADDING_X
        } else {
            RECORDER_REACH[1] - RECORDER_RIM
        };
        let right = bx.right() - RECORDER_RIM - padding;
        Frame::new(
            right - size.0,
            bx.y + (bx.height - size.1) / 2.0,
            size.0,
            size.1,
        )
    }

    /// The hotkey row's text height: the name, the caption
    /// `caption_height` tall and, with a `reset` link, its line.
    pub fn hotkey_text_height(caption_height: f64, reset: bool) -> f64 {
        text_block_height(caption_height) + if reset { RESET_LINE } else { 0.0 }
    }

    /// The reset link, `width` wide, under the caption (`caption_height`
    /// tall) of the row text `text`.
    pub fn reset_link(text: Frame, caption_height: f64, width: f64) -> Frame {
        let top = text.y + text_block_height(caption_height) + RESET_GAP;
        Frame::new(text.x, top, width, styles::LINK_BUTTON.line_height)
    }

    /// Where CSS puts the first baseline in a line box `line_height` tall,
    /// for a font `ascent` above and `descent` below its baseline: as
    /// Chrome lays out the board, the font's ascent and descent rounded to
    /// whole pixels, and the rest of the line split evenly above and below.
    pub fn css_baseline(line_height: f64, ascent: f64, descent: f64) -> f64 {
        let (a, d) = (ascent.round(), descent.round());
        (line_height - (a + d)) / 2.0 + a
    }

    /// The baseline `y` on the pixel grid of a screen of `scale` pixels per
    /// point; a baseline half way between two pixels takes the upper one,
    /// as Chrome draws the board at 1x.
    pub fn snap_baseline(y: f64, scale: f64) -> f64 {
        (y * scale - 0.5).ceil() / scale
    }

    /// `y` moved up onto the pixel grid of a `scale` screen, so text drawn
    /// from it is not blurred across two pixel rows.
    pub fn snap_down(y: f64, scale: f64) -> f64 {
        (y * scale).floor() / scale
    }

    /// Where TextKit puts a line's baseline when the line is exactly
    /// `line_height` tall (minimum and maximum line height): the font's
    /// descent, rounded up, above the line's bottom (measured on macOS 27;
    /// the text view moves it to [`css_baseline`] with a baseline offset).
    pub fn textkit_baseline(line_height: f64, descent: f64) -> f64 {
        line_height - descent.ceil()
    }

    /// The Full Disk Access dot in the row's name line at (`x`,
    /// `line_top`), for a name font `ascent` above and `descent` below its
    /// baseline: an inline box whose bottom sits 1 above the baseline
    /// (`vertical-align: 1px`), the baseline being where CSS puts it in the
    /// 22-point line ([`css_baseline`]).
    pub fn access_dot(x: f64, line_top: f64, ascent: f64, descent: f64) -> Frame {
        let line = styles::SETTING_LABEL.line_height;
        let baseline = line_top + css_baseline(line, ascent, descent);
        Frame::new(x, baseline - 1.0 - ACCESS_DOT, ACCESS_DOT, ACCESS_DOT)
    }

    /// The top bar: its height and the top of its title row.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct TopBar {
        /// The bar's height.
        pub height: f64,
        /// The top of the 24-point title row.
        pub title_top: f64,
    }

    impl TopBar {
        /// The top of the title's 20-point line, centred in the row.
        pub fn title_line_top(&self) -> f64 {
            self.title_top + (metrics::TITLE_ROW_HEIGHT - styles::WINDOW_TITLE.line_height) / 2.0
        }
    }

    /// The top bar for traffic lights centred `lights_center` below the
    /// window's top: the title row is centred on them, as on the board, and
    /// the bar ends [`TOP_BAR_PADDING_Y`] below the row. With the board's
    /// lights (22 down) that is the board's bar, 10 + 24 + 10.
    pub fn top_bar(lights_center: f64) -> TopBar {
        let title_top = (lights_center - metrics::TITLE_ROW_HEIGHT / 2.0).max(0.0);
        TopBar {
            height: title_top + metrics::TITLE_ROW_HEIGHT + TOP_BAR_PADDING_Y,
            title_top,
        }
    }

    /// The window's height: the top bar and the sheet, at most
    /// `screen_height` (then the sheet scrolls).
    pub fn window_height(top_bar: f64, sheet: f64, screen_height: f64) -> f64 {
        (top_bar + sheet).min(screen_height)
    }
}
