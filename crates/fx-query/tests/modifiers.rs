//! Search modifiers and their `no` forms.
//!
//! Rules under test, restated in our own words from voidtools' public
//! Search Modifiers and Search Syntax pages
//! (https://www.voidtools.com/support/everything/searching/) and
//! docs/PLAN.md:
//!
//! - A modifier is a `name:` prefix on a term, a quoted phrase or a `< >`
//!   group. Names are matched case-insensitively, and the long aliases are
//!   accepted: `case:`/`nocase:`, `path:`/`nopath:`, `file:` (`files:`),
//!   `folder:` (`folders:`), `ww:` (`wholeword:`)/`noww:`, `wfn:`
//!   (`wholefilename:`)/`nowfn:`, `regex:`/`noregex:`,
//!   `diacritics:`/`nodiacritics:`, `start-with:`, `end-with:`.
//! - A modifier overrides the global [`SearchOptions`] for everything it
//!   covers. Modifiers stack (`case:ww:foo`); the innermost (closest to the
//!   term) wins.
//! - `regex:` keeps the rest of the term raw (no wildcard or operator
//!   handling inside quotes) as `Pattern::Regex`; the reference evaluator
//!   matches regex terms (an invalid one matches nothing).
//! - A modifier with nothing after it matches everything and leaves a
//!   diagnostic. An unknown `word:` prefix is plain text; search functions
//!   such as `ext:` are recognised by name (see `tests/size_functions.rs`).
//!
//! Evaluation: `ww:` matches only between word boundaries (a non-alphanumeric
//! character or the edge of the string); `wfn:` needs the whole name to equal
//! the text; `start-with:`/`end-with:` anchor the match; `file:`/`folder:`
//! filter on `is_dir`; `!` composes with all of them.

use fx_query::eval::{Candidate, matches};
use fx_query::{Node, Pattern, SearchOptions, Term, TermKind, TermOptions, parse};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn collect_terms<'a>(node: &'a Node, out: &mut Vec<&'a Term>) {
    match node {
        Node::And(children) | Node::Or(children) => {
            for c in children {
                collect_terms(c, out);
            }
        }
        Node::Not(inner) => collect_terms(inner, out),
        Node::Term(t) => out.push(t),
        Node::Function(_) | Node::MatchAll => {}
    }
}

/// All terms of `query` (parsed with `opts`), in input order. Asserts the
/// parse is clean.
#[track_caller]
fn terms_with(query: &str, opts: &SearchOptions) -> Vec<Term> {
    let parsed = parse(query, opts);
    assert!(
        parsed.diagnostics.is_empty(),
        "{query:?} should parse cleanly, got {:?}",
        parsed.diagnostics
    );
    let mut out = Vec::new();
    collect_terms(&parsed.query, &mut out);
    out.into_iter().cloned().collect()
}

#[track_caller]
fn terms(query: &str) -> Vec<Term> {
    terms_with(query, &SearchOptions::default())
}

/// The single term of `query` under default options.
#[track_caller]
fn single(query: &str) -> Term {
    single_with(query, &SearchOptions::default())
}

#[track_caller]
fn single_with(query: &str, opts: &SearchOptions) -> Term {
    let parsed = parse(query, opts);
    assert!(
        parsed.diagnostics.is_empty(),
        "{query:?} should parse cleanly, got {:?}",
        parsed.diagnostics
    );
    match parsed.query {
        Node::Term(t) => t,
        other => panic!("{query:?} should be a single term, got {other:?}"),
    }
}

/// The single term of `query`, which must be a literal with text `text`.
#[track_caller]
fn literal(query: &str, text: &str) -> TermOptions {
    let t = single(query);
    assert_eq!(t.text, text, "term text of {query:?}");
    assert_eq!(
        t.pattern,
        Pattern::Literal(text.to_owned()),
        "pattern of {query:?}"
    );
    t.options
}

#[track_caller]
fn eval_full(query: &str, opts: &SearchOptions, name: &str, parent: &str, is_dir: bool) -> bool {
    let parsed = parse(query, opts);
    let cand = Candidate {
        name,
        parent_path: parent,
        is_dir,
        ..Default::default()
    };
    matches(&parsed.query, &cand)
        .unwrap_or_else(|e| panic!("query {query:?} should be supported by the evaluator: {e}"))
}

/// Evaluates `query` (default options) against a file `name` in `/Users/me`.
#[track_caller]
fn file(query: &str, name: &str) -> bool {
    eval_full(query, &SearchOptions::default(), name, "/Users/me", false)
}

/// Evaluates `query` (default options) against a folder `name` in `/Users/me`.
#[track_caller]
fn folder(query: &str, name: &str) -> bool {
    eval_full(query, &SearchOptions::default(), name, "/Users/me", true)
}

#[track_caller]
fn assert_diag_spans_ok(query: &str) {
    let parsed = parse(query, &SearchOptions::default());
    for d in &parsed.diagnostics {
        assert!(
            d.span.start <= d.span.end && d.span.end <= query.len(),
            "diagnostic span {:?} out of bounds for {query:?}",
            d.span
        );
        assert!(query.is_char_boundary(d.span.start) && query.is_char_boundary(d.span.end));
        assert!(!d.kind.message().is_empty());
    }
}

// ---------------------------------------------------------------------------
// Parsing: each modifier sets its option and strips its prefix
// ---------------------------------------------------------------------------

#[test]
fn case_modifier_sets_match_case() {
    let o = literal("case:Foo", "Foo");
    assert!(o.match_case);
    let o = literal("nocase:Foo", "Foo");
    assert!(!o.match_case);
}

#[test]
fn path_modifier_sets_match_path() {
    assert!(literal("path:docs", "docs").match_path);
    assert!(!literal("nopath:docs", "docs").match_path);
}

#[test]
fn file_and_folder_modifiers_set_kind() {
    assert_eq!(literal("file:foo", "foo").kind, TermKind::FilesOnly);
    assert_eq!(literal("files:foo", "foo").kind, TermKind::FilesOnly);
    assert_eq!(literal("folder:foo", "foo").kind, TermKind::FoldersOnly);
    assert_eq!(literal("folders:foo", "foo").kind, TermKind::FoldersOnly);
    assert_eq!(literal("foo", "foo").kind, TermKind::Any);
}

#[test]
fn whole_word_modifier_and_alias() {
    assert!(literal("ww:foo", "foo").whole_word);
    assert!(literal("wholeword:foo", "foo").whole_word);
    assert!(!literal("noww:foo", "foo").whole_word);
}

#[test]
fn whole_filename_modifier_and_alias() {
    assert!(literal("wfn:foo", "foo").whole_filename);
    assert!(literal("wholefilename:foo", "foo").whole_filename);
    assert!(!literal("nowfn:foo", "foo").whole_filename);
}

#[test]
fn diacritics_modifier() {
    assert!(literal("diacritics:resume", "resume").match_diacritics);
    assert!(!literal("nodiacritics:resume", "resume").match_diacritics);
}

#[test]
fn start_and_end_with_modifiers() {
    let o = literal("start-with:foo", "foo");
    assert!(o.start_with && !o.end_with);
    let o = literal("end-with:foo", "foo");
    assert!(o.end_with && !o.start_with);
}

#[test]
fn modifier_names_are_case_insensitive() {
    assert!(literal("CASE:Foo", "Foo").match_case);
    assert!(!literal("NoCase:Foo", "Foo").match_case);
    assert!(literal("WholeWord:foo", "foo").whole_word);
    assert!(literal("WW:foo", "foo").whole_word);
    assert!(literal("WFN:foo", "foo").whole_filename);
    assert!(literal("Path:foo", "foo").match_path);
    assert_eq!(literal("Folders:foo", "foo").kind, TermKind::FoldersOnly);
    assert_eq!(literal("FILE:foo", "foo").kind, TermKind::FilesOnly);
    assert!(literal("Start-With:foo", "foo").start_with);
    assert!(literal("END-WITH:foo", "foo").end_with);
    assert!(literal("Diacritics:foo", "foo").match_diacritics);
}

#[test]
fn unmodified_term_has_default_options() {
    let o = literal("foo", "foo");
    assert_eq!(o, TermOptions::from_search(&SearchOptions::default()));
}

#[test]
fn modifiers_stack() {
    let o = literal("case:ww:Foo", "Foo");
    assert!(o.match_case && o.whole_word);
    let o = literal("folder:wfn:path:case:Foo", "Foo");
    assert!(o.match_case && o.whole_filename && o.match_path);
    assert_eq!(o.kind, TermKind::FoldersOnly);
    let o = literal("start-with:end-with:foo", "foo");
    assert!(o.start_with && o.end_with);
}

#[test]
fn innermost_modifier_wins_in_a_stack() {
    assert!(!literal("case:nocase:Foo", "Foo").match_case);
    assert!(literal("nocase:case:Foo", "Foo").match_case);
    assert!(!literal("ww:noww:foo", "foo").whole_word);
    assert_eq!(
        literal("file:folder:foo", "foo").kind,
        TermKind::FoldersOnly
    );
    assert_eq!(literal("folder:file:foo", "foo").kind, TermKind::FilesOnly);
}

#[test]
fn modifier_applies_to_quoted_phrase() {
    let o = literal("case:\"Foo Bar\"", "Foo Bar");
    assert!(o.match_case);
    let o = literal("ww:\"a|b <c>\"", "a|b <c>");
    assert!(o.whole_word);
}

#[test]
fn modifier_applies_to_every_term_in_a_group() {
    let ts = terms("case:<Foo Bar|Baz>");
    assert_eq!(ts.len(), 3);
    for t in &ts {
        assert!(t.options.match_case, "{t:?} should be case-sensitive");
    }
    let ts = terms("folder:<a !b>");
    assert_eq!(ts.len(), 2);
    for t in &ts {
        assert_eq!(t.options.kind, TermKind::FoldersOnly, "{t:?}");
    }
}

#[test]
fn group_modifier_reaches_nested_groups() {
    let ts = terms("ww:<a <b|c>>");
    assert_eq!(ts.len(), 3);
    assert!(ts.iter().all(|t| t.options.whole_word));
}

#[test]
fn inner_modifier_beats_outer_group_modifier() {
    let ts = terms("case:<nocase:Foo Bar>");
    assert_eq!(ts.len(), 2);
    assert_eq!(ts[0].text, "Foo");
    assert!(!ts[0].options.match_case);
    assert_eq!(ts[1].text, "Bar");
    assert!(ts[1].options.match_case);

    let ts = terms("folder:<file:a b>");
    assert_eq!(ts[0].options.kind, TermKind::FilesOnly);
    assert_eq!(ts[1].options.kind, TermKind::FoldersOnly);

    let ts = terms("wfn:<nowfn:a <b ww:c>>");
    assert!(!ts[0].options.whole_filename);
    assert!(ts[1].options.whole_filename && !ts[1].options.whole_word);
    assert!(ts[2].options.whole_filename && ts[2].options.whole_word);
}

#[test]
fn modifier_covers_only_its_own_term() {
    let ts = terms("case:Foo bar");
    assert_eq!(ts.len(), 2);
    assert!(ts[0].options.match_case);
    assert!(!ts[1].options.match_case);

    let ts = terms("case:Foo|bar");
    assert_eq!(ts.len(), 2);
    assert!(ts[0].options.match_case);
    assert!(!ts[1].options.match_case);

    let ts = terms("<case:a> b");
    assert!(ts[0].options.match_case);
    assert!(!ts[1].options.match_case);
}

#[test]
fn modifier_overrides_search_options() {
    let all_on = SearchOptions {
        match_case: true,
        whole_word: true,
        match_path: true,
        match_diacritics: true,
        regex: true,
    };
    let t = single_with("nocase:noww:nopath:nodiacritics:noregex:foo", &all_on);
    assert_eq!(t.text, "foo");
    assert_eq!(t.pattern, Pattern::Literal("foo".to_owned()));
    assert!(!t.options.match_case);
    assert!(!t.options.whole_word);
    assert!(!t.options.match_path);
    assert!(!t.options.match_diacritics);
    assert!(!t.options.regex);

    // Unmentioned flags keep the global value.
    let t = single_with("nocase:foo", &all_on);
    assert!(!t.options.match_case);
    assert!(t.options.whole_word && t.options.match_path && t.options.match_diacritics);

    let t = single_with("case:foo", &SearchOptions::default());
    assert!(t.options.match_case && !t.options.whole_word);
}

#[test]
fn not_composes_with_modifiers() {
    let parsed = parse("!folder:foo", &SearchOptions::default());
    assert!(parsed.diagnostics.is_empty());
    match parsed.query {
        Node::Not(inner) => match *inner {
            Node::Term(t) => {
                assert_eq!(t.text, "foo");
                assert_eq!(t.options.kind, TermKind::FoldersOnly);
            }
            other => panic!("expected a term under NOT, got {other:?}"),
        },
        other => panic!("expected NOT, got {other:?}"),
    }
    let ts = terms("a !case:B");
    assert!(!ts[0].options.match_case);
    assert!(ts[1].options.match_case);
}

#[test]
fn modifier_with_wildcard_operand() {
    let t = single("case:*.TXT");
    assert!(t.options.match_case);
    assert!(matches!(t.pattern, Pattern::Wildcard(_)), "{t:?}");
    let t = single("noregex:*.txt");
    assert!(matches!(t.pattern, Pattern::Wildcard(_)), "{t:?}");
}

#[test]
fn term_span_lies_inside_input() {
    for q in ["case:Foo", "ww:\"a b\"", "case:ww:x", "a folder:b"] {
        for t in terms(q) {
            assert!(t.span.start < t.span.end && t.span.end <= q.len(), "{q:?}");
        }
    }
}

// ---------------------------------------------------------------------------
// regex:
// ---------------------------------------------------------------------------

#[test]
fn regex_modifier_stores_raw_pattern() {
    let t = single("regex:^foo.*bar$");
    assert!(t.options.regex);
    assert_eq!(t.pattern, Pattern::Regex("^foo.*bar$".to_owned()));

    // No wildcard interpretation.
    let t = single("regex:a?b*");
    assert_eq!(t.pattern, Pattern::Regex("a?b*".to_owned()));

    let t = single("REGEX:x+");
    assert_eq!(t.pattern, Pattern::Regex("x+".to_owned()));
}

#[test]
fn regex_inside_quotes_is_raw() {
    let t = single("regex:\"a b|c\"");
    assert_eq!(t.pattern, Pattern::Regex("a b|c".to_owned()));
    let t = single("regex:\"<x|y>!*.txt\"");
    assert_eq!(t.pattern, Pattern::Regex("<x|y>!*.txt".to_owned()));
}

#[test]
fn regex_terms_are_evaluated() {
    let cand = Candidate {
        name: "foo.txt",
        parent_path: "/Users/me",
        is_dir: false,
        ..Default::default()
    };
    for (q, want) in [
        ("regex:foo", true),
        ("regex:^foo\\.txt$", true),
        ("regex:^oo", false),
        ("a regex:foo", false),
        ("t regex:foo", true),
        ("regex:foo|a", true),
        ("regex:bar|a", false),
        ("!regex:foo", false),
        ("!regex:bar", true),
        ("regex:(", false),
    ] {
        let parsed = parse(q, &SearchOptions::default());
        let got = matches(&parsed.query, &cand)
            .unwrap_or_else(|e| panic!("{q:?} should be supported: {e}"));
        assert_eq!(got, want, "{q:?}");
    }
}

#[test]
fn noregex_turns_global_regex_off() {
    let opts = SearchOptions {
        regex: true,
        ..SearchOptions::default()
    };
    assert!(eval_full("noregex:foo", &opts, "my foo.txt", "/x", false));
    assert!(eval_full("noregex:*.txt", &opts, "a.txt", "/x", false));
    assert!(!eval_full("noregex:*.txt", &opts, "a.md", "/x", false));
}

// ---------------------------------------------------------------------------
// Bare modifiers and unknown prefixes
// ---------------------------------------------------------------------------

#[test]
fn bare_modifier_matches_all_with_diagnostic() {
    for q in ["case:", "ww:", "nocase:", "wfn:", "case:ww:"] {
        let parsed = parse(q, &SearchOptions::default());
        assert_eq!(parsed.query, Node::MatchAll, "{q:?}");
        assert!(!parsed.diagnostics.is_empty(), "{q:?} should warn");
        assert_diag_spans_ok(q);
    }
}

#[test]
fn bare_modifier_next_to_terms_is_ignored() {
    for q in ["foo case:", "case: foo", "foo ww: | bar"] {
        let parsed = parse(q, &SearchOptions::default());
        assert!(!parsed.diagnostics.is_empty(), "{q:?} should warn");
        assert_diag_spans_ok(q);
    }
    // `case:` with a space after it does not cover `FOO`.
    assert!(file("case: FOO", "foo.txt"));
    assert!(!file("case: FOO", "bar.txt"));
    assert!(file("foo case:", "foo.txt"));
    assert!(!file("foo case:", "bar.txt"));
}

#[test]
fn unknown_prefix_is_literal_text() {
    let o = literal("abc:def", "abc:def");
    assert_eq!(o, TermOptions::default());
    // A modifier word glued to other letters is not a modifier.
    literal("xcase:foo", "xcase:foo");
    literal("cases:foo", "cases:foo");
    // A modifier before an unknown prefix applies; the rest stays text.
    let o = literal("case:abc:Def", "abc:Def");
    assert!(o.match_case);
    assert!(file("abc:def", "x abc:def y"));
    assert!(!file("abc:def", "def"));
}

#[test]
fn parsing_never_panics_on_partial_modifiers() {
    for q in [
        "c",
        "ca",
        "cas",
        "case",
        "case:",
        "case:<",
        "case:<a",
        "case:\"",
        "case:\"a",
        "!case:",
        "case:!",
        "case:|",
        "|case:",
        "regex:",
        "regex:\"",
        "regex:\"a|",
        "<case:>",
        "ww:<>",
        "start-with:",
        "end-with:",
        "file:folder:",
        "é:é",
        "case:é",
        ":",
        "::",
        "case::",
        "a:b:c",
    ] {
        let parsed = parse(q, &SearchOptions::default());
        assert_diag_spans_ok(q);
        let cand = Candidate {
            name: "a",
            parent_path: "/",
            is_dir: false,
            ..Default::default()
        };
        let _ = matches(&parsed.query, &cand);
    }
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

#[test]
fn eval_case() {
    assert!(file("Foo", "foo.txt"));
    assert!(file("case:Foo", "Foo.txt"));
    assert!(!file("case:Foo", "foo.txt"));
    assert!(file("CASE:foo", "foo.txt"));
    let cs = SearchOptions {
        match_case: true,
        ..SearchOptions::default()
    };
    assert!(!eval_full("FOO", &cs, "foo.txt", "/x", false));
    assert!(eval_full("nocase:FOO", &cs, "foo.txt", "/x", false));
}

#[test]
fn eval_path() {
    // Plain terms test the name only.
    assert!(!file("users", "notes.txt"));
    assert!(file("path:users", "notes.txt"));
    assert!(file("path:me/notes", "notes.txt"));
    let po = SearchOptions {
        match_path: true,
        ..SearchOptions::default()
    };
    assert!(eval_full("users", &po, "notes.txt", "/Users/me", false));
    assert!(!eval_full(
        "nopath:users",
        &po,
        "notes.txt",
        "/Users/me",
        false
    ));
    assert!(eval_full(
        "nopath:notes",
        &po,
        "notes.txt",
        "/Users/me",
        false
    ));
}

#[test]
fn eval_file_and_folder() {
    assert!(file("file:foo", "foo"));
    assert!(!folder("file:foo", "foo"));
    assert!(folder("folder:foo", "foo"));
    assert!(!file("folder:foo", "foo"));
    assert!(!folder("folder:foo", "bar"));
    assert!(folder("folders:foo", "foo"));
    assert!(file("files:foo", "foo"));
}

#[test]
fn eval_not_with_modifier() {
    assert!(file("!folder:foo", "foo"));
    assert!(!folder("!folder:foo", "foo"));
    assert!(folder("!folder:foo", "bar"));
    assert!(file("!folder:foo", "bar"));
    assert!(!file("!case:Foo", "Foo.txt"));
    assert!(file("!case:Foo", "foo.txt"));
    assert!(file("doc !wfn:doc", "doc.txt"));
    assert!(!file("doc !wfn:doc", "doc"));
}

#[test]
fn eval_whole_word() {
    assert!(file("ww:foo", "foo"));
    assert!(file("ww:foo", "my foo.txt"));
    assert!(file("ww:foo", "my-foo.txt"));
    assert!(file("ww:foo", "foo.txt"));
    assert!(file("ww:foo", "(foo)"));
    assert!(!file("ww:foo", "myfoo.txt"));
    assert!(!file("ww:foo", "foobar.txt"));
    assert!(!file("ww:foo", "foo2.txt"));
    assert!(file("ww:foo", "myfoo foo"));
    assert!(file("wholeword:foo", "a foo b"));
    assert!(file("ww:\"my foo\"", "my foo.txt"));
    assert!(!file("ww:\"my foo\"", "amy foo.txt"));
    let wo = SearchOptions {
        whole_word: true,
        ..SearchOptions::default()
    };
    assert!(!eval_full("foo", &wo, "myfoo", "/x", false));
    assert!(eval_full("noww:foo", &wo, "myfoo", "/x", false));
}

#[test]
fn eval_whole_filename() {
    assert!(file("wfn:foo.txt", "foo.txt"));
    assert!(file("wfn:foo.txt", "FOO.TXT"));
    assert!(!file("case:wfn:foo.txt", "FOO.TXT"));
    assert!(!file("wfn:foo", "foo.txt"));
    assert!(!file("wfn:oo.txt", "foo.txt"));
    assert!(file("wholefilename:foo", "foo"));
    assert!(file("nowfn:foo", "foo.txt"));
    assert!(file("wfn:\"my file.txt\"", "my file.txt"));
}

#[test]
fn eval_start_and_end_with() {
    assert!(file("start-with:foo", "foobar"));
    assert!(file("start-with:foo", "Foo.txt"));
    assert!(!file("start-with:foo", "barfoo"));
    assert!(!file("start-with:foo", "a foo"));
    assert!(file("end-with:.txt", "notes.txt"));
    assert!(!file("end-with:.txt", "notes.txt.bak"));
    assert!(file("end-with:foo", "foofoo"));
    assert!(file("start-with:end-with:foo", "foo"));
    assert!(!file("start-with:end-with:foo", "foo foo"));
}

#[test]
fn eval_group_modifiers() {
    assert!(folder("folder:<foo|bar>", "bar"));
    assert!(!file("folder:<foo|bar>", "bar"));
    assert!(file("case:<Foo|Bar>", "Bar.txt"));
    assert!(!file("case:<Foo|Bar>", "bar.txt"));
    assert!(file("case:<nocase:Foo Bar>", "foo Bar"));
    assert!(!file("case:<nocase:Foo Bar>", "foo bar"));
    assert!(file("ww:<a b>", "a-b"));
    assert!(!file("ww:<a b>", "ab"));
    assert!(file("!folder:<foo|bar>", "foo"));
    assert!(!folder("!folder:<foo|bar>", "foo"));
    assert!(folder("!folder:<foo|bar>", "baz"));
}

#[test]
fn eval_mixed_queries() {
    assert!(file("file:case:Report ww:2024", "Report 2024.pdf"));
    assert!(!file("file:case:Report ww:2024", "report 2024.pdf"));
    assert!(!file("file:case:Report ww:2024", "Report 20245.pdf"));
    assert!(!folder("file:case:Report ww:2024", "Report 2024"));
    assert!(file("start-with:a | end-with:z", "abc"));
    assert!(file("start-with:a | end-with:z", "xyz"));
    assert!(!file("start-with:a | end-with:z", "bcd"));
    assert!(file("case:*.TXT", "a.TXT"));
    assert!(!file("case:*.TXT", "a.txt"));
}
