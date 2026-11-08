//! The search field's per-search options: the window's chips.
//!
//! [`QueryOptions`] holds the three options the search window offers next
//! to its field: match case (Aa), match the full path (Path) and regular
//! expression (.*). The first two are global toggles of
//! [`SearchOptions`]. The third differs from [`SearchOptions::regex`],
//! which makes every space-separated term a regex: with it, the *whole*
//! input is one regular expression, spaces and operator characters
//! included, like the command line's `-r`. [`whole_regex_text`] turns the
//! input into that one term (a quoted phrase under the regex toggle), and
//! [`QueryOptions::query_text`] / [`QueryOptions::parse`] give the query
//! text and tree to search with, so the engine, its refinement cache and
//! the highlighter all see the same query.

use std::borrow::Cow;

use crate::syntax::{Parsed, SearchOptions, parse};

/// The per-search options of the search field. All off by default, which
/// is the plain search.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct QueryOptions {
    /// Match upper and lower case exactly (the Aa chip).
    pub match_case: bool,
    /// Match terms against the full path instead of the name (the Path
    /// chip).
    pub match_path: bool,
    /// The whole input is one regular expression (the .* chip):
    /// unanchored, case-insensitive unless `match_case`, tested against
    /// the name, or the full path with `match_path`. See
    /// [`crate::regex_term`] for the dialect.
    pub regex: bool,
}

impl QueryOptions {
    /// The global search toggles these options stand for. With `regex`
    /// the toggle is on and [`query_text`](Self::query_text) makes the
    /// input a single term.
    pub fn search_options(&self) -> SearchOptions {
        SearchOptions {
            match_case: self.match_case,
            match_path: self.match_path,
            regex: self.regex,
            ..SearchOptions::default()
        }
    }

    /// The fx-query text to search for `input`: the input itself, or with
    /// `regex` the whole input as one regex term ([`whole_regex_text`]).
    /// An empty input stays empty, so it matches everything either way.
    ///
    /// ```
    /// use fx_query::QueryOptions;
    ///
    /// let regex = QueryOptions { regex: true, ..QueryOptions::default() };
    /// assert_eq!(regex.query_text("a b"), "\"a b\"");
    /// assert_eq!(QueryOptions::default().query_text("a b"), "a b");
    /// ```
    pub fn query_text<'a>(&self, input: &'a str) -> Cow<'a, str> {
        if self.regex && !input.is_empty() {
            Cow::Owned(whole_regex_text(input))
        } else {
            Cow::Borrowed(input)
        }
    }

    /// Parses `input` under these options: [`parse`] of
    /// [`query_text`](Self::query_text) with
    /// [`search_options`](Self::search_options).
    pub fn parse(&self, input: &str) -> Parsed {
        parse(&self.query_text(input), &self.search_options())
    }
}

/// The fx-query text that makes the whole of `pattern` one term, to be
/// parsed with the regex toggle on ([`SearchOptions::regex`]): the pattern
/// wrapped in double quotes, so `|`, `<`, `>`, `!` and spaces stay part of
/// it instead of being query operators. fx-query drops quote characters,
/// so a `"` in the pattern becomes the equivalent regex escape `\x22` (an
/// escaped `\"` becomes `\x22` too, and a run of two backslashes before a
/// quote keeps its meaning).
///
/// ```
/// use fx_query::whole_regex_text;
///
/// assert_eq!(whole_regex_text("a|b c"), "\"a|b c\"");
/// assert_eq!(whole_regex_text("say \"hi\""), r#""say \x22hi\x22""#);
/// ```
pub fn whole_regex_text(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for c in pattern.chars() {
        match c {
            '"' if backslashes % 2 == 1 => out.push_str("x22"),
            '"' => out.push_str(r"\x22"),
            c => out.push(c),
        }
        backslashes = if c == '\\' { backslashes + 1 } else { 0 };
    }
    out.push('"');
    out
}
