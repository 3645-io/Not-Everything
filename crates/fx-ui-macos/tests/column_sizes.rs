//! Contract tests for `fx_ui_macos::column_sizes`: the widths the user
//! gives the results columns by dragging the dividers between their titles
//! (the Main board's column dividers), and with `main_layout` the table
//! they lay out.
//!
//! - `ColumnSizes`: the Name, Path and Size widths in whole points, each
//!   within its range (Name 140, Path 120, Size 64, all at most 800);
//!   printed and parsed as the settings file's `300,480,88`.
//! - `Drag`: started on a divider over the laid-out widths; the column on
//!   the divider's left follows the pointer, the others keep their widths.
//! - `step`: Left or Right on a focused divider, 8 points, or 32 with Shift.
//! - `divider_token`: the line's colour at rest and active.

use fx_ui_macos::column_sizes::{ColumnSizes, Drag, Step, divider_token, range, step};
use fx_ui_macos::main_layout::{card, column_widths, resized_column_widths};
use fx_ui_macos::theme::Token;

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

/// The default layout of the board's 1,248-point card: Name 420, Path
/// 504, Size 88, Date Modified 156 (`column_widths(1216)`).
const BOARD: [f64; 4] = [420.0, 504.0, 88.0, 156.0];
/// The board card's columns' width: 1,248 less 16 points of padding a side.
const CONTENT: f64 = 1216.0;

#[test]
fn the_ranges_are_the_boards() {
    assert_eq!(range(0), (140.0, 800.0));
    assert_eq!(range(1), (120.0, 800.0));
    assert_eq!(range(2), (64.0, 800.0));
}

#[test]
fn sizes_are_whole_points_within_each_columns_range() {
    let s = ColumnSizes::new([300.4, 480.6, 88.0]);
    assert_eq!(s.widths(), [300.0, 481.0, 88.0]);
    let s = ColumnSizes::new([10.0, -5.0, 0.0]);
    assert_eq!(s.widths(), [140.0, 120.0, 64.0], "never under a minimum");
    let s = ColumnSizes::new([5000.0, 801.0, 1e9]);
    assert_eq!(s.widths(), [800.0, 800.0, 800.0], "never over 800");
    let s = ColumnSizes::new([f64::NAN, 300.0, 90.0]);
    assert_eq!(s.widths(), [140.0, 300.0, 90.0], "nonsense is the minimum");
}

/// Dragging resizes the column on the divider's left by the pointer's
/// movement; the other two keep their widths and Date Modified takes the
/// rest.
#[test]
fn a_drag_resizes_the_column_on_the_dividers_left() {
    assert_eq!(column_widths(CONTENT), BOARD);
    let mut d = Drag::new(0, BOARD);
    let s = d.to(60.0).unwrap();
    assert_eq!(s.widths(), [480.0, 504.0, 88.0]);
    // 1,216 − 480 − 504 − 88 − 3 × 16 leaves 96: Date Modified keeps its
    // 140 and the table scrolls.
    let w = resized_column_widths(CONTENT, s);
    assert_eq!(w, [480.0, 504.0, 88.0, 140.0]);
    let mut d = Drag::new(0, BOARD);
    let s = d.to(-20.0).unwrap();
    assert_eq!(
        resized_column_widths(CONTENT, s),
        [400.0, 504.0, 88.0, 176.0]
    );
    // Path: narrower by 100.4 points, rounded.
    let mut d = Drag::new(1, BOARD);
    assert_eq!(d.to(-100.4).unwrap().widths(), [420.0, 404.0, 88.0]);
    // Size: the last divider.
    let mut d = Drag::new(2, BOARD);
    assert_eq!(d.to(12.0).unwrap().widths(), [420.0, 504.0, 100.0]);
}

/// Each step of the drag is measured from where it started, so the
/// column follows the pointer back and forth.
#[test]
fn a_drag_follows_the_pointer_from_where_it_started() {
    let mut d = Drag::new(1, BOARD);
    assert_eq!(d.to(30.0).unwrap().widths()[1], 534.0);
    assert_eq!(d.to(-30.0).unwrap().widths()[1], 474.0);
    assert_eq!(
        d.to(0.0).unwrap().widths()[1],
        504.0,
        "back where it started, still the user's widths"
    );
}

/// A click without movement changes nothing: the default layout stays
/// until a column's width really changes.
#[test]
fn a_click_alone_changes_nothing() {
    let mut d = Drag::new(0, BOARD);
    assert_eq!(d.to(0.0), None);
    assert_eq!(d.to(0.4), None, "less than half a point");
    assert!(d.to(0.6).is_some());
}

/// The first drag starts from the laid-out widths, rounded to whole
/// points: the default layout's fractional widths move by at most half a
/// point.
#[test]
fn the_first_drag_starts_from_the_default_layout() {
    let w = column_widths(1000.0);
    assert!(w[0].fract() != 0.0, "{w:?}");
    let mut d = Drag::new(2, w);
    let s = d.to(4.0).unwrap();
    assert_eq!(s.widths(), [w[0].round(), w[1].round(), 92.0]);
}

#[test]
fn a_drag_stops_at_the_columns_minimum_and_at_800() {
    for (i, min) in [(0, 140.0), (1, 120.0), (2, 64.0)] {
        let mut d = Drag::new(i, BOARD);
        assert_eq!(d.to(-2000.0).unwrap().widths()[i], min, "column {i}");
        assert_eq!(d.to(2000.0).unwrap().widths()[i], 800.0, "column {i}");
    }
}

/// Date Modified takes what the other three leave, never less than 140
/// points; when the columns don't fit, the table is wider than the card
/// and scrolls sideways.
#[test]
fn date_modified_takes_the_rest_and_the_table_scrolls_when_full() {
    let s = ColumnSizes::new([300.0, 400.0, 88.0]);
    let w = resized_column_widths(CONTENT, s);
    assert_eq!(w, [300.0, 400.0, 88.0, CONTENT - 788.0 - 48.0]);
    let s = ColumnSizes::new([600.0, 500.0, 88.0]);
    let w = resized_column_widths(CONTENT, s);
    assert_eq!(w[3], 140.0, "never under 140");
    let c = card(1248.0, 600.0, 0.0, Some(s));
    assert_eq!(c.columns.widths, w);
    let end = c.columns.x[3] + c.columns.widths[3] + 16.0;
    assert!(close(end, 16.0 + 1188.0 + 48.0 + 140.0 + 16.0));
    assert!(close(c.width, end), "scrolls sideways by {}", end - 1248.0);
    assert!(c.width > 1248.0);
    // When they fit, nothing scrolls.
    assert_eq!(card(1248.0, 600.0, 0.0, None).width, 1248.0);
    assert_eq!(
        card(
            1248.0,
            600.0,
            0.0,
            Some(ColumnSizes::new([300.0, 400.0, 88.0]))
        )
        .width,
        1248.0
    );
}

/// Resized columns keep their widths when the window narrows; Date
/// Modified gives way, down to 140 points, then the table scrolls.
#[test]
fn resized_columns_keep_their_widths_in_a_narrower_window() {
    let s = ColumnSizes::new([300.0, 300.0, 88.0]);
    for width in [1248.0, 1000.0, 900.0, 728.0] {
        let c = card(width, 600.0, 0.0, Some(s));
        assert_eq!(c.columns.widths[..3], [300.0, 300.0, 88.0], "{width}");
        let rest = (width - 32.0 - 688.0 - 48.0).max(140.0);
        assert!(close(c.columns.widths[3], rest), "{width}");
        assert!(close(c.width, width.max(32.0 + 688.0 + 48.0 + 140.0)));
    }
}

/// Double-click: back to the default layout, which follows the window.
#[test]
fn a_reset_restores_the_default_layout() {
    let resized = card(
        1248.0,
        600.0,
        0.0,
        Some(ColumnSizes::new([140.0, 120.0, 64.0])),
    );
    let reset = card(1248.0, 600.0, 0.0, None);
    assert_ne!(resized.columns.widths, reset.columns.widths);
    assert_eq!(reset.columns.widths, BOARD);
}

/// A legacy scroller's width comes out of Date Modified, which takes the
/// rest, so the rows still fit beside it.
#[test]
fn the_rest_column_makes_room_for_a_legacy_scroller() {
    let s = ColumnSizes::new([300.0, 400.0, 88.0]);
    let overlay = card(1248.0, 600.0, 0.0, Some(s));
    let legacy = card(1248.0, 600.0, 17.0, Some(s));
    assert_eq!(legacy.columns.widths[3], overlay.columns.widths[3] - 17.0);
    assert_eq!(legacy.width, 1248.0);
}

/// Left or Right on a focused divider: 8 points, 32 with Shift, within
/// the column's range.
#[test]
fn arrow_keys_step_a_focused_divider() {
    let s = step(BOARD, 0, Step::Wider, false);
    assert_eq!(s.widths(), [428.0, 504.0, 88.0]);
    let s = step(BOARD, 1, Step::Narrower, false);
    assert_eq!(s.widths(), [420.0, 496.0, 88.0]);
    let s = step(BOARD, 2, Step::Wider, true);
    assert_eq!(s.widths(), [420.0, 504.0, 120.0]);
    let s = step(BOARD, 2, Step::Narrower, true);
    assert_eq!(s.widths(), [420.0, 504.0, 64.0], "88 − 32 is under 64");
    let at_max = resized_column_widths(CONTENT, ColumnSizes::new([800.0, 504.0, 88.0]));
    assert_eq!(step(at_max, 0, Step::Wider, true).widths()[0], 800.0);
    // Steps add up from the widths they start from.
    let mut w = BOARD;
    for _ in 0..3 {
        w = resized_column_widths(CONTENT, step(w, 1, Step::Narrower, false));
    }
    assert_eq!(w[1], 480.0);
}

#[test]
fn sizes_print_and_parse_as_the_settings_value() {
    let s = ColumnSizes::new([300.0, 480.0, 88.0]);
    assert_eq!(s.to_string(), "300,480,88");
    assert_eq!(ColumnSizes::parse("300,480,88"), Some(s));
    assert_eq!(ColumnSizes::parse(" 300 , 480 , 88 "), Some(s));
    // Out of range: kept within it, as a drag would.
    assert_eq!(
        ColumnSizes::parse("10,9000,64"),
        Some(ColumnSizes::new([140.0, 800.0, 64.0]))
    );
    for bad in [
        "",
        "300",
        "300,480",
        "300,480,88,156",
        "300,480,x",
        "300.5,480,88",
        "-1,480,88",
        "300;480;88",
    ] {
        assert_eq!(ColumnSizes::parse(bad), None, "{bad:?}");
    }
    for widths in [
        [140.0, 120.0, 64.0],
        [800.0, 800.0, 800.0],
        [421.0, 503.0, 89.0],
    ] {
        let s = ColumnSizes::new(widths);
        assert_eq!(ColumnSizes::parse(&s.to_string()), Some(s));
    }
}

/// `.ne-col-handle::before`: `edge` at rest, `green-ink` hovered, focused
/// or dragged.
#[test]
fn the_divider_turns_green_when_active() {
    assert_eq!(divider_token(false), Token::Edge);
    assert_eq!(divider_token(true), Token::GreenInk);
}
