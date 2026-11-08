//! Tests for the parallel substring scan in `fx_query::substring`.
//!
//! Every search result is checked against a naive per-name `contains`, over
//! hand-built columns with edge cases and over synthetic names from
//! `fx-bench`. Results must not depend on the thread count or chunk size.

use fx_bench::{Names, generate_names};
use fx_query::{
    CHECK_INTERVAL, CancelToken, Cancelled, CaseMode, DEFAULT_CHUNK_ENTRIES, Generation,
    NameColumn, SubstringQuery, count, fold_ascii_lowercase, search, search_chunked,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Unwraps a result without requiring `E: Debug`.
fn must<T, E>(r: Result<T, E>, what: &str) -> T {
    match r {
        Ok(v) => v,
        Err(_) => panic!("{what}: unexpected error"),
    }
}

/// Builds the NUL-separated buffer and offsets for `names`.
fn build(names: &[&str]) -> (Vec<u8>, Vec<u32>) {
    let mut buf = Vec::new();
    let mut offsets = Vec::new();
    for name in names {
        offsets.push(u32::try_from(buf.len()).unwrap());
        buf.extend_from_slice(name.as_bytes());
        buf.push(0);
    }
    (buf, offsets)
}

fn column<'a>(buf: &'a [u8], offsets: &'a [u32]) -> NameColumn<'a> {
    must(
        NameColumn::new(buf, offsets),
        "NameColumn::new on a valid buffer",
    )
}

fn bench_column(names: &Names) -> NameColumn<'_> {
    column(names.buffer(), names.offsets())
}

fn naive_sensitive(names: &[&str], needle: &str) -> Vec<u32> {
    names
        .iter()
        .enumerate()
        .filter(|(_, n)| !needle.contains('\0') && n.contains(needle))
        .map(|(i, _)| u32::try_from(i).unwrap())
        .collect()
}

fn naive_insensitive(names: &[&str], needle: &str) -> Vec<u32> {
    let needle = needle.to_ascii_lowercase();
    names
        .iter()
        .enumerate()
        .filter(|(_, n)| !needle.contains('\0') && n.to_ascii_lowercase().contains(&needle))
        .map(|(i, _)| u32::try_from(i).unwrap())
        .collect()
}

fn sensitive(needle: &str) -> SubstringQuery {
    SubstringQuery::new(needle, CaseMode::Sensitive)
}

fn insensitive(needle: &str) -> SubstringQuery {
    SubstringQuery::new(needle, CaseMode::AsciiInsensitive)
}

fn run_search(col: NameColumn<'_>, q: &SubstringQuery) -> Vec<u32> {
    must(search(col, q, &CancelToken::never()), "search")
}

fn run_count(col: NameColumn<'_>, q: &SubstringQuery) -> usize {
    must(count(col, q, &CancelToken::never()), "count")
}

fn pool(threads: usize) -> rayon::ThreadPool {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .expect("build rayon pool")
}

/// Checks search and count for `needle` (case-sensitive) against the naive
/// scan.
fn check_sensitive(col: NameColumn<'_>, names: &[&str], needle: &str) {
    let expected = naive_sensitive(names, needle);
    let q = sensitive(needle);
    assert_eq!(run_search(col, &q), expected, "search for {needle:?}");
    assert_eq!(run_count(col, &q), expected.len(), "count for {needle:?}");
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

#[test]
fn check_interval_is_65536() {
    assert_eq!(CHECK_INTERVAL, 65_536);
    const { assert!(DEFAULT_CHUNK_ENTRIES > 0) };
}

// ---------------------------------------------------------------------------
// NameColumn
// ---------------------------------------------------------------------------

#[test]
fn name_column_accessors() {
    let names = ["alpha.txt", "b", "caf\u{e9}", "last entry"];
    let (buf, offsets) = build(&names);
    let col = column(&buf, &offsets);
    assert_eq!(col.len(), names.len());
    assert!(!col.is_empty());
    for (i, n) in names.iter().enumerate() {
        assert_eq!(col.name_bytes(i), n.as_bytes(), "name {i}");
    }
}

#[test]
fn name_column_accepts_empty() {
    let col = column(&[], &[]);
    assert_eq!(col.len(), 0);
    assert!(col.is_empty());
    assert!(run_search(col, &sensitive("")).is_empty());
    assert!(run_search(col, &sensitive("a")).is_empty());
    assert_eq!(run_count(col, &sensitive("")), 0);
}

#[test]
fn name_column_accepts_fx_bench_names() {
    let names = generate_names(1_000, 7);
    let col = bench_column(&names);
    assert_eq!(col.len(), names.len());
    for i in [0, 1, 500, 999] {
        assert_eq!(col.name_bytes(i), names.get(i).as_bytes());
    }
}

#[test]
fn name_column_accepts_empty_names_that_follow_the_rules() {
    // A lone NUL is one empty name; "a\0\0" is "a" then an empty name.
    let col = column(b"\0", &[0]);
    assert_eq!(col.len(), 1);
    assert_eq!(col.name_bytes(0), b"");

    let col = column(b"a\0\0", &[0, 2]);
    assert_eq!(col.len(), 2);
    assert_eq!(col.name_bytes(0), b"a");
    assert_eq!(col.name_bytes(1), b"");
}

#[test]
fn name_column_rejects_malformed_input() {
    let cases: &[(&[u8], &[u32], &str)] = &[
        (b"a\0", &[], "empty offsets with non-empty buffer"),
        (b"", &[0], "offset into an empty buffer"),
        (b"a\0b\0", &[2], "first offset not zero"),
        (b"a\0b\0", &[1, 2], "first offset not zero (2)"),
        (
            b"a\0b\0",
            &[0, 0],
            "offsets not strictly increasing (equal)",
        ),
        (b"a\0b\0c\0", &[0, 4, 2], "offsets decreasing"),
        (b"a\0", &[0, 2], "offset equal to buffer length"),
        (b"a\0", &[0, 9], "offset past buffer end"),
        (b"ab\0", &[0, 1], "byte before later offset is not NUL"),
        (b"a\0bc\0", &[0, 3], "later offset inside a name"),
        (b"a\0b", &[0, 2], "buffer does not end with NUL"),
        (b"a", &[0], "single name without NUL terminator"),
    ];
    for (buf, offsets, what) in cases {
        assert!(
            NameColumn::new(buf, offsets).is_err(),
            "should reject: {what}"
        );
    }
}

// ---------------------------------------------------------------------------
// Case-sensitive semantics on hand-built columns
// ---------------------------------------------------------------------------

const HAND: &[&str] = &[
    "first.txt",
    "ab",
    "cd",
    "aaaa",
    "Report 2023.PDF",
    "report-final.pdf",
    "caf\u{e9}.txt",
    "cafe\u{301}.txt",
    "CAF\u{c9}",
    "\u{65e5}\u{672c}\u{8a9e}.md",
    "M\u{fc}nchen",
    "x",
    "the last one",
];

#[test]
fn empty_needle_matches_every_entry() {
    let (buf, offsets) = build(HAND);
    let col = column(&buf, &offsets);
    let all: Vec<u32> = (0..u32::try_from(HAND.len()).unwrap()).collect();
    assert_eq!(run_search(col, &sensitive("")), all);
    assert_eq!(run_count(col, &sensitive("")), HAND.len());
    assert_eq!(run_search(col, &insensitive("")), all);
}

#[test]
fn hand_built_edge_cases_match_naive() {
    let (buf, offsets) = build(HAND);
    let col = column(&buf, &offsets);
    let needles = [
        // Whole names.
        "first.txt",
        "ab",
        "x",
        "the last one",
        "Report 2023.PDF",
        // Hits on first and last entry.
        "first",
        "one",
        "last",
        "f",
        // Repeated / overlapping hits inside one entry count once.
        "a",
        "aa",
        "aaa",
        // Longer than every name.
        "this needle is much longer than every single name in the column",
        // Absent.
        "zzz",
        // Would span the boundary between "ab" and "cd" if NUL were ignored.
        "bc",
        // Case matters.
        "report",
        "Report",
        "PDF",
        "pdf",
        // Non-ASCII.
        "caf\u{e9}",
        "\u{301}",
        "\u{e9}",
        "\u{c9}",
        "\u{65e5}\u{672c}",
        "\u{8a9e}.md",
        "M\u{fc}n",
        ".txt",
        "t",
    ];
    for needle in needles {
        check_sensitive(col, HAND, needle);
    }
}

#[test]
fn needle_with_nul_matches_nothing() {
    let (buf, offsets) = build(HAND);
    let col = column(&buf, &offsets);
    for needle in ["\0", "b\0c", "ab\0", "\0cd", "first.txt\0ab"] {
        let q = sensitive(needle);
        assert!(run_search(col, &q).is_empty(), "{needle:?}");
        assert_eq!(run_count(col, &q), 0, "{needle:?}");
        let q = insensitive(needle);
        assert!(run_search(col, &q).is_empty(), "insensitive {needle:?}");
    }
}

#[test]
fn single_entry_column() {
    let (buf, offsets) = build(&["only"]);
    let col = column(&buf, &offsets);
    assert_eq!(run_search(col, &sensitive("only")), vec![0]);
    assert_eq!(run_search(col, &sensitive("nl")), vec![0]);
    assert_eq!(run_search(col, &sensitive("y")), vec![0]);
    assert!(run_search(col, &sensitive("onlyx")).is_empty());
}

#[test]
fn hits_on_first_and_last_entries_with_every_chunk_size() {
    let mut names: Vec<String> = (0..50).map(|i| format!("filler{i}")).collect();
    names[0] = "needle-at-start".to_owned();
    names[49] = "ends-with-needle".to_owned();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let (buf, offsets) = build(&refs);
    let col = column(&buf, &offsets);
    let q = sensitive("needle");
    for chunk in [0, 1, 2, 3, 7, 49, 50, 51, 1_000] {
        let got = must(
            search_chunked(col, &q, &CancelToken::never(), chunk),
            "chunked",
        );
        assert_eq!(got, vec![0, 49], "chunk {chunk}");
    }
}

// ---------------------------------------------------------------------------
// ASCII case-insensitive search on the folded copy
// ---------------------------------------------------------------------------

#[test]
fn fold_ascii_lowercase_keeps_length_and_only_folds_ascii() {
    let input = "AbC-XYZ_09 CAF\u{c9} caf\u{e9} \u{c5}ngstr\u{f6}m \u{65e5}\0Q\0";
    let folded = fold_ascii_lowercase(input.as_bytes());
    assert_eq!(folded.len(), input.len());
    assert_eq!(folded, input.to_ascii_lowercase().into_bytes());

    let all: Vec<u8> = (0..=255).collect();
    let folded = fold_ascii_lowercase(&all);
    assert_eq!(folded.len(), 256);
    for (b, f) in all.iter().zip(&folded) {
        assert_eq!(*f, b.to_ascii_lowercase(), "byte {b:#04x}");
    }
    assert!(fold_ascii_lowercase(&[]).is_empty());
}

#[test]
fn ascii_insensitive_on_folded_copy_matches_naive_hand_built() {
    let (buf, offsets) = build(HAND);
    let folded = fold_ascii_lowercase(&buf);
    let col = column(&folded, &offsets);
    let needles = [
        "",
        "REPORT",
        "report",
        "RePoRt",
        "pdf",
        "PDF",
        ".TXT",
        "FIRST.TXT",
        "AAA",
        "caf\u{e9}",
        "CAF\u{e9}", // matches only the NFC é names, not "CAFÉ" (É is not ASCII)
        "CAF\u{c9}",
        "caf\u{c9}",
        "M\u{dc}N",
        "m\u{fc}N",
        "\u{65e5}",
        "ONE",
        "zzz",
        "bc",
    ];
    for needle in needles {
        let expected = naive_insensitive(HAND, needle);
        let q = insensitive(needle);
        assert_eq!(run_search(col, &q), expected, "insensitive {needle:?}");
        assert_eq!(
            run_count(col, &q),
            expected.len(),
            "insensitive count {needle:?}"
        );
    }
}

#[test]
fn ascii_insensitive_on_generated_names_matches_naive() {
    let names = generate_names(50_000, 11);
    let refs: Vec<&str> = names.iter().collect();
    let folded = fold_ascii_lowercase(names.buffer());
    let col = column(&folded, names.offsets());
    for needle in [
        "IMG",
        "img",
        "JPG",
        ".Txt",
        "README",
        "e",
        "Caf\u{e9}",
        "M\u{fc}NCHEN",
        "ZZQX",
    ] {
        let expected = naive_insensitive(&refs, needle);
        let q = insensitive(needle);
        assert_eq!(run_search(col, &q), expected, "insensitive {needle:?}");
        assert_eq!(
            run_count(col, &q),
            expected.len(),
            "insensitive count {needle:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Generated names: correctness against the naive scan
// ---------------------------------------------------------------------------

/// Needles for a generated set: fixed ones plus substrings of real names.
fn needles_for(names: &Names) -> Vec<String> {
    let mut needles: Vec<String> = [
        "",
        "e",
        ".",
        ".txt",
        ".jpg",
        "IMG",
        "img_",
        "2023",
        "-",
        " ",
        "caf\u{e9}",
        "\u{301}",
        "M\u{fc}nchen",
        "\u{c5}",
        "qqqzzzxxx",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    // Substrings and whole copies of a few real names, chosen by stride.
    let n = names.len();
    for k in 0..6 {
        let name = names.get((k * 7_919 + 13) % n);
        needles.push(name.to_owned());
        let chars: Vec<char> = name.chars().collect();
        if chars.len() >= 3 {
            let mid: String = chars[1..chars.len().min(5)].iter().collect();
            needles.push(mid);
        }
    }
    needles
}

#[test]
fn generated_names_match_naive_across_seeds() {
    for seed in [1_u64, 42, 0xDEAD_BEEF] {
        let names = generate_names(200_000, seed);
        let refs: Vec<&str> = names.iter().collect();
        let col = bench_column(&names);
        for needle in needles_for(&names) {
            let expected = naive_sensitive(&refs, &needle);
            let q = sensitive(&needle);
            let got = run_search(col, &q);
            assert_eq!(
                got.len(),
                expected.len(),
                "seed {seed} needle {needle:?} hit count"
            );
            assert_eq!(got, expected, "seed {seed} needle {needle:?}");
            assert_eq!(
                run_count(col, &q),
                expected.len(),
                "seed {seed} count {needle:?}"
            );
        }
    }
}

#[test]
fn search_equals_search_chunked_with_default_chunk() {
    let names = generate_names(150_000, 3);
    let col = bench_column(&names);
    for needle in ["", "e", ".png", "IMG", "zzzzqx"] {
        let q = sensitive(needle);
        let a = run_search(col, &q);
        let b = must(
            search_chunked(col, &q, &CancelToken::never(), DEFAULT_CHUNK_ENTRIES),
            "search_chunked",
        );
        assert_eq!(a, b, "{needle:?}");
    }
}

// ---------------------------------------------------------------------------
// Independence from thread count and chunk size
// ---------------------------------------------------------------------------

#[test]
fn results_identical_across_threads_and_chunk_sizes() {
    let names = generate_names(20_000, 99);
    let refs: Vec<&str> = names.iter().collect();
    let col = bench_column(&names);
    let folded = fold_ascii_lowercase(names.buffer());
    let fcol = column(&folded, names.offsets());

    let cases: Vec<(NameColumn<'_>, SubstringQuery, Vec<u32>, String)> =
        ["", "e", ".jpg", "IMG", "2023", "nomatchqq"]
            .iter()
            .flat_map(|n| {
                [
                    (
                        col,
                        sensitive(n),
                        naive_sensitive(&refs, n),
                        format!("sensitive {n:?}"),
                    ),
                    (
                        fcol,
                        insensitive(n),
                        naive_insensitive(&refs, n),
                        format!("insensitive {n:?}"),
                    ),
                ]
            })
            .collect();

    for threads in [1, 2, 8] {
        let p = pool(threads);
        for chunk in [0, 1, 3, 7, DEFAULT_CHUNK_ENTRIES] {
            for (c, q, expected, label) in &cases {
                let got = p.install(|| search_chunked(*c, q, &CancelToken::never(), chunk));
                let got = must(got, "search_chunked");
                assert_eq!(&got, expected, "{label}, threads {threads}, chunk {chunk}");
            }
        }
        for (c, q, expected, label) in &cases {
            let got = must(p.install(|| search(*c, q, &CancelToken::never())), "search");
            assert_eq!(&got, expected, "{label}, threads {threads}, default");
            let n = must(p.install(|| count(*c, q, &CancelToken::never())), "count");
            assert_eq!(n, expected.len(), "{label} count, threads {threads}");
        }
    }
}

#[test]
fn results_are_ascending_and_unique_on_large_input() {
    // More entries than CHECK_INTERVAL and DEFAULT_CHUNK_ENTRIES so several
    // chunks and checks happen.
    let n = (CHECK_INTERVAL * 2 + 1_234).max(DEFAULT_CHUNK_ENTRIES * 3 + 17);
    let names = generate_names(n, 5);
    let col = bench_column(&names);
    let p = pool(8);
    let all = must(
        p.install(|| search(col, &sensitive(""), &CancelToken::never())),
        "search",
    );
    assert_eq!(all.len(), n);
    assert!(all.iter().enumerate().all(|(i, &id)| id as usize == i));
    let some = must(
        p.install(|| search(col, &sensitive("e"), &CancelToken::never())),
        "search",
    );
    assert!(some.windows(2).all(|w| w[0] < w[1]), "ascending and unique");
}

// ---------------------------------------------------------------------------
// Cancellation
// ---------------------------------------------------------------------------

#[test]
fn generation_and_tokens() {
    let g = Generation::new();
    let before = g.current();
    let t = g.token();
    assert!(!t.is_cancelled());
    g.bump();
    assert_ne!(g.current(), before);
    assert!(t.is_cancelled());
    let fresh = g.token();
    assert!(!fresh.is_cancelled());
    g.bump();
    assert!(fresh.is_cancelled());
    assert!(t.is_cancelled());
    assert!(!CancelToken::never().is_cancelled());
}

#[test]
fn bumped_token_returns_cancelled() {
    let (buf, offsets) = build(HAND);
    let small = column(&buf, &offsets);
    let names = generate_names(100_000, 8);
    let large = bench_column(&names);

    let g = Generation::new();
    let t = g.token();
    g.bump();

    for col in [small, large] {
        for needle in ["", "e", "qqqzzz"] {
            let q = sensitive(needle);
            let _: Cancelled = match search(col, &q, &t) {
                Ok(_) => panic!("search {needle:?} should be cancelled"),
                Err(e) => e,
            };
            assert!(count(col, &q, &t).is_err(), "count {needle:?}");
            for chunk in [0, 1, 7, DEFAULT_CHUNK_ENTRIES] {
                assert!(search_chunked(col, &q, &t, chunk).is_err(), "chunk {chunk}");
            }
            for threads in [1, 8] {
                let p = pool(threads);
                assert!(
                    p.install(|| search(col, &q, &t)).is_err(),
                    "threads {threads}"
                );
            }
        }
    }
}

#[test]
fn live_token_completes() {
    let names = generate_names(10_000, 21);
    let refs: Vec<&str> = names.iter().collect();
    let col = bench_column(&names);
    let g = Generation::new();
    g.bump();
    let t = g.token();
    let got = must(search(col, &sensitive("e"), &t), "search with live token");
    assert_eq!(got, naive_sensitive(&refs, "e"));
    assert_eq!(must(count(col, &sensitive("e"), &t), "count"), got.len());
}

#[test]
fn bump_from_another_thread_cancels_the_token() {
    // A bump made on another thread is visible to a token taken earlier, and
    // later searches with that token fail.
    let names = generate_names(100_000, 4);
    let col = bench_column(&names);
    let g = Generation::new();
    let t = g.token();
    std::thread::scope(|s| {
        s.spawn(|| {
            g.bump();
        });
    });
    assert!(t.is_cancelled());
    assert!(search(col, &sensitive("a"), &t).is_err());
}
