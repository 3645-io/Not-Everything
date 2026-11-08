//! Contract tests for `fx_ui_macos::window_view`: what the search window
//! shows in each state (the Main, NoMatches and Components boards).

// Highlights are lists of ranges; many lists here hold one.
#![allow(clippy::single_range_in_vec_init)]

use fx_sort::Direction;
use fx_ui_macos::columns::Column;
use fx_ui_macos::file_glyph::Glyph;
use fx_ui_macos::surface::Surface;
use fx_ui_macos::theme::Token;
use fx_ui_macos::window_view::{
    Body, body, button_surface, folder_highlights, glyph_tint, header_text, is_numeric,
    row_surface, sort_glyph, with_example,
};

/// The card shows rows, "No matches for …" with a query and no rows, the
/// invalid regular expression message when the search could not run, and
/// nothing (the header only) without a query or rows.
#[test]
fn what_the_card_shows() {
    assert_eq!(body(true, 8, false), Body::Rows);
    assert_eq!(body(false, 2_014_377, false), Body::Rows);
    assert_eq!(body(true, 0, false), Body::NoMatches);
    assert_eq!(body(true, 0, true), Body::InvalidRegex);
    assert_eq!(body(false, 0, false), Body::Blank);
    // Spaces alone are no query.
    assert_eq!(body(false, 0, false), Body::Blank);
}

/// `.ne-row:hover { background: well }` and `[aria-pressed=true]`: washed,
/// rimmed and pressed; selection wins over hover.
#[test]
fn rows_at_rest_hover_and_selected() {
    assert_eq!(row_surface(false, false), None);
    assert_eq!(row_surface(false, true), Some(Surface::RowHover));
    assert_eq!(row_surface(true, false), Some(Surface::RowSelected));
    assert_eq!(row_surface(true, true), Some(Surface::RowSelected));
}

#[test]
fn file_glyphs_are_muted_or_green_ink_when_selected() {
    assert_eq!(glyph_tint(false), Token::Muted);
    assert_eq!(glyph_tint(true), Token::GreenInk);
}

/// `.ne-th { color: muted }`, `:hover { color: ink-strong }`,
/// `[data-active=true] { color: green-ink }` (later, so it wins).
#[test]
fn header_colours() {
    assert_eq!(header_text(false, false), Token::Muted);
    assert_eq!(header_text(false, true), Token::InkStrong);
    assert_eq!(header_text(true, false), Token::GreenInk);
    assert_eq!(header_text(true, true), Token::GreenInk);
    assert_eq!(sort_glyph(Direction::Ascending), Glyph::SortUp);
    assert_eq!(sort_glyph(Direction::Descending), Glyph::SortDown);
}

/// `.ne-th.num` and `.ne-num`: only Size is set right.
#[test]
fn only_the_size_column_is_right_aligned() {
    let numeric: Vec<Column> = Column::ALL.into_iter().filter(|c| is_numeric(*c)).collect();
    assert_eq!(numeric, [Column::Size]);
}

/// `.btn`, `.btn.primary` and `.btn:active`.
#[test]
fn buttons_press_in() {
    assert_eq!(button_surface(false, false), Surface::Button);
    assert_eq!(button_surface(false, true), Surface::ButtonDown);
    assert_eq!(button_surface(true, false), Surface::PrimaryButton);
    assert_eq!(button_surface(true, true), Surface::PrimaryButtonDown);
}

/// A click on a syntax example types it at the end of the search, after a
/// space ("add one of these"), or alone into an empty field.
#[test]
fn a_syntax_example_is_added_to_the_search() {
    assert_eq!(
        with_example("quarterly reprot", "ext:pdf"),
        "quarterly reprot ext:pdf"
    );
    assert_eq!(
        with_example("invoice  ", "dm:thisweek"),
        "invoice dm:thisweek"
    );
    assert_eq!(with_example("", "size:>10mb"), "size:>10mb");
    assert_eq!(with_example("   ", "\"tax return\""), "\"tax return\"");
}

/// The Path column shows the home folder as "~": highlights found in the
/// full folder move onto the shorter text (UTF-16 offsets).
#[test]
fn folder_highlights_follow_the_tilde() {
    let home = Some("/Users/user");
    // "/Users/user/Documents/Finance" shown as "~/Documents/Finance";
    // "Finance" at 22..29 moves to 12..19.
    assert_eq!(
        folder_highlights("/Users/user/Documents/Finance", home, &[22..29]),
        vec![12..19]
    );
    // A hit inside the replaced part highlights the "~".
    assert_eq!(
        folder_highlights("/Users/user/Documents", home, &[7..11, 12..15]),
        vec![0..1, 2..5]
    );
    // Folders outside home keep their ranges.
    assert_eq!(
        folder_highlights("/Volumes/Backup/Finance", home, &[16..23]),
        vec![16..23]
    );
    assert_eq!(folder_highlights("/Users/user", home, &[1..5]), vec![0..1]);
    // No home: unchanged.
    assert_eq!(
        folder_highlights("/Users/user/Docs", None, &[12..16]),
        vec![12..16]
    );
    // Non-ASCII home: the prefix is counted in UTF-16 units.
    assert_eq!(
        folder_highlights("/Users/zoë/Docs", Some("/Users/zoë"), &[11..15]),
        vec![2..6]
    );
    assert!(folder_highlights("/Users/user/Docs", home, &[]).is_empty());
}
