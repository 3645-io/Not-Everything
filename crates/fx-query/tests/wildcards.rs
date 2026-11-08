//! Wildcards `*`, `?` and `**`.
//!
//! Rules under test, restated in our own words from voidtools' public
//! Search Syntax page (https://www.voidtools.com/support/everything/searching/
//! and https://www.voidtools.com/support/everything/search_syntax/) and
//! docs/PLAN.md:
//!
//! - `*` matches any run of characters (possibly empty), `?` matches exactly
//!   one character (one Unicode scalar value, not a byte or a UTF-16 unit),
//!   `**` matches any run of characters including path separators.
//! - A term with a wildcard must match the WHOLE name, or the whole path
//!   when path matching is on; it is not a substring search.
//! - With path matching, `*` and `?` stop at `/` while `**` crosses it. On
//!   names (which never contain `/`) `*` and `**` behave the same.
//! - Quotes only make spaces and operator characters literal; the public
//!   pages do not say quotes switch wildcards off, so `"*.txt"` and
//!   `"my *.txt"` are still wildcard terms.
//!
//! `WildcardPattern::matches(text, case_sensitive)` tests one string with
//! these rules (so `*`/`?` never match `/`, `**` does); the reference
//! evaluator uses it for both name and path matching.

use std::time::{Duration, Instant};

use fx_query::eval::{Candidate, matches};
use fx_query::syntax::wildcard::WildcardPattern;
use fx_query::{Node, Pattern, SearchOptions, Term, parse};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parses `query` (default options) and returns its single term.
#[track_caller]
fn single_term(query: &str) -> Term {
    let parsed = parse(query, &SearchOptions::default());
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

/// Parses `query` and returns the compiled wildcard pattern of its single
/// term, panicking if the term is not a wildcard.
#[track_caller]
fn wildcard(query: &str) -> WildcardPattern {
    let term = single_term(query);
    match term.pattern {
        Pattern::Wildcard(w) => w,
        #[allow(unreachable_patterns)]
        other => panic!("{query:?} should compile to a wildcard pattern, got {other:?}"),
    }
}

fn is_wildcard(query: &str) -> bool {
    matches!(single_term(query).pattern, Pattern::Wildcard(_))
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

/// Name-only, case-insensitive evaluation of `query` against a file named
/// `name` in `/Users/me`.
#[track_caller]
fn eval(query: &str, name: &str) -> bool {
    eval_full(query, &SearchOptions::default(), name, "/Users/me", false)
}

fn path_opts() -> SearchOptions {
    SearchOptions {
        match_path: true,
        ..SearchOptions::default()
    }
}

fn case_opts() -> SearchOptions {
    SearchOptions {
        match_case: true,
        ..SearchOptions::default()
    }
}

// ---------------------------------------------------------------------------
// Parsing: which terms become wildcards
// ---------------------------------------------------------------------------

#[test]
fn star_and_question_mark_make_a_wildcard_term() {
    assert!(is_wildcard("*.txt"));
    assert!(is_wildcard("foo*"));
    assert!(is_wildcard("*foo*"));
    assert!(is_wildcard("a?c"));
    assert!(is_wildcard("?"));
    assert!(is_wildcard("*"));
    assert!(is_wildcard("**"));
    assert!(is_wildcard("src**rs"));
}

#[test]
fn plain_words_stay_literal() {
    for q in ["foo", "notes.txt", "a-b_c"] {
        let t = single_term(q);
        assert_eq!(t.pattern, Pattern::Literal(q.to_string()), "{q:?}");
    }
}

#[test]
fn wildcard_term_keeps_its_text() {
    let t = single_term("*.txt");
    assert_eq!(t.text, "*.txt");
    let t = single_term("\"my *.txt\"");
    assert_eq!(t.text, "my *.txt");
}

/// voidtools' Search Syntax page lists quotes as grouping a phrase (making
/// spaces and operators literal) and does not say they disable wildcards,
/// so wildcards still apply inside quotes.
#[test]
fn quotes_do_not_disable_wildcards() {
    assert!(is_wildcard("\"*.txt\""));
    assert!(is_wildcard("\"my *.txt\""));
    assert!(eval("\"*.txt\"", "notes.txt"));
    assert!(!eval("\"*.txt\"", "notes.md"));
    assert!(eval("\"my *.txt\"", "my notes.txt"));
    assert!(eval("\"my *.txt\"", "My Holiday Plans.txt"));
    assert!(!eval("\"my *.txt\"", "mynotes.txt"));
    assert!(!eval("\"my *.txt\"", "all my notes.txt"));
}

#[test]
fn wildcards_combine_with_operators() {
    let p = parse("*.txt|*.md", &SearchOptions::default());
    assert!(p.diagnostics.is_empty());
    match &p.query {
        Node::Or(children) => {
            assert_eq!(children.len(), 2);
            for c in children {
                assert!(
                    matches!(c, Node::Term(t) if matches!(t.pattern, Pattern::Wildcard(_))),
                    "{c:?}"
                );
            }
        }
        other => panic!("expected an OR, got {other:?}"),
    }
    assert!(eval("*.txt|*.md", "a.txt"));
    assert!(eval("*.txt|*.md", "b.md"));
    assert!(!eval("*.txt|*.md", "c.rs"));

    assert!(eval("!*.tmp", "keep.txt"));
    assert!(!eval("!*.tmp", "scratch.tmp"));

    assert!(eval("report *.pdf", "annual report.pdf"));
    assert!(!eval("report *.pdf", "annual report.docx"));
    assert!(!eval("report *.pdf", "invoice.pdf"));

    assert!(eval("<*.rs|*.toml> !test*", "main.rs"));
    assert!(eval("<*.rs|*.toml> !test*", "Cargo.toml"));
    assert!(!eval("<*.rs|*.toml> !test*", "tests_util.rs"));
    assert!(!eval("<*.rs|*.toml> !test*", "README.md"));
}

// ---------------------------------------------------------------------------
// Whole-name matching
// ---------------------------------------------------------------------------

#[test]
fn wildcard_must_match_the_whole_name() {
    assert!(eval("*.txt", "notes.txt"));
    assert!(eval("*.txt", ".txt"));
    assert!(eval("*.txt", "a.txt.txt"));
    assert!(!eval("*.txt", "notes.txt.bak"));
    assert!(!eval("*.txt", "notes.text"));

    assert!(eval("foo*", "foo"));
    assert!(eval("foo*", "foobar.rs"));
    assert!(!eval("foo*", "xfoobar"));

    assert!(eval("*bar", "foobar"));
    assert!(!eval("*bar", "foobarz"));

    assert!(eval("*oba*", "foobar"));
    assert!(!eval("*xyz*", "foobar"));
}

#[test]
fn question_mark_is_exactly_one_char() {
    assert!(eval("a?c", "abc"));
    assert!(eval("a?c", "a.c"));
    assert!(!eval("a?c", "ac"));
    assert!(!eval("a?c", "abbc"));
    assert!(!eval("a?c", "abcd"));
    assert!(!eval("a?c", "xabc"));
    assert!(eval("???", "abc"));
    assert!(!eval("???", "ab"));
    assert!(!eval("???", "abcd"));
}

#[test]
fn question_mark_matches_one_unicode_char_not_one_byte() {
    // é is 2 bytes in UTF-8, the emoji is 4 bytes and 2 UTF-16 units.
    assert!(eval("a?c", "aéc"));
    assert!(eval("a?c", "a😀c"));
    assert!(!eval("a??c", "aéc"));
    assert!(!eval("a??c", "a😀c"));
    assert!(eval("?.txt", "日.txt"));
    assert!(!eval("?.txt", "日本.txt"));
}

#[test]
fn lone_star_matches_everything() {
    for name in ["a", "notes.txt", "日本語", ".hidden"] {
        assert!(eval("*", name), "{name:?}");
        assert!(eval("**", name), "{name:?}");
    }
}

#[test]
fn star_and_double_star_are_the_same_on_names() {
    for (q1, q2) in [("*.txt", "**.txt"), ("a*b", "a**b"), ("*x*", "**x**")] {
        for name in ["notes.txt", "ab", "axyzb", "x", "abc", "aXb", "note.md"] {
            let (r1, r2) = (eval(q1, name), eval(q2, name));
            assert_eq!(r1, r2, "{q1:?} vs {q2:?} on {name:?}");
        }
    }
    assert!(eval("a**b", "axyzb"));
}

#[test]
fn several_stars_backtrack_correctly() {
    assert!(eval("a*b*c", "abc"));
    assert!(eval("a*b*c", "aXbYc"));
    assert!(eval("a*b*c", "abcbc"));
    assert!(!eval("a*b*c", "acb"));
    assert!(!eval("a*b*c", "abcx"));
    assert!(eval("*a*a*", "banana"));
    assert!(!eval("*a*a*a*a*", "banana"));
    assert!(eval("*?", "x"));
    assert!(eval("?*?", "xy"));
    assert!(!eval("?*?", "x"));
}

// ---------------------------------------------------------------------------
// Case folding
// ---------------------------------------------------------------------------

#[test]
fn wildcards_ignore_case_by_default() {
    assert!(eval("*.txt", "NOTES.TXT"));
    assert!(eval("*.TXT", "notes.txt"));
    assert!(eval("Foo?Bar", "fooXbar"));
    assert!(eval("é*", "École.txt"));
    assert!(eval("*Σ", "ΑΣ"));
}

#[test]
fn match_case_makes_wildcards_case_sensitive() {
    let o = case_opts();
    assert!(at("*.txt", &o, "/", "notes.txt"));
    assert!(!at("*.txt", &o, "/", "notes.TXT"));
    assert!(!at("*.TXT", &o, "/", "notes.txt"));
    assert!(at("Foo?Bar", &o, "/", "FooXBar"));
    assert!(!at("Foo?Bar", &o, "/", "fooXbar"));
}

// ---------------------------------------------------------------------------
// Path matching
// ---------------------------------------------------------------------------

/// Evaluates `query` with `opts` against the file `parent` + `/` + `name`.
#[track_caller]
fn at(query: &str, opts: &SearchOptions, parent: &str, name: &str) -> bool {
    eval_full(query, opts, name, parent, false)
}

#[test]
fn with_path_matching_the_whole_path_must_match() {
    let o = path_opts();
    let q = "/Users/*/notes.txt";
    assert!(at(q, &o, "/Users/alice", "notes.txt"));
    assert!(!at(q, &o, "/Users/alice/sub", "notes.txt"));
    assert!(!at("/Users/*/notes", &o, "/Users/alice", "notes.txt"));
    let q = "/Users/**/notes.txt";
    assert!(at(q, &o, "/Users/alice/sub", "notes.txt"));
    assert!(at(q, &o, "/Users/alice", "notes.txt"));
    assert!(at("**/src/*.rs", &o, "/home/me/proj/src", "main.rs"));
    assert!(!at("**/src/*.rs", &o, "/home/me/proj/src/util", "mod.rs"));
    assert!(at("**/src/**.rs", &o, "/home/me/proj/src/util", "mod.rs"));
}

#[test]
fn star_and_question_mark_do_not_cross_slash_but_double_star_does() {
    let o = path_opts();
    // `*` alone cannot span the separators of a full path...
    assert!(!at("*.txt", &o, "/Users/me", "notes.txt"));
    assert!(!at("*", &o, "/Users/me", "notes.txt"));
    // ...but `**` can.
    assert!(at("**.txt", &o, "/Users/me", "notes.txt"));
    assert!(at("**", &o, "/Users/me", "notes.txt"));
    // `?` does not match '/'.
    assert!(!at("/a?b", &o, "/a", "b"));
    assert!(at("/a/?", &o, "/a", "b"));
}

/// A `/` in the term makes it match against the path even without the
/// global toggle (the T1 rule), and wildcards then follow path rules.
#[test]
fn slash_in_wildcard_term_matches_the_path() {
    let d = SearchOptions::default();
    assert!(at("/Users/*/notes.txt", &d, "/Users/alice", "notes.txt"));
    assert!(!at("/Users/*/notes.txt", &d, "/Users/a/b", "notes.txt"));
    assert!(at("/Users/**/notes.txt", &d, "/Users/a/b", "notes.txt"));
}

#[test]
fn wildcard_terms_match_folders_too() {
    let d = SearchOptions::default();
    assert!(eval_full("*.app", &d, "Safari.app", "/Applications", true));
    assert!(eval_full("*.app", &d, "Safari.app", "/Applications", false));
}

// ---------------------------------------------------------------------------
// WildcardPattern API
// ---------------------------------------------------------------------------

#[test]
fn matches_api_whole_string_and_case() {
    let w = wildcard("*.txt");
    assert!(w.matches("notes.txt", false));
    assert!(w.matches("NOTES.TXT", false));
    assert!(!w.matches("NOTES.TXT", true));
    assert!(w.matches("notes.txt", true));
    assert!(!w.matches("notes.txt.bak", false));

    let w = wildcard("*");
    assert!(w.matches("", false));
    assert!(w.matches("anything", true));

    let w = wildcard("?");
    assert!(!w.matches("", false));
    assert!(w.matches("é", false));
    assert!(!w.matches("ab", false));
}

#[test]
fn matches_api_separator_rules() {
    assert!(!wildcard("*").matches("a/b", false));
    assert!(wildcard("**").matches("a/b", false));
    assert!(!wildcard("a?b").matches("a/b", false));
    assert!(wildcard("a/*").matches("a/b", false));
    assert!(!wildcard("a/*").matches("a/b/c", false));
    assert!(wildcard("a/**").matches("a/b/c", false));
}

#[test]
fn extension_hint_only_for_plain_star_dot_ext() {
    assert_eq!(wildcard("*.txt").extension_hint(), Some("txt"));
    assert_eq!(wildcard("*.rs").extension_hint(), Some("rs"));
    assert_eq!(wildcard("\"*.txt\"").extension_hint(), Some("txt"));

    for q in [
        "a*.txt", "*.t?t", "*.*", "*txt", "*", "?.txt", "*.txt*", "foo*", "*a.txt",
    ] {
        assert_eq!(wildcard(q).extension_hint(), None, "{q:?}");
    }
}

// ---------------------------------------------------------------------------
// Cross-check against a naive matcher
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Tok {
    Lit(char),
    One,
    Seq,
    Cross,
}

fn tokenize(p: &str) -> Vec<Tok> {
    let chars: Vec<char> = p.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' => {
                let mut j = i;
                while j < chars.len() && chars[j] == '*' {
                    j += 1;
                }
                out.push(if j - i >= 2 { Tok::Cross } else { Tok::Seq });
                i = j;
            }
            '?' => {
                out.push(Tok::One);
                i += 1;
            }
            c => {
                out.push(Tok::Lit(c));
                i += 1;
            }
        }
    }
    out
}

/// Exponential but obviously correct; only used on tiny inputs.
fn naive(p: &[Tok], s: &[char]) -> bool {
    match p.split_first() {
        None => s.is_empty(),
        Some((Tok::Lit(c), rest)) => s.first() == Some(c) && naive(rest, &s[1..]),
        Some((Tok::One, rest)) => s.first().is_some_and(|&c| c != '/') && naive(rest, &s[1..]),
        Some((Tok::Seq, rest)) => {
            for k in 0..=s.len() {
                if naive(rest, &s[k..]) {
                    return true;
                }
                if k < s.len() && s[k] == '/' {
                    break;
                }
            }
            false
        }
        Some((Tok::Cross, rest)) => (0..=s.len()).any(|k| naive(rest, &s[k..])),
    }
}

#[test]
fn agrees_with_naive_matcher_on_small_inputs() {
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let pat_alpha = ['a', 'b', '/', '*', '?'];
    let str_alpha = ['a', 'b', '/'];
    let mut checked = 0;
    for _ in 0..3000 {
        let plen = 1 + (next() % 6) as usize;
        let pat: String = (0..plen)
            .map(|_| pat_alpha[(next() % pat_alpha.len() as u64) as usize])
            .collect();
        if !pat.contains('*') && !pat.contains('?') {
            continue;
        }
        let w = wildcard(&pat);
        let toks = tokenize(&pat);
        for _ in 0..8 {
            let slen = (next() % 7) as usize;
            let s: String = (0..slen)
                .map(|_| str_alpha[(next() % str_alpha.len() as u64) as usize])
                .collect();
            let chars: Vec<char> = s.chars().collect();
            assert_eq!(
                w.matches(&s, true),
                naive(&toks, &chars),
                "pattern {pat:?} on {s:?}"
            );
            checked += 1;
        }
    }
    assert!(checked > 1000);
}

// ---------------------------------------------------------------------------
// No exponential blowup
// ---------------------------------------------------------------------------

#[test]
fn pathological_patterns_finish_quickly() {
    let hay = "a".repeat(3000);
    let started = Instant::now();

    let w = wildcard(&format!("{}b", "*a".repeat(20)));
    assert!(!w.matches(&hay, true));
    assert!(!w.matches(&hay, false));

    let w = wildcard(&format!("{}b", "**a".repeat(20)));
    assert!(!w.matches(&hay, true));

    let w = wildcard(&format!("{}b", "?*".repeat(20)));
    assert!(!w.matches(&hay, false));

    let mixed = format!("{}{}", "a/".repeat(1000), "a".repeat(1000));
    let w = wildcard(&format!("{}b", "**a*".repeat(15)));
    assert!(!w.matches(&mixed, true));

    // Through the evaluator as well.
    assert!(!eval(&format!("{}b", "*a".repeat(20)), &hay));

    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "pathological wildcard matching took {elapsed:?}"
    );
}
