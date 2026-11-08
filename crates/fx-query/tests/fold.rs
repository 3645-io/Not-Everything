//! Unicode case and diacritic folding, the folded search copy of a name
//! column, and folding in the reference evaluator.
//!
//! Contract under test (from docs/PLAN.md):
//!
//! - `fx_query::fold::fold_str(s, FoldOptions { case, diacritics })`
//!   returns a `Cow<str>`. `case: true` folds case with std's per-`char`
//!   Unicode lowercase mapping (no expansion: `ß` stays `ß`; no Turkish
//!   special-casing beyond what std does; no Greek final-sigma rule).
//!   `diacritics: true` strips accents: NFD-decompose, drop combining marks,
//!   recompose nothing. Whatever the flags, the output is normalized so that
//!   an NFD name (as APFS may store it) and the NFC spelling of the same
//!   text fold to the same string; with `diacritics: false` that form is NFC.
//! - The apostrophe rule: with `diacritics: true`
//!   the apostrophes `'`, `’`, `‘` and `ʼ` are written as `'` when
//!   `keep_apostrophes` is set and dropped when it is not.
//!   `FoldOptions::for_match` (the folding of names, and of the in-memory
//!   folded copy) keeps them; `FoldOptions::for_term(text, match_case,
//!   match_diacritics)` keeps them only for a text with an apostrophe, so a
//!   term without one is compared with the name's apostrophes removed.
//!   `FoldedColumn` keeps that apostrophe-free form in a side table, for
//!   the names that have an apostrophe only.
//! - An ASCII input that folding leaves unchanged comes back as
//!   `Cow::Borrowed` (the ASCII fast path).
//! - `fx_query::fold::FoldedColumn::build(&NameColumn, FoldOptions)` makes a
//!   folded copy of a column: its own NUL-separated buffer (`buf()`) and
//!   `u32` start offsets (`offsets()`), laid out exactly like a valid
//!   `NameColumn`, where entry `i` is `fold_str(name i)`. All-ASCII columns
//!   are a byte copy (lowercased when folding case).
//! - The `eval` oracle folds both sides: Unicode lowercase when
//!   `match_case` is off, accent stripping when `match_diacritics` is off,
//!   and NFD/NFC-insensitive comparison always.

use std::borrow::Cow;

use fx_query::eval::{Candidate, matches};
use fx_query::fold::{FoldOptions, FoldedColumn, fold_str};
use fx_query::{NameColumn, SearchOptions, parse};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const NONE: FoldOptions = FoldOptions {
    case: false,
    diacritics: false,
    keep_apostrophes: false,
};
const CASE: FoldOptions = FoldOptions { case: true, ..NONE };
const DIACRITICS: FoldOptions = FoldOptions {
    diacritics: true,
    ..NONE
};
const BOTH: FoldOptions = FoldOptions {
    case: true,
    diacritics: true,
    ..NONE
};
/// What a term made only of apostrophes folds with by default.
const KEEP: FoldOptions = FoldOptions {
    keep_apostrophes: true,
    ..BOTH
};
const ALL_OPTS: [FoldOptions; 4] = [NONE, CASE, DIACRITICS, BOTH];

fn opts_name(o: FoldOptions) -> &'static str {
    match (o.case, o.diacritics, o.keep_apostrophes) {
        (false, false, _) => "none",
        (true, false, _) => "case",
        (false, true, false) => "diacritics",
        (true, true, false) => "both",
        (false, true, true) => "diacritics, keep apostrophes",
        (true, true, true) => "both, keep apostrophes",
    }
}

#[track_caller]
fn fold(s: &str, o: FoldOptions) -> String {
    fold_str(s, o).into_owned()
}

/// A NUL-separated buffer and start offsets for `names`.
fn column_parts(names: &[&str]) -> (Vec<u8>, Vec<u32>) {
    let mut buf = Vec::new();
    let mut offsets = Vec::with_capacity(names.len());
    for n in names {
        offsets.push(buf.len() as u32);
        buf.extend_from_slice(n.as_bytes());
        buf.push(0);
    }
    (buf, offsets)
}

/// Checks that `folded` is a valid column whose entry `i` is
/// `fold_str(names[i], opts)`.
#[track_caller]
fn assert_folded_matches(folded: &FoldedColumn, names: &[&str], opts: FoldOptions) {
    assert_eq!(folded.len(), names.len(), "entry count");
    assert_eq!(folded.is_empty(), names.is_empty());
    assert_eq!(folded.offsets().len(), names.len(), "one offset per entry");
    let column = NameColumn::new(folded.buf(), folded.offsets())
        .expect("folded buffer and offsets must form a valid NameColumn");
    assert_eq!(column.len(), names.len());
    let mut expected_len = 0usize;
    for (i, name) in names.iter().enumerate() {
        let want = fold_str(name, opts);
        assert_eq!(
            folded.name_bytes(i),
            want.as_bytes(),
            "entry {i} ({name:?}) under {}",
            opts_name(opts)
        );
        assert_eq!(column.name_bytes(i), want.as_bytes(), "entry {i} via buf()");
        assert_eq!(
            folded.offsets()[i] as usize,
            expected_len,
            "offset of entry {i}"
        );
        expected_len += want.len() + 1;
    }
    assert_eq!(folded.buf().len(), expected_len, "buffer length");
}

#[track_caller]
fn eval_with(query: &str, opts: &SearchOptions, name: &str, parent: &str) -> bool {
    let parsed = parse(query, opts);
    matches(&parsed.query, &Candidate::file(name, parent))
        .unwrap_or_else(|e| panic!("{query:?} should be supported: {e:?}"))
}

#[track_caller]
fn eval(query: &str, name: &str) -> bool {
    eval_with(query, &SearchOptions::default(), name, "/Users/me")
}

// ---------------------------------------------------------------------------
// fold_str: ASCII fast path
// ---------------------------------------------------------------------------

#[test]
fn ascii_unchanged_is_borrowed_under_every_option() {
    for o in ALL_OPTS {
        for s in ["readme.txt", "", "a-b_c 1.2", "~/x/y"] {
            match fold_str(s, o) {
                Cow::Borrowed(b) => {
                    assert_eq!(b, s);
                    assert!(std::ptr::eq(b, s), "{s:?} should borrow the input");
                }
                Cow::Owned(_) => panic!("{s:?} under {} should be Borrowed", opts_name(o)),
            }
        }
    }
}

#[test]
fn ascii_uppercase_is_borrowed_when_case_is_kept() {
    for o in [NONE, DIACRITICS] {
        let s = "README.TXT";
        assert!(
            matches!(fold_str(s, o), Cow::Borrowed(b) if b == s),
            "{} should borrow uppercase ASCII",
            opts_name(o)
        );
    }
}

#[test]
fn ascii_case_folding_lowercases() {
    for o in [CASE, BOTH] {
        assert_eq!(fold("README.TXT", o), "readme.txt");
        assert_eq!(fold("MiXeD Case 42", o), "mixed case 42");
    }
}

// ---------------------------------------------------------------------------
// fold_str: diacritics
// ---------------------------------------------------------------------------

#[test]
fn e_acute_folds_to_e() {
    assert_eq!(fold("é", DIACRITICS), "e");
    assert_eq!(fold("é", BOTH), "e");
    assert_eq!(fold("É", DIACRITICS), "E");
    assert_eq!(fold("É", BOTH), "e");
    assert_eq!(fold("É", CASE), "é");
    assert_eq!(fold("é", NONE), "é");
    assert_eq!(fold("Résumé.pdf", BOTH), "resume.pdf");
    assert_eq!(fold("Résumé.pdf", DIACRITICS), "Resume.pdf");
    assert_eq!(fold("Résumé.pdf", CASE), "résumé.pdf");
}

#[test]
fn a_ring_folds_to_a() {
    assert_eq!(fold("Å", BOTH), "a");
    assert_eq!(fold("Å", DIACRITICS), "A");
    assert_eq!(fold("å", DIACRITICS), "a");
    assert_eq!(fold("Å", CASE), "å");
    assert_eq!(fold("ÅNGSTRÖM", BOTH), "angstrom");
    assert_eq!(fold("Ærø", BOTH), "ærø", "Æ and ø have no decomposition");
}

#[test]
fn many_latin_accents_strip() {
    assert_eq!(fold("àáâãäā", DIACRITICS), "aaaaaa");
    assert_eq!(fold("Çà va? Naïve façade", BOTH), "ca va? naive facade");
    assert_eq!(fold("Ñandú", BOTH), "nandu");
    assert_eq!(fold("Dvořák", BOTH), "dvorak");
    assert_eq!(fold("Œuvre", BOTH), "œuvre", "ligatures do not expand");
}

#[test]
fn lone_combining_marks_are_dropped() {
    assert_eq!(fold("\u{301}", DIACRITICS), "");
    assert_eq!(fold("a\u{301}\u{308}b", DIACRITICS), "ab");
    assert_eq!(
        fold("x\u{301}", NONE),
        "x\u{301}",
        "nothing composes with x"
    );
}

// ---------------------------------------------------------------------------
// fold_str: case folding edge cases
// ---------------------------------------------------------------------------

#[test]
fn sharp_s_lowercases_without_expansion() {
    assert_eq!(fold("ß", CASE), "ß");
    assert_eq!(fold("ß", BOTH), "ß");
    assert_eq!(fold("ẞ", CASE), "ß", "capital sharp s lowercases to ß");
    assert_eq!(fold("Straße", BOTH), "straße");
    assert_ne!(fold("Straße", BOTH), fold("STRASSE", BOTH));
    assert_eq!(fold("ß", NONE), "ß");
}

#[test]
fn turkish_i_follows_std_lowercase() {
    // std maps İ (U+0130) to "i\u{307}" and leaves dotless ı alone.
    let std_lower: String = 'İ'.to_lowercase().collect();
    assert_eq!(fold("İ", CASE), std_lower);
    assert_eq!(fold("İ", CASE), "i\u{307}");
    assert_eq!(fold("İ", BOTH), "i", "the dot above is a combining mark");
    assert_eq!(fold("İ", DIACRITICS), "I");
    assert_eq!(fold("ı", CASE), "ı");
    assert_eq!(fold("ı", BOTH), "ı", "dotless i is a base letter, not i");
    assert_eq!(fold("I", CASE), "i", "ASCII I is never mapped to ı");
    assert_eq!(fold("İstanbul", BOTH), "istanbul");
}

#[test]
fn greek_case_and_accents() {
    assert_eq!(fold("ΑΒΓΔ", CASE), "αβγδ");
    assert_eq!(fold("ΟΔΟΣ", CASE), "οδοσ", "no final-sigma rule");
    assert_eq!(fold("Σ", CASE), "σ");
    assert_eq!(fold("Αθήνα", CASE), "αθήνα");
    assert_eq!(fold("Αθήνα", BOTH), "αθηνα");
    assert_eq!(fold("Ά", BOTH), "α");
    assert_eq!(fold("ΑΒΓΔ", NONE), "ΑΒΓΔ");
}

#[test]
fn cyrillic_case_and_accents() {
    assert_eq!(fold("ПРИВЕТ", CASE), "привет");
    assert_eq!(fold("Москва", CASE), "москва");
    assert_eq!(fold("ПРИВЕТ", NONE), "ПРИВЕТ");
    assert_eq!(fold("Ё", CASE), "ё");
    assert_eq!(fold("Ё", BOTH), "е", "Ё decomposes to Е + diaeresis");
}

#[test]
fn scripts_without_case_pass_through() {
    for o in ALL_OPTS {
        assert_eq!(fold("日本語.txt", o), "日本語.txt", "{}", opts_name(o));
        assert_eq!(fold("📄 notes", o), "📄 notes", "{}", opts_name(o));
    }
}

// ---------------------------------------------------------------------------
// fold_str: NFD versus NFC
// ---------------------------------------------------------------------------

#[test]
fn nfd_and_nfc_fold_equal_under_every_option() {
    let pairs = [
        ("Résumé.pdf", "Re\u{301}sume\u{301}.pdf"),
        ("ÅNGSTRÖM", "A\u{30a}NGSTRO\u{308}M"),
        ("Ñandú", "N\u{303}andu\u{301}"),
        ("Αθήνα", "Αθη\u{301}να"),
        ("Ёлка", "Е\u{308}лка"),
        ("한글", "\u{1112}\u{1161}\u{11ab}\u{1100}\u{1173}\u{11af}"),
    ];
    for (nfc, nfd) in pairs {
        for o in ALL_OPTS {
            assert_eq!(
                fold(nfc, o),
                fold(nfd, o),
                "{nfc:?} vs its NFD form under {}",
                opts_name(o)
            );
        }
    }
}

#[test]
fn diacritics_kept_output_is_nfc() {
    assert_eq!(fold("Re\u{301}sume\u{301}.pdf", NONE), "Résumé.pdf");
    assert_eq!(fold("RE\u{301}SUME\u{301}.PDF", CASE), "résumé.pdf");
    assert_eq!(fold("A\u{30a}", NONE), "Å");
    assert_eq!(fold("A\u{30a}", CASE), "å");
}

#[test]
fn folding_is_idempotent() {
    let samples = [
        "Résumé.pdf",
        "Re\u{301}sume\u{301}.pdf",
        "ÅNGSTRÖM",
        "Straße",
        "ΟΔΟΣ Αθήνα",
        "ПРИВЕТ Ёлка",
        "İstanbul",
        "日本語",
        "README.TXT",
        "",
    ];
    for s in samples {
        for o in ALL_OPTS {
            let once = fold(s, o);
            assert_eq!(fold(&once, o), once, "{s:?} under {}", opts_name(o));
        }
    }
}

// ---------------------------------------------------------------------------
// fold_str: apostrophes (dropped without keep_apostrophes)
// ---------------------------------------------------------------------------

/// The apostrophes folding drops: ASCII `'`, `’` (what macOS types), `‘`
/// and the modifier letter `ʼ`.
const APOSTROPHES: [char; 4] = ['\'', '\u{2019}', '\u{2018}', '\u{2bc}'];

#[test]
fn every_apostrophe_is_dropped_when_folding_diacritics() {
    for a in APOSTROPHES {
        let name = format!("Plumb{a}s");
        assert_eq!(fold(&name, DIACRITICS), "Plumbs", "{:04x}", a as u32);
        assert_eq!(fold(&name, BOTH), "plumbs", "{:04x}", a as u32);
        // Matching diacritics keeps the exact text, apostrophe included.
        assert_eq!(fold(&name, NONE), name, "{:04x}", a as u32);
        assert_eq!(fold(&name, CASE), name.to_lowercase(), "{:04x}", a as u32);
    }
}

#[test]
fn ascii_apostrophes_take_the_fast_path() {
    assert_eq!(fold("plumb's", BOTH), "plumbs");
    assert_eq!(fold("PLUMB'S", BOTH), "plumbs");
    assert_eq!(fold("PLUMB'S", DIACRITICS), "PLUMBS");
    assert_eq!(fold("''", BOTH), "");
    assert_eq!(fold("'tis", BOTH), "tis");
    assert_eq!(fold("rock 'n' roll.mp3", BOTH), "rock n roll.mp3");
    // Nothing to drop or lowercase: the input is borrowed.
    for o in ALL_OPTS {
        let s = "plumbs";
        assert!(
            matches!(fold_str(s, o), Cow::Borrowed(b) if std::ptr::eq(b, s)),
            "{}",
            opts_name(o)
        );
    }
    // Apostrophes kept when matching diacritics, so still borrowed.
    for o in [NONE, CASE] {
        let s = "plumb's";
        assert!(
            matches!(fold_str(s, o), Cow::Borrowed(b) if std::ptr::eq(b, s)),
            "{}",
            opts_name(o)
        );
    }
    assert!(matches!(fold_str("plumb's", BOTH), Cow::Owned(_)));
    assert!(matches!(fold_str("plumb's", DIACRITICS), Cow::Owned(_)));
}

#[test]
fn non_ascii_names_drop_apostrophes_with_their_accents() {
    assert_eq!(
        fold("Plumb’s Veterinary Drug Handbook 10th Edition.pdf", BOTH),
        "plumbs veterinary drug handbook 10th edition.pdf"
    );
    assert_eq!(fold("Café’s menu", BOTH), "cafes menu");
    assert_eq!(fold("Café’s menu", DIACRITICS), "Cafes menu");
    assert_eq!(fold("Café’s menu", CASE), "café’s menu");
    assert_eq!(fold("‘quoted’", BOTH), "quoted");
    assert_eq!(
        fold("Hawaiʻi ʼokina", BOTH),
        "hawaiʻi okina",
        "U+02BB is not dropped"
    );
    assert_eq!(fold("’", BOTH), "");
}

#[test]
fn nfd_names_with_apostrophes_fold_like_nfc() {
    let pairs = [
        ("Café’s menu", "Cafe\u{301}’s menu"),
        ("École d'été", "E\u{301}cole d'e\u{301}te\u{301}"),
        ("Zoë’s", "Zoe\u{308}’s"),
    ];
    for (nfc, nfd) in pairs {
        for o in ALL_OPTS {
            assert_eq!(fold(nfc, o), fold(nfd, o), "{nfc:?} under {}", opts_name(o));
        }
    }
    assert_eq!(fold("E\u{301}cole d'e\u{301}te\u{301}", BOTH), "ecole dete");
    assert_eq!(fold("Zoe\u{308}’s", BOTH), "zoes");
}

#[test]
fn folding_with_apostrophes_is_idempotent() {
    let samples = [
        "Plumb’s",
        "plumb's",
        "‘x’",
        "''",
        "Cafe\u{301}’s",
        // Dropping the apostrophe lets these Hangul jamo join; folding
        // again must not change the result.
        "\u{1112}'\u{1161}",
    ];
    for s in samples {
        for o in ALL_OPTS {
            let once = fold(s, o);
            assert_eq!(fold(&once, o), once, "{s:?} under {}", opts_name(o));
        }
    }
    assert_eq!(fold("\u{1112}'\u{1161}", BOTH), "\u{d558}");
}

#[test]
fn folded_column_drops_apostrophes() {
    let names = [
        "Plumb’s.pdf",
        "plumb's.pdf",
        "PLAIN.TXT",
        "''",
        "Cafe\u{301}’s",
        "it‘s",
        "rock ʼn roll",
    ];
    let (buf, offsets) = column_parts(&names);
    let column = NameColumn::new(&buf, &offsets).unwrap();
    for o in ALL_OPTS {
        let folded = FoldedColumn::build(&column, o);
        assert_folded_matches(&folded, &names, o);
    }
    let folded = FoldedColumn::build(&column, BOTH);
    let want = [
        "plumbs.pdf",
        "plumbs.pdf",
        "plain.txt",
        "",
        "cafes",
        "its",
        "rock n roll",
    ];
    for (i, w) in want.iter().enumerate() {
        assert_eq!(folded.name_bytes(i), w.as_bytes(), "entry {i}");
    }
    // Matching diacritics keeps an ASCII column byte for byte.
    let ascii = ["it's", "plumb's.pdf", "x"];
    let (buf, offsets) = column_parts(&ascii);
    let column = NameColumn::new(&buf, &offsets).unwrap();
    let kept = FoldedColumn::build(&column, NONE);
    assert_eq!(kept.buf(), &buf[..]);
    assert_eq!(kept.offsets(), &offsets[..]);
    let dropped = FoldedColumn::build(&column, DIACRITICS);
    assert_eq!(dropped.buf(), b"its\0plumbs.pdf\0x\0");
    assert_eq!(dropped.offsets(), &[0, 4, 15]);
}

#[test]
fn for_match_keeps_apostrophes_and_for_term_only_for_a_text_with_one() {
    // Names are folded with their apostrophes kept, each written as `'`.
    assert_eq!(FoldOptions::for_match(false, false), KEEP);
    assert_eq!(
        FoldOptions::for_match(true, false),
        FoldOptions {
            case: false,
            ..KEEP
        }
    );
    // Matching diacritics there is nothing to keep or drop.
    assert_eq!(FoldOptions::for_match(false, true), CASE);
    assert_eq!(FoldOptions::for_match(true, true), NONE);
    // A term with an apostrophe of any kind keeps them; the others drop
    // them, so they are compared with the name's apostrophes removed.
    for text in ["'", "’‘ʼ", "plumb's", "plumb’s", "'s", "x\u{301}’"] {
        assert_eq!(FoldOptions::for_term(text, false, false), KEEP, "{text:?}");
    }
    for text in ["plumbs", "", "x", "\u{301}", "Hawaiʻi"] {
        assert_eq!(FoldOptions::for_term(text, false, false), BOTH, "{text:?}");
    }
    assert_eq!(
        FoldOptions::for_term("'", true, false),
        FoldOptions {
            case: false,
            ..KEEP
        }
    );
    assert_eq!(FoldOptions::for_term("plumbs", true, false), DIACRITICS);
    assert_eq!(FoldOptions::for_term("'", false, true), CASE);
    assert_eq!(FoldOptions::for_term("plumbs", false, true), CASE);
    // The term's folding without apostrophes is the side table's.
    assert_eq!(KEEP.without_apostrophes(), BOTH);
    assert_eq!(CASE.without_apostrophes(), CASE);
}

#[test]
fn kept_apostrophes_are_unified_to_ascii() {
    for a in APOSTROPHES {
        assert_eq!(
            fold(&format!("Plumb{a}s"), KEEP),
            "plumb's",
            "{:04x}",
            a as u32
        );
        assert_eq!(fold(&a.to_string(), KEEP), "'", "{:04x}", a as u32);
    }
    assert_eq!(fold("Cafe\u{301}’s", KEEP), "cafe's");
    let s = "it's";
    assert!(matches!(fold_str(s, KEEP), Cow::Borrowed(b) if std::ptr::eq(b, s)));
    for s in ["Plumb’s", "‘’", "Cafe\u{301}’s", "\u{1112}'\u{1161}"] {
        let once = fold(s, KEEP);
        assert_eq!(fold(&once, KEEP), once, "{s:?}");
    }
    let names = ["Plumb’s.pdf", "it's", "x"];
    let (buf, offsets) = column_parts(&names);
    let column = NameColumn::new(&buf, &offsets).unwrap();
    let folded = FoldedColumn::build(&column, KEEP);
    assert_folded_matches(&folded, &names, KEEP);
    assert_eq!(folded.buf(), b"plumb's.pdf\0it's\0x\0");
}

#[test]
fn folded_column_keeps_a_side_table_for_names_with_an_apostrophe() {
    let names = [
        "Plumb’s.pdf",
        "plumbs.pdf",
        "PLAIN.TXT",
        "''",
        "Cafe\u{301}’s",
        "it‘s",
        "rock ʼn roll",
        "Hawaiʻi.jpg",
        // Dropping the apostrophe lets these Hangul jamo join, so the side
        // form is folded afresh rather than cut from the main one.
        "\u{1112}'\u{1161}",
    ];
    let (buf, offsets) = column_parts(&names);
    let column = NameColumn::new(&buf, &offsets).unwrap();
    let folded = FoldedColumn::build(&column, KEEP);
    assert_folded_matches(&folded, &names, KEEP);
    assert_eq!(folded.options(), KEEP);
    let side = [
        Some("plumbs.pdf"),
        None,
        None,
        Some(""),
        Some("cafes"),
        Some("its"),
        Some("rock n roll"),
        None,
        Some("\u{d558}"),
    ];
    for (i, want) in side.iter().enumerate() {
        assert_eq!(
            folded.without_apostrophes(i),
            want.map(str::as_bytes),
            "entry {i} ({:?})",
            names[i]
        );
        // The term folding without apostrophes reads the side form when
        // there is one, else the main form; the main folding the main form.
        let both = fold_str(names[i], BOTH);
        assert_eq!(
            folded.folded_name(i, BOTH),
            Some(both.as_bytes()),
            "entry {i}"
        );
        let keep = fold_str(names[i], KEEP);
        assert_eq!(
            folded.folded_name(i, KEEP),
            Some(keep.as_bytes()),
            "entry {i}"
        );
        // Any other folding is not kept.
        assert_eq!(folded.folded_name(i, NONE), None);
        assert_eq!(folded.folded_name(i, CASE), None);
    }
    assert_eq!(folded.side_table_len(), 6);
    assert!(folded.side_table_bytes() > 0);
    assert!(folded.heap_bytes() >= folded.buf().len() + 4 * names.len());
}

#[test]
fn folded_column_side_table_is_empty_without_apostrophes() {
    let names = ["readme.txt", "Résumé.pdf", "Hawaiʻi.jpg", "x"];
    let (buf, offsets) = column_parts(&names);
    let column = NameColumn::new(&buf, &offsets).unwrap();
    for o in [KEEP, BOTH, NONE, CASE, DIACRITICS] {
        let folded = FoldedColumn::build(&column, o);
        assert_eq!(folded.side_table_len(), 0, "{}", opts_name(o));
        assert_eq!(folded.side_table_bytes(), 0, "{}", opts_name(o));
        assert_eq!(folded.without_apostrophes(0), None);
    }
    // A column that drops apostrophes (or keeps them because diacritics
    // are matched) needs no side table either.
    let names = ["Plumb’s.pdf", "it's"];
    let (buf, offsets) = column_parts(&names);
    let column = NameColumn::new(&buf, &offsets).unwrap();
    for o in [BOTH, NONE, CASE, DIACRITICS] {
        let folded = FoldedColumn::build(&column, o);
        assert_eq!(folded.side_table_len(), 0, "{}", opts_name(o));
        assert_eq!(folded.without_apostrophes(0), None);
        assert_eq!(
            folded.folded_name(1, o),
            Some(fold_str("it's", o).as_bytes())
        );
    }
}

#[test]
fn folded_column_side_table_in_a_large_parallel_build() {
    let pool = ["File.TXT", "Plumb’s", "it's", "x", "Café’s", "plain"];
    let owned: Vec<String> = (0..70_000)
        .map(|i| format!("{}{}", pool[i % pool.len()], i % 11))
        .collect();
    let names: Vec<&str> = owned.iter().map(String::as_str).collect();
    let (buf, offsets) = column_parts(&names);
    let column = NameColumn::new(&buf, &offsets).unwrap();
    let folded = FoldedColumn::build(&column, KEEP);
    assert_folded_matches(&folded, &names, KEEP);
    let mut with = 0;
    for (i, name) in names.iter().enumerate() {
        let has = name.contains(['\'', '’']);
        with += usize::from(has);
        let want = has.then(|| fold_str(name, BOTH).into_owned());
        assert_eq!(
            folded.without_apostrophes(i),
            want.as_deref().map(str::as_bytes),
            "entry {i}"
        );
    }
    assert_eq!(folded.side_table_len(), with);
}

// ---------------------------------------------------------------------------
// FoldedColumn
// ---------------------------------------------------------------------------

const MIXED: &[&str] = &[
    "readme.txt",
    "Résumé.pdf",
    "",
    "ÅNGSTRÖM",
    "日本語",
    "Re\u{301}sume\u{301}.pdf",
    "ß",
    "İ",
    "\u{301}",
    "",
    "PLAIN.TXT",
    "ΟΔΟΣ",
];

#[test]
fn folded_column_mixed_names_every_option() {
    let (buf, offsets) = column_parts(MIXED);
    let column = NameColumn::new(&buf, &offsets).unwrap();
    for o in ALL_OPTS {
        let folded = FoldedColumn::build(&column, o);
        assert_folded_matches(&folded, MIXED, o);
    }
}

#[test]
fn folded_column_exact_contents_when_folding_both() {
    let (buf, offsets) = column_parts(MIXED);
    let column = NameColumn::new(&buf, &offsets).unwrap();
    let folded = FoldedColumn::build(&column, BOTH);
    let want: [&str; 12] = [
        "readme.txt",
        "resume.pdf",
        "",
        "angstrom",
        "日本語",
        "resume.pdf",
        "ß",
        "i",
        "",
        "",
        "plain.txt",
        "οδοσ",
    ];
    for (i, w) in want.iter().enumerate() {
        assert_eq!(folded.name_bytes(i), w.as_bytes(), "entry {i}");
    }
    let mut expected = Vec::new();
    for w in want {
        expected.extend_from_slice(w.as_bytes());
        expected.push(0);
    }
    assert_eq!(folded.buf(), &expected[..]);
}

#[test]
fn folded_column_handles_growing_names() {
    // İ (2 bytes) lowercases to "i\u{307}" (3 bytes): offsets after it move.
    let names = ["İ", "a", "İİ", "b"];
    let (buf, offsets) = column_parts(&names);
    let column = NameColumn::new(&buf, &offsets).unwrap();
    let folded = FoldedColumn::build(&column, CASE);
    assert_eq!(folded.offsets(), &[0, 4, 6, 13]);
    assert_folded_matches(&folded, &names, CASE);
}

#[test]
fn folded_column_ascii_is_a_byte_copy() {
    let names = ["Makefile", "src", "README.md", "", "a.b.c", "x"];
    let (buf, offsets) = column_parts(&names);
    let column = NameColumn::new(&buf, &offsets).unwrap();

    for o in [NONE, DIACRITICS] {
        let folded = FoldedColumn::build(&column, o);
        assert_eq!(folded.buf(), &buf[..], "{}", opts_name(o));
        assert_eq!(folded.offsets(), &offsets[..], "{}", opts_name(o));
    }
    for o in [CASE, BOTH] {
        let folded = FoldedColumn::build(&column, o);
        assert_eq!(
            folded.buf(),
            &buf.to_ascii_lowercase()[..],
            "{}",
            opts_name(o)
        );
        assert_eq!(folded.offsets(), &offsets[..], "{}", opts_name(o));
    }
}

#[test]
fn folded_column_empty_column() {
    let column = NameColumn::new(&[], &[]).unwrap();
    for o in ALL_OPTS {
        let folded = FoldedColumn::build(&column, o);
        assert_eq!(folded.len(), 0);
        assert!(folded.is_empty());
        assert!(folded.buf().is_empty());
        assert!(folded.offsets().is_empty());
    }
}

#[test]
fn folded_column_only_empty_names() {
    let names = ["", "", ""];
    let (buf, offsets) = column_parts(&names);
    let column = NameColumn::new(&buf, &offsets).unwrap();
    for o in ALL_OPTS {
        let folded = FoldedColumn::build(&column, o);
        assert_eq!(folded.buf(), b"\0\0\0");
        assert_eq!(folded.offsets(), &[0, 1, 2]);
        assert_eq!(folded.name_bytes(1), b"");
    }
}

#[test]
fn folded_column_large_keeps_entry_order() {
    // Enough entries that a parallel build splits the work; entry i must
    // still map to entry i.
    let pool = [
        "File.TXT",
        "Résumé",
        "Re\u{301}sume\u{301}",
        "ΟΔΟΣ",
        "",
        "日本",
        "İ",
        "Straße",
        "x",
    ];
    let owned: Vec<String> = (0..60_000)
        .map(|i| format!("{}{}", pool[i % pool.len()], i % 7))
        .collect();
    let names: Vec<&str> = owned.iter().map(String::as_str).collect();
    let (buf, offsets) = column_parts(&names);
    let column = NameColumn::new(&buf, &offsets).unwrap();
    for o in [NONE, BOTH] {
        let folded = FoldedColumn::build(&column, o);
        assert_folded_matches(&folded, &names, o);
    }
}

// ---------------------------------------------------------------------------
// The eval oracle folds diacritics and Unicode case
// ---------------------------------------------------------------------------

#[test]
fn oracle_resume_matches_accented_name_by_default() {
    assert!(eval("resume", "Résumé.pdf"));
    assert!(eval("résumé", "resume.pdf"));
    assert!(eval("RESUME", "résumé.pdf"));
    assert!(eval("cafe", "Café au lait"));
    assert!(eval("nodiacritics:resume", "Résumé.pdf"));
}

#[test]
fn oracle_diacritics_modifier_keeps_accents() {
    assert!(!eval("diacritics:resume", "Résumé.pdf"));
    assert!(eval("diacritics:résumé", "Résumé.pdf"));
    assert!(!eval("diacritics:résumé", "resume.pdf"));
    let strict = SearchOptions {
        match_diacritics: true,
        ..Default::default()
    };
    assert!(!eval_with("resume", &strict, "Résumé.pdf", "/Users/me"));
    assert!(eval_with(
        "nodiacritics:resume",
        &strict,
        "Résumé.pdf",
        "/Users/me"
    ));
}

#[test]
fn oracle_case_modifier_with_folded_accents() {
    assert!(eval("case:Resume", "Résumé.pdf"));
    assert!(!eval("case:resume", "Résumé.pdf"));
    assert!(!eval("case:diacritics:Resume", "Résumé.pdf"));
    assert!(eval("case:diacritics:Résumé", "Résumé.pdf"));
}

#[test]
fn oracle_nfd_names_match_nfc_queries() {
    let nfd = "Re\u{301}sume\u{301}.pdf";
    assert!(eval("résumé", nfd));
    assert!(eval("resume", nfd));
    assert!(eval("diacritics:Résumé", nfd));
    assert!(eval("diacritics:résumé", nfd));
    assert!(!eval("diacritics:resume", nfd));
    // An NFD query against an NFC name.
    assert!(eval("diacritics:Re\u{301}sume\u{301}", "Résumé.pdf"));
}

#[test]
fn oracle_unicode_case_folding() {
    assert!(eval("привет", "ПРИВЕТ.doc"));
    assert!(eval("ΑΘΗΝΑ", "αθηνα.txt"));
    assert!(eval("οδοσ", "ΟΔΟΣ.txt"));
    assert!(eval("αθηνα", "Αθήνα.txt"));
    assert!(!eval("diacritics:αθηνα", "Αθήνα.txt"));
    assert!(eval("straße", "STRAẞE.txt"));
    assert!(!eval("strasse", "Straße.txt"), "ß does not expand to ss");
    assert!(eval("angstrom", "ÅNGSTRÖM.dat"));
}

#[test]
fn oracle_wildcards_fold_too() {
    assert!(eval("res*.pdf", "Résumé.pdf"));
    assert!(eval("resume.*", "Résumé.pdf"));
    assert!(!eval("diacritics:resume.*", "Résumé.pdf"));
    // `?` sees one character for an NFD é once names are normalized.
    assert!(eval("diacritics:r?sum?.pdf", "Re\u{301}sume\u{301}.pdf"));
    assert!(eval("r?sum?.pdf", "Re\u{301}sume\u{301}.pdf"));
}

#[test]
fn oracle_modifiers_on_folded_text() {
    assert!(eval("ww:cafe", "Café au lait"));
    assert!(eval("wfn:resume.pdf", "Résumé.pdf"));
    assert!(eval("start-with:ecole", "École primaire"));
    assert!(!eval("diacritics:start-with:ecole", "École primaire"));
}

#[test]
fn oracle_path_terms_fold() {
    let opts = SearchOptions::default();
    assert!(eval_with("zoe/notes", &opts, "notes.txt", "/Users/Zoë"));
    assert!(eval_with("path:zoe", &opts, "notes.txt", "/Users/Zoë"));
    assert!(!eval_with(
        "diacritics:path:zoe",
        &opts,
        "notes.txt",
        "/Users/Zoë"
    ));
    assert!(eval_with(
        "diacritics:path:zoë",
        &opts,
        "notes.txt",
        "/Users/Zoe\u{308}"
    ));
}

// ---------------------------------------------------------------------------
// The eval oracle's apostrophe rule: a term without an apostrophe
// ignores the name's; a term with one needs one in that place
// ---------------------------------------------------------------------------

const PLUMB: &str = "Plumb’s Veterinary Drug Handbook 10th Edition.pdf";

#[test]
fn oracle_plumbs_finds_every_apostrophe_spelling() {
    // A term without an apostrophe ignores the name's apostrophes.
    for query in ["plumbs", "PLUMBS", "Plumbs"] {
        assert!(eval(query, PLUMB), "{query:?}");
        assert!(eval(query, "plumb's.pdf"), "{query:?}");
        assert!(eval(query, "plumbs.pdf"), "{query:?}");
    }
    // A term with one finds only names with an apostrophe in that place,
    // whichever of the four kinds either side uses.
    for query in ["plumb's", "plumb’s", "PLUMB‘S", "plumbʼs", "PLUMB'S"] {
        assert!(eval(query, PLUMB), "{query:?}");
        assert!(eval(query, "plumb's.pdf"), "{query:?}");
        assert!(!eval(query, "plumbs.pdf"), "{query:?}");
        assert!(!eval(query, "plumb s.pdf"), "{query:?}");
    }
    assert!(eval("veterinary plumbs", PLUMB));
    assert!(eval("plumb's handbook", PLUMB));
    assert!(!eval("plumb's handbook", "plumbs handbook.pdf"));
    assert!(!eval("plumbz", PLUMB));
    // NFD accents and apostrophes fold together.
    assert!(eval("cafes", "Cafe\u{301}’s menu"));
    assert!(eval("café's", "Cafe\u{301}’s menu"));
    assert!(!eval("café's", "Cafes menu"));
}

#[test]
fn oracle_diacritics_keeps_apostrophes_exact() {
    assert!(!eval("diacritics:plumbs", PLUMB));
    assert!(!eval("diacritics:plumb's", PLUMB));
    assert!(eval("diacritics:plumb’s", PLUMB));
    assert!(eval("diacritics:PLUMB’S", PLUMB), "case still folds");
    let strict = SearchOptions {
        match_diacritics: true,
        ..Default::default()
    };
    assert!(!eval_with("plumbs", &strict, PLUMB, "/Users/me"));
    assert!(eval_with("plumb’s", &strict, PLUMB, "/Users/me"));
    assert!(eval_with(
        "nodiacritics:plumbs",
        &strict,
        PLUMB,
        "/Users/me"
    ));
}

#[test]
fn oracle_regex_sees_the_name_as_written() {
    assert!(eval("regex:plumb.?s", PLUMB));
    assert!(eval("regex:plumb’s", PLUMB));
    assert!(!eval("regex:plumbs", PLUMB));
    assert!(!eval("regex:plumb's", PLUMB));
}

#[test]
fn oracle_whole_word_uses_the_folded_word() {
    assert!(!eval("ww:plumb", PLUMB), "the folded word is plumbs");
    assert!(eval("ww:plumbs", PLUMB));
    assert!(eval("ww:plumb's", PLUMB));
    assert!(eval("ww:plumb’s", PLUMB));
    assert!(eval("diacritics:ww:plumb", PLUMB), "’ is a word boundary");
}

#[test]
fn oracle_wildcards_and_paths_follow_the_apostrophe_rule() {
    assert!(eval("plumbs*.pdf", PLUMB));
    assert!(eval("plumb's*", PLUMB));
    assert!(!eval("plumb's*", "plumbs.pdf"));
    // Without an apostrophe the term sees the name without its
    // apostrophes, so `?` never stands for one.
    assert!(!eval("plumb?s*", PLUMB));
    assert!(eval("plumb?s*", "plumbXs.pdf"));
    assert!(eval("wfn:plumbs.pdf", "Plumb’s.pdf"));
    assert!(!eval("diacritics:plumbs*", PLUMB));
    let opts = SearchOptions::default();
    assert!(eval_with("joes/notes", &opts, "notes.txt", "/Users/Joe’s"));
    assert!(eval_with("path:joe's", &opts, "notes.txt", "/Users/Joe’s"));
    assert!(!eval_with("path:joe's", &opts, "notes.txt", "/Users/Joes"));
    assert!(eval_with("joe’s/notes", &opts, "notes.txt", "/Users/Joe's"));
}

#[test]
fn oracle_a_query_of_only_apostrophes_finds_names_with_one() {
    let with = [PLUMB, "it's.txt", "‘quoted’.md", "rock ʼn roll.mp3"];
    let without = ["plain.txt", "Plumbs.pdf", "Hawaiʻi.jpg"];
    for query in ["'", "’", "‘", "ʼ", "\"'\""] {
        for name in with {
            assert!(eval(query, name), "{query:?} should find {name:?}");
        }
        for name in without {
            assert!(!eval(query, name), "{query:?} should skip {name:?}");
        }
    }
    // Every apostrophe counts as `'`, so `''` finds any two in a row.
    assert!(eval("''", "say ‘’.txt"));
    assert!(eval("’’", "rock ''n roll"));
    assert!(!eval("''", "it's.txt"));
    // Wildcards made only of apostrophes behave the same way.
    assert!(eval("*'*", PLUMB));
    assert!(!eval("*'*", "plain.txt"));
    assert!(eval("'*", "’tis.txt"));
    assert!(!eval("'*", PLUMB));
    // Mixed with other text, the apostrophe must still be there.
    assert!(eval("'s", PLUMB));
    assert!(!eval("'s", "Plumbs.pdf"));
    // Matching diacritics, an apostrophe is compared exactly.
    assert!(eval("diacritics:'", "it's.txt"));
    assert!(!eval("diacritics:'", PLUMB));
}
