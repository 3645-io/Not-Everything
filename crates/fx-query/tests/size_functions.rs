//! `size:` with its comparison and range grammar, size
//! units and constants, and the entry functions `ext:`, `parent:` /
//! `infolder:`, `depth:`, `len:`, `empty:`, `count:` and `attrib:`.
//!
//! Readings pinned here (our own words, after voidtools' public Search
//! Functions and Searching pages):
//!
//! - Size units are `b`, `kb`, `mb`, `gb`, `tb`, case-insensitive, in
//!   steps of 1024, with optional decimals; a bare number is bytes.
//! - A size value covers its unit's precision: `size:1kb` is the bucket
//!   1024..=2047, `size:1mb` is 1 MiB up to 2 MiB - 1, `size:1500` is just
//!   1500. `==` compares exactly against the value's first byte. `<` and
//!   `>=` compare against the bucket's start, `<=` and `>` against its end,
//!   `!=` is outside the bucket, and a range `a..b` runs from the start of
//!   `a`'s bucket to the end of `b`'s.
//! - Constants: `empty` is 0, `tiny` up to 10 KB, `small` 10-100 KB,
//!   `medium` 100 KB-1 MB, `large` 1-16 MB, `huge` 16-128 MB, `gigantic`
//!   over 128 MB, `unknown` an unknown size. Only interior points of each
//!   band are checked here, so the exact boundary convention is free.
//! - Folders have no size: no `size:` test matches a folder except
//!   `size:unknown` on a folder whose size is not known.
//! - `ext:` takes a `;` list, case-insensitive, against the text after the
//!   last dot; an empty `ext:` matches names without a dot.
//! - `parent:` / `infolder:` match direct children of exactly that folder.
//! - `depth:` counts the full path's components (`/Volumes` is 1,
//!   `/Users/me/a.txt` is 3); `len:` counts the name's chars.
//! - `empty:` matches folders whose child count is known to be 0.
//! - `count:N` sets `Parsed::count` and filters nothing.
//! - `attrib:` letters D, H, L, R; every listed letter must hold. Other
//!   letters parse without a diagnostic but never match.
//! - Malformed arguments produce a diagnostic and never panic.

use fx_query::eval::{Candidate, matches};
use fx_query::syntax::functions::Filter;
use fx_query::{Node, Parsed, SearchOptions, parse};

const KB: u64 = 1024;
const MB: u64 = 1024 * KB;
const GB: u64 = 1024 * MB;
const TB: u64 = 1024 * GB;

fn p(query: &str) -> Parsed {
    parse(query, &SearchOptions::default())
}

/// Whether `query` matches `cand`; panics if the evaluator refuses.
fn m(query: &str, cand: &Candidate<'_>) -> bool {
    let parsed = p(query);
    matches(&parsed.query, cand)
        .unwrap_or_else(|e| panic!("{query:?} on {cand:?}: unsupported {e:?}"))
}

/// A file in `/Users/me` with a known size.
fn sized(size: u64) -> Candidate<'static> {
    Candidate {
        name: "data.bin",
        parent_path: "/Users/me",
        is_dir: false,
        size: Some(size),
        ..Default::default()
    }
}

/// An entry at an absolute path such as `/Users/me/a.txt`.
fn at(path: &'static str, is_dir: bool) -> Candidate<'static> {
    let cut = path.rfind('/').expect("absolute path");
    let parent = if cut == 0 { "/" } else { &path[..cut] };
    Candidate {
        name: &path[cut + 1..],
        parent_path: parent,
        is_dir,
        ..Default::default()
    }
}

fn file(name: &'static str) -> Candidate<'static> {
    Candidate {
        name,
        parent_path: "/Users/me",
        is_dir: false,
        ..Default::default()
    }
}

fn folder(name: &'static str) -> Candidate<'static> {
    Candidate {
        name,
        parent_path: "/Users/me",
        is_dir: true,
        ..Default::default()
    }
}

/// Asserts `query` matches exactly the sizes in `yes` and none in `no`.
fn sizes(query: &str, yes: &[u64], no: &[u64]) {
    let parsed = p(query);
    assert!(
        parsed.diagnostics.is_empty(),
        "{query:?}: {:?}",
        parsed.diagnostics
    );
    for &s in yes {
        assert!(m(query, &sized(s)), "{query:?} should match size {s}");
    }
    for &s in no {
        assert!(!m(query, &sized(s)), "{query:?} should not match size {s}");
    }
}

fn assert_function(query: &str, name: &str) {
    let parsed = p(query);
    assert!(
        parsed.diagnostics.is_empty(),
        "{query:?}: {:?}",
        parsed.diagnostics
    );
    match &parsed.query {
        Node::Function(filter) => assert_eq!(filter.name(), name, "{query:?}"),
        other => panic!("{query:?} should parse to a function, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// size: parsing
// ---------------------------------------------------------------------

#[test]
fn size_words_parse_to_size_functions() {
    for q in [
        "size:1kb",
        "size:<1mb",
        "size:<=1mb",
        "size:>1mb",
        "size:>=1mb",
        "size:=1mb",
        "size:==1mb",
        "size:!=1mb",
        "size:1mb..2mb",
        "size:1500",
        "size:1.5mb",
        "size:1KB",
        "size:empty",
        "size:tiny",
        "size:small",
        "size:medium",
        "size:large",
        "size:huge",
        "size:gigantic",
        "size:unknown",
    ] {
        assert_function(q, "size");
    }
}

#[test]
fn size_unknown_parses_to_the_unknown_test() {
    use fx_query::syntax::functions::SizeTest;
    assert_eq!(
        p("size:unknown").query,
        Node::Function(Filter::Size(SizeTest::Unknown))
    );
}

// ---------------------------------------------------------------------
// size: units and precision
// ---------------------------------------------------------------------

#[test]
fn bare_number_is_bytes() {
    sizes("size:1500", &[1500], &[1499, 1501, 0, 1500 * KB]);
    sizes("size:0", &[0], &[1, KB]);
    sizes("size:==1500", &[1500], &[1499, 1501]);
    sizes("size:>1500", &[1501, MB], &[1500, 0]);
    sizes("size:<1500", &[0, 1499], &[1500, 1501]);
}

#[test]
fn units_are_powers_of_1024() {
    sizes("size:==1b", &[1], &[0, 2]);
    sizes("size:==10b", &[10], &[9, 11]);
    sizes("size:==1kb", &[KB], &[1000, KB + 1, KB - 1]);
    sizes("size:==1mb", &[MB], &[1_000_000, MB + 1]);
    sizes("size:==1gb", &[GB], &[1_000_000_000, GB + 1]);
    sizes("size:==1tb", &[TB], &[1_000_000_000_000, TB + 1]);
    sizes("size:==3kb", &[3 * KB], &[3000, 3 * KB + 1]);
}

#[test]
fn units_are_case_insensitive() {
    for q in ["size:==1KB", "size:==1Kb", "size:==1kB", "size:==1kb"] {
        sizes(q, &[KB], &[KB + 1, 1000]);
    }
    for q in ["size:==2MB", "size:==2Mb", "size:==2mb"] {
        sizes(q, &[2 * MB], &[2 * MB + 1]);
    }
    sizes("size:==1GB", &[GB], &[GB - 1]);
    sizes("size:==1TB", &[TB], &[TB - 1]);
    sizes("size:==7B", &[7], &[8]);
}

#[test]
fn eq_covers_the_units_precision() {
    sizes("size:1kb", &[KB, 1500, 2 * KB - 1], &[KB - 1, 2 * KB, 0]);
    sizes("size:=1kb", &[KB, 2 * KB - 1], &[KB - 1, 2 * KB]);
    sizes(
        "size:1mb",
        &[MB, MB + 500 * KB, 2 * MB - 1],
        &[MB - 1, 2 * MB],
    );
    sizes("size:2gb", &[2 * GB, 3 * GB - 1], &[2 * GB - 1, 3 * GB]);
}

#[test]
fn exact_eq_is_the_exact_value() {
    sizes("size:==1kb", &[KB], &[KB + 1, 1500, 2 * KB - 1]);
    sizes("size:==1mb", &[MB], &[MB + 1]);
}

#[test]
fn comparisons_use_the_bucket_bounds() {
    // `<` and `>=` against the bucket start.
    sizes("size:<1kb", &[0, KB - 1], &[KB, 1500, 2 * KB]);
    sizes("size:>=1kb", &[KB, 1500, MB], &[KB - 1, 0]);
    // `<=` and `>` against the bucket end.
    sizes("size:<=1kb", &[0, KB, 2 * KB - 1], &[2 * KB, MB]);
    sizes("size:>1kb", &[2 * KB, 5000, MB], &[KB, 2 * KB - 1, 0]);
    // `!=` is outside the bucket.
    sizes("size:!=1kb", &[0, KB - 1, 2 * KB, MB], &[KB, 2 * KB - 1]);
    sizes("size:<1mb", &[0, MB - 1], &[MB, 2 * MB]);
    sizes("size:>1mb", &[2 * MB, GB], &[MB, 2 * MB - 1]);
}

#[test]
fn decimals_scale_the_unit() {
    sizes("size:==1.5kb", &[1536], &[1535, 1537, KB]);
    sizes("size:==0.5kb", &[512], &[511, 513]);
    sizes("size:==1.5mb", &[MB + MB / 2], &[MB, 2 * MB]);
    sizes(
        "size:<1.5mb",
        &[0, MB, MB + MB / 2 - 1],
        &[MB + MB / 2, 2 * MB],
    );
    sizes(
        "size:>=1.5mb",
        &[MB + MB / 2, 3 * MB],
        &[MB + MB / 2 - 1, MB],
    );
    sizes("size:==2.25KB", &[2304], &[2303, 2305]);
}

#[test]
fn ranges_include_both_buckets() {
    sizes(
        "size:1mb..2mb",
        &[MB, MB + 1, 2 * MB, 3 * MB - 1],
        &[MB - 1, 3 * MB, 0],
    );
    sizes("size:1kb..1kb", &[KB, 2 * KB - 1], &[KB - 1, 2 * KB]);
    sizes("size:100..200", &[100, 150, 200], &[99, 201]);
    sizes("size:0..10mb", &[0, 5 * MB, 11 * MB - 1], &[11 * MB, GB]);
    sizes(
        "size:500kb..1mb",
        &[500 * KB, MB, 2 * MB - 1],
        &[500 * KB - 1, 2 * MB],
    );
    sizes("size:1KB..2KB", &[KB, 3 * KB - 1], &[KB - 1, 3 * KB]);
}

// ---------------------------------------------------------------------
// size: constants
// ---------------------------------------------------------------------

#[test]
fn size_constants_cover_their_bands() {
    sizes("size:empty", &[0], &[1, KB, MB]);
    sizes("size:tiny", &[1, 5000], &[20 * KB, MB]);
    sizes("size:small", &[50 * KB], &[5000, 200 * KB]);
    sizes("size:medium", &[500 * KB], &[50 * KB, 5 * MB]);
    sizes("size:large", &[5 * MB], &[500 * KB, 50 * MB]);
    sizes("size:huge", &[50 * MB], &[5 * MB, 200 * MB]);
    sizes(
        "size:gigantic",
        &[200 * MB, 5 * GB],
        &[50 * MB, 100 * MB, 0],
    );
}

#[test]
fn size_unknown_matches_only_unknown_sizes() {
    assert!(m("size:unknown", &file("a.txt")));
    assert!(!m("size:unknown", &sized(0)));
    assert!(!m("size:unknown", &sized(5 * MB)));
    for q in ["size:empty", "size:tiny", "size:gigantic", "size:>=0"] {
        assert!(!m(q, &file("a.txt")), "{q:?} on unknown size");
    }
}

#[test]
fn size_never_matches_folders() {
    for size in [None, Some(0), Some(5000), Some(5 * MB)] {
        let dir = Candidate {
            size,
            ..folder("Projects")
        };
        for q in [
            "size:0",
            "size:>=0",
            "size:<1tb",
            "size:!=1kb",
            "size:0..1tb",
            "size:empty",
            "size:tiny",
            "size:medium",
            "size:large",
            "size:gigantic",
            "size:5000",
        ] {
            assert!(!m(q, &dir), "{q:?} should not match folder with {size:?}");
        }
    }
    assert!(m("size:unknown", &folder("Projects")));
}

// ---------------------------------------------------------------------
// ext:
// ---------------------------------------------------------------------

#[test]
fn ext_matches_the_last_extension() {
    assert_function("ext:txt", "ext");
    assert!(m("ext:txt", &file("a.txt")));
    assert!(m("ext:txt", &file("A.TXT")));
    assert!(m("ext:TXT", &file("a.txt")));
    assert!(m("ext:gz", &file("archive.tar.gz")));
    assert!(!m("ext:tar", &file("archive.tar.gz")));
    assert!(!m("ext:txt", &file("a.txt.bak")));
    assert!(!m("ext:txt", &file("txt")));
    assert!(!m("ext:txt", &file("a.txta")));
    assert!(!m("ext:tx", &file("a.txt")));
}

#[test]
fn ext_takes_a_semicolon_list() {
    assert_function("ext:txt;md", "ext");
    for name in ["a.txt", "b.md", "C.Md"] {
        assert!(m("ext:txt;md", &file(name)), "{name}");
    }
    for name in ["c.rs", "md", "a.mdx"] {
        assert!(!m("ext:txt;md", &file(name)), "{name}");
    }
    assert!(m("ext:jpg;png;gif", &file("x.png")));
    assert!(!m("ext:jpg;png;gif", &file("x.bmp")));
}

#[test]
fn empty_ext_matches_names_without_extension() {
    assert_function("ext:", "ext");
    assert!(m("ext:", &file("Makefile")));
    assert!(m("ext:", &file("README")));
    assert!(!m("ext:", &file("a.txt")));
    assert!(!m("ext:", &file("archive.tar.gz")));
}

// ---------------------------------------------------------------------
// parent: / infolder:
// ---------------------------------------------------------------------

#[test]
fn parent_matches_direct_children_only() {
    for f in ["parent", "infolder"] {
        let q = format!("{f}:/Users/me");
        let parsed = p(&q);
        assert!(
            parsed.diagnostics.is_empty(),
            "{q}: {:?}",
            parsed.diagnostics
        );
        assert!(
            matches!(parsed.query, Node::Function(_)),
            "{q} should be a function"
        );
        assert!(m(&q, &at("/Users/me/a.txt", false)), "{q}");
        assert!(m(&q, &at("/Users/me/src", true)), "{q}");
        assert!(!m(&q, &at("/Users/me/src/main.rs", false)), "{q}");
        assert!(!m(&q, &at("/Users/meow/a.txt", false)), "{q}");
        assert!(!m(&q, &at("/Users/me", true)), "{q}");
        assert!(!m(&q, &at("/Users/a.txt", false)), "{q}");
    }
}

#[test]
fn parent_root_and_quoted_paths() {
    assert!(m("parent:/", &at("/Volumes", true)));
    assert!(!m("parent:/", &at("/Users/me", true)));
    let q = "parent:\"/Users/me/my docs\"";
    assert!(p(q).diagnostics.is_empty(), "{:?}", p(q).diagnostics);
    assert!(m(q, &at("/Users/me/my docs/x.txt", false)));
    assert!(!m(q, &at("/Users/me/x.txt", false)));
}

// ---------------------------------------------------------------------
// depth: and len:
// ---------------------------------------------------------------------

#[test]
fn depth_counts_path_components() {
    assert_function("depth:3", "depth");
    let vol = at("/Volumes", true);
    let me = at("/Users/me", true);
    let txt = at("/Users/me/a.txt", false);
    let deep = at("/Users/me/src/main.rs", false);
    assert!(m("depth:1", &vol));
    assert!(m("depth:2", &me));
    assert!(m("depth:3", &txt));
    assert!(m("depth:4", &deep));
    assert!(!m("depth:3", &deep));
    assert!(m("depth:>2", &txt) && !m("depth:>2", &me));
    assert!(m("depth:<=1", &vol) && !m("depth:<=1", &me));
    assert!(m("depth:2..3", &me) && m("depth:2..3", &txt));
    assert!(!m("depth:2..3", &vol) && !m("depth:2..3", &deep));
    assert!(m("depth:!=3", &deep) && !m("depth:!=3", &txt));
    assert!(m("depth:>=4", &deep) && m("depth:<4", &txt));
    assert!(m("depth:==3", &txt));
}

#[test]
fn len_counts_name_chars() {
    assert_function("len:5", "len");
    let a = file("a.txt"); // 5 chars
    let accented = file("é.txt"); // 5 chars, 6 bytes
    let long = file("long-name.txt"); // 13 chars
    assert!(m("len:5", &a) && m("len:5", &accented));
    assert!(!m("len:6", &accented));
    assert!(!m("len:5", &long));
    assert!(m("len:<6", &a) && !m("len:<5", &a));
    assert!(m("len:>=5", &a) && m("len:>10", &long) && !m("len:>10", &a));
    assert!(m("len:4..6", &a) && !m("len:4..6", &long));
    assert!(m("len:!=5", &long) && !m("len:!=5", &a));
    assert!(m("len:<=13", &long) && m("len:==13", &long));
    assert!(m("len:3", &folder("src")));
}

// ---------------------------------------------------------------------
// empty:, count:, attrib:
// ---------------------------------------------------------------------

#[test]
fn empty_matches_folders_without_children() {
    assert_function("empty:", "empty");
    let empty_dir = Candidate {
        child_count: Some(0),
        ..folder("src")
    };
    let full_dir = Candidate {
        child_count: Some(3),
        ..folder("src")
    };
    assert!(m("empty:", &empty_dir));
    assert!(!m("empty:", &full_dir));
    assert!(!m("empty:", &folder("src")));
    assert!(!m("empty:", &file("a.txt")));
    assert!(!m("empty:", &sized(0)));
}

#[test]
fn count_sets_the_limit_and_filters_nothing() {
    let parsed = p("count:5");
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    assert_eq!(parsed.count, Some(5));
    for c in [file("a.txt"), folder("src"), sized(MB)] {
        assert!(matches(&parsed.query, &c).unwrap(), "{c:?}");
    }

    let parsed = p("count:10 foo");
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    assert_eq!(parsed.count, Some(10));
    assert!(matches(&parsed.query, &file("foo.txt")).unwrap());
    assert!(!matches(&parsed.query, &file("bar.txt")).unwrap());

    let parsed = p("foo count:3");
    assert_eq!(parsed.count, Some(3));
    assert!(matches(&parsed.query, &file("foo.txt")).unwrap());
    assert!(!matches(&parsed.query, &file("bar.txt")).unwrap());
}

#[test]
fn attrib_letters() {
    assert_function("attrib:H", "attrib");
    let hidden = Candidate {
        hidden: true,
        ..file(".secret")
    };
    let link = Candidate {
        symlink: true,
        ..file("link")
    };
    let locked = Candidate {
        read_only: true,
        ..file("locked.txt")
    };
    let hidden_dir = Candidate {
        hidden: true,
        ..folder(".git")
    };
    let plain = file("a.txt");

    assert!(m("attrib:H", &hidden) && m("attrib:h", &hidden));
    assert!(!m("attrib:H", &plain) && !m("attrib:H", &link));
    assert!(m("attrib:L", &link) && !m("attrib:L", &plain));
    assert!(m("attrib:R", &locked) && !m("attrib:R", &plain));
    assert!(m("attrib:D", &folder("src")) && m("attrib:D", &hidden_dir));
    assert!(!m("attrib:D", &plain) && !m("attrib:D", &hidden));
    // Every listed letter must hold.
    assert!(m("attrib:HD", &hidden_dir));
    assert!(!m("attrib:HD", &hidden) && !m("attrib:HD", &folder("src")));
}

#[test]
fn attrib_letters_without_macos_meaning_never_match() {
    let everything = Candidate {
        hidden: true,
        symlink: true,
        read_only: true,
        ..folder("x")
    };
    for q in ["attrib:A", "attrib:S", "attrib:HA"] {
        let parsed = p(q);
        assert!(
            parsed.diagnostics.is_empty(),
            "{q}: {:?}",
            parsed.diagnostics
        );
        assert!(matches!(parsed.query, Node::Function(_)), "{q}");
        for c in [&everything, &file("a.txt"), &folder("src")] {
            assert!(!m(q, c), "{q} on {c:?}");
        }
    }
}

// ---------------------------------------------------------------------
// Combining functions
// ---------------------------------------------------------------------

#[test]
fn functions_combine_with_operators() {
    let big_txt = Candidate {
        size: Some(5 * MB),
        ..file("notes.txt")
    };
    let small_txt = Candidate {
        size: Some(100),
        ..file("notes.txt")
    };
    let big_md = Candidate {
        size: Some(5 * MB),
        ..file("notes.md")
    };

    assert!(m("ext:txt size:>1mb", &big_txt));
    assert!(!m("ext:txt size:>1mb", &small_txt));
    assert!(!m("ext:txt size:>1mb", &big_md));

    assert!(m("ext:txt|ext:md", &big_md) && m("ext:txt|ext:md", &small_txt));
    assert!(!m("ext:txt|ext:md", &file("a.rs")));

    assert!(m("!ext:txt", &big_md) && !m("!ext:txt", &big_txt));
    assert!(m("!size:>1mb", &small_txt) && !m("!size:>1mb", &big_txt));

    // A space before the closing `>` keeps it apart from the argument.
    let grouped = "<ext:md|size:<1kb > notes";
    assert!(m(grouped, &small_txt));
    assert!(m(grouped, &big_md));
    assert!(!m(grouped, &big_txt));
    assert!(m("<size:>1mb ext:txt>", &big_txt));
    assert!(!m("<size:>1mb ext:txt>", &big_md));

    assert!(m("notes size:large", &big_txt));
    assert!(!m("other size:large", &big_txt));
    assert!(m("depth:3 len:9", &at("/Users/me/notes.txt", false)));
}

// ---------------------------------------------------------------------
// Malformed and partial input
// ---------------------------------------------------------------------

fn check_spans(query: &str, parsed: &Parsed) {
    for d in &parsed.diagnostics {
        assert!(
            d.span.start <= d.span.end && d.span.end <= query.len(),
            "{query:?}: span {:?} out of range",
            d.span
        );
        assert!(
            query.is_char_boundary(d.span.start) && query.is_char_boundary(d.span.end),
            "{query:?}: span {:?} not on char boundaries",
            d.span
        );
    }
}

#[test]
fn malformed_arguments_give_diagnostics() {
    for q in [
        "size:abc",
        "size:>",
        "size:<=",
        "size:1xb",
        "size:1..abc",
        "size:>>1",
        "depth:abc",
        "depth:>",
        "len:x",
        "len:<",
        "count:abc",
        "count:-1",
    ] {
        let parsed = p(q);
        assert!(
            !parsed.diagnostics.is_empty(),
            "{q:?} should give a diagnostic"
        );
        check_spans(q, &parsed);
        for c in [sized(KB), file("a.txt"), folder("src")] {
            let _ = matches(&parsed.query, &c);
        }
    }
}

#[test]
fn malformed_function_does_not_swallow_other_terms() {
    for q in ["foo size:abc", "size:> foo", "foo depth:x", "foo len:<"] {
        let parsed = p(q);
        assert!(!parsed.diagnostics.is_empty(), "{q:?}");
        check_spans(q, &parsed);
        assert_eq!(
            matches(&parsed.query, &sized(KB)).ok(),
            Some(false),
            "{q:?}: `foo` is still required"
        );
    }
}

#[test]
fn partial_input_never_panics() {
    let full =
        "size:>=1.5mb..2gb ext:txt;md parent:/Users/me depth:2..3 len:<10 count:5 attrib:HD empty:";
    let mut end = 0;
    while end <= full.len() {
        let q = &full[..end];
        let parsed = p(q);
        check_spans(q, &parsed);
        for c in [sized(KB), file("a.txt"), folder("src")] {
            let _ = matches(&parsed.query, &c);
        }
        end += 1;
    }
    for q in [
        "size:",
        "size:1",
        "size:1.",
        "size:1.5",
        "size:1k",
        "size:1kb.",
        "size:1kb..",
        "size:..1kb",
        "size:!",
        "size:=",
        "size:==",
        "size:!=",
        "ext:;",
        "ext:txt;",
        "attrib:",
        "depth:",
        "len:",
        "count:",
        "parent:",
        "infolder:\"",
        "size:99999999999999999999tb",
        "size:1e5",
        "len:4294967296",
        "count:99999999999999999999999",
    ] {
        let parsed = p(q);
        check_spans(q, &parsed);
        for c in [sized(KB), file("a.txt"), folder("src")] {
            let _ = matches(&parsed.query, &c);
        }
    }
}
