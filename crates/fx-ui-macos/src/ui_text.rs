//! The search window's copy, as written on the app mockups canvas (the Main
//! and NoMatches boards), with the owner's changes: the access callout keeps
//! the Main board's text, and every syntax example in "No matches" is real
//! search syntax that parses
//! (`tests/ui_text.rs` checks it with `fx_query::parse`).
//!
//! The Settings window's copy is in [`crate::settings_view`]; key caps and
//! hints in [`crate::key_caps`]; the status bar's text in
//! [`crate::status_view`].

#![forbid(unsafe_code)]

use fx_sort::Direction;

/// The search field's placeholder.
pub const SEARCH_PLACEHOLDER: &str = "Search";

/// The search field's accessibility label.
pub const SEARCH_LABEL: &str = "Search file and folder names";

/// The clear button's accessibility label.
pub const CLEAR_LABEL: &str = "Clear search";

/// The clear button's mark: a multiplication sign.
pub const CLEAR_GLYPH: &str = "\u{d7}";

/// The accessibility label of the group of option chips.
pub const OPTIONS_LABEL: &str = "Search options";

/// A search option chip.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OptionChip {
    /// What the chip says.
    pub label: &'static str,
    /// Its tooltip and accessibility label.
    pub title: &'static str,
    /// Whether the label is set in the monospaced font.
    pub mono: bool,
}

/// The three option chips after the search field, in order.
pub const OPTION_CHIPS: [OptionChip; 3] = [
    OptionChip {
        label: "Aa",
        title: "Match case",
        mono: false,
    },
    OptionChip {
        label: "Path",
        title: "Match full path",
        mono: false,
    },
    OptionChip {
        label: ".*",
        title: "Regular expression",
        mono: true,
    },
];

/// The results card's accessibility label.
pub const RESULTS_LABEL: &str = "Results";

/// A column header's accessibility label: "Sort by Size", plus ",
/// ascending" or ", descending" for the sorted column.
pub fn sort_label(column_title: &str, sorted: Option<Direction>) -> String {
    match sorted {
        None => format!("Sort by {column_title}"),
        Some(Direction::Ascending) => format!("Sort by {column_title}, ascending"),
        Some(Direction::Descending) => format!("Sort by {column_title}, descending"),
    }
}

/// A column divider's accessibility label, for the column on its left
/// (`.ne-col-handle`'s `aria-label`).
pub fn resize_label(column_title: &str) -> String {
    format!("Resize the {column_title} column")
}

/// A column divider's tooltip and accessibility help (its `title`).
pub const RESIZE_HINT: &str = "Drag to resize. Double-click to reset.";

/// The empty state's title for `query` (trimmed), in curly quotes.
pub fn no_matches_title(query: &str) -> String {
    format!("No matches for \u{201c}{}\u{201d}", query.trim())
}

/// The empty state's text under the title.
pub const NO_MATCHES_TEXT: &str =
    "Check the spelling or try fewer words. To narrow a search instead, add one of these:";

/// The label above the syntax examples (written in capitals).
pub const SYNTAX_LABEL: &str = "SEARCH SYNTAX";

/// One syntax example: a query a click types into the search field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SyntaxExample {
    /// The query, shown as a pressed token.
    pub query: &'static str,
    /// What it finds.
    pub meaning: &'static str,
}

/// The syntax examples of the NoMatches board, in order.
pub const SYNTAX_EXAMPLES: [SyntaxExample; 6] = [
    SyntaxExample {
        query: "ext:pdf",
        meaning: "Files with this extension",
    },
    SyntaxExample {
        query: "dm:thisweek",
        meaning: "Modified this week",
    },
    SyntaxExample {
        query: "size:>10mb",
        meaning: "Larger than 10 MB",
    },
    SyntaxExample {
        query: "path:Finance",
        meaning: "Matches the full path, not only the name",
    },
    SyntaxExample {
        query: "report|summary",
        meaning: "Either word",
    },
    SyntaxExample {
        query: "\"tax return\"",
        meaning: "The words together, in this order",
    },
];

/// The title shown instead of results when the `.*` chip is on and the
/// search is not a valid regular expression.
pub const REGEX_ERROR_TITLE: &str = "That isn\u{2019}t a valid regular expression";

/// The message under [`REGEX_ERROR_TITLE`], in `danger-ink`.
pub const REGEX_ERROR_TEXT: &str =
    "Check its brackets and escapes, or turn off .* to search for the text as typed.";

/// The access callout's lead-in, in bold.
pub const BANNER_LEAD: &str = "Full Disk Access is off.";

/// The access callout's text after the lead-in (the Main board's).
pub const BANNER_TEXT: &str = "Desktop, Documents, Downloads, iCloud Drive, Mail and other \
     protected folders are left out of the index until you grant it.";

/// The callout's primary button.
pub const BANNER_GRANT: &str = "Grant Full Disk Access\u{2026}";

/// The callout's other button.
pub const BANNER_NOT_NOW: &str = "Not Now";
