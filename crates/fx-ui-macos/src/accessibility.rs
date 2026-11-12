//! What VoiceOver reads for the search window's custom-drawn parts.
//!
//! The window draws its own look on AppKit's views (`conventions.md`), so
//! where a view's text is not what should be spoken, the AppKit layer
//! reports these texts through the NSAccessibility methods instead:
//!
//! - Each cell's text reads in words ([`cell_speech`]): "~" as "Home"
//!   ([`spoken_folder`]), the month in full ([`spoken_date`]), "folder"
//!   after a folder's name, and a folder's "—" size not at all. A row,
//!   which VoiceOver reads cell by cell, so reads in the columns' order
//!   ([`row_label`]): name, folder, size, date modified.
//! - Each cell names its column ([`column_label`]); AppKit adds the table's
//!   row count, which is the result count, and each row's index. (AppKit
//!   reports a view-based table's rows through elements of its own, so the
//!   row's reading comes from its cells, which are the app's views.)
//! - The status bar reads its " · " separators as commas ([`spoken`]) and
//!   each key hint as its action, then its keys in words ([`hint_label`]).
//! - A new search announces its count ([`results_announcement`]) while
//!   VoiceOver runs, so it is heard without leaving the search field.

#![forbid(unsafe_code)]

use crate::columns::Column;
use crate::formats::group_thousands;
use crate::key_caps::{COMMAND, CONTROL, KeyHint, OPTION, RETURN, SHIFT};
use crate::ui_text::REGEX_ERROR_TITLE;

/// The separator the status bar puts between its parts.
const DOT_SEPARATOR: &str = " \u{b7} ";

/// The months as the Date Modified column abbreviates them, and in full.
const MONTHS: [(&str, &str); 12] = [
    ("Jan", "January"),
    ("Feb", "February"),
    ("Mar", "March"),
    ("Apr", "April"),
    ("May", "May"),
    ("Jun", "June"),
    ("Jul", "July"),
    ("Aug", "August"),
    ("Sep", "September"),
    ("Oct", "October"),
    ("Nov", "November"),
    ("Dec", "December"),
];

/// One result row as the table shows it, for [`row_label`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowSpeech<'a> {
    /// The Name column.
    pub name: &'a str,
    /// Whether the entry is a folder.
    pub is_folder: bool,
    /// The Path column ("~/Documents" for a folder in the home folder).
    pub folder: &'a str,
    /// The Size column ("182 KB"; "—" for a folder).
    pub size: &'a str,
    /// The Date Modified column ("14 Mar 2026 09:12").
    pub modified: &'a str,
}

/// What VoiceOver reads for a result row, its cells' [`cell_speech`] in the
/// columns' order: "report.pdf, Home/Documents/Finance, 182 KB, 14 March
/// 2026 at 09:12", or for a folder "Finance, folder, Home/Documents, 2
/// March 2026 at 10:00". Empty parts are left out.
pub fn row_label(row: &RowSpeech<'_>) -> String {
    let shown = [
        (Column::Name, row.name),
        (Column::Path, row.folder),
        (Column::Size, row.size),
        (Column::Modified, row.modified),
    ];
    let parts: Vec<String> = shown
        .into_iter()
        .filter_map(|(column, text)| cell_speech(column, text, row.is_folder))
        .collect();
    parts.join(", ")
}

/// What a cell's text reads, from what it shows in `column` for an entry
/// that is a folder or not: the name ("Finance, folder" for a folder), the
/// folder in words ([`spoken_folder`]), the size, the date in words
/// ([`spoken_date`]). `None` when it reads nothing: an empty cell, or a
/// folder's size, which the column shows as "—".
pub fn cell_speech(column: Column, shown: &str, is_folder: bool) -> Option<String> {
    if shown.is_empty() {
        return None;
    }
    match column {
        Column::Name if is_folder => Some(format!("{shown}, folder")),
        Column::Name => Some(shown.to_owned()),
        Column::Path => Some(spoken_folder(shown)),
        Column::Size if is_folder => None,
        Column::Size => Some(shown.to_owned()),
        Column::Modified => Some(spoken_date(shown)),
    }
}

/// The Path column's text as spoken: a leading "~" (the home folder) reads
/// "Home"; anything else as shown.
pub fn spoken_folder(shown: &str) -> String {
    match shown.strip_prefix('~') {
        Some("") => "Home".to_owned(),
        Some(rest) if rest.starts_with('/') => format!("Home{rest}"),
        _ => shown.to_owned(),
    }
}

/// The Date Modified column's text as spoken: "14 Mar 2026 09:12" reads
/// "14 March 2026 at 09:12". A text of any other shape is read as shown.
pub fn spoken_date(shown: &str) -> String {
    let parts: Vec<&str> = shown.split(' ').collect();
    let [day, month, year, time] = parts[..] else {
        return shown.to_owned();
    };
    match MONTHS.iter().find(|(short, _)| *short == month) {
        Some((_, long)) => format!("{day} {long} {year} at {time}"),
        None => shown.to_owned(),
    }
}

/// What a cell reads as its label: its column's title.
pub fn column_label(column: Column) -> &'static str {
    column.title()
}

/// A status bar text as spoken: its " · " separators read as commas.
pub fn spoken(text: &str) -> String {
    text.replace(DOT_SEPARATOR, ", ")
}

/// A key cap in words: "⌘" reads "Command", "↩" "Return"; a letter or a
/// named key ("Space") as it is.
pub fn cap_word(cap: &str) -> &str {
    match cap {
        COMMAND => "Command",
        OPTION => "Option",
        SHIFT => "Shift",
        CONTROL => "Control",
        RETURN => "Return",
        other => other,
    }
}

/// What a status bar hint reads: its action, then its keys in words, as
/// VoiceOver reads a menu item: "Reveal in Finder, Command Return".
pub fn hint_label(hint: &KeyHint) -> String {
    let keys: Vec<&str> = hint.keys.iter().map(|cap| cap_word(cap)).collect();
    format!("{}, {}", hint.label, keys.join(" "))
}

/// What a new search announces once its results are shown: "1,234
/// results", "1 result", "No matches", or the invalid regular expression's
/// title. Nothing without a query: the emptied field lists the whole index
/// again, whose size is in the status bar.
pub fn results_announcement(
    has_query: bool,
    results: usize,
    invalid_regex: bool,
) -> Option<String> {
    if !has_query {
        return None;
    }
    Some(if invalid_regex {
        REGEX_ERROR_TITLE.to_owned()
    } else if results == 0 {
        "No matches".to_owned()
    } else if results == 1 {
        "1 result".to_owned()
    } else {
        format!("{} results", group_thousands(results))
    })
}
