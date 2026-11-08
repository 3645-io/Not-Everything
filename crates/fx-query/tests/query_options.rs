//! The search field's per-search options (`fx_query::QueryOptions`).
//!
//! Contract under test:
//!
//! - `QueryOptions { match_case, match_path, regex }` (all off by default)
//!   are the window's three chips. `search_options()` gives the
//!   `SearchOptions` they stand for.
//! - Without `regex`, `query_text(input)` is the input and `parse(input)`
//!   is `parse(input, &search_options())`.
//! - With `regex`, the whole input is one regular expression: one regex
//!   term whose pattern is the input, so spaces, `|`, `<`, `>`, `!` and `"`
//!   are pattern text, not query syntax (`whole_regex_text`, which the
//!   command line's `-r` uses too). An empty input matches everything.

use fx_query::eval::{Candidate, matches};
use fx_query::{Node, Pattern, QueryOptions, RegexTerm, SearchOptions, parse, whole_regex_text};

const REGEX: QueryOptions = QueryOptions {
    match_case: false,
    match_path: false,
    regex: true,
};

/// The single regex term `input` parses to with `options`, as its pattern
/// and whether it matches case and paths.
fn regex_term(input: &str, options: QueryOptions) -> (String, bool, bool) {
    match options.parse(input).query {
        Node::Term(term) => match term.pattern {
            Pattern::Regex(p) => (p, term.options.match_case, term.options.match_path),
            other => panic!("{input:?}: not a regex term: {other:?}"),
        },
        other => panic!("{input:?}: not a single term: {other:?}"),
    }
}

#[test]
fn default_options_change_nothing() {
    let o = QueryOptions::default();
    assert_eq!(o.search_options(), SearchOptions::default());
    for input in ["", "a b", "a|b", "\"x\"", "regex:^a"] {
        assert_eq!(o.query_text(input), input);
        assert_eq!(o.parse(input), parse(input, &SearchOptions::default()));
    }
}

#[test]
fn chips_set_the_search_options() {
    let o = QueryOptions {
        match_case: true,
        match_path: true,
        regex: false,
    };
    let s = o.search_options();
    assert!(s.match_case && s.match_path && !s.regex);
    assert!(!s.whole_word && !s.match_diacritics);
    assert_eq!(o.parse("a b"), parse("a b", &s));
}

#[test]
fn regex_makes_the_whole_input_one_pattern() {
    for input in [
        "a b",
        "^main|^notes",
        "!x",
        "<b>",
        "a  b ",
        r"\d+\.jpg$",
        "case:x",
    ] {
        let (pattern, case, path) = regex_term(input, REGEX);
        assert!(!case && !path, "{input:?}");
        // The pattern compiles to the input itself.
        let want = RegexTerm::compile(input, &Default::default()).unwrap();
        let got = RegexTerm::compile(&pattern, &Default::default()).unwrap();
        assert_eq!(
            got.as_regex().as_str(),
            want.as_regex().as_str(),
            "{input:?}"
        );
    }
    let (_, case, path) = regex_term(
        "x",
        QueryOptions {
            match_case: true,
            match_path: true,
            regex: true,
        },
    );
    assert!(case && path);
}

#[test]
fn quotes_in_a_whole_regex_stay_literal() {
    let says = Candidate::file("say \"hi\".txt", "/q");
    let plain = Candidate::file("say hi.txt", "/q");
    for input in [r#"say "hi""#, r#"say \"hi\""#] {
        let parsed = REGEX.parse(input);
        assert!(matches(&parsed.query, &says).unwrap(), "{input:?}");
        assert!(!matches(&parsed.query, &plain).unwrap(), "{input:?}");
    }
    // A run of two backslashes keeps its meaning before a quote.
    let parsed = REGEX.parse(r#"a\\""#);
    assert!(matches(&parsed.query, &Candidate::file("a\\\".txt", "/q")).unwrap());
}

#[test]
fn whole_regex_text_quotes_and_escapes() {
    assert_eq!(whole_regex_text("a b"), "\"a b\"");
    assert_eq!(whole_regex_text(r#"say "hi""#), r#""say \x22hi\x22""#);
    assert_eq!(whole_regex_text(r#"\""#), r#""\x22""#);
    assert_eq!(whole_regex_text(r#"\\""#), r#""\\\x22""#);
    assert_eq!(whole_regex_text(""), "\"\"");
}

#[test]
fn an_empty_regex_query_matches_everything() {
    assert_eq!(REGEX.parse("").query, Node::MatchAll);
    assert_eq!(REGEX.query_text(""), "");
    assert!(matches(&REGEX.parse("").query, &Candidate::file("x", "/")).unwrap());
}
