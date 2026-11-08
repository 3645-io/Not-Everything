//! Tests for `fx_ui_macos::settings_view`: the Settings window's copy and
//! contents (after the canvas's Settings board),
//! the rows' states and the sheet's geometry.
//!
//! Contract exercised here:
//!
//! - The window title, the Appearance note, the App Icon label, the
//!   "Default" tag and the icon note (which names the app).
//! - `appearance_segments()` are the `AppearanceMode::ALL` titles and
//!   `icon_tiles()` the `AppIcon::ALL` designs, in order, so the window
//!   shows exactly the approved choices.
//! - `login_row` follows the login item's state as the menu-bar menu does
//!   (on while enabled or waiting for approval), says when approval is
//!   needed, greys out when there is no bundle, and shows an error in place
//!   of the caption.
//! - `access_row` is the board's Granted row, or the derived Off row.
//! - `layout`: the board's CSS boxes (`.ne-sheet`, `.card`, `.sec-title`,
//!   `.ne-setting`, `.ne-note`, `.ne-icons`, `.ne-icon-pick`, `.seg`,
//!   `.btn`, `.ne-switch`, `.ne-key`, `.tag`), measured on the board at its
//!   640-point preview width, and the top bar lined up with the traffic
//!   lights.
//! - `APP_NAME` is the product name, Not-Everything, and `APP_BUNDLE_ID`
//!   its bundle ID, in a release build; a Preview build (release candidate
//!   or beta) is Not-Everything Preview, `io.3645.not-everything.preview`.

use fx_channel::Channel;
use fx_fs::AccessState;
use fx_ui_macos::app_args::APP_FOLDER;
use fx_ui_macos::app_icon::AppIcon;
use fx_ui_macos::appearance::AppearanceMode;
use fx_ui_macos::settings_view::layout::{self, Frame, Sheet};
use fx_ui_macos::settings_view::{
    ACCESS_GRANTED_CAPTION, ACCESS_OFF_CAPTION, APPEARANCE_NOTE, DEFAULT_BADGE, ICON_LABEL,
    LOGIN_APPROVAL_CAPTION, LOGIN_UNAVAILABLE_CAPTION, LoginState, SECTIONS, SwitchRow, TITLE,
    YOUR_FILES_FIRST_CAPTION, YOUR_FILES_FIRST_LABEL, access_row, appearance_segments, icon_note,
    icon_tiles, login_caption, login_row, your_files_first_row,
};
use fx_ui_macos::{APP_BUNDLE_ID, APP_NAME};

#[test]
fn copy_matches_the_board() {
    assert_eq!(TITLE, "Settings");
    assert_eq!(
        APPEARANCE_NOTE,
        "System follows the light or dark setting of your Mac."
    );
    assert_eq!(ICON_LABEL, "App Icon");
    assert_eq!(SECTIONS[3], ICON_LABEL);
    assert_eq!(DEFAULT_BADGE, "Default");
    assert_eq!(YOUR_FILES_FIRST_LABEL, "Your files first");
    assert_eq!(
        icon_note("Not-Everything"),
        "The Dock shows your choice while Not-Everything is running. \
         Finder and Launchpad show the default icon."
    );
}

#[test]
fn segments_are_the_appearance_modes() {
    let segments: Vec<(AppearanceMode, &str)> = appearance_segments().collect();
    assert_eq!(
        segments,
        [
            (AppearanceMode::System, "System"),
            (AppearanceMode::Light, "Light"),
            (AppearanceMode::Dark, "Dark"),
        ]
    );
}

#[test]
fn tiles_are_the_approved_icons_with_one_default_badge() {
    let tiles: Vec<AppIcon> = icon_tiles().map(|t| t.icon).collect();
    assert_eq!(tiles, AppIcon::ALL.as_slice());
    let badged: Vec<AppIcon> = icon_tiles()
        .filter(|t| t.badge == Some(DEFAULT_BADGE))
        .map(|t| t.icon)
        .collect();
    assert_eq!(badged, [AppIcon::MatchRow]);
    for tile in icon_tiles() {
        assert_eq!(tile.title, tile.icon.title());
    }
}

#[test]
fn the_login_switch_follows_the_login_item() {
    let caption = login_caption("Not-Everything");
    assert_eq!(
        login_row("Not-Everything", LoginState::Enabled, None),
        SwitchRow {
            on: true,
            enabled: true,
            caption: caption.clone(),
            error: false,
        }
    );
    assert_eq!(
        login_row("Not-Everything", LoginState::Off, None),
        SwitchRow {
            on: false,
            enabled: true,
            caption: caption.clone(),
            error: false,
        }
    );
    // Waiting for approval counts as on, as the menu's check mark does, and
    // says where to approve it (derived: not on the board).
    assert_eq!(
        LOGIN_APPROVAL_CAPTION,
        "Requires approval in System Settings \u{203a} Login Items."
    );
    assert_eq!(
        login_row("Not-Everything", LoginState::NeedsApproval, None),
        SwitchRow {
            on: true,
            enabled: true,
            caption: LOGIN_APPROVAL_CAPTION.to_string(),
            error: false,
        }
    );
    // Without a bundle there is no login item: off and greyed out, with the
    // menu item's tool tip as the caption.
    assert_eq!(
        LOGIN_UNAVAILABLE_CAPTION,
        "Available when run from the .app bundle."
    );
    assert_eq!(
        login_row("Not-Everything", LoginState::Unavailable, None),
        SwitchRow {
            on: false,
            enabled: false,
            caption: LOGIN_UNAVAILABLE_CAPTION.to_string(),
            error: false,
        }
    );
    // A refused change shows the reason in place of the caption.
    assert_eq!(
        login_row(
            "Not-Everything",
            LoginState::Off,
            Some("Operation not permitted")
        ),
        SwitchRow {
            on: false,
            enabled: true,
            caption: "Operation not permitted".to_string(),
            error: true,
        }
    );
}

#[test]
fn the_your_files_first_switch() {
    assert_eq!(
        your_files_first_row(true),
        SwitchRow {
            on: true,
            enabled: true,
            caption: YOUR_FILES_FIRST_CAPTION.to_string(),
            error: false,
        }
    );
    assert!(!your_files_first_row(false).on);
}

#[test]
fn the_access_row_is_granted_or_the_derived_off_state() {
    let granted = access_row(AccessState::Granted);
    assert!(granted.granted);
    assert_eq!(granted.label, "Granted");
    assert_eq!(granted.caption, ACCESS_GRANTED_CAPTION);
    assert_eq!(granted.button, "Open System Settings");
    assert!(!granted.primary, "the board's button is a plain .btn");
    for access in [AccessState::Denied, AccessState::Unknown] {
        let off = access_row(access);
        assert!(!off.granted);
        assert_eq!(off.label, "Off");
        assert_eq!(off.caption, ACCESS_OFF_CAPTION);
        assert_eq!(off.button, "Open System Settings");
        assert!(off.primary, "the one action that matters while off");
    }
}

fn frame(x: f64, y: f64, width: f64, height: f64) -> Frame {
    Frame {
        x,
        y,
        width,
        height,
    }
}

#[test]
fn the_sheet_is_640_wide_at_most_and_centred() {
    assert_eq!(layout::sheet_frame(640.0), (0.0, 640.0));
    assert_eq!(layout::sheet_frame(800.0), (80.0, 640.0));
    assert_eq!(layout::sheet_frame(500.0), (0.0, 500.0));
    // .ne-sheet padding 16 and .card padding 16 on each side.
    assert_eq!(layout::card_content_width(640.0), 576.0);
}

/// The General card at the board's width, with one-line captions: the
/// title, a first row (no divider, 4 on top) and a second row under a
/// divider, controls centred on the text.
#[test]
fn a_card_with_two_setting_rows() {
    let mut sheet = Sheet::new(640.0);
    let mut card = sheet.card();
    assert_eq!(card.title(), frame(32.0, 32.0, 576.0, 16.0));
    // Text: label 22 + 2 + caption 18. Control: key caps 22 high.
    let text = layout::text_block_height(18.0);
    assert_eq!(text, 42.0);
    let keys = card.row(text, (90.0, 22.0));
    assert_eq!(keys.divider, None);
    assert_eq!(keys.text, frame(32.0, 60.0, 576.0 - 16.0 - 90.0, 42.0));
    assert_eq!(keys.control, frame(32.0 + 576.0 - 90.0, 70.0, 90.0, 22.0));
    let login = card.row(text, (46.0, 28.0));
    assert_eq!(login.divider, Some(frame(32.0, 114.0, 576.0, 1.0)));
    assert_eq!(login.text, frame(32.0, 127.0, 576.0 - 16.0 - 46.0, 42.0));
    assert_eq!(login.control, frame(562.0, 134.0, 46.0, 28.0));
    let general = sheet.end(card);
    assert_eq!(general, frame(16.0, 16.0, 608.0, 181.0));
    // The next card starts 16 below.
    let next = sheet.card();
    assert_eq!(next.top(), 213.0);
}

#[test]
fn a_tall_control_centres_the_text() {
    let mut sheet = Sheet::new(640.0);
    let mut card = sheet.card();
    card.title();
    let row = card.row(20.0, (100.0, 36.0));
    assert_eq!(row.text.y, 56.0 + 4.0 + 8.0);
    assert_eq!(row.control.y, 60.0);
    assert_eq!(
        sheet.end(card).height,
        16.0 + 16.0 + 8.0 + 4.0 + 36.0 + 12.0 + 16.0
    );
}

/// The Appearance card: the segmented control right under the title, then
/// a note 12 below it.
#[test]
fn a_card_with_a_block_and_a_note() {
    let mut sheet = Sheet::new(640.0);
    let mut card = sheet.card();
    card.title();
    assert_eq!(card.block(35.0), 56.0);
    assert_eq!(card.note(18.0), 56.0 + 35.0 + 12.0);
    assert_eq!(
        sheet.end(card).height,
        16.0 + 16.0 + 8.0 + 35.0 + 12.0 + 18.0 + 16.0
    );
}

#[test]
fn the_footnote_ends_the_sheet() {
    let mut sheet = Sheet::new(640.0);
    let mut card = sheet.card();
    card.title();
    card.block(100.0);
    let end = sheet.end(card);
    assert_eq!(end.y + end.height, 16.0 + 156.0);
    let foot = sheet.footnote();
    assert_eq!(foot, frame(16.0, 172.0 + 16.0, 608.0, 16.0));
    assert_eq!(sheet.height(), 172.0 + 16.0 + 16.0 + 32.0);
}

#[test]
fn segments_and_their_container() {
    let (container, buttons) = layout::segments(32.0, 56.0, &[50.0, 36.0, 34.0]);
    // Each button: text + 10 + 10 padding + 1.5 + 1.5 border; 31 high.
    assert_eq!(buttons[0], frame(34.0, 58.0, 73.0, 31.0));
    assert_eq!(buttons[1], frame(34.0 + 73.0 + 2.0, 58.0, 59.0, 31.0));
    assert_eq!(
        buttons[2],
        frame(34.0 + 73.0 + 2.0 + 59.0 + 2.0, 58.0, 57.0, 31.0)
    );
    assert_eq!(
        container,
        frame(32.0, 56.0, 2.0 + 73.0 + 2.0 + 59.0 + 2.0 + 57.0 + 2.0, 35.0)
    );
    assert_eq!(
        layout::segment_text(buttons[0]),
        frame(45.5, 63.5, 50.0, 20.0)
    );
}

#[test]
fn the_icon_grid() {
    let tagged = [false, false, true, false, false, false];
    let (picks, height) = layout::icon_grid(32.0, 100.0, 576.0, &tagged);
    // Three columns 12 apart; the tagged row is 150 high, the other 140
    // (min-height); rows 12 apart.
    assert_eq!(picks[0], frame(32.0, 100.0, 184.0, 150.0));
    assert_eq!(picks[1], frame(228.0, 100.0, 184.0, 150.0));
    assert_eq!(picks[2], frame(424.0, 100.0, 184.0, 150.0));
    assert_eq!(picks[3], frame(32.0, 262.0, 184.0, 140.0));
    assert_eq!(picks[5], frame(424.0, 262.0, 184.0, 140.0));
    assert_eq!(height, 150.0 + 12.0 + 140.0);
    // Inside a pick: the 72-point icon 16 from the top (2 border + 14),
    // the name 6 below it, the tag 6 below the name.
    let inside = layout::icon_pick_content(picks[2]);
    assert_eq!(inside.icon, frame(424.0 + 56.0, 116.0, 72.0, 72.0));
    assert_eq!(inside.name, frame(424.0 + 10.0, 194.0, 164.0, 18.0));
    assert_eq!(inside.tag_top, 218.0);
    // An untagged grid row of 126 content grows to the 140 minimum.
    let (_, h) = layout::icon_grid(0.0, 0.0, 576.0, &[false; 6]);
    assert_eq!(h, 140.0 + 12.0 + 140.0);
}

#[test]
fn tags_buttons_switches_and_key_caps() {
    // .tag: padding 2 7, line 14.
    assert_eq!(layout::tag_size(48.0), (62.0, 18.0));
    // .btn: border 2, padding 6 12, line 20.
    assert_eq!(layout::button_size(145.0), (173.0, 36.0));
    assert_eq!(
        layout::button_text(frame(10.0, 20.0, 173.0, 36.0)),
        frame(24.0, 28.0, 145.0, 20.0)
    );
    // .ne-switch 46 x 28 with a 1-point edge; the knob 3 inside it, moving
    // 18 when on.
    assert_eq!(layout::SWITCH_SIZE, (46.0, 28.0));
    assert_eq!(layout::knob(false), frame(4.0, 4.0, 20.0, 20.0));
    assert_eq!(layout::knob(true), frame(22.0, 4.0, 20.0, 20.0));
    // .ne-key: min 22 wide, padding 0 6, 22 high; caps 3 apart, right
    // aligned in the row.
    let (all, caps) = layout::key_caps(608.0, 70.0, &[9.0, 9.0, 37.0]);
    assert_eq!(
        caps[0],
        frame(608.0 - 49.0 - 3.0 - 22.0 - 3.0 - 22.0, 70.0, 22.0, 22.0)
    );
    assert_eq!(caps[2], frame(608.0 - 49.0, 70.0, 49.0, 22.0));
    assert_eq!(all, frame(608.0 - 99.0, 70.0, 99.0, 22.0));
    assert_eq!(layout::key_caps_width(&[9.0, 9.0, 37.0]), 99.0);
}

#[test]
fn baselines_sit_where_css_puts_them() {
    // Blink rounds the font's ascent and descent to whole pixels and splits
    // the rest of the line height evenly above and below.
    assert_eq!(layout::css_baseline(22.0, 14.5, 3.16), 17.0);
    assert_eq!(layout::css_baseline(16.0, 11.6, 2.53), 12.5);
    assert_eq!(layout::css_baseline(18.0, 12.57, 2.74), 14.0);
    assert_eq!(layout::css_baseline(14.0, 10.63, 2.32), 11.5);
    // On screen a baseline lands on a pixel; a half pixel goes up, as
    // Chrome draws it at 1x.
    assert_eq!(layout::snap_baseline(12.5, 1.0), 12.0);
    assert_eq!(layout::snap_baseline(12.5, 2.0), 12.5);
    assert_eq!(layout::snap_baseline(16.6, 1.0), 17.0);
    assert_eq!(layout::snap_baseline(17.2, 2.0), 17.0);
    assert_eq!(layout::snap_down(10.75, 2.0), 10.5);
    assert_eq!(layout::snap_down(10.75, 1.0), 10.0);
    // TextKit sets a fixed-height line's baseline its descent, rounded up,
    // above the line's bottom.
    assert_eq!(layout::textkit_baseline(22.0, 3.16), 18.0);
    assert_eq!(layout::textkit_baseline(14.0, 2.32), 11.0);
}

#[test]
fn the_access_dot_sits_on_the_label_s_baseline() {
    // A 22-point line of a font 14.5 up and 3.16 down: the baseline is 17
    // down; the dot's bottom 1 above it (vertical-align 1px).
    let dot = layout::access_dot(32.0, 100.0, 14.5, 3.16);
    assert_eq!(dot.y, 100.0 + 17.0 - 1.0 - 8.0);
    assert_eq!((dot.x, dot.width, dot.height), (32.0, 8.0, 8.0));
    // "Granted" starts after the dot and its 10-point margin.
    assert_eq!(layout::ACCESS_LABEL_INDENT, 18.0);
}

#[test]
fn the_top_bar_lines_up_with_the_traffic_lights() {
    // The board: lights centred 22 down, so padding 10, row 24, padding 10.
    let bar = layout::top_bar(22.0);
    assert_eq!((bar.height, bar.title_top), (44.0, 10.0));
    // The system's lights 14 down: the 24-point title row is centred on
    // them and the bar keeps its 10 below.
    let bar = layout::top_bar(14.0);
    assert_eq!((bar.height, bar.title_top), (36.0, 2.0));
    // The title's 20-point line in the middle of the row.
    assert_eq!(bar.title_line_top(), 4.0);
}

#[test]
fn the_window_is_capped_at_the_screen() {
    assert_eq!(layout::window_height(44.0, 1116.0, 2000.0), 1160.0);
    assert_eq!(layout::window_height(44.0, 1116.0, 900.0), 900.0);
}

#[test]
fn app_name_and_bundle_id_are_the_owner_s_choice() {
    let (name, id) = match Channel::CURRENT {
        Channel::Release => ("Not-Everything", "io.3645.not-everything"),
        Channel::Rc | Channel::Beta => ("Not-Everything Preview", "io.3645.not-everything.preview"),
    };
    assert_eq!(APP_NAME, name);
    assert_eq!(APP_BUNDLE_ID, id);
    assert_eq!(APP_FOLDER, name, "the data folder is named after the app");
}

// ------------------------------------------------------- shortcut recorder
//
// Run 2.10: the "Show the search window" row records a new shortcut, after
// the canvas's shortcut recorder board (its nine states, light and dark).
//
// - `hotkey_row(spec, default, recording, error) -> HotkeyRow`: what the
//   row shows at rest (the caps, the caption or the reason a change was
//   refused in `danger-ink`, and "Use Default (…)" while the shortcut isn't
//   the default) and while recording (the held modifiers' caps, the
//   recording caption, no reset link).
// - `layout`: the recorder's box (`.rec`), which reaches 4 past the caps
//   on each side and 5 above and below, so at rest the row lays out as
//   before; while recording it is at least 144 wide. Measured on the board
//   at its 1440-point width, where each card is the Settings sheet's 608.

use fx_ui_macos::hotkey::{HotkeySpec, Modifiers, Refusal};
use fx_ui_macos::settings_view::{
    HOTKEY_CAPTION, HOTKEY_LABEL, HOTKEY_RECORDING_CAPTION, HOTKEY_RECORDING_HINT, HotkeyRow,
    hotkey_reset_title, hotkey_row,
};

fn hk(s: &str) -> HotkeySpec {
    HotkeySpec::parse(s).expect("a valid hotkey")
}

#[test]
fn recorder_copy_matches_the_board() {
    assert_eq!(HOTKEY_LABEL, "Show the search window");
    assert_eq!(HOTKEY_CAPTION, "Press this shortcut in any app.");
    assert_eq!(
        HOTKEY_RECORDING_CAPTION,
        "Type the new shortcut. Esc cancels."
    );
    assert_eq!(HOTKEY_RECORDING_HINT, "Type shortcut");
    assert_eq!(
        hotkey_reset_title(hk("option+space")),
        "Use Default (\u{2325}Space)"
    );
    // The Preview app's default, in Apple's modifier order.
    assert_eq!(
        hotkey_reset_title(hk("shift+option+space")),
        "Use Default (\u{2325}\u{21e7}Space)"
    );
}

#[test]
fn the_row_at_rest_shows_the_caps_and_the_caption() {
    let default = hk("option+space");
    assert_eq!(
        hotkey_row(default, default, None, None),
        HotkeyRow {
            caption: HOTKEY_CAPTION.to_string(),
            error: false,
            recording: false,
            caps: vec!["\u{2325}", "Space"],
            reset: None,
            spoken: "Show the search window: Option Space".to_string(),
        }
    );
}

#[test]
fn another_shortcut_offers_the_default() {
    let row = hotkey_row(hk("control+option+k"), hk("option+space"), None, None);
    assert_eq!(row.caps, ["\u{2303}", "\u{2325}", "K"]);
    assert_eq!(row.reset.as_deref(), Some("Use Default (\u{2325}Space)"));
    assert_eq!(row.spoken, "Show the search window: Control Option K");
    assert!(!row.error);
}

#[test]
fn recording_shows_the_held_modifiers() {
    let default = hk("option+space");
    let none = Modifiers::default();
    let row = hotkey_row(hk("control+option+k"), default, Some(none), None);
    assert_eq!(
        row,
        HotkeyRow {
            caption: HOTKEY_RECORDING_CAPTION.to_string(),
            error: false,
            recording: true,
            caps: vec![],
            reset: None,
            spoken: "Show the search window: recording".to_string(),
        }
    );
    let held = Modifiers {
        cmd: false,
        shift: false,
        option: true,
        control: true,
    };
    let row = hotkey_row(default, default, Some(held), None);
    assert_eq!(row.caps, ["\u{2303}", "\u{2325}"]);
    assert!(row.recording);
}

#[test]
fn a_refusal_replaces_the_caption_in_danger_ink() {
    let default = hk("option+space");
    let refusal = Refusal::UsedByApps(hk("cmd+q"));
    let row = hotkey_row(default, default, None, Some(&refusal));
    assert_eq!(row.caption, refusal.to_string());
    assert!(row.error);
    assert_eq!(row.caps, ["\u{2325}", "Space"]);
    // While recording the recording caption shows instead.
    let row = hotkey_row(default, default, Some(Modifiers::default()), Some(&refusal));
    assert_eq!(row.caption, HOTKEY_RECORDING_CAPTION);
    assert!(!row.error);
    // A failed launch is shown the same way.
    let launch = Refusal::NotAcceptedAtLaunch(default);
    let row = hotkey_row(default, default, None, Some(&launch));
    assert_eq!(row.caption, launch.to_string());
    assert!(row.error);
}

#[test]
fn the_recorder_at_rest_lays_out_as_the_caps() {
    // ⌥ and Space: labels 11.98 and 37.22 wide, caps 23.98 and 49.22.
    let caps = layout::key_caps_width(&[11.98, 37.22]);
    assert!((caps - 76.2).abs() < 1e-9);
    assert_eq!(layout::recorder_size(caps, false), (caps, 22.0));
    let mut sheet = Sheet::new(640.0);
    let mut card = sheet.card();
    card.title();
    let row = card.row(layout::text_block_height(18.0), (caps, 22.0));
    assert_eq!(row.control, frame(608.0 - caps, 70.0, caps, 22.0));
    // The box reaches 4 past the caps on each side and 5 above and below.
    assert_eq!(layout::RECORDER_REACH, [5.0, 4.0]);
    assert_eq!(layout::RECORDER_HEIGHT, 32.0);
    let bx = layout::recorder_box(row.control);
    assert_eq!(bx, frame(608.0 - caps - 4.0, 65.0, caps + 8.0, 32.0));
    // The caps sit where they always did: right-aligned, centred.
    assert_eq!(
        layout::recorder_content(bx, (caps, 22.0), false),
        row.control
    );
}

#[test]
fn the_recording_box_is_at_least_144_wide() {
    assert_eq!(layout::RECORDING_MIN_WIDTH, 144.0);
    // "Type shortcut": text 83.94 wide, 4 of padding each side.
    let hint = layout::recorder_hint_width(83.94);
    assert!((hint - 91.94).abs() < 1e-9);
    // Its footprint in the row: the box less the 4 it reaches on each side.
    assert_eq!(layout::recorder_size(hint, true), (136.0, 22.0));
    let mut sheet = Sheet::new(640.0);
    let mut card = sheet.card();
    card.title();
    let row = card.row(layout::text_block_height(18.0), (136.0, 22.0));
    let bx = layout::recorder_box(row.control);
    assert_eq!(bx, frame(468.0, 65.0, 144.0, 32.0));
    // The hint ends inside the 2-point rim and 6 of padding, centred on
    // its 18-point line.
    let content = layout::recorder_content(bx, (hint, 18.0), true);
    assert!((content.x - (612.0 - 8.0 - 91.94)).abs() < 1e-9);
    assert_eq!((content.y, content.height), (72.0, 18.0));
    // Two held caps (⌃ 22.89 and ⌥ 23.98) end at the same place.
    let held = 22.89 + 3.0 + 23.98;
    let content = layout::recorder_content(bx, (held, 22.0), true);
    assert!((content.right() - 604.0).abs() < 1e-9);
    assert_eq!(content.y, 70.0);
    // Wider content grows the box past 144: content plus rim and padding.
    assert_eq!(layout::recorder_size(200.0, true), (208.0, 22.0));
}

#[test]
fn the_reset_link_sits_under_the_caption() {
    // The 18-point link 2 below the caption, on the row's 21-point line.
    assert_eq!(layout::hotkey_text_height(18.0, false), 42.0);
    assert_eq!(layout::hotkey_text_height(18.0, true), 63.0);
    assert_eq!(layout::hotkey_text_height(36.0, true), 81.0);
    let text = frame(32.0, 60.0, 400.0, 63.0);
    assert_eq!(
        layout::reset_link(text, 18.0, 142.33),
        frame(32.0, 104.0, 142.33, 18.0)
    );
    // The caps centre on the taller text, as on the board (31.5 down).
    let mut sheet = Sheet::new(640.0);
    let mut card = sheet.card();
    card.title();
    let row = card.row(63.0, (76.0, 22.0));
    assert_eq!(layout::recorder_box(row.control).y, 60.0 + 20.5 - 5.0);
}
