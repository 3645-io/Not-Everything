//! Regular-expression terms.
//!
//! Rules under test, restated in our own words from voidtools' public
//! Search Syntax and Search Modifiers pages
//! (https://www.voidtools.com/support/everything/searching/) and
//! docs/PLAN.md:
//!
//! - `regex:` (or the global regex toggle in [`SearchOptions`]) makes a term
//!   a regular expression, compiled with the `regex` crate.
//! - Matching is unanchored: the expression may match anywhere in the
//!   haystack; the user writes `^` and `$` to anchor it.
//! - Case-insensitive unless the term matches case (`case:` or the global
//!   toggle). The diacritics option does not apply to regex terms: the
//!   expression sees the characters as stored.
//! - In path mode (`path:` or the global toggle) the expression is tested
//!   against the full path; otherwise against the name only.
//! - `noregex:` turns a global regex toggle off for one term, which then
//!   takes the literal or wildcard path.
//! - Regex terms combine with AND, OR and NOT like any other term.
//! - An invalid expression (including PCRE-only features such as
//!   look-around and back-references, which the `regex` crate rejects)
//!   matches nothing; evaluating it never panics or errors.

use fx_query::eval::{Candidate, matches};
use fx_query::regex_term::{RegexError, RegexTerm};
use fx_query::{Pattern, SearchOptions, TermOptions, parse};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Evaluates `query` parsed with `opts` against one entry. The evaluator
/// must support regex terms (no `Unsupported` error).
#[track_caller]
fn eval_with(query: &str, opts: &SearchOptions, name: &str, parent: &str, is_dir: bool) -> bool {
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

/// A file `name` in `/Users/me`, default options.
#[track_caller]
fn file(query: &str, name: &str) -> bool {
    eval_with(query, &SearchOptions::default(), name, "/Users/me", false)
}

/// A file `name` in `parent`, default options.
#[track_caller]
fn file_in(query: &str, name: &str, parent: &str) -> bool {
    eval_with(query, &SearchOptions::default(), name, parent, false)
}

/// A folder `name` in `/Users/me`, default options.
#[track_caller]
fn folder(query: &str, name: &str) -> bool {
    eval_with(query, &SearchOptions::default(), name, "/Users/me", true)
}

fn regex_on() -> SearchOptions {
    SearchOptions {
        regex: true,
        ..SearchOptions::default()
    }
}

fn regex_opts() -> TermOptions {
    TermOptions {
        regex: true,
        ..TermOptions::default()
    }
}

// ---------------------------------------------------------------------------
// Basic semantics
// ---------------------------------------------------------------------------

#[test]
fn anchored_regex_matches_whole_name() {
    let q = "regex:^foo.*bar$";
    assert!(file(q, "foobar"));
    assert!(file(q, "foo-middle-bar"));
    assert!(file(q, "foo.bar"));
    assert!(!file(q, "foobar.txt"));
    assert!(!file(q, "xfoobar"));
    assert!(!file(q, "barfoo"));
    assert!(!file(q, "foo"));
}

#[test]
fn regex_is_unanchored_by_default() {
    assert!(file("regex:oo", "foo.txt"));
    assert!(file("regex:\\.txt", "notes.txt.bak"));
    assert!(file("regex:t.t", "a.txt"));
    assert!(!file("regex:t.t", "a.md"));
    // Only `$` anchors to the end.
    assert!(file("regex:\\.txt$", "notes.txt"));
    assert!(!file("regex:\\.txt$", "notes.txt.bak"));
    // Only `^` anchors to the start.
    assert!(file("regex:^note", "notes.txt"));
    assert!(!file("regex:^otes", "notes.txt"));
}

#[test]
fn regex_metacharacters_are_live() {
    assert!(file("regex:^a\\d+\\.log$", "a123.log"));
    assert!(!file("regex:^a\\d+\\.log$", "ab.log"));
    assert!(file("regex:^[a-c]x", "bx.txt"));
    assert!(!file("regex:^[a-c]x", "dx.txt"));
    // `?` and `*` are regex quantifiers, not wildcards.
    assert!(file("regex:^colou?r$", "color"));
    assert!(file("regex:^colou?r$", "colour"));
    assert!(!file("regex:^colou?r$", "colouur"));
    // Alternation inside quotes stays inside the expression.
    assert!(file("regex:\"^(cat|dog)\\.png$\"", "dog.png"));
    assert!(!file("regex:\"^(cat|dog)\\.png$\"", "cow.png"));
}

#[test]
fn regex_pattern_is_kept_raw() {
    let parsed = parse("regex:^foo.*bar$", &SearchOptions::default());
    match parsed.query {
        fx_query::Node::Term(t) => {
            assert_eq!(t.pattern, Pattern::Regex("^foo.*bar$".to_owned()));
        }
        other => panic!("expected a single term, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Case sensitivity
// ---------------------------------------------------------------------------

#[test]
fn regex_is_case_insensitive_by_default() {
    assert!(file("regex:^FOO", "foo.txt"));
    assert!(file("regex:^foo", "FOO.TXT"));
    assert!(file("regex:\\.TXT$", "a.txt"));
    // Unicode case folding, not just ASCII.
    assert!(file("regex:^ÉCOLE", "école.pdf"));
}

#[test]
fn case_modifier_makes_regex_case_sensitive() {
    assert!(!file("case:regex:^FOO", "foo.txt"));
    assert!(file("case:regex:^FOO", "FOO.txt"));
    assert!(!file("regex:case:^FOO", "foo.txt"));
    assert!(file("regex:case:^FOO", "FOO.txt"));
    assert!(!file("case:regex:^ÉCOLE", "école.pdf"));
}

#[test]
fn global_match_case_applies_to_regex() {
    let case_on = SearchOptions {
        match_case: true,
        ..SearchOptions::default()
    };
    assert!(!eval_with("regex:^FOO", &case_on, "foo.txt", "/x", false));
    assert!(eval_with("regex:^FOO", &case_on, "FOO.txt", "/x", false));
    // `nocase:` turns it back off for one term.
    assert!(eval_with(
        "nocase:regex:^FOO",
        &case_on,
        "foo.txt",
        "/x",
        false
    ));
}

#[test]
fn diacritics_option_does_not_apply_to_regex() {
    // Literal terms ignore accents by default; regex terms see the
    // characters as stored.
    assert!(file("resume", "Résumé.pdf"));
    assert!(!file("regex:resume", "Résumé.pdf"));
    assert!(!file("nodiacritics:regex:resume", "Résumé.pdf"));
    assert!(file("regex:résumé", "Résumé.pdf"));
    assert!(file("regex:^r.sum.\\.pdf$", "Résumé.pdf"));
}

// ---------------------------------------------------------------------------
// Path mode
// ---------------------------------------------------------------------------

#[test]
fn path_regex_matches_full_path() {
    let q = "path:regex:^/Users/me/docs/.*\\.txt$";
    assert!(file_in(q, "a.txt", "/Users/me/docs"));
    assert!(file_in(q, "b.txt", "/Users/me/docs/sub"));
    assert!(!file_in(q, "a.txt", "/Users/you/docs"));
    assert!(!file_in(q, "a.md", "/Users/me/docs"));

    // Without `path:` the expression only sees the name.
    let q = "regex:^/Users/me/docs/.*\\.txt$";
    assert!(!file_in(q, "a.txt", "/Users/me/docs"));
}

#[test]
fn path_regex_sees_parent_folders() {
    assert!(file_in("path:regex:docs", "a.txt", "/Users/me/docs"));
    assert!(!file_in("regex:docs", "a.txt", "/Users/me/docs"));
    assert!(file_in("regex:docs", "docs.txt", "/Users/me"));
    // Unanchored in path mode as well.
    assert!(file_in("path:regex:me/do", "a.txt", "/Users/me/docs"));
}

#[test]
fn path_regex_under_root() {
    assert!(file_in("path:regex:^/a\\.txt$", "a.txt", "/"));
    assert!(!file_in("path:regex:^//", "a.txt", "/"));
}

#[test]
fn global_match_path_applies_to_regex() {
    let path_on = SearchOptions {
        match_path: true,
        ..SearchOptions::default()
    };
    assert!(eval_with(
        "regex:^/Users/me/.*\\.txt$",
        &path_on,
        "a.txt",
        "/Users/me",
        false
    ));
    assert!(!eval_with(
        "nopath:regex:^/Users/me/.*\\.txt$",
        &path_on,
        "a.txt",
        "/Users/me",
        false
    ));
}

#[test]
fn path_regex_is_case_insensitive_by_default() {
    assert!(file_in("path:regex:^/users/ME/", "a.txt", "/Users/me"));
    assert!(!file_in(
        "case:path:regex:^/users/ME/",
        "a.txt",
        "/Users/me"
    ));
}

// ---------------------------------------------------------------------------
// Global regex toggle and noregex:
// ---------------------------------------------------------------------------

#[test]
fn global_regex_toggle_makes_terms_regex() {
    let opts = regex_on();
    assert!(eval_with("^foo.*bar$", &opts, "foo-x-bar", "/x", false));
    assert!(!eval_with(
        "^foo.*bar$",
        &opts,
        "foo-x-bar.txt",
        "/x",
        false
    ));
    // `.` is any character, not a literal dot.
    assert!(eval_with("a.c", &opts, "abc", "/x", false));
    // The same text without the toggle is a literal (or wildcard) term.
    let plain = SearchOptions::default();
    assert!(!eval_with("a.c", &plain, "abc", "/x", false));
    assert!(eval_with("a.c", &plain, "a.c", "/x", false));
}

#[test]
fn global_regex_toggle_with_case_and_path() {
    let opts = regex_on();
    assert!(eval_with("^FOO", &opts, "foo.txt", "/x", false));
    assert!(!eval_with("case:^FOO", &opts, "foo.txt", "/x", false));
    assert!(eval_with("path:^/x/y/", &opts, "foo.txt", "/x/y", false));
    assert!(!eval_with("^/x/y/", &opts, "foo.txt", "/x/y", false));
}

#[test]
fn noregex_takes_the_literal_path() {
    let opts = regex_on();
    // A literal: `.` is a dot, and the text is a plain substring.
    assert!(eval_with("noregex:a.c", &opts, "x a.c y", "/x", false));
    assert!(!eval_with("noregex:a.c", &opts, "abc", "/x", false));
    // Literal terms fold accents by default; regex terms do not.
    assert!(eval_with(
        "noregex:resume",
        &opts,
        "Résumé.pdf",
        "/x",
        false
    ));
    assert!(!eval_with("resume", &opts, "Résumé.pdf", "/x", false));
    // A wildcard: must match the whole name.
    assert!(eval_with("noregex:*.txt", &opts, "a.txt", "/x", false));
    assert!(!eval_with("noregex:*.txt", &opts, "a.txt.bak", "/x", false));
    // `^` and `$` are ordinary characters for a literal.
    assert!(!eval_with("noregex:^a", &opts, "a.txt", "/x", false));
    assert!(eval_with("noregex:^a", &opts, "x^a", "/x", false));
}

// ---------------------------------------------------------------------------
// Boolean operators and kind filters
// ---------------------------------------------------------------------------

#[test]
fn regex_combines_with_and() {
    let q = "regex:^foo regex:bar$";
    assert!(file(q, "foo-bar"));
    assert!(!file(q, "foo-baz"));
    assert!(!file(q, "xfoo-bar"));
    // With a literal term.
    assert!(file("regex:^rep report", "report-2024.pdf"));
    assert!(!file("regex:^rep invoice", "report-2024.pdf"));
}

#[test]
fn regex_combines_with_or() {
    let q = "regex:^foo|regex:bar$";
    assert!(file(q, "foo.txt"));
    assert!(file(q, "a-bar"));
    assert!(!file(q, "baz.txt"));
    // OR with a literal term.
    assert!(file("regex:^zzz|report", "report.pdf"));
    assert!(file("report|regex:^zzz", "zzz.pdf"));
}

#[test]
fn regex_combines_with_not() {
    assert!(!file("!regex:\\.txt$", "a.txt"));
    assert!(file("!regex:\\.txt$", "a.md"));
    assert!(file("report !regex:\\d", "report.pdf"));
    assert!(!file("report !regex:\\d", "report1.pdf"));
}

#[test]
fn regex_inside_groups() {
    let q = "<regex:^a|regex:^b> regex:\\.txt$";
    assert!(file(q, "a1.txt"));
    assert!(file(q, "b1.txt"));
    assert!(!file(q, "c1.txt"));
    assert!(!file(q, "a1.md"));
}

#[test]
fn regex_respects_file_and_folder_kind() {
    assert!(folder("folder:regex:^src$", "src"));
    assert!(!file("folder:regex:^src$", "src"));
    assert!(file("file:regex:^src$", "src"));
    assert!(!folder("file:regex:^src$", "src"));
}

// ---------------------------------------------------------------------------
// Invalid expressions
// ---------------------------------------------------------------------------

#[test]
fn invalid_regex_matches_nothing() {
    for name in ["(", "a(b", "foo.txt", "", "x"] {
        assert!(!file("regex:(", name), "regex:( should not match {name:?}");
        assert!(
            !folder("regex:(", name),
            "regex:( should not match folder {name:?}"
        );
    }
    let opts = regex_on();
    assert!(!eval_with("(", &opts, "(", "/x", false));
}

#[test]
fn other_invalid_regexes_match_nothing() {
    for q in ["regex:[a-", "regex:a{2,1}", "regex:\"*\"", "regex:\\"] {
        assert!(!file(q, "a.txt"), "{q:?}");
        assert!(!file(q, "aa"), "{q:?}");
    }
}

#[test]
fn pcre_only_features_are_invalid_and_match_nothing() {
    // The `regex` crate has no look-around and no back-references.
    assert!(!file("regex:\"foo(?=bar)\"", "foobar"));
    assert!(!file("regex:\"(?<!x)foo\"", "foo"));
    assert!(!file("regex:\"(a)\\1\"", "aa"));
}

#[test]
fn invalid_regex_does_not_poison_other_branches() {
    // OR: the valid branch still decides.
    assert!(file("regex:(|foo", "foo.txt"));
    assert!(!file("regex:(|foo", "bar.txt"));
    // AND: the invalid term matches nothing, so the whole AND fails.
    assert!(!file("regex:( foo", "foo.txt"));
}

#[test]
fn regex_never_panics_on_odd_input() {
    for q in [
        "regex:",
        "regex:\"",
        "regex:\"(",
        "regex:)",
        "regex:]",
        "regex:{",
        "regex:+",
        "regex:?",
        "regex:\\p{Nope}",
        "regex:é",
        "path:regex:(",
        "case:regex:[",
        "!regex:(",
        "regex:^$",
    ] {
        let parsed = parse(q, &SearchOptions::default());
        for (name, parent) in [("a", "/"), ("", ""), ("é.txt", "/Users/me")] {
            let cand = Candidate::file(name, parent);
            let _ = matches(&parsed.query, &cand)
                .unwrap_or_else(|e| panic!("{q:?} should be supported: {e}"));
        }
    }
}

// ---------------------------------------------------------------------------
// The compiled regex term
// ---------------------------------------------------------------------------

#[test]
fn compiled_term_matches_haystack() {
    let t = RegexTerm::compile("^foo.*bar$", &regex_opts()).expect("valid pattern");
    assert!(t.is_match("foo-bar"));
    assert!(t.is_match("FOO-BAR"));
    assert!(!t.is_match("foo-bar.txt"));

    let t = RegexTerm::compile("oo", &regex_opts()).expect("valid pattern");
    assert!(t.is_match("foo.txt"), "unanchored");
}

#[test]
fn compiled_term_honours_match_case() {
    let opts = TermOptions {
        match_case: true,
        ..regex_opts()
    };
    let t = RegexTerm::compile("^FOO", &opts).expect("valid pattern");
    assert!(t.is_match("FOO.txt"));
    assert!(!t.is_match("foo.txt"));
}

#[test]
fn invalid_pattern_gives_regex_error_with_message() {
    for pattern in ["(", "[a-", "foo(?=bar)", "(a)\\1"] {
        let err: RegexError = match RegexTerm::compile(pattern, &regex_opts()) {
            Ok(_) => panic!("{pattern:?} should not compile"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(!msg.trim().is_empty(), "{pattern:?} error has no message");
        let _: &dyn std::error::Error = &err;
    }
}
