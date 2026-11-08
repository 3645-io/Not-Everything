//! The type macros `audio:`, `doc:`, `exe:`, `pic:`,
//! `video:` and `zip:`.
//!
//! Contract pinned here (our own reading of voidtools' public Searching
//! page, plus common macOS file types):
//!
//! - `fx_query::syntax::macros::macro_extensions(name)` returns the
//!   extension list a macro expands to (lowercase, no leading `.`, no
//!   duplicates), case-insensitively by name, or `None` for any other name.
//! - A bare macro (`pic:`) parses to exactly `Node::Function(Filter::Ext)`
//!   holding that list, with a clean parse.
//! - A macro with an argument (`pic:holiday`) parses to an `And` of that
//!   filter and the argument term.
//!
//! Only a core of each list is required, so the lists can grow.

use fx_query::eval::{Candidate, EvalContext, matches_in};
use fx_query::syntax::macros::macro_extensions;
use fx_query::{Filter, Node, SearchOptions, parse};

const MACROS: [&str; 6] = ["audio", "doc", "exe", "pic", "video", "zip"];

fn list(name: &str) -> &'static [&'static str] {
    macro_extensions(name).unwrap_or_else(|| panic!("{name}: is not a macro"))
}

fn ext_filter(node: &Node) -> Option<&Vec<String>> {
    match node {
        Node::Function(Filter::Ext(exts)) => Some(exts),
        _ => None,
    }
}

fn file(name: &str) -> Candidate<'_> {
    Candidate {
        name,
        parent_path: "/Users/me",
        is_dir: false,
        ..Default::default()
    }
}

fn hits(query: &str, name: &str) -> bool {
    let parsed = parse(query, &SearchOptions::default());
    matches_in(&parsed.query, &file(name), &EvalContext::default())
        .unwrap_or_else(|e| panic!("{query:?} on {name:?}: {e}"))
}

#[test]
fn every_macro_has_a_well_formed_list() {
    for name in MACROS {
        let exts = list(name);
        assert!(!exts.is_empty(), "{name}: empty list");
        for (i, ext) in exts.iter().enumerate() {
            assert!(!ext.is_empty(), "{name}: empty extension");
            assert!(!ext.starts_with('.'), "{name}: {ext:?} has a leading dot");
            assert_eq!(*ext, ext.to_lowercase(), "{name}: {ext:?} not lowercase");
            assert!(
                !ext.contains([';', ' ', '/', ':']),
                "{name}: {ext:?} has a separator"
            );
            assert!(!exts[..i].contains(ext), "{name}: {ext:?} listed twice");
        }
    }
}

#[test]
fn macro_lists_hold_the_core_types() {
    let core: [(&str, &[&str]); 6] = [
        ("audio", &["mp3", "wav", "flac", "aac", "m4a", "ogg"]),
        (
            "doc",
            &[
                "doc", "docx", "pdf", "txt", "rtf", "xls", "xlsx", "ppt", "pptx", "pages",
                "numbers", "key",
            ],
        ),
        ("exe", &["exe", "bat", "msi", "app", "pkg"]),
        (
            "pic",
            &[
                "jpg", "jpeg", "png", "gif", "bmp", "tif", "tiff", "webp", "heic",
            ],
        ),
        ("video", &["mp4", "mkv", "avi", "mov", "wmv", "m4v"]),
        ("zip", &["zip", "rar", "7z", "tar", "gz"]),
    ];
    for (name, wanted) in core {
        let exts = list(name);
        for ext in wanted {
            assert!(exts.contains(ext), "{name}: is missing {ext:?}: {exts:?}");
        }
    }
    assert!(
        MACROS.iter().any(|m| list(m).contains(&"dmg")),
        "dmg belongs to one of the macros"
    );
}

#[test]
fn macro_lookup_is_case_insensitive_and_closed() {
    assert_eq!(macro_extensions("PIC"), macro_extensions("pic"));
    assert_eq!(macro_extensions("Video"), macro_extensions("video"));
    for other in ["", "pics", "size", "ext", "music", "image", "case"] {
        assert_eq!(macro_extensions(other), None, "{other:?} is not a macro");
    }
}

#[test]
fn bare_macro_is_exactly_its_ext_filter() {
    for name in MACROS {
        for typed in [format!("{name}:"), format!("{}:", name.to_uppercase())] {
            let parsed = parse(&typed, &SearchOptions::default());
            assert!(
                parsed.diagnostics.is_empty(),
                "{typed}: {:?}",
                parsed.diagnostics
            );
            let exts = ext_filter(&parsed.query)
                .unwrap_or_else(|| panic!("{typed}: not an ext filter: {:?}", parsed.query));
            let want: Vec<String> = list(name).iter().map(|e| e.to_string()).collect();
            assert_eq!(exts, &want, "{typed}");
        }
    }
}

#[test]
fn macro_with_argument_is_filter_and_term() {
    let parsed = parse("pic:holiday", &SearchOptions::default());
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let Node::And(children) = &parsed.query else {
        panic!("expected an And, got {:?}", parsed.query);
    };
    assert_eq!(children.len(), 2, "{children:?}");
    let filters: Vec<_> = children.iter().filter_map(ext_filter).collect();
    assert_eq!(filters.len(), 1, "{children:?}");
    let want: Vec<String> = list("pic").iter().map(|e| e.to_string()).collect();
    assert_eq!(filters[0], &want);
    assert!(
        children
            .iter()
            .any(|c| matches!(c, Node::Term(t) if t.text == "holiday")),
        "{children:?}"
    );
}

#[test]
fn every_macro_matches_its_own_extensions_only() {
    for name in MACROS {
        let query = format!("{name}:");
        for ext in list(name) {
            assert!(hits(&query, &format!("file.{ext}")), "{query} file.{ext}");
            assert!(
                hits(&query, &format!("FILE.{}", ext.to_uppercase())),
                "{query} FILE.{ext} (upper case)"
            );
        }
        assert!(!hits(&query, "Makefile"), "{query} Makefile");
        assert!(!hits(&query, "notes.zzzq"), "{query} notes.zzzq");
    }
}

#[test]
fn quoted_macro_is_literal_text() {
    let parsed = parse("\"pic:\"", &SearchOptions::default());
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    assert!(
        matches!(&parsed.query, Node::Term(t) if t.text == "pic:"),
        "{:?}",
        parsed.query
    );
}
