//! What the search window shows in each state (the Main, NoMatches and
//! Components boards), as pure rules the AppKit layer
//! applies: what the results card holds ([`body`]), the look of a row
//! ([`row_surface`], [`glyph_tint`]), of a column header ([`header_text`],
//! [`sort_glyph`]) and of a button ([`button_surface`]), what a click on a
//! syntax example types ([`with_example`]), and where matches are
//! highlighted in a folder shown from "~" ([`folder_highlights`]).

#![forbid(unsafe_code)]

use std::ops::Range;

use fx_query::highlight::replace_prefix;
use fx_sort::Direction;

use crate::columns::Column;
use crate::file_glyph::Glyph;
use crate::formats::display_folder;
use crate::surface::Surface;
use crate::theme::Token;

/// What the results card shows under its header.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Body {
    /// The result rows.
    Rows,
    /// "No matches for “…”" with the syntax examples.
    NoMatches,
    /// "That isn't a valid regular expression".
    InvalidRegex,
    /// Nothing: no query and no rows (an empty index).
    Blank,
}

/// The card's body for a search field that holds a query (`has_query`:
/// anything but spaces) or not, the result count, and whether the search
/// failed on an invalid regular expression.
pub const fn body(has_query: bool, rows: usize, invalid_regex: bool) -> Body {
    if invalid_regex {
        Body::InvalidRegex
    } else if rows > 0 {
        Body::Rows
    } else if has_query {
        Body::NoMatches
    } else {
        Body::Blank
    }
}

/// A row's surface: selected (washed, rimmed, pressed) over hovered
/// (`well`); `None` at rest.
pub const fn row_surface(selected: bool, hovered: bool) -> Option<Surface> {
    if selected {
        Some(Surface::RowSelected)
    } else if hovered {
        Some(Surface::RowHover)
    } else {
        None
    }
}

/// A row's file glyph colour: `green-ink` on the selected row, else
/// `muted`.
pub const fn glyph_tint(selected: bool) -> Token {
    if selected {
        Token::GreenInk
    } else {
        Token::Muted
    }
}

/// A column header's colour: `green-ink` for the sorted column, else
/// `ink-strong` under the pointer and `muted` at rest.
pub const fn header_text(sorted: bool, hovered: bool) -> Token {
    if sorted {
        Token::GreenInk
    } else if hovered {
        Token::InkStrong
    } else {
        Token::Muted
    }
}

/// The chevron after the sorted column's title.
pub const fn sort_glyph(direction: Direction) -> Glyph {
    match direction {
        Direction::Ascending => Glyph::SortUp,
        Direction::Descending => Glyph::SortDown,
    }
}

/// Whether `column` is set right (figures): only Size.
pub const fn is_numeric(column: Column) -> bool {
    matches!(column, Column::Size)
}

/// A button's surface: the primary (green) one or not, being pressed or
/// not.
pub const fn button_surface(primary: bool, pressed: bool) -> Surface {
    match (primary, pressed) {
        (false, false) => Surface::Button,
        (false, true) => Surface::ButtonDown,
        (true, false) => Surface::PrimaryButton,
        (true, true) => Surface::PrimaryButtonDown,
    }
}

/// The search after a click on the syntax example `example`: the example
/// typed at the end of `query`, after one space, or alone when `query` is
/// blank. "No matches" offers the examples to narrow a search, so they add
/// to it rather than replace it.
pub fn with_example(query: &str, example: &str) -> String {
    let kept = query.trim_end();
    if kept.trim_start().is_empty() {
        example.to_owned()
    } else {
        format!("{kept} {example}")
    }
}

/// The highlighted `ranges` of `folder` (UTF-16 units, as the search's
/// highlighter gives them) moved onto the Path column's text, which shows
/// the `home` folder as "~" ([`display_folder`]): a range inside the home
/// folder's part highlights the "~".
pub fn folder_highlights(
    folder: &str,
    home: Option<&str>,
    ranges: &[Range<usize>],
) -> Vec<Range<usize>> {
    if ranges.is_empty() {
        return Vec::new();
    }
    let shown = display_folder(folder, home);
    if shown == folder {
        return ranges.to_vec();
    }
    let home = home.map(|h| h.trim_end_matches('/')).unwrap_or("");
    replace_prefix(ranges, home.encode_utf16().count(), 1)
}
