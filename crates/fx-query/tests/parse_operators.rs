//! Operator parsing and the reference evaluator for
//! literal terms.
//!
//! Grammar under test (docs/PLAN.md and voidtools' public Search Syntax page,
//! restated in our own words): whitespace is AND, `|` is OR, a `!` prefix is
//! NOT, `< >` groups, and double quotes make spaces and operator characters
//! literal. NOT binds tightest, then OR, then AND, so `a b|c` means
//! a AND (b OR c). Parsing never fails: malformed input yields diagnostics
//! and a leniently recovered tree.

use fx_query::eval::{Candidate, matches};
use fx_query::{Node, Pattern, SearchOptions, parse};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Renders a pattern compactly. Literal patterns render as their text; other
/// variants (added by later tasks) fall back to their Debug form.
#[allow(unreachable_patterns)]
fn render_pattern(p: &Pattern) -> String {
    match p {
        Pattern::Literal(s) => s.clone(),
        other => format!("{other:?}"),
    }
}

/// Renders a query tree as a compact S-expression, so tests can compare
/// structure without building `TermOptions` by hand.
fn render(node: &Node) -> String {
    match node {
        Node::And(children) => format!(
            "and({})",
            children.iter().map(render).collect::<Vec<_>>().join(", ")
        ),
        Node::Or(children) => format!(
            "or({})",
            children.iter().map(render).collect::<Vec<_>>().join(", ")
        ),
        Node::Not(inner) => format!("not({})", render(inner)),
        Node::Term(t) => render_pattern(&t.pattern),
        Node::Function(f) => format!("fn({},{})", f.name(), f.args()),
        Node::MatchAll => "ALL".to_string(),
    }
}

fn tree(input: &str) -> String {
    render(&parse(input, &SearchOptions::default()).query)
}

/// Parses `input` and asserts it is clean (no diagnostics) and has the given
/// shape.
#[track_caller]
fn assert_clean(input: &str, expected: &str) {
    let parsed = parse(input, &SearchOptions::default());
    assert!(
        parsed.diagnostics.is_empty(),
        "input {input:?} should parse without diagnostics, got {:?}",
        parsed.diagnostics.len()
    );
    assert_eq!(render(&parsed.query), expected, "input {input:?}");
}

/// Parses `input` and asserts it produced at least one diagnostic and was
/// recovered into the given shape.
#[track_caller]
fn assert_recovered(input: &str, expected: &str) {
    let parsed = parse(input, &SearchOptions::default());
    assert!(
        !parsed.diagnostics.is_empty(),
        "input {input:?} should produce a diagnostic"
    );
    assert_eq!(render(&parsed.query), expected, "input {input:?}");
    for d in &parsed.diagnostics {
        assert!(
            d.span.start <= d.span.end && d.span.end <= input.len(),
            "diagnostic span {:?} out of bounds for {input:?}",
            d.span
        );
    }
}

/// Collects every term in the tree, in source order.
fn terms(node: &Node) -> Vec<&fx_query::Term> {
    let mut out = Vec::new();
    fn walk<'a>(n: &'a Node, out: &mut Vec<&'a fx_query::Term>) {
        match n {
            Node::And(c) | Node::Or(c) => c.iter().for_each(|x| walk(x, out)),
            Node::Not(inner) => walk(inner, out),
            Node::Term(t) => out.push(t),
            Node::Function(_) | Node::MatchAll => {}
        }
    }
    walk(node, &mut out);
    out
}

fn cand<'a>(name: &'a str, parent_path: &'a str, is_dir: bool) -> Candidate<'a> {
    Candidate {
        name,
        parent_path,
        is_dir,
        ..Default::default()
    }
}

#[track_caller]
fn eval_with(query: &str, opts: &SearchOptions, name: &str, parent: &str) -> bool {
    let parsed = parse(query, opts);
    matches(&parsed.query, &cand(name, parent, false))
        .unwrap_or_else(|_| panic!("query {query:?} should be supported by the evaluator"))
}

#[track_caller]
fn eval(query: &str, name: &str) -> bool {
    eval_with(query, &SearchOptions::default(), name, "/Users/me")
}

// ---------------------------------------------------------------------------
// Basic shapes
// ---------------------------------------------------------------------------

#[test]
fn single_word_is_a_bare_term() {
    assert_clean("abc", "abc");
    let parsed = parse("abc", &SearchOptions::default());
    assert!(matches!(parsed.query, Node::Term(_)));
}

#[test]
fn empty_query_matches_all() {
    assert_clean("", "ALL");
}

#[test]
fn whitespace_only_query_matches_all() {
    assert_clean("   ", "ALL");
    assert_clean("\t \t", "ALL");
}

#[test]
fn space_is_and() {
    assert_clean("a b", "and(a, b)");
    assert_eq!(tree("a b"), "and(a, b)");
}

#[test]
fn repeated_and_leading_trailing_whitespace_is_ignored() {
    assert_clean("  a   b  ", "and(a, b)");
    assert_clean("a\tb", "and(a, b)");
}

#[test]
fn and_chain_is_flat() {
    assert_clean("a b c", "and(a, b, c)");
}

#[test]
fn bar_is_or() {
    assert_clean("a|b", "or(a, b)");
}

#[test]
fn or_chain_is_flat() {
    assert_clean("a|b|c", "or(a, b, c)");
}

#[test]
fn bang_is_not() {
    assert_clean("!a", "not(a)");
}

#[test]
fn not_inside_and() {
    assert_clean("a !b", "and(a, not(b))");
}

// ---------------------------------------------------------------------------
// Precedence: NOT > OR > AND
// ---------------------------------------------------------------------------

#[test]
fn or_binds_tighter_than_and_on_the_right() {
    assert_clean("a b|c", "and(a, or(b, c))");
}

#[test]
fn or_binds_tighter_than_and_on_the_left() {
    assert_clean("a|b c", "and(or(a, b), c)");
}

#[test]
fn or_binds_tighter_than_and_in_the_middle() {
    assert_clean("a b|c d", "and(a, or(b, c), d)");
}

#[test]
fn not_binds_tighter_than_or() {
    assert_clean("!a|b", "or(not(a), b)");
    assert_clean("a|!b", "or(a, not(b))");
}

#[test]
fn not_binds_tighter_than_and() {
    assert_clean("!a b", "and(not(a), b)");
}

// ---------------------------------------------------------------------------
// Grouping
// ---------------------------------------------------------------------------

#[test]
fn group_overrides_precedence() {
    assert_clean("<a b>|c", "or(and(a, b), c)");
}

#[test]
fn group_of_or_then_and() {
    assert_clean("<a|b> c", "and(or(a, b), c)");
}

#[test]
fn not_applies_to_a_group() {
    assert_clean("!<a|b>", "not(or(a, b))");
    assert_clean("!<a b>", "not(and(a, b))");
}

#[test]
fn nested_groups() {
    assert_clean("<<a|b> c>|d", "or(and(or(a, b), c), d)");
}

#[test]
fn group_of_same_kind_is_flattened() {
    assert_clean("<a b> c", "and(a, b, c)");
    assert_clean("<a|b>|c", "or(a, b, c)");
    assert_clean("a|<b|c>", "or(a, b, c)");
}

#[test]
fn single_term_group_collapses() {
    assert_clean("<a>", "a");
    assert_clean("<a> b", "and(a, b)");
}

// ---------------------------------------------------------------------------
// Quotes
// ---------------------------------------------------------------------------

#[test]
fn quotes_make_space_literal() {
    assert_clean("\"foo bar\"", "foo bar");
}

#[test]
fn quotes_make_operators_literal() {
    assert_clean("\"a|b\"", "a|b");
    assert_clean("\"!a\"", "!a");
    assert_clean("\"<a>\"", "<a>");
}

#[test]
fn quoted_part_joins_unquoted_text() {
    assert_clean("foo\" bar\"", "foo bar");
    assert_clean("\"foo \"bar", "foo bar");
}

#[test]
fn quoted_term_combines_with_operators() {
    assert_clean("\"a b\" c", "and(a b, c)");
    assert_clean("\"a b\"|c", "or(a b, c)");
    assert_clean("!\"a b\"", "not(a b)");
}

// ---------------------------------------------------------------------------
// Spans and options
// ---------------------------------------------------------------------------

#[test]
fn term_spans_are_byte_ranges_into_the_input() {
    let input = "abc  déf|gh";
    let parsed = parse(input, &SearchOptions::default());
    let ts = terms(&parsed.query);
    let texts: Vec<&str> = ts.iter().map(|t| &input[t.span.clone()]).collect();
    assert_eq!(texts, vec!["abc", "déf", "gh"]);
}

#[test]
fn term_options_are_seeded_from_search_options() {
    let opts = SearchOptions {
        match_case: true,
        whole_word: true,
        match_path: true,
        match_diacritics: true,
        ..SearchOptions::default()
    };
    let parsed = parse("a b|c", &opts);
    let ts = terms(&parsed.query);
    assert_eq!(ts.len(), 3);
    for t in ts {
        assert!(t.options.match_case);
        assert!(t.options.whole_word);
        assert!(t.options.match_path);
        assert!(t.options.match_diacritics);
    }
}

#[test]
fn search_options_default_to_off() {
    let opts = SearchOptions::default();
    assert!(!opts.match_case);
    assert!(!opts.whole_word);
    assert!(!opts.match_path);
    assert!(!opts.match_diacritics);
    assert!(!opts.regex);
    let parsed = parse("a", &opts);
    let t = terms(&parsed.query)[0];
    assert!(!t.options.match_case);
    assert!(!t.options.whole_word);
    assert!(!t.options.match_path);
    assert!(!t.options.match_diacritics);
    assert!(!t.options.regex);
    assert!(!t.options.whole_filename);
    assert!(!t.options.start_with);
    assert!(!t.options.end_with);
}

// ---------------------------------------------------------------------------
// Lenient recovery and diagnostics
// ---------------------------------------------------------------------------

#[test]
fn unclosed_quote_closes_at_end_of_input() {
    assert_recovered("\"foo bar", "foo bar");
    let parsed = parse("x \"foo", &SearchOptions::default());
    assert_eq!(render(&parsed.query), "and(x, foo)");
    assert!(
        parsed.diagnostics.iter().any(|d| d.span.start == 2),
        "diagnostic should point at the opening quote"
    );
}

#[test]
fn unclosed_group_closes_at_end_of_input() {
    assert_recovered("<a|b", "or(a, b)");
    assert_recovered("<a|b> <c d", "and(or(a, b), c, d)");
    let parsed = parse("x <a", &SearchOptions::default());
    assert!(
        parsed.diagnostics.iter().any(|d| d.span.start == 2),
        "diagnostic should point at the unclosed '<'"
    );
}

#[test]
fn stray_close_angle_is_ignored() {
    assert_recovered("a > b", "and(a, b)");
    let parsed = parse("a > b", &SearchOptions::default());
    assert!(
        parsed.diagnostics.iter().any(|d| d.span.start == 2),
        "diagnostic should point at the stray '>'"
    );
    assert_recovered("> a", "a");
}

#[test]
fn dangling_or_operand_is_dropped() {
    assert_recovered("a|", "a");
    assert_recovered("|a", "a");
    assert_recovered("a||b", "or(a, b)");
}

#[test]
fn dangling_not_is_dropped() {
    assert_recovered("a !", "a");
    assert_recovered("!", "ALL");
}

#[test]
fn clean_queries_have_no_diagnostics() {
    for q in ["a", "a b", "a|b", "!a", "<a b>|c", "\"x y\"", "foo\" bar\""] {
        assert!(
            parse(q, &SearchOptions::default()).diagnostics.is_empty(),
            "{q:?} should be clean"
        );
    }
}

#[test]
fn parsing_never_panics_on_partial_input() {
    // Inputs are separated by '¦' (broken bar) for compactness.
    let inputs = "¦\"¦\"\"¦<¦>¦<>¦|¦!¦!!¦||¦<<¦>>¦!<¦<!¦!|¦|!¦<|>¦\"<\"¦a\"¦\"a¦a<¦a>¦<a¦a\"b\"c\"\
                  ¦é\"|<¦<é|ü>!¦日本 語|¦!\"¦<\"a b>\"¦a|<b !>¦  !  ¦\u{0}¦a\u{0}b";
    let inputs: Vec<&str> = inputs.split('¦').collect();
    for input in inputs {
        let parsed = parse(input, &SearchOptions::default());
        for d in &parsed.diagnostics {
            assert!(
                d.span.start <= d.span.end && d.span.end <= input.len(),
                "diagnostic span {:?} out of bounds for {input:?}",
                d.span
            );
            assert!(input.is_char_boundary(d.span.start) && input.is_char_boundary(d.span.end));
        }
        for t in terms(&parsed.query) {
            assert!(t.span.start <= t.span.end && t.span.end <= input.len());
            assert!(input.is_char_boundary(t.span.start) && input.is_char_boundary(t.span.end));
        }
        // The evaluator must also cope with whatever the parser recovered.
        let _ = matches(&parsed.query, &cand("abc", "/x", false));
    }
}

#[test]
fn every_prefix_of_a_complex_query_parses() {
    let full = "<foo|\"bar baz\"> !qux|é <a b";
    for (i, _) in full.char_indices().chain([(full.len(), ' ')]) {
        let prefix = &full[..i];
        let _ = parse(prefix, &SearchOptions::default());
    }
}

// ---------------------------------------------------------------------------
// Reference evaluator: literal terms
// ---------------------------------------------------------------------------

#[test]
fn literal_is_a_substring_test_on_the_name() {
    assert!(eval("foo", "myfoo.txt"));
    assert!(eval("foo", "foo"));
    assert!(!eval("foo", "bar.txt"));
    assert!(!eval("foo", "fo"));
}

#[test]
fn literal_does_not_match_parent_path_by_default() {
    assert!(!eval_with(
        "docs",
        &SearchOptions::default(),
        "a.txt",
        "/Users/docs"
    ));
}

#[test]
fn matching_is_case_insensitive_by_default() {
    assert!(eval("FOO", "foo.txt"));
    assert!(eval("foo", "FOO.TXT"));
}

#[test]
fn case_insensitive_matching_uses_unicode_lowercase() {
    assert!(eval("ÄBC", "xäbc"));
    assert!(eval("café", "CAFÉ.txt"));
    assert!(eval("ΣΟΦ", "σοφία"));
}

#[test]
fn match_case_option_makes_matching_exact() {
    let opts = SearchOptions {
        match_case: true,
        ..SearchOptions::default()
    };
    assert!(!eval_with("FOO", &opts, "foo.txt", "/x"));
    assert!(eval_with("foo", &opts, "foo.txt", "/x"));
    assert!(!eval_with("Ä", &opts, "ä", "/x"));
}

#[test]
fn non_ascii_is_compared_exactly_without_nfd_data() {
    // Documented limitation: without NFD tables, diacritics-insensitive
    // matching cannot fold é to e, but exact non-ASCII text still matches.
    assert!(eval("résumé", "my résumé.pdf"));
}

#[test]
fn and_requires_every_term() {
    assert!(eval("foo bar", "bar_foo.txt"));
    assert!(!eval("foo bar", "foo.txt"));
}

#[test]
fn or_requires_any_term() {
    assert!(eval("foo|bar", "bar.txt"));
    assert!(eval("foo|bar", "foo.txt"));
    assert!(!eval("foo|bar", "baz.txt"));
}

#[test]
fn not_inverts() {
    assert!(eval("!foo", "bar.txt"));
    assert!(!eval("!foo", "foo.txt"));
    assert!(eval("txt !foo", "bar.txt"));
    assert!(!eval("txt !foo", "foo.txt"));
}

#[test]
fn precedence_is_respected_by_the_evaluator() {
    // a b|c == a AND (b OR c)
    assert!(eval("a b|c", "a-c"));
    assert!(!eval("a b|c", "c"));
    // <a b>|c == (a AND b) OR c
    assert!(eval("<a b>|c", "c"));
    assert!(!eval("<a b>|c", "a"));
    // !a|b == (NOT a) OR b
    assert!(eval("!a|b", "ab"));
    assert!(!eval("!a|b", "a"));
    // !<a|b> == NOT (a OR b)
    assert!(!eval("!<a|b>", "b"));
    assert!(eval("!<a|b>", "x"));
}

#[test]
fn match_all_matches_everything() {
    assert!(eval("", "anything"));
    assert!(eval("  ", "x"));
    assert!(matches(&Node::MatchAll, &cand("d", "/", true)).unwrap_or(false));
}

#[test]
fn quoted_literal_with_space() {
    assert!(eval("\"foo bar\"", "a foo bar.txt"));
    assert!(!eval("\"foo bar\"", "foo_bar.txt"));
    assert!(eval("\"a|b\"", "xa|by"));
    assert!(!eval("\"a|b\"", "a"));
}

#[test]
fn match_path_option_tests_the_full_path() {
    let opts = SearchOptions {
        match_path: true,
        ..SearchOptions::default()
    };
    assert!(eval_with("docs", &opts, "a.txt", "/Users/docs"));
    assert!(eval_with("docs a.t", &opts, "a.txt", "/Users/docs"));
    assert!(!eval_with("music", &opts, "a.txt", "/Users/docs"));
}

#[test]
fn full_path_joins_parent_and_name_with_a_slash() {
    let opts = SearchOptions {
        match_path: true,
        ..SearchOptions::default()
    };
    assert!(eval_with("me/notes", &opts, "notes.md", "/Users/me"));
    let full = "/users/me/notes.md";
    assert!(eval_with(full, &opts, "notes.md", "/Users/me"));
}

#[test]
fn literal_with_slash_implicitly_matches_the_full_path() {
    let opts = SearchOptions::default();
    assert!(eval_with("docs/a", &opts, "a.txt", "/Users/docs"));
    assert!(eval_with("users/docs/", &opts, "a.txt", "/Users/docs"));
    assert!(!eval_with("docs/b", &opts, "a.txt", "/Users/docs"));
}

#[test]
fn evaluator_ignores_is_dir_for_plain_literals() {
    let parsed = parse("foo", &SearchOptions::default());
    assert!(matches(&parsed.query, &cand("foo", "/x", true)).unwrap_or(false));
    assert!(matches(&parsed.query, &cand("foo", "/x", false)).unwrap_or(false));
}
