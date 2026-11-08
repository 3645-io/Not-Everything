//! Refinement cache for search-as-you-type (`fx_query::refine`).
//!
//! The contract these tests pin:
//!
//! - `Searcher::new()` builds a cache for one view generation with an LRU
//!   of 8 recent `(query text, options)` result sets.
//! - `searcher.search(text, &opts, &view, &cancel)` returns
//!   `Result<Arc<[u32]>, _>` (the error is `Cancelled`): the live matching
//!   IDs in ascending order, exactly what a fresh
//!   `CompiledQuery::search` returns for the same text and options.
//! - When the new query provably narrows the previous one (a single
//!   positive AND of plain terms, same options, every old term a prefix of
//!   a new term or terms only appended), it filters the previous hits with
//!   `search_within` instead of scanning the view. OR, NOT, regex,
//!   wildcards and functions fall back to a full scan.
//! - `searcher.stats()` returns the counters by value, with public integer
//!   `u64` fields `full_scans`, `refined` and `cache_hits`; every successful
//!   search bumps exactly one of them.
//! - `searcher.invalidate()` clears everything; `searcher.set_epoch(e)`
//!   clears everything when `e` differs from the current epoch and keeps
//!   the cache when it is the same.
//! - A cancelled search never populates the cache.

use std::sync::Arc;

use fx_query::eval::EvalContext;
use fx_query::matcher::{CompiledQuery, Metadata, SearchView, VecView};
use fx_query::refine::Searcher;
use fx_query::{CancelToken, Generation, SearchOptions, parse};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The reference: a fresh compile and full search, no cache involved.
fn fresh<V: SearchView>(view: &V, query: &str, opts: &SearchOptions) -> Vec<u32> {
    let parsed = parse(query, opts);
    CompiledQuery::compile(&parsed, opts, EvalContext::default())
        .unwrap_or_else(|e| panic!("compiling {query:?} failed: {e:?}"))
        .search(view, &CancelToken::never())
        .expect("a never-cancelled search succeeds")
}

/// Searches through the cache and checks the result against [`fresh`].
fn run(s: &mut Searcher, view: &VecView, query: &str, opts: &SearchOptions) -> Arc<[u32]> {
    let got = s
        .search(query, opts, view, &CancelToken::never())
        .unwrap_or_else(|e| panic!("searching {query:?} failed: {e:?}"));
    assert_eq!(
        &*got,
        fresh(view, query, opts).as_slice(),
        "cached search for {query:?} with {opts:?} differs from a fresh search"
    );
    got
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Counts {
    full: u64,
    refined: u64,
    hits: u64,
}

fn counts(s: &Searcher) -> Counts {
    let st = s.stats();
    Counts {
        full: st.full_scans,
        refined: st.refined,
        hits: st.cache_hits,
    }
}

fn delta(before: Counts, after: Counts) -> Counts {
    Counts {
        full: after.full - before.full,
        refined: after.refined - before.refined,
        hits: after.hits - before.hits,
    }
}

const FULL: Counts = Counts {
    full: 1,
    refined: 0,
    hits: 0,
};
const REFINED: Counts = Counts {
    full: 0,
    refined: 1,
    hits: 0,
};
const HIT: Counts = Counts {
    full: 0,
    refined: 0,
    hits: 1,
};

/// Runs one search and returns which counter it bumped.
fn step(s: &mut Searcher, view: &VecView, query: &str, opts: &SearchOptions) -> Counts {
    let before = counts(s);
    run(s, view, query, opts);
    delta(before, counts(s))
}

fn dir(v: &mut VecView, name: &str, parent: Option<u32>) -> u32 {
    v.push(name, parent, true, Metadata::default())
}

fn file(v: &mut VecView, name: &str, parent: Option<u32>, size: u64) -> u32 {
    v.push(
        name,
        parent,
        false,
        Metadata {
            size: Some(size),
            ..Default::default()
        },
    )
}

/// A small tree with mixed case, accents, whole-word traps and folders
/// whose names only match through the path.
fn sample_view() -> VecView {
    let mut v = VecView::new();
    let root = dir(&mut v, "", None);
    let home = dir(&mut v, "home", Some(root));
    let reports = dir(&mut v, "reports", Some(home));
    let rep_dir = dir(&mut v, "Répertoire", Some(home));
    let misc = dir(&mut v, "misc", Some(home));
    file(&mut v, "report.txt", Some(home), 10);
    file(&mut v, "Report-final.PDF", Some(home), 2_000);
    file(&mut v, "repo", Some(home), 3);
    file(&mut v, "rep notes.txt", Some(home), 40);
    file(&mut v, "README.md", Some(home), 50);
    file(&mut v, "résumé.doc", Some(home), 60);
    file(&mut v, "Résumé.pdf", Some(home), 70);
    file(&mut v, "prepare.sh", Some(misc), 80);
    file(&mut v, "x.rs", Some(misc), 90);
    dir(&mut v, "repository", Some(home));
    file(&mut v, "rePO.txt", Some(misc), 100);
    file(&mut v, "notes", Some(misc), 110);
    file(&mut v, "carpet.txt", Some(misc), 120);
    file(&mut v, "REPO_old", Some(misc), 130);
    file(&mut v, "q1.txt", Some(reports), 140);
    file(&mut v, "x", Some(reports), 150);
    file(&mut v, "plan.md", Some(rep_dir), 160);
    file(&mut v, "re", Some(misc), 170);
    file(&mut v, "re port.txt", Some(misc), 180);
    file(&mut v, "Ærø.txt", Some(misc), 190);
    file(&mut v, "straße.txt", Some(misc), 200);
    let gone = file(&mut v, "deleted repo", Some(misc), 210);
    v.mark_deleted(gone);
    v
}

fn opts() -> SearchOptions {
    SearchOptions::default()
}

/// Every non-empty prefix of `query`, on char boundaries.
fn typed_prefixes(query: &str) -> Vec<&str> {
    query
        .char_indices()
        .map(|(i, c)| &query[..i + c.len_utf8()])
        .collect()
}

// ---------------------------------------------------------------------------
// Type-ahead refinement and backspace
// ---------------------------------------------------------------------------

#[test]
fn new_searcher_has_zero_stats() {
    let s = Searcher::new();
    assert_eq!(
        counts(&s),
        Counts {
            full: 0,
            refined: 0,
            hits: 0
        }
    );
}

#[test]
fn typing_refines_and_matches_fresh_searches() {
    let v = sample_view();
    let o = opts();
    let mut s = Searcher::new();

    assert_eq!(
        step(&mut s, &v, "r", &o),
        FULL,
        "first query scans the view"
    );
    assert_eq!(step(&mut s, &v, "re", &o), REFINED);
    assert_eq!(step(&mut s, &v, "rep", &o), REFINED);
    assert_eq!(step(&mut s, &v, "repo", &o), REFINED);
    assert_eq!(
        counts(&s),
        Counts {
            full: 1,
            refined: 3,
            hits: 0
        }
    );
    // The final result is not trivially empty.
    let repo = run(&mut s, &v, "repo", &o);
    assert!(repo.len() >= 4, "expected several repo hits, got {repo:?}");
}

#[test]
fn appending_a_term_refines() {
    let v = sample_view();
    let o = opts();
    let mut s = Searcher::new();
    assert_eq!(step(&mut s, &v, "rep", &o), FULL);
    assert_eq!(step(&mut s, &v, "rep t", &o), REFINED);
    assert_eq!(step(&mut s, &v, "rep tx", &o), REFINED);
    assert_eq!(step(&mut s, &v, "rep txt", &o), REFINED);
    assert!(!run(&mut s, &v, "rep txt", &o).is_empty());
}

#[test]
fn backspace_hits_the_cache() {
    let v = sample_view();
    let o = opts();
    let mut s = Searcher::new();
    let mut seen = Vec::new();
    for q in ["r", "re", "rep", "repo"] {
        seen.push((q, run(&mut s, &v, q, &o)));
    }
    for (q, earlier) in seen.iter().rev().skip(1) {
        let before = counts(&s);
        let again = run(&mut s, &v, q, &o);
        assert_eq!(delta(before, counts(&s)), HIT, "backspace to {q:?}");
        assert!(
            Arc::ptr_eq(earlier, &again),
            "backspace to {q:?} should return the cached result set"
        );
    }
}

#[test]
fn repeating_the_same_query_hits_the_cache() {
    let v = sample_view();
    let o = opts();
    let mut s = Searcher::new();
    assert_eq!(step(&mut s, &v, "txt", &o), FULL);
    assert_eq!(step(&mut s, &v, "txt", &o), HIT);
}

#[test]
fn typing_and_backspacing_unicode_matches_fresh_searches() {
    let v = sample_view();
    for o in [
        opts(),
        SearchOptions {
            match_case: true,
            ..opts()
        },
        SearchOptions {
            match_diacritics: true,
            ..opts()
        },
        SearchOptions {
            match_path: true,
            ..opts()
        },
    ] {
        for query in ["résumé", "Répertoire", "ærø", "straße", "strasse", "REPO"] {
            let mut s = Searcher::new();
            let prefixes = typed_prefixes(query);
            for q in &prefixes {
                run(&mut s, &v, q, &o);
            }
            for q in prefixes.iter().rev() {
                run(&mut s, &v, q, &o);
            }
        }
    }
}

/// The apostrophe rule: a term without an apostrophe ignores the name's, a
/// term with one needs one in that place.
#[test]
fn typing_apostrophes_matches_fresh_searches() {
    let mut v = sample_view();
    let misc = 4;
    assert_eq!(v.name(misc), "misc");
    for name in [
        "Plumb’s handbook.pdf",
        "plumb's notes.txt",
        "plumbs.txt",
        "plumb.txt",
        "it’s.md",
        "its.md",
        "say ‘’.txt",
    ] {
        file(&mut v, name, Some(misc), 1);
    }
    for o in [
        opts(),
        SearchOptions {
            match_diacritics: true,
            ..opts()
        },
        SearchOptions {
            whole_word: true,
            ..opts()
        },
    ] {
        for query in [
            "plumb's",
            "plumb’s",
            "plumbs",
            "'s",
            "''s",
            "’’",
            "' plumb",
            "its'",
            "it's",
        ] {
            let mut s = Searcher::new();
            let prefixes = typed_prefixes(query);
            for q in &prefixes {
                run(&mut s, &v, q, &o);
            }
            for q in prefixes.iter().rev() {
                run(&mut s, &v, q, &o);
            }
        }
    }
    // Typing the apostrophe switches to the folding that keeps it, so
    // `plumb'` scans afresh; `plumb's` then narrows `plumb'`, and `'s`
    // narrows `'`. Without the apostrophe `plumbs` narrows `plumb`.
    let o = opts();
    let mut s = Searcher::new();
    assert_eq!(step(&mut s, &v, "plumb", &o), FULL);
    assert_eq!(step(&mut s, &v, "plumbs", &o), REFINED);
    assert_eq!(run(&mut s, &v, "plumbs", &o).len(), 3);
    assert_eq!(step(&mut s, &v, "plumb'", &o), FULL);
    assert_eq!(step(&mut s, &v, "plumb's", &o), REFINED);
    assert_eq!(run(&mut s, &v, "plumb's", &o).len(), 2);
    let mut s = Searcher::new();
    assert_eq!(step(&mut s, &v, "'", &o), FULL);
    assert_eq!(step(&mut s, &v, "'s", &o), REFINED);
    assert_eq!(step(&mut s, &v, "s", &o), FULL);
}

#[test]
fn lru_keeps_the_eight_most_recent_queries() {
    let v = sample_view();
    let o = opts();
    let mut s = Searcher::new();
    let words = ["txt", "pdf", "notes", "home", "x", "md", "doc", "sh"];
    for w in words {
        assert_eq!(step(&mut s, &v, w, &o), FULL, "{w:?}");
    }
    // Touch the oldest so it becomes the most recent.
    assert_eq!(step(&mut s, &v, "txt", &o), HIT);
    // A ninth distinct query evicts the least recently used: "pdf".
    assert_eq!(step(&mut s, &v, "rs", &o), FULL);
    assert_eq!(step(&mut s, &v, "txt", &o), HIT);
    assert_eq!(step(&mut s, &v, "sh", &o), HIT);
    assert_eq!(step(&mut s, &v, "notes", &o), HIT);
    assert_eq!(step(&mut s, &v, "pdf", &o), FULL, "evicted entry rescans");
}

// ---------------------------------------------------------------------------
// Fallbacks to a full scan
// ---------------------------------------------------------------------------

#[test]
fn or_not_regex_wildcards_and_functions_fall_back_to_full_scans() {
    let v = sample_view();
    let o = opts();
    for next in [
        "re|x",
        "re | x",
        "re !x",
        "re !port",
        "!re",
        "regex:rep",
        "regex:^re",
        "re*",
        "re?",
        "rep*.txt",
        "*.txt",
        "re ext:txt",
        "re size:>50",
        "re <a|b>",
    ] {
        let mut s = Searcher::new();
        assert_eq!(step(&mut s, &v, "re", &o), FULL);
        assert_eq!(
            step(&mut s, &v, next, &o),
            FULL,
            "{next:?} after \"re\" must not be refined"
        );
    }
}

#[test]
fn regex_option_falls_back_to_full_scans() {
    let v = sample_view();
    let o = SearchOptions {
        regex: true,
        ..opts()
    };
    let mut s = Searcher::new();
    assert_eq!(step(&mut s, &v, "re", &o), FULL);
    assert_eq!(step(&mut s, &v, "re.", &o), FULL);
    assert_eq!(step(&mut s, &v, "re.o", &o), FULL);
    // `re|` widens the regex: refinement would be wrong here.
    assert_eq!(step(&mut s, &v, "re|x", &o), FULL);
}

#[test]
fn non_narrowing_edits_give_fresh_results() {
    // Each sequence is checked against fresh searches at every step; the
    // counters are not pinned, only correctness.
    let v = sample_view();
    let sequences: &[&[&str]] = &[
        &["re", "re|", "re|x", "re|x.rs"],
        &["re", "re ", "re !", "re !t", "re !txt"],
        &["!", "!r", "!re", "!rep"],
        &["e", "ex", "ext", "ext:", "ext:t", "ext:tx", "ext:txt"],
        &["re", "re<", "re<p", "re<p|x", "re<p|x>"],
        &["re", "re\"", "re\" p", "re\" p\""],
        &["rep", "re", "r", "rx", "rxe"],
        &["re port", "report", "repor", "re por"],
        &["a", "a*", "a*.", "a*.t", "a*.txt"],
        &["repo", "repo/", "repo/x", "reports\\", "reports\\q"],
        &["case:", "case:r", "case:re", "case:rep", "case:Rep"],
        &["file:", "file:r", "file:re", "folder:re", "folder:rep"],
        &["ww:", "ww:r", "ww:re", "ww:rep"],
        &["path:", "path:r", "path:re", "path:rep"],
        &["r", "re", "rep", "repo", "repo_", "repo_o", "repo_old"],
    ];
    let option_sets = [
        opts(),
        SearchOptions {
            match_case: true,
            ..opts()
        },
        SearchOptions {
            whole_word: true,
            ..opts()
        },
        SearchOptions {
            match_path: true,
            ..opts()
        },
        SearchOptions {
            match_diacritics: true,
            ..opts()
        },
    ];
    for o in option_sets {
        let mut s = Searcher::new();
        for seq in sequences {
            for q in *seq {
                run(&mut s, &v, q, &o);
            }
        }
    }
}

#[test]
fn whole_word_typing_matches_fresh_searches() {
    // With whole words on, "rep" is not narrower than "re": "rep notes.txt"
    // matches whole-word "rep" but not whole-word "re".
    let v = sample_view();
    let o = SearchOptions {
        whole_word: true,
        ..opts()
    };
    let mut s = Searcher::new();
    for q in ["r", "re", "rep", "repo"] {
        run(&mut s, &v, q, &o);
    }
    assert!(!run(&mut s, &v, "rep", &o).is_empty());
}

// ---------------------------------------------------------------------------
// Options never reuse wrong results
// ---------------------------------------------------------------------------

#[test]
fn changing_options_does_not_reuse_results() {
    let v = sample_view();
    let d = opts();
    let case = SearchOptions {
        match_case: true,
        ..d
    };
    let path = SearchOptions {
        match_path: true,
        ..d
    };
    let diac = SearchOptions {
        match_diacritics: true,
        ..d
    };
    let mut s = Searcher::new();

    assert_eq!(step(&mut s, &v, "rep", &d), FULL);
    // Same text, different options: not a cache hit.
    let before = counts(&s);
    let with_case = run(&mut s, &v, "rep", &case);
    assert_eq!(delta(before, counts(&s)).hits, 0, "case option changed");
    let before = counts(&s);
    run(&mut s, &v, "rep", &path);
    assert_eq!(delta(before, counts(&s)).hits, 0, "path option changed");
    let before = counts(&s);
    run(&mut s, &v, "rep", &diac);
    assert_eq!(
        delta(before, counts(&s)).hits,
        0,
        "diacritics option changed"
    );
    // Back to the default options: the original entry is still there.
    let default_again = run(&mut s, &v, "rep", &d);
    assert_ne!(
        &*with_case, &*default_again,
        "the sample must tell them apart"
    );

    // Narrowing text with different options never refines.
    let ww = SearchOptions {
        whole_word: true,
        ..d
    };
    assert_eq!(step(&mut s, &v, "rep", &d), HIT);
    assert_eq!(step(&mut s, &v, "repo", &ww), FULL);
}

#[test]
fn growing_text_with_wider_options_gives_fresh_results() {
    let v = sample_view();
    let d = opts();
    // Case-sensitive "re" misses "Report-final.PDF"; case-insensitive "rep"
    // must find it.
    let mut s = Searcher::new();
    run(
        &mut s,
        &v,
        "re",
        &SearchOptions {
            match_case: true,
            ..d
        },
    );
    let got = run(&mut s, &v, "rep", &d);
    assert!(got.iter().any(|&id| v.name(id) == "Report-final.PDF"));

    // Name-only "re" misses "reports/x"; path "rep" must find it.
    let mut s = Searcher::new();
    run(&mut s, &v, "re", &d);
    let got = run(
        &mut s,
        &v,
        "rep",
        &SearchOptions {
            match_path: true,
            ..d
        },
    );
    assert!(got.iter().any(|&id| v.name(id) == "x"));

    // Diacritic-exact "re" misses "Résumé.pdf"; folded "res" must find it.
    let mut s = Searcher::new();
    run(
        &mut s,
        &v,
        "re",
        &SearchOptions {
            match_diacritics: true,
            ..d
        },
    );
    let got = run(&mut s, &v, "res", &d);
    assert!(got.iter().any(|&id| v.name(id) == "Résumé.pdf"));
}

// ---------------------------------------------------------------------------
// Invalidation
// ---------------------------------------------------------------------------

/// The sample view plus a few entries, as a newer index snapshot.
fn grown_view() -> VecView {
    let mut v = sample_view();
    file(&mut v, "rep_new.txt", Some(1), 5);
    file(&mut v, "repo2", Some(1), 6);
    v
}

#[test]
fn epoch_change_invalidates() {
    let v1 = sample_view();
    let v2 = grown_view();
    let o = opts();
    let mut s = Searcher::new();

    s.set_epoch(7);
    assert_eq!(step(&mut s, &v1, "rep", &o), FULL);
    s.set_epoch(7);
    assert_eq!(
        step(&mut s, &v1, "rep", &o),
        HIT,
        "same epoch keeps the cache"
    );

    s.set_epoch(8);
    assert_eq!(
        step(&mut s, &v2, "rep", &o),
        FULL,
        "new epoch drops the cache"
    );
    let got = run(&mut s, &v2, "rep", &o);
    assert!(got.iter().any(|&id| v2.name(id) == "rep_new.txt"));
}

#[test]
fn epoch_change_stops_refinement_from_old_hits() {
    let v1 = sample_view();
    let v2 = grown_view();
    let o = opts();
    let mut s = Searcher::new();
    s.set_epoch(1);
    run(&mut s, &v1, "re", &o);
    s.set_epoch(2);
    let before = counts(&s);
    let got = run(&mut s, &v2, "rep", &o);
    assert_eq!(delta(before, counts(&s)), FULL);
    assert!(got.iter().any(|&id| v2.name(id) == "repo2"));
}

#[test]
fn invalidate_clears_cache_and_refinement_base() {
    let v1 = sample_view();
    let v2 = grown_view();
    let o = opts();
    let mut s = Searcher::new();
    run(&mut s, &v1, "re", &o);
    run(&mut s, &v1, "rep", &o);

    s.invalidate();
    assert_eq!(
        step(&mut s, &v2, "rep", &o),
        FULL,
        "no cache hit after invalidate"
    );

    s.invalidate();
    run(&mut s, &v1, "re", &o);
    s.invalidate();
    let before = counts(&s);
    let got = run(&mut s, &v2, "repo", &o);
    assert_eq!(
        delta(before, counts(&s)),
        FULL,
        "no refinement after invalidate"
    );
    assert!(got.iter().any(|&id| v2.name(id) == "repo2"));
}

// ---------------------------------------------------------------------------
// Cancellation
// ---------------------------------------------------------------------------

#[test]
fn cancelled_search_does_not_populate_the_cache() {
    let v = sample_view();
    let o = opts();
    let mut s = Searcher::new();
    let generation = Generation::new();
    let stale = generation.token();
    generation.bump();

    assert!(
        s.search("txt", &o, &v, &stale).is_err(),
        "a superseded token cancels the search"
    );
    // The cancelled query was not cached: searching it again scans.
    assert_eq!(step(&mut s, &v, "txt", &o), FULL);
    assert_eq!(step(&mut s, &v, "txt", &o), HIT);
}

#[test]
fn cancelled_refinement_leaves_the_cache_unchanged() {
    let v = sample_view();
    let o = opts();
    let mut s = Searcher::new();
    let re = run(&mut s, &v, "re", &o);

    let generation = Generation::new();
    let stale = generation.token();
    generation.bump();
    assert!(s.search("rep", &o, &v, &stale).is_err());

    // "rep" was not cached by the cancelled attempt.
    let before = counts(&s);
    run(&mut s, &v, "rep", &o);
    assert_eq!(delta(before, counts(&s)).hits, 0);

    // "re" is still cached and unchanged.
    let before = counts(&s);
    let again = run(&mut s, &v, "re", &o);
    assert_eq!(delta(before, counts(&s)), HIT);
    assert!(Arc::ptr_eq(&re, &again));
}

#[test]
fn cancelled_search_does_not_become_the_refinement_base() {
    let v = sample_view();
    let o = opts();
    let mut s = Searcher::new();
    run(&mut s, &v, "r", &o);

    let generation = Generation::new();
    let stale = generation.token();
    generation.bump();
    assert!(s.search("x", &o, &v, &stale).is_err());

    // "xy" must not be refined from a cancelled "x"; whatever path it
    // takes, its results equal a fresh search.
    run(&mut s, &v, "xy", &o);
    run(&mut s, &v, "x.", &o);
    // Growing the last successful query still works and stays correct.
    run(&mut s, &v, "re", &o);
}

// ---------------------------------------------------------------------------
// Randomized type-ahead against fresh searches
// ---------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

#[test]
fn randomized_typing_matches_fresh_searches() {
    const ALPHABET: &[&str] = &[
        "r", "e", "p", "o", "t", "x", ".", " ", "é", "R", "E", "|", "!", "*", "/", "s",
    ];
    let mut rng = Rng(0x5EED_1234_ABCD_0001);
    let mut v = VecView::new();
    let root = dir(&mut v, "", None);
    let mut folders = vec![root];
    for i in 0..400 {
        let len = 1 + rng.below(8) as usize;
        let mut name = String::new();
        for _ in 0..len {
            let c = ALPHABET[rng.below(ALPHABET.len() as u64) as usize];
            if !matches!(c, "|" | "!" | "*" | "/") {
                name.push_str(c);
            }
        }
        if name.is_empty() {
            name.push('r');
        }
        let parent = folders[rng.below(folders.len() as u64) as usize];
        if i % 7 == 0 {
            let id = dir(&mut v, &name, Some(parent));
            folders.push(id);
        } else {
            file(&mut v, &name, Some(parent), i);
        }
    }

    let option_sets = [
        opts(),
        SearchOptions {
            match_case: true,
            ..opts()
        },
        SearchOptions {
            match_path: true,
            ..opts()
        },
        SearchOptions {
            whole_word: true,
            ..opts()
        },
    ];
    for o in option_sets {
        let mut s = Searcher::new();
        let mut query = String::new();
        for _ in 0..300 {
            // Mostly type, sometimes backspace.
            if !query.is_empty() && rng.below(4) == 0 {
                query.pop();
            } else {
                query.push_str(ALPHABET[rng.below(ALPHABET.len() as u64) as usize]);
            }
            run(&mut s, &v, &query, &o);
        }
        let st = counts(&s);
        assert_eq!(st.full + st.refined + st.hits, 300);
    }
}
