//! The groundwork API for search functions.
//!
//! Checks the public types the function suites build on (`Node::Function`, the
//! `syntax::functions` module with `Filter`, `Cmp` and `Range`,
//! `Parsed::count`), the metadata on `eval::Candidate`, `eval::EvalContext`
//! with `matches_in`, and the conformance harness's candidate specs with
//! trailing ` key=value` metadata.
//!
//! Contracts pinned here:
//!
//! - `Candidate { name, parent_path, is_dir, ..Default::default() }` works,
//!   and the default has no metadata (`None` everywhere, flags off).
//! - `matches(node, cand)` equals `matches_in(node, cand, &EvalContext::default())`.
//! - Missing metadata never makes a function test match; a candidate whose
//!   size is unknown matches only `size:unknown`.
//! - A word with an unknown `name:` prefix is literal text.
//! - Harness: `conformance::parse_spec(spec)` turns a spec into a
//!   `Candidate`. Trailing ` key=value` words (`size`, `dm`, `dc`, `da`,
//!   `attrib`) become metadata; everything before them is the usual path
//!   spec, spaces included. Spec dates are ISO 8601 at UTC (offset 0, the
//!   default context): `YYYY-MM-DDTHH:MM` or a bare `YYYY-MM-DD` (midnight).
//!   `attrib=H` sets `hidden`.
//!
//! Some checks only apply when the parser turns a word into a function;
//! they are guarded by `is_function`, and the rest hold whichever way the
//! word parses.

mod conformance;

use conformance::{DEFAULT, case, check, parse_spec};
use fx_query::eval::{Candidate, EvalContext, matches, matches_in};
use fx_query::syntax::functions::{Cmp, Filter, Range};
use fx_query::{Node, Pattern, parse};

/// 2026-11-03T10:00:00Z in Unix seconds.
const NOV3_10H: i64 = 1_793_700_000;
/// 2026-11-03T00:00:00Z in Unix seconds.
const NOV3_MIDNIGHT: i64 = 1_793_664_000;

fn bare<'a>(name: &'a str, is_dir: bool) -> Candidate<'a> {
    Candidate {
        name,
        parent_path: "/Users/me",
        is_dir,
        ..Default::default()
    }
}

fn is_function(node: &Node) -> bool {
    matches!(node, Node::Function(_))
}

fn other_ctx() -> EvalContext {
    EvalContext {
        now: NOV3_10H,
        utc_offset_secs: 3600,
    }
}

// ---------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------

#[test]
fn function_node_and_comparison_types_exist() {
    // `Node::Function` holds a `Filter`.
    fn _holds_filter(node: &Node) -> Option<&Filter> {
        match node {
            Node::Function(filter) => Some(filter),
            _ => None,
        }
    }
    // `Range<T>` is generic over the compared value.
    fn _ranges(_: Option<Range<u64>>, _: Option<Range<i64>>) {}

    let all = [
        Cmp::Eq,
        Cmp::ExactEq,
        Cmp::Ne,
        Cmp::Lt,
        Cmp::Le,
        Cmp::Gt,
        Cmp::Ge,
    ];
    assert!(matches!(all[0], Cmp::Eq));
    assert!(matches!(all[1], Cmp::ExactEq));
    assert!(matches!(all[2], Cmp::Ne));
    assert!(matches!(all[3], Cmp::Lt));
    assert!(matches!(all[4], Cmp::Le));
    assert!(matches!(all[5], Cmp::Gt));
    assert!(matches!(all[6], Cmp::Ge));
}

#[test]
fn plain_queries_have_no_count_limit() {
    for q in ["", "abc", "a b|c", "!x", "<a b>", "*.txt", "case:Foo"] {
        let parsed = parse(q, &DEFAULT);
        assert_eq!(parsed.count, None, "{q:?} should have no count limit");
    }
}

#[test]
fn unknown_prefix_is_literal_text() {
    for q in ["zzz:abc", "foo:bar", "notafunction:12"] {
        let parsed = parse(q, &DEFAULT);
        assert!(
            parsed.diagnostics.is_empty(),
            "{q:?}: {:?}",
            parsed.diagnostics
        );
        match &parsed.query {
            Node::Term(term) => {
                assert_eq!(term.text, q, "{q:?} keeps its whole text");
                assert_eq!(term.pattern, Pattern::Literal(q.to_owned()));
            }
            other => panic!("{q:?} should be a literal term, got {other:?}"),
        }
    }
    let parsed = parse("zzz:abc", &DEFAULT);
    assert_eq!(
        matches(&parsed.query, &bare("x zzz:abc y", false)),
        Ok(true)
    );
    assert_eq!(matches(&parsed.query, &bare("abc", false)), Ok(false));
}

// ---------------------------------------------------------------------
// Candidate metadata
// ---------------------------------------------------------------------

#[test]
fn candidate_default_has_no_metadata() {
    let c = bare("a.txt", false);
    assert_eq!(c.name, "a.txt");
    assert_eq!(c.parent_path, "/Users/me");
    assert!(!c.is_dir);
    assert_eq!(c.size, None);
    assert_eq!(c.modified, None);
    assert_eq!(c.created, None);
    assert_eq!(c.accessed, None);
    assert_eq!(c.child_count, None);
    assert!(!c.hidden);
    assert!(!c.symlink);

    let d: Candidate<'static> = Candidate::default();
    assert_eq!(d.name, "");
    assert!(!d.is_dir);
    assert_eq!(d.size, None);
}

#[test]
fn candidate_metadata_fields_are_settable() {
    let c = Candidate {
        name: "a.txt",
        parent_path: "/Users/me",
        is_dir: false,
        size: Some(1500),
        modified: Some(NOV3_10H),
        created: Some(NOV3_MIDNIGHT),
        accessed: Some(-1),
        child_count: Some(3),
        hidden: true,
        symlink: true,
        ..Default::default()
    };
    assert_eq!(c.size, Some(1500u64));
    assert_eq!(c.modified, Some(NOV3_10H));
    assert_eq!(c.created, Some(NOV3_MIDNIGHT));
    assert_eq!(c.accessed, Some(-1i64));
    assert_eq!(c.child_count, Some(3u32));
    assert!(c.hidden && c.symlink);
    assert_eq!(c.full_path(), "/Users/me/a.txt");
}

// ---------------------------------------------------------------------
// Evaluation context
// ---------------------------------------------------------------------

#[test]
fn default_context_is_fixed() {
    let a = EvalContext::default();
    let b = EvalContext::default();
    assert_eq!(a.now, b.now);
    assert_eq!(a.utc_offset_secs, b.utc_offset_secs);
    let _: i64 = a.now;
    let _: i32 = a.utc_offset_secs;
}

#[test]
fn matches_equals_matches_in_default_context() {
    let queries = [
        "",
        "foo",
        "!foo",
        "foo|bar",
        "<a b>|c",
        "*.txt",
        "folder:",
        "case:Foo",
        "size:>1kb",
        "size:unknown",
        "dm:2026",
        "dm:today",
        "ext:txt",
    ];
    let with_meta = Candidate {
        size: Some(5000),
        modified: Some(NOV3_10H),
        created: Some(NOV3_10H),
        accessed: Some(NOV3_10H),
        ..bare("foo.txt", false)
    };
    let cands = [
        bare("foo.txt", false),
        bare("Foo bar", false),
        bare("src", true),
        with_meta,
    ];
    let ctx = EvalContext::default();
    for q in queries {
        let parsed = parse(q, &DEFAULT);
        for c in &cands {
            assert_eq!(
                matches(&parsed.query, c),
                matches_in(&parsed.query, c, &ctx),
                "{q:?} on {c:?}"
            );
        }
    }
}

#[test]
fn context_does_not_affect_name_terms() {
    let ctx = other_ctx();
    for (q, name, want) in [
        ("foo", "a foo.txt", true),
        ("foo", "bar.txt", false),
        ("!foo", "bar.txt", true),
        ("*.txt", "bar.txt", true),
        ("a|b", "c", false),
    ] {
        let parsed = parse(q, &DEFAULT);
        let c = bare(name, false);
        assert_eq!(
            matches_in(&parsed.query, &c, &ctx),
            Ok(want),
            "{q:?} on {name:?}"
        );
        assert_eq!(matches(&parsed.query, &c), Ok(want), "{q:?} on {name:?}");
    }
}

// ---------------------------------------------------------------------
// Missing metadata
// ---------------------------------------------------------------------

#[test]
fn missing_metadata_never_matches_a_function() {
    let queries = [
        "size:>1kb",
        "size:<1kb",
        "size:0..10mb",
        "dm:2026",
        "dm:today",
        "dc:2026",
        "da:2026",
        "dm:>2000-01-01",
        "dc:<2100-01-01",
    ];
    let ctxs = [EvalContext::default(), other_ctx()];
    for q in queries {
        let parsed = parse(q, &DEFAULT);
        for c in [bare("a.txt", false), bare("src", true)] {
            assert_eq!(matches(&parsed.query, &c), Ok(false), "{q:?} on {c:?}");
            for ctx in &ctxs {
                assert_eq!(
                    matches_in(&parsed.query, &c, ctx),
                    Ok(false),
                    "{q:?} on {c:?} with {ctx:?}"
                );
            }
        }
    }
}

#[test]
fn empty_needs_a_known_child_count() {
    let parsed = parse("empty:", &DEFAULT);
    assert_eq!(matches(&parsed.query, &bare("src", true)), Ok(false));
    if is_function(&parsed.query) {
        let empty_dir = Candidate {
            child_count: Some(0),
            ..bare("src", true)
        };
        assert_eq!(matches(&parsed.query, &empty_dir), Ok(true));
    }
}

#[test]
fn known_metadata_can_match_once_parsed_as_function() {
    let c = Candidate {
        size: Some(5000),
        modified: Some(NOV3_10H),
        created: Some(NOV3_10H),
        accessed: Some(NOV3_10H),
        ..bare("a.txt", false)
    };
    for q in ["size:>1kb", "dm:2026", "dc:2026", "da:2026"] {
        let parsed = parse(q, &DEFAULT);
        if is_function(&parsed.query) {
            assert_eq!(matches(&parsed.query, &c), Ok(true), "{q:?}");
            assert_eq!(
                matches(&parsed.query, &bare("a.txt", false)),
                Ok(false),
                "{q:?}"
            );
        }
    }
}

#[test]
fn unknown_size_matches_only_size_unknown() {
    let known = Candidate {
        size: Some(1500),
        ..bare("a.txt", false)
    };
    let unknown = bare("a.txt", false);

    let parsed = parse("size:unknown", &DEFAULT);
    assert_eq!(matches(&parsed.query, &known), Ok(false));
    if is_function(&parsed.query) {
        assert_eq!(matches(&parsed.query, &unknown), Ok(true));
        assert_eq!(matches_in(&parsed.query, &unknown, &other_ctx()), Ok(true));
    }

    for q in ["size:0", "size:>0", "size:<1", "size:>=0", "size:!=1500"] {
        let parsed = parse(q, &DEFAULT);
        assert_eq!(matches(&parsed.query, &unknown), Ok(false), "{q:?}");
    }
}

// ---------------------------------------------------------------------
// Conformance harness: candidate specs with metadata
// ---------------------------------------------------------------------

#[test]
fn spec_without_metadata_is_unchanged() {
    let c = parse_spec("notes.txt");
    assert_eq!(
        (c.name, c.parent_path, c.is_dir),
        ("notes.txt", "/Users/me", false)
    );
    assert_eq!(c.size, None);
    assert_eq!(c.modified, None);
    assert_eq!(c.created, None);
    assert_eq!(c.accessed, None);
    assert_eq!(c.child_count, None);
    assert!(!c.hidden && !c.symlink);

    let c = parse_spec("Projects/");
    assert_eq!(
        (c.name, c.parent_path, c.is_dir),
        ("Projects", "/Users/me", true)
    );

    let c = parse_spec("/Users/me/src/main.rs");
    assert_eq!(
        (c.name, c.parent_path, c.is_dir),
        ("main.rs", "/Users/me/src", false)
    );

    let c = parse_spec("/Volumes/");
    assert_eq!((c.name, c.parent_path, c.is_dir), ("Volumes", "/", true));
}

#[test]
fn spec_names_with_spaces_are_kept() {
    let c = parse_spec("foo bar.txt");
    assert_eq!((c.name, c.is_dir), ("foo bar.txt", false));
    let c = parse_spec("a  b");
    assert_eq!(c.name, "a  b");
    let c = parse_spec("my docs/");
    assert_eq!((c.name, c.is_dir), ("my docs", true));
    let c = parse_spec("/Users/me/my docs/x.txt");
    assert_eq!((c.name, c.parent_path), ("x.txt", "/Users/me/my docs"));
    assert_eq!(c.size, None);
}

#[test]
fn spec_with_full_metadata() {
    let c = parse_spec("a.txt size=1500 dm=2026-11-03T10:00 attrib=H");
    assert_eq!(
        (c.name, c.parent_path, c.is_dir),
        ("a.txt", "/Users/me", false)
    );
    assert_eq!(c.size, Some(1500));
    assert_eq!(c.modified, Some(NOV3_10H));
    assert_eq!(c.created, None);
    assert_eq!(c.accessed, None);
    assert!(c.hidden);
    assert!(!c.symlink);
}

#[test]
fn spec_dates_for_each_timestamp() {
    let c = parse_spec("a.txt dc=2026-11-03T10:00");
    assert_eq!(
        (c.created, c.modified, c.accessed),
        (Some(NOV3_10H), None, None)
    );
    let c = parse_spec("a.txt da=2026-11-03T10:00");
    assert_eq!(
        (c.accessed, c.modified, c.created),
        (Some(NOV3_10H), None, None)
    );
    let c = parse_spec("a.txt dm=2026-11-03");
    assert_eq!(c.modified, Some(NOV3_MIDNIGHT));
    let c = parse_spec("a.txt dm=1970-01-01T00:00");
    assert_eq!(c.modified, Some(0));
}

#[test]
fn spec_metadata_after_names_with_spaces_and_folders() {
    let c = parse_spec("a b size=10");
    assert_eq!((c.name, c.size), ("a b", Some(10)));
    let c = parse_spec("Projects/ size=0");
    assert_eq!((c.name, c.is_dir, c.size), ("Projects", true, Some(0)));
    let c = parse_spec("/Users/me/src/main.rs size=42 attrib=H");
    assert_eq!((c.name, c.parent_path), ("main.rs", "/Users/me/src"));
    assert_eq!(c.size, Some(42));
    assert!(c.hidden);
}

#[test]
fn harness_strips_metadata_before_matching() {
    let problems = check(
        &case("api", "a.txt")
            .yes(&["a.txt size=1500 dm=2026-11-03T10:00 attrib=H"])
            .no(&["b.txt size=1500"]),
    );
    assert!(problems.is_empty(), "{problems:#?}");

    // The metadata words are not part of the name the query sees.
    let problems = check(&case("api", "size").yes(&["size.txt size=1"]).no(&[
        "a.txt size=1500",
        "b.txt dm=2026-11-03T10:00",
        "c attrib=H",
    ]));
    assert!(problems.is_empty(), "{problems:#?}");

    // Mismatches are still reported.
    let problems = check(&case("api", "zzz").yes(&["a.txt size=1"]));
    assert_eq!(problems.len(), 1, "{problems:#?}");
}
