//! Per-search options: the window's three chips.
//!
//! Contract under test:
//!
//! - `Engine::search`, `search_grouped`, `positions` and
//!   `positions_grouped` take a `fx_engine::QueryOptions` right after the
//!   query (`fx_query::QueryOptions`, re-exported): `match_case` (Aa),
//!   `match_path` (Path) and `regex` (.*). `QueryOptions::default()` is the
//!   plain search of earlier versions.
//! - With `regex` on, the whole query text is one regular expression:
//!   spaces, `|`, `<`, `>`, `!` and `"` are part of the pattern, not query
//!   syntax. It is case-insensitive unless `match_case`, and sees the full
//!   path with `match_path`.
//! - Errors are `fx_engine::SearchError`: `InvalidRegex { pattern,
//!   message }` when the expression (the whole query with `regex`, or a
//!   `regex:` term) does not compile, with the pattern as typed, and
//!   `Cancelled` when `cancel_searches` stopped the search.
//! - Results for different options are never mixed up by the refinement
//!   cache.

mod common;

use common::*;
use fx_engine::{Engine, EngineSnapshot, QueryOptions, SearchError};
use fx_model::{EntryId, StartOutcome};
use fx_sort::{Direction, SortKey};

const CASE: QueryOptions = QueryOptions {
    match_case: true,
    match_path: false,
    regex: false,
};
const PATH: QueryOptions = QueryOptions {
    match_case: false,
    match_path: true,
    regex: false,
};
const REGEX: QueryOptions = QueryOptions {
    match_case: false,
    match_path: false,
    regex: true,
};

fn tree() -> FakeTree {
    let tree = FakeTree::new();
    tree.mkdir("/vol/Docs");
    tree.mkdir("/vol/src");
    tree.touch("/vol/Docs/Report 2024.PDF", 10);
    tree.touch("/vol/Docs/report-final.txt", 11);
    tree.touch("/vol/Docs/notes.txt", 12);
    tree.touch("/vol/src/main.rs", 13);
    tree.touch("/vol/src/report.rs", 14);
    tree.touch("/vol/wow b.txt", 15);
    tree.touch("/vol/b wow.txt", 16);
    tree.touch("/vol/say \"hi\".txt", 17);
    tree.touch("/vol/!bang.txt", 18);
    tree
}

fn started() -> (FakeTree, FakeSource, Engine) {
    let tree = tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = start(&tree, &source, tree.build_index(), config());
    wait_for_tree(&engine, &tree, "initial index");
    (tree, source, engine)
}

fn names(snap: &EngineSnapshot, ids: &[u32]) -> Vec<String> {
    let mut names: Vec<String> = ids
        .iter()
        .map(|&id| snap.index().name(EntryId(id)).to_string())
        .collect();
    names.sort();
    names
}

/// The sorted names `query` finds under `options`.
fn found(engine: &Engine, query: &str, options: QueryOptions) -> Vec<String> {
    let page = engine
        .search(query, options, SortKey::Name, Direction::Ascending, 0, 1000)
        .unwrap_or_else(|e| panic!("{query:?} {options:?}: {e}"));
    assert_eq!(page.total, page.ids.len());
    names(&engine.snapshot(), &page.ids)
}

fn strs(v: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = v.iter().map(|s| s.to_string()).collect();
    v.sort();
    v
}

#[test]
fn default_options_are_the_plain_search() {
    let (_tree, _source, engine) = started();
    assert_eq!(
        found(&engine, "report", QueryOptions::default()),
        strs(&["Report 2024.PDF", "report-final.txt", "report.rs"])
    );
    assert_eq!(
        found(&engine, "wow b", QueryOptions::default()),
        strs(&["wow b.txt", "b wow.txt"])
    );
    engine.shutdown();
}

#[test]
fn match_case_option() {
    let (_tree, _source, engine) = started();
    assert_eq!(found(&engine, "Report", CASE), strs(&["Report 2024.PDF"]));
    assert_eq!(
        found(&engine, "report", CASE),
        strs(&["report-final.txt", "report.rs"])
    );
    // The refinement cache keeps options apart: the same text without
    // the option finds every case again.
    assert_eq!(
        found(&engine, "Report", QueryOptions::default()),
        strs(&["Report 2024.PDF", "report-final.txt", "report.rs"])
    );
    assert_eq!(found(&engine, "Report", CASE), strs(&["Report 2024.PDF"]));
    // `nocase:` in the query still overrides the option for its term.
    assert_eq!(
        found(&engine, "nocase:REPORT.RS", CASE),
        strs(&["report.rs"])
    );
    engine.shutdown();
}

#[test]
fn match_path_option() {
    let (_tree, _source, engine) = started();
    assert_eq!(
        found(&engine, "docs", QueryOptions::default()),
        strs(&["Docs"])
    );
    assert_eq!(
        found(&engine, "docs", PATH),
        strs(&["Docs", "Report 2024.PDF", "report-final.txt", "notes.txt"])
    );
    assert_eq!(
        found(&engine, "src rs", PATH),
        strs(&["main.rs", "report.rs"])
    );
    // Both options together.
    assert_eq!(found(&engine, "Docs", PATH), found(&engine, "docs", PATH));
    let both = QueryOptions {
        match_case: true,
        ..PATH
    };
    assert_eq!(found(&engine, "docs", both), Vec::<String>::new());
    assert_eq!(found(&engine, "Docs/n", both), strs(&["notes.txt"]));
    engine.shutdown();
}

#[test]
fn regex_option_makes_the_whole_query_one_expression() {
    let (_tree, _source, engine) = started();
    assert_eq!(
        found(&engine, r"^report.*\.(rs|txt)$", REGEX),
        strs(&["report-final.txt", "report.rs"])
    );
    // A space is part of the pattern, not an AND.
    assert_eq!(found(&engine, "wow b", REGEX), strs(&["wow b.txt"]));
    // So are `|`, `!`, `<`, `>` and `"`.
    assert_eq!(
        found(&engine, "^main|^notes", REGEX),
        strs(&["main.rs", "notes.txt"])
    );
    assert_eq!(found(&engine, "^!bang", REGEX), strs(&["!bang.txt"]));
    assert_eq!(
        found(&engine, r#"say "hi""#, REGEX),
        strs(&["say \"hi\".txt"])
    );
    assert_eq!(found(&engine, "<b>", REGEX), Vec::<String>::new());
    // Case-insensitive unless matching case.
    assert_eq!(
        found(&engine, "^Report", REGEX),
        strs(&["Report 2024.PDF", "report-final.txt", "report.rs"])
    );
    let case = QueryOptions {
        match_case: true,
        ..REGEX
    };
    assert_eq!(found(&engine, "^Report", case), strs(&["Report 2024.PDF"]));
    // The name only, unless matching paths.
    assert_eq!(found(&engine, "src/ma", REGEX), Vec::<String>::new());
    let path = QueryOptions {
        match_path: true,
        ..REGEX
    };
    assert_eq!(found(&engine, "src/ma", path), strs(&["main.rs"]));
    // An empty expression finds everything, like an empty query.
    let all = engine
        .search("", REGEX, SortKey::Name, Direction::Ascending, 0, 0)
        .unwrap();
    assert_eq!(all.total, engine.snapshot().index().live_count());
    engine.shutdown();
}

#[test]
fn an_invalid_regex_is_a_typed_error() {
    let (_tree, _source, engine) = started();
    let is_invalid = |r: Result<(), SearchError>, want: &str| match r {
        Err(SearchError::InvalidRegex { pattern, message }) => {
            assert_eq!(pattern, want);
            assert!(!message.is_empty());
        }
        other => panic!("expected InvalidRegex for {want:?}, got {other:?}"),
    };
    let (name, asc) = (SortKey::Name, Direction::Ascending);
    for query in ["(", "report(", r#"say "(hi""#, "[a-"] {
        is_invalid(
            engine.search(query, REGEX, name, asc, 0, 10).map(drop),
            query,
        );
        is_invalid(
            engine
                .search_grouped(query, REGEX, name, asc, true, 0, 10)
                .map(drop),
            query,
        );
        is_invalid(
            engine.positions(query, REGEX, name, asc, &["x"]).map(drop),
            query,
        );
        is_invalid(
            engine
                .positions_grouped(query, REGEX, name, asc, true, &["x"])
                .map(drop),
            query,
        );
    }
    // A `regex:` term in a plain query is reported the same way.
    is_invalid(
        engine
            .search("notes regex:(", QueryOptions::default(), name, asc, 0, 10)
            .map(drop),
        "(",
    );
    // The same text is fine without the option.
    assert_eq!(
        found(&engine, "report(", QueryOptions::default()),
        Vec::<String>::new()
    );
    let e = engine.search("(", REGEX, name, asc, 0, 10).unwrap_err();
    assert!(e.to_string().contains("regular expression"), "{e}");
    engine.shutdown();
}

#[test]
fn positions_follow_the_options() {
    let (_tree, _source, engine) = started();
    let snap = engine.snapshot();
    let report = snap
        .index()
        .live_ids()
        .find(|&id| snap.index().name(id) == "Report 2024.PDF")
        .expect("entry");
    let path = snap.index().path(report);
    let (name, asc) = (SortKey::Name, Direction::Ascending);
    let plain = engine
        .positions("report", QueryOptions::default(), name, asc, &[&path])
        .unwrap();
    let case = engine
        .positions("Report", CASE, name, asc, &[&path])
        .unwrap();
    assert_eq!(case.rows, vec![Some(0)]);
    let lower = engine
        .positions("report", CASE, name, asc, &[&path])
        .unwrap();
    assert_eq!(lower.rows, vec![None]);
    let regex = engine
        .positions("^REPORT 20", REGEX, name, asc, &[&path])
        .unwrap();
    assert_eq!(regex.rows, vec![Some(0)]);
    // Without options it sits among the three "report" matches.
    let page = engine
        .search("report", QueryOptions::default(), name, asc, 0, 10)
        .unwrap();
    let at = page.ids.iter().position(|&id| id == report.0);
    assert_eq!(plain.rows, vec![at]);
    engine.shutdown();
}

#[test]
fn cancelled_is_a_search_error() {
    let e: SearchError = fx_query::Cancelled.into();
    assert_eq!(e, SearchError::Cancelled);
    assert!(!e.to_string().is_empty());
}
