//! Match highlighting (`fx_query::highlight`).
//!
//! Contract under test:
//!
//! - `Highlighter::new(&parsed)` (or `Highlighter::for_query(input,
//!   QueryOptions)`) prepares a query once; `highlight(folder, name,
//!   is_folder)` returns `Highlights { name, folder }`: the ranges to
//!   highlight in the entry's `name` and in `folder` (the full path of the
//!   folder holding it, "" for a root), in UTF-16 code units of those
//!   original strings (what `NSRange` and `NSAttributedString` use),
//!   sorted, with overlapping and touching ranges merged.
//! - Every occurrence of every positive literal term is highlighted
//!   (terms under `!` are not), found the way the matcher finds it: the
//!   same case, diacritic and apostrophe folding, mapped back to the
//!   original text, so a folded match covers the decomposed accents,
//!   expanded letters and dropped apostrophes of the original. `ww:`,
//!   `start-with:`, `end-with:` and `wfn:` keep only the occurrences they
//!   accept, and `file:` / `folder:` terms only highlight entries of their
//!   kind.
//! - Terms that match paths are found in the full path (`folder`, the
//!   separator, `name`) and split between the two strings.
//! - Regex terms highlight their matches (leftmost-first, non-empty).
//! - Wildcard terms highlight their literal pieces where the pattern
//!   matches; patterns with `?` are skipped. Functions highlight nothing.
//! - `replace_prefix` moves ranges onto a display string whose prefix was
//!   replaced (the home folder shown as `~`).

// The expected values are lists of ranges, often of one range.
#![allow(clippy::single_range_in_vec_init)]

use std::ops::Range;

use fx_query::highlight::{Highlighter, Highlights, replace_prefix};
use fx_query::{QueryOptions, SearchOptions, parse};

fn hl(query: &str) -> Highlighter {
    Highlighter::new(&parse(query, &SearchOptions::default()))
}

/// The name ranges of `query` on file `name` in `/Users/me`.
fn name_ranges(query: &str, name: &str) -> Vec<Range<usize>> {
    hl(query).highlight("/Users/me", name, false).name
}

/// The highlighted parts of `text`, read back through UTF-16.
fn parts(text: &str, ranges: &[Range<usize>]) -> Vec<String> {
    let units: Vec<u16> = text.encode_utf16().collect();
    ranges
        .iter()
        .map(|r| String::from_utf16(&units[r.clone()]).expect("ranges on UTF-16 boundaries"))
        .collect()
}

#[test]
fn every_occurrence_of_a_literal_case_folded() {
    assert_eq!(name_ranges("rep", "Report rep.pdf"), vec![0..3, 7..10]);
    assert_eq!(name_ranges("REP", "report"), vec![0..3]);
    assert_eq!(name_ranges("case:Rep", "rep Rep"), vec![4..7]);
    assert_eq!(name_ranges("zzz", "report"), Vec::<Range<usize>>::new());
}

#[test]
fn multiple_terms_and_merged_overlaps() {
    assert_eq!(
        name_ranges("rep 2024", "Report 2024 rep.pdf"),
        vec![0..3, 7..11, 12..15]
    );
    // Overlapping occurrences of one term, and of two terms, merge.
    assert_eq!(name_ranges("aba", "ababa.txt"), vec![0..5]);
    assert_eq!(name_ranges("abc bcd", "abcd"), vec![0..4]);
    // Touching ranges merge too.
    assert_eq!(name_ranges("ab cd", "abcd"), vec![0..4]);
    // OR branches: every positive term counts.
    assert_eq!(name_ranges("x|rep", "rep x"), vec![0..3, 4..5]);
}

#[test]
fn negated_terms_and_functions_highlight_nothing() {
    assert_eq!(name_ranges("rep !x", "rep x"), vec![0..3]);
    assert_eq!(name_ranges("!rep", "x"), Vec::<Range<usize>>::new());
    assert_eq!(name_ranges("!<rep|x>", "y"), Vec::<Range<usize>>::new());
    assert!(hl("ext:pdf size:>1mb").is_empty());
    assert!(hl("").is_empty());
    assert!(!hl("ext:pdf rep").is_empty());
    assert_eq!(name_ranges("ext:pdf rep", "rep.pdf"), vec![0..3]);
}

#[test]
fn accents_map_back_to_the_original_text() {
    // NFC name: é is one UTF-16 unit.
    let name = "Résumé.pdf";
    let r = name_ranges("resume", name);
    assert_eq!(parts(name, &r), ["Résumé"]);
    assert_eq!(r, vec![0..6]);
    // NFD name (as APFS may store it): the marks are inside the range.
    let nfd = "Re\u{301}sume\u{301}.pdf";
    let r = name_ranges("résumé", nfd);
    assert_eq!(r, vec![0..8]);
    assert_eq!(parts(nfd, &r), ["Re\u{301}sume\u{301}"]);
    // Part of a word ending in a decomposed letter keeps its mark.
    assert_eq!(parts(nfd, &name_ranges("sume", nfd)), ["sume\u{301}"]);
    // An accent typed in the query.
    assert_eq!(
        parts("café au lait", &name_ranges("CAFÉ", "café au lait")),
        ["café"]
    );
    // `diacritics:` matches the exact text only.
    assert_eq!(
        name_ranges("diacritics:resume", name),
        Vec::<Range<usize>>::new()
    );
    assert_eq!(
        parts(nfd, &name_ranges("diacritics:résumé", nfd)),
        ["Re\u{301}sume\u{301}"]
    );
}

#[test]
fn utf16_offsets_count_surrogate_pairs_and_expansions() {
    // 📄 is two UTF-16 units.
    assert_eq!(name_ranges("x", "📄x.doc"), vec![2..3]);
    assert_eq!(name_ranges("x", "📄x.txt"), vec![2..3, 5..6]);
    assert_eq!(
        parts("📄 notes", &name_ranges("notes", "📄 notes")),
        ["notes"]
    );
    // İ folds to i (one unit in the original).
    assert_eq!(name_ranges("istanbul", "İstanbul"), vec![0..8]);
    assert_eq!(
        name_ranges("case:i\u{307}", "İ"),
        Vec::<Range<usize>>::new()
    );
    assert_eq!(name_ranges("i\u{307}", "İx"), vec![0..1]);
    // CJK and Hangul.
    assert_eq!(name_ranges("本", "日本語.txt"), vec![1..2]);
    assert_eq!(name_ranges("글", "한글.txt"), vec![1..2]);
    let jamo = "\u{1112}\u{1161}\u{11ab}\u{1100}\u{1173}\u{11af}.txt";
    assert_eq!(
        parts(jamo, &name_ranges("한글", jamo)),
        ["\u{1112}\u{1161}\u{11ab}\u{1100}\u{1173}\u{11af}"]
    );
}

#[test]
fn apostrophes_follow_the_apostrophe_rule() {
    let curly = "Plumb’s Handbook.pdf";
    // Without an apostrophe the term spans the name's apostrophe.
    assert_eq!(parts(curly, &name_ranges("plumbs", curly)), ["Plumb’s"]);
    // With one it needs one in that place, of any kind.
    assert_eq!(parts(curly, &name_ranges("plumb's", curly)), ["Plumb’s"]);
    assert_eq!(
        name_ranges("plumb's", "plumbs.pdf"),
        Vec::<Range<usize>>::new()
    );
    assert_eq!(name_ranges("'", "it’s ‘x’"), vec![2..3, 5..6, 7..8]);
    assert_eq!(parts("it's", &name_ranges("its", "it's")), ["it's"]);
    // Whole words read the apostrophe-free name for such a term.
    assert_eq!(name_ranges("ww:plumb", curly), Vec::<Range<usize>>::new());
    assert_eq!(parts(curly, &name_ranges("ww:plumbs", curly)), ["Plumb’s"]);
}

#[test]
fn constrained_literals_keep_only_accepted_occurrences() {
    assert_eq!(name_ranges("ww:rep", "rep report rep"), vec![0..3, 11..14]);
    assert_eq!(name_ranges("start-with:rep", "rep report rep"), vec![0..3]);
    assert_eq!(name_ranges("end-with:rep", "rep report rep"), vec![11..14]);
    assert_eq!(
        name_ranges("wfn:rep", "rep report rep"),
        Vec::<Range<usize>>::new()
    );
    assert_eq!(name_ranges("wfn:notes.txt", "Notes.TXT"), vec![0..9]);
}

#[test]
fn kind_filters_follow_the_entry() {
    let h = hl("file:rep folder:docs");
    assert_eq!(h.highlight("/a", "rep docs", false).name, vec![0..3]);
    assert_eq!(h.highlight("/a", "rep docs", true).name, vec![4..8]);
}

#[test]
fn regex_terms_highlight_their_matches() {
    assert_eq!(
        name_ranges(r"regex:\d+", "img_12_345.jpg"),
        vec![4..6, 7..10]
    );
    assert_eq!(name_ranges("regex:^IMG", "img_IMG"), vec![0..3]);
    assert_eq!(
        name_ranges("case:regex:^IMG", "img_IMG"),
        Vec::<Range<usize>>::new()
    );
    // On an NFD name the expression sees NFC text; ranges cover the marks.
    let nfd = "Re\u{301}sume\u{301}";
    assert_eq!(
        parts(nfd, &name_ranges("regex:é", nfd)),
        ["e\u{301}", "e\u{301}"]
    );
    // An expression that matches empty text highlights nothing.
    assert_eq!(name_ranges("regex:x*", "abc"), Vec::<Range<usize>>::new());
    // An invalid expression is skipped.
    assert!(hl("regex:(").is_empty());
}

#[test]
fn the_whole_query_as_one_regex() {
    let regex = QueryOptions {
        regex: true,
        ..QueryOptions::default()
    };
    let h = Highlighter::for_query("a b", regex);
    assert_eq!(h.highlight("/", "a b c a b", false).name, vec![0..3, 6..9]);
    let h = Highlighter::for_query("^main|^notes", regex);
    assert_eq!(h.highlight("/", "notes.txt", false).name, vec![0..5]);
    let plain = Highlighter::for_query("a b", QueryOptions::default());
    assert_eq!(plain.highlight("/", "a b", false).name, vec![0..1, 2..3]);
}

#[test]
fn path_terms_split_between_folder_and_name() {
    let h = hl("docs/rep");
    let got = h.highlight("/Users/me/Docs", "report.pdf", false);
    assert_eq!(
        got,
        Highlights {
            name: vec![0..3],
            folder: vec![10..14],
        }
    );
    // The Path option makes every term a path term.
    let path = QueryOptions {
        match_path: true,
        ..QueryOptions::default()
    };
    let h = Highlighter::for_query("me", path);
    let got = h.highlight("/Users/me/Docs", "Memo.txt", false);
    assert_eq!(got.folder, vec![7..9]);
    assert_eq!(got.name, vec![0..2]);
    // Name terms stay in the name.
    let got = hl("me").highlight("/Users/me/Docs", "Memo.txt", false);
    assert_eq!(got.folder, Vec::<Range<usize>>::new());
    assert_eq!(got.name, vec![0..2]);
    // Under the root `/` and for a root itself.
    let got = hl("/us").highlight("/", "Users", true);
    assert_eq!((got.folder, got.name), (vec![0..1], vec![0..2]));
    let got = hl("path:vol").highlight("", "/Volumes/Ext", true);
    assert_eq!((got.folder, got.name), (vec![], vec![1..4]));
    // Accents in the folder, NFD.
    let got = hl("zoe/").highlight("/Users/Zoe\u{308}", "notes.txt", false);
    assert_eq!(parts("/Users/Zoe\u{308}", &got.folder), ["Zoe\u{308}"]);
    // A path regex.
    let got = hl("path:regex:docs/r").highlight("/a/docs", "readme", false);
    assert_eq!((got.folder, got.name), (vec![3..7], vec![0..1]));
}

#[test]
fn wildcards_highlight_their_literal_pieces() {
    assert_eq!(name_ranges("*.pdf", "report.pdf"), vec![6..10]);
    assert_eq!(name_ranges("rep*", "report.pdf"), vec![0..3]);
    assert_eq!(name_ranges("re*rt*", "report.pdf"), vec![0..2, 4..6]);
    assert_eq!(name_ranges("*por*", "report.pdf"), vec![2..5]);
    // No match, no highlight; `?` is skipped.
    assert_eq!(
        name_ranges("*.txt", "report.pdf"),
        Vec::<Range<usize>>::new()
    );
    assert_eq!(
        name_ranges("r?port*", "report.pdf"),
        Vec::<Range<usize>>::new()
    );
    // Folding applies, accents and all.
    assert_eq!(
        parts("Résumé.PDF", &name_ranges("res*.pdf", "Résumé.PDF")),
        ["Rés", ".PDF"]
    );
    // Path wildcards.
    let got = hl("**/docs/*").highlight("/a/docs", "x", false);
    assert_eq!(got.folder, vec![2..7]);
}

#[test]
fn replace_prefix_moves_ranges_onto_the_displayed_path() {
    // "/Users/me/Docs" shown as "~/Docs": 9 units become 1. A range
    // inside the prefix highlights the replacement, one after it moves,
    // one across it covers both.
    assert_eq!(replace_prefix(&[0..2], 9, 1), vec![0..1]);
    assert_eq!(replace_prefix(&[7..9], 9, 1), vec![0..1]);
    assert_eq!(replace_prefix(&[10..14], 9, 1), vec![2..6]);
    assert_eq!(replace_prefix(&[9..10], 9, 1), vec![1..2]);
    assert_eq!(replace_prefix(&[5..12], 9, 1), vec![0..4]);
    // The result stays sorted and merged.
    assert_eq!(
        replace_prefix(&[0..2, 7..9, 10..14], 9, 1),
        vec![0..1, 2..6]
    );
    assert_eq!(replace_prefix(&[0..2, 9..10], 9, 1), vec![0..2]);
    assert_eq!(replace_prefix(&[3..5], 0, 1), vec![4..6]);
    assert_eq!(replace_prefix(&[], 9, 1), Vec::<Range<usize>>::new());
}

/// For single literal terms, a name is highlighted exactly when the
/// reference evaluator matches it, and each highlighted part, folded the
/// term's way, contains the folded term.
#[test]
fn literal_highlights_agree_with_the_evaluator() {
    use fx_query::eval::{Candidate, matches};
    use fx_query::fold::{FoldOptions, fold_str};

    const NAMES: &[&str] = &[
        "Résumé.pdf",
        "Re\u{301}sume\u{301} final.PDF",
        "Plumb’s Handbook.pdf",
        "plumbs.pdf",
        "it's ‘quoted’",
        "İstanbul İzmir",
        "ΣΊΣΥΦΟΣ σίσυφος",
        "Straße STRASSE",
        "📄 notes 📄.md",
        "\u{1112}\u{1161}\u{11ab} 한글",
        "Café’s café CAFE",
        "a'b'c",
    ];
    const QUERIES: &[&str] = &[
        "resume",
        "résumé",
        "sume",
        "plumbs",
        "plumb's",
        "'",
        "its",
        "it's",
        "istanbul",
        "i",
        "σίσυφος",
        "σ",
        "straße",
        "ss",
        "notes",
        "📄",
        "한",
        "한글",
        "cafe",
        "café's",
        "cafes",
        "ab",
        "a'b",
        "b'c",
        "ww:cafe",
        "end-with:md",
        "case:CAFE",
        "diacritics:cafe",
        "diacritics:résumé",
    ];
    for &query in QUERIES {
        let parsed = parse(query, &SearchOptions::default());
        let h = Highlighter::new(&parsed);
        let fx_query::Node::Term(term) = &parsed.query else {
            panic!("{query:?} is one term");
        };
        let o = term.options;
        let fold = FoldOptions::for_term(&term.text, o.match_case, o.match_diacritics);
        let needle = fold_str(&term.text, fold).into_owned();
        for &name in NAMES {
            let matched = matches(&parsed.query, &Candidate::file(name, "/x")).unwrap();
            let got = h.highlight("/x", name, false);
            assert_eq!(
                !got.name.is_empty(),
                matched,
                "{query:?} on {name:?}: {got:?}"
            );
            for part in parts(name, &got.name) {
                assert!(
                    fold_str(&part, fold).contains(needle.as_str()),
                    "{query:?} on {name:?}: part {part:?}"
                );
            }
        }
    }
}
