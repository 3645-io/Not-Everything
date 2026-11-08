//! Tests for the compiled parallel matcher (`fx_query::matcher`).
//!
//! The reference evaluator ([`fx_query::eval`]) is the oracle: for every
//! live entry of a view, the compiled matcher must return exactly the IDs
//! for which `eval::matches_in` is true, in ascending order.
//!
//! Contracts pinned here:
//!
//! - [`SearchView`] needs only `len`, `name`, `parent`, `is_folder` and
//!   `is_live`; metadata and the folded copy are optional (absent metadata
//!   behaves like `None` / `false` in [`Candidate`]).
//! - Paths are never stored: an entry's full path is built from its name and
//!   its parents' names. A root entry (no parent) has an empty parent path,
//!   so its name is its full path (`"/"`, `"/Volumes/Ext"`); a child's
//!   parent path is its parent's full path, exactly as
//!   [`Candidate::full_path`] joins them.
//! - [`VecView`] is an owned view built with `push(name, parent, is_folder,
//!   Metadata) -> id` (IDs count up from 0) and `mark_deleted(id)`.
//!   [`Metadata`] is a `Copy + Default` struct with the same public metadata
//!   fields as [`Candidate`].
//! - `search` returns ascending IDs and skips deleted entries; `count`
//!   equals the length of `search`; `search_within` filters a candidate list.
//! - `count:N` is exposed through [`CompiledQuery::limit`], not applied.
//! - A cancelled token makes `search`, `count` and `search_within` return
//!   [`Cancelled`].

use fx_query::eval::{Candidate, EvalContext, matches_in};
use fx_query::matcher::{CompiledQuery, Metadata, SearchView, VecView};
use fx_query::{CancelToken, Cancelled, FoldOptions, Generation, Parsed, SearchOptions, parse};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn compile_with(query: &str, opts: &SearchOptions, ctx: EvalContext) -> (Parsed, CompiledQuery) {
    let parsed = parse(query, opts);
    let compiled = CompiledQuery::compile(&parsed, opts, ctx)
        .unwrap_or_else(|e| panic!("compiling {query:?} failed: {e:?}"));
    (parsed, compiled)
}

fn compile(query: &str) -> CompiledQuery {
    compile_with(query, &SearchOptions::default(), EvalContext::default()).1
}

fn ids<V: SearchView>(view: &V, query: &str) -> Vec<u32> {
    compile(query)
        .search(view, &CancelToken::never())
        .expect("a never-cancelled search succeeds")
}

fn file_meta(size: u64) -> Metadata {
    Metadata {
        size: Some(size),
        ..Default::default()
    }
}

/// A small hand-built tree:
///
/// ```text
///  0 /                      folder (root)
///  1 /Users                 folder
///  2 /Users/me              folder
///  3 /Users/me/Résumé.pdf   size 2048
///  4 /Users/me/notes.txt    size 100
///  5 /Users/me/src          folder
///  6 /Users/me/src/main.rs
///  7 /Users/me/Report 2024.PDF
///  8 /Users/me/école.txt    (stored decomposed, NFD)
///  9 /Users/me/archive.tar.gz
/// 10 /Users/me/src.txt
/// 11 /Users/me/deleted report.txt   (deleted)
/// ```
fn hand_view() -> VecView {
    let mut v = VecView::new();
    let root = v.push("/", None, true, Metadata::default());
    let users = v.push("Users", Some(root), true, Metadata::default());
    let me = v.push("me", Some(users), true, Metadata::default());
    assert_eq!(v.push("Résumé.pdf", Some(me), false, file_meta(2048)), 3);
    v.push("notes.txt", Some(me), false, file_meta(100));
    let src = v.push("src", Some(me), true, Metadata::default());
    v.push("main.rs", Some(src), false, file_meta(10));
    v.push("Report 2024.PDF", Some(me), false, file_meta(10));
    v.push("e\u{301}cole.txt", Some(me), false, file_meta(10));
    v.push("archive.tar.gz", Some(me), false, file_meta(10));
    v.push("src.txt", Some(me), false, file_meta(10));
    let deleted = v.push("deleted report.txt", Some(me), false, file_meta(10));
    assert_eq!(deleted, 11);
    v.mark_deleted(deleted);
    v
}

// ---------------------------------------------------------------------------
// Hand cases per term kind
// ---------------------------------------------------------------------------

#[test]
fn literal_substrings_fold_case_and_diacritics() {
    let v = hand_view();
    assert_eq!(ids(&v, "resume"), vec![3]);
    assert_eq!(ids(&v, "RÉSUMÉ"), vec![3]);
    assert_eq!(ids(&v, "diacritics:resume"), Vec::<u32>::new());
    assert_eq!(ids(&v, "diacritics:résumé"), vec![3]);
    // NFC query against an NFD name, in any case.
    assert_eq!(ids(&v, "école"), vec![8]);
    assert_eq!(ids(&v, "ECOLE"), vec![8]);
    assert_eq!(ids(&v, "case:Report"), vec![7]);
    assert_eq!(ids(&v, "case:report"), Vec::<u32>::new());
    assert_eq!(ids(&v, "\"report 2024\""), vec![7]);
}

#[test]
fn anchored_and_whole_word_modifiers() {
    let v = hand_view();
    assert_eq!(ids(&v, "start-with:rep"), vec![7]);
    assert_eq!(ids(&v, "end-with:.txt"), vec![4, 8, 10]);
    assert_eq!(ids(&v, "ww:2024"), vec![7]);
    assert_eq!(ids(&v, "ww:port"), Vec::<u32>::new());
    assert_eq!(ids(&v, "wfn:notes.txt"), vec![4]);
    assert_eq!(ids(&v, "wfn:notes"), Vec::<u32>::new());
}

#[test]
fn wildcards_and_extension_patterns() {
    let v = hand_view();
    assert_eq!(ids(&v, "*.pdf"), vec![3, 7]);
    assert_eq!(ids(&v, "*.PDF"), vec![3, 7]);
    assert_eq!(ids(&v, "case:*.pdf"), vec![3]);
    assert_eq!(ids(&v, "*.gz"), vec![9]);
    assert_eq!(ids(&v, "*.tar.gz"), vec![9]);
    assert_eq!(ids(&v, "re?ort*"), vec![7]);
    assert_eq!(ids(&v, "*.txt !src*"), vec![4, 8]);
}

#[test]
fn regex_terms() {
    let v = hand_view();
    assert_eq!(ids(&v, "regex:^r.sum.\\.pdf$"), vec![3]);
    // Alternation inside a regex needs quotes: an unquoted `|` is the OR operator.
    assert_eq!(ids(&v, "regex:\"\\.(rs|gz)$\""), vec![6, 9]);
    assert_eq!(ids(&v, "case:regex:^R"), vec![3, 7]);
    assert_eq!(ids(&v, "path:regex:/src/.*\\.rs$"), vec![6]);
}

#[test]
fn invalid_regex_matches_nothing_like_the_oracle() {
    let v = hand_view();
    assert_eq!(ids(&v, "regex:("), Vec::<u32>::new());
    assert_eq!(ids(&v, "!regex:(").len(), 11);
}

#[test]
fn kind_filters() {
    let v = hand_view();
    assert_eq!(ids(&v, "folder:src"), vec![5]);
    assert_eq!(ids(&v, "file:src"), vec![10]);
    assert_eq!(ids(&v, "folder:"), vec![0, 1, 2, 5]);
}

#[test]
fn full_path_terms_are_built_from_parents() {
    let v = hand_view();
    assert_eq!(ids(&v, "path:src"), vec![5, 6, 10]);
    assert_eq!(ids(&v, "src/"), vec![6]);
    assert_eq!(ids(&v, "/users/me/"), (3..=10).collect::<Vec<u32>>());
    assert_eq!(ids(&v, "path:start-with:/users/me/src"), vec![5, 6, 10]);
    assert_eq!(ids(&v, "path:wfn:/Users/me"), vec![2]);
    // Wildcards match the whole path; `*` stops at `/`, `**` crosses it.
    assert_eq!(ids(&v, "path:**/src/*.rs"), vec![6]);
    assert_eq!(ids(&v, "**/me/*.pdf"), vec![3, 7]);
    assert_eq!(ids(&v, "path:/*/*/*.pdf"), vec![3, 7]);
    let opts = SearchOptions {
        match_path: true,
        ..Default::default()
    };
    let (_, q) = compile_with("me", &opts, EvalContext::default());
    assert_eq!(
        q.search(&v, &CancelToken::never()).unwrap(),
        (2..=10).collect::<Vec<u32>>()
    );
}

#[test]
fn functions_on_metadata() {
    let v = hand_view();
    assert_eq!(ids(&v, "ext:rs"), vec![6]);
    assert_eq!(ids(&v, "ext:pdf"), vec![3, 7]);
    assert_eq!(ids(&v, "ext:pdf;gz"), vec![3, 7, 9]);
    assert_eq!(ids(&v, "size:>1kb"), vec![3]);
    assert_eq!(ids(&v, "parent:/Users/me/src"), vec![6]);
    assert_eq!(ids(&v, "ext:txt size:<50"), vec![8, 10]);
}

#[test]
fn boolean_operators() {
    let v = hand_view();
    assert_eq!(ids(&v, "rep|main"), vec![6, 7]);
    assert_eq!(ids(&v, "pdf !report"), vec![3]);
    assert_eq!(ids(&v, "<rep|notes> !*.pdf"), vec![4]);
}

#[test]
fn deleted_entries_are_skipped() {
    let v = hand_view();
    assert_eq!(ids(&v, "report"), vec![7]);
    assert_eq!(ids(&v, "deleted"), Vec::<u32>::new());
    let all = ids(&v, "");
    assert_eq!(all, (0..=10).collect::<Vec<u32>>());
    assert_eq!(compile("").count(&v, &CancelToken::never()), Ok(11));
}

#[test]
fn count_limit_is_exposed_not_applied() {
    let mut v = VecView::new();
    let root = v.push("/", None, true, Metadata::default());
    for i in 0..10 {
        v.push(
            &format!("foo{i}.txt"),
            Some(root),
            false,
            Metadata::default(),
        );
    }
    let q = compile("count:5 foo");
    assert_eq!(q.limit(), Some(5));
    assert_eq!(q.search(&v, &CancelToken::never()).unwrap().len(), 10);
    assert_eq!(compile("foo").limit(), None);
}

#[test]
fn search_within_filters_candidates() {
    let v = hand_view();
    let q = compile("*.txt");
    let never = CancelToken::never();
    assert_eq!(
        q.search_within(&v, &[0, 4, 6, 8], &never).unwrap(),
        vec![4, 8]
    );
    assert_eq!(q.search_within(&v, &[], &never).unwrap(), Vec::<u32>::new());
    // Deleted candidates are still skipped.
    assert_eq!(q.search_within(&v, &[10, 11], &never).unwrap(), vec![10]);
}

/// A view that implements only the required methods: no metadata, no
/// folded copy.
struct NamesOnly {
    names: Vec<&'static str>,
}

impl SearchView for NamesOnly {
    fn len(&self) -> usize {
        self.names.len()
    }
    fn name(&self, id: u32) -> &str {
        self.names[id as usize]
    }
    fn parent(&self, id: u32) -> Option<u32> {
        if id == 0 { None } else { Some(0) }
    }
    fn is_folder(&self, id: u32) -> bool {
        id == 0
    }
    fn is_live(&self, _id: u32) -> bool {
        true
    }
}

#[test]
fn minimal_view_without_metadata_or_folded_copy() {
    let v = NamesOnly {
        names: vec!["/", "a.txt", "Ünïcode.TXT", "b.md"],
    };
    assert_eq!(ids(&v, "*.txt"), vec![1, 2]);
    assert_eq!(ids(&v, "unicode"), vec![2]);
    assert_eq!(ids(&v, "path:/b"), vec![3]);
    assert_eq!(ids(&v, "size:>0"), Vec::<u32>::new());
    assert_eq!(ids(&v, "dm:thisyear"), Vec::<u32>::new());
}

/// Apostrophe names, as macOS types them (`’`) and as typed on a keyboard
/// (`'`), for the apostrophe rule.
///
/// ```text
/// 0 /                                                     folder (root)
/// 1 /Downloads                                            folder
/// 2 /Downloads/Plumb’s Veterinary Drug Handbook 10th Edition.pdf
/// 3 /Desktop’s                                            folder
/// 4 /Desktop’s/plumb's notes.txt
/// 5 /Desktop’s/plumbs.txt
/// 6 /Desktop’s/plumb.txt
/// 7 /Desktop’s/Cafe\u{301}’s menu.txt                     (NFD)
/// ```
fn apostrophe_view() -> VecView {
    let mut v = VecView::new();
    let root = v.push("/", None, true, Metadata::default());
    let downloads = v.push("Downloads", Some(root), true, Metadata::default());
    v.push(
        "Plumb’s Veterinary Drug Handbook 10th Edition.pdf",
        Some(downloads),
        false,
        file_meta(1),
    );
    let desktop = v.push("Desktop’s", Some(root), true, Metadata::default());
    v.push("plumb's notes.txt", Some(desktop), false, file_meta(1));
    v.push("plumbs.txt", Some(desktop), false, file_meta(1));
    v.push("plumb.txt", Some(desktop), false, file_meta(1));
    v.push("Cafe\u{301}’s menu.txt", Some(desktop), false, file_meta(1));
    v
}

/// Every query against [`apostrophe_view`], with the expected IDs.
const APOSTROPHE_CASES: &[(&str, &[u32])] = &[
    // Without an apostrophe a term ignores the name's.
    ("plumbs", &[2, 4, 5]),
    ("plumb", &[2, 4, 5, 6]),
    ("cafes", &[7]),
    // With one, only names with an apostrophe in that place.
    ("plumb's", &[2, 4]),
    ("plumb’s", &[2, 4]),
    ("PLUMBʼS", &[2, 4]),
    ("café's", &[7]),
    // Per term.
    ("plumb's notes", &[4]),
    ("plumbs notes", &[4]),
    ("plumbs|plumb's", &[2, 4, 5]),
    ("plumbs !plumb's", &[5]),
    ("diacritics:plumbs", &[5]),
    ("diacritics:plumb's", &[4]),
    ("diacritics:plumb’s", &[2]),
    ("regex:plumb.?s", &[2, 4, 5]),
    ("regex:plumbs", &[5]),
    ("ww:plumb", &[6]),
    ("ww:plumbs", &[2, 4, 5]),
    ("ww:plumb's", &[2, 4]),
    ("diacritics:ww:plumb", &[2, 4, 6]),
    ("wfn:plumbs.txt", &[5]),
    ("start-with:plumb's", &[2, 4]),
    ("plumbs*", &[2, 4, 5]),
    ("plumb's*", &[2, 4]),
    ("plumb?s*", &[]),
    ("*'s*.txt", &[4, 7]),
    ("path:desktops", &[3, 4, 5, 6, 7]),
    ("path:desktop's", &[3, 4, 5, 6, 7]),
    ("desktop's/plumb", &[4, 5, 6]),
    ("desktop's/plumb's", &[4]),
    // One term: its apostrophe applies to the whole path it is matched
    // against, so the folder needs one too.
    ("desktops/plumb's", &[]),
    ("'", &[2, 3, 4, 7]),
    ("’", &[2, 3, 4, 7]),
    ("*'*", &[2, 3, 4, 7]),
    ("file:'", &[2, 4, 7]),
    ("path:'", &[2, 3, 4, 5, 6, 7]),
    ("diacritics:'", &[4]),
    ("diacritics:’", &[2, 3, 7]),
    ("' plumbs", &[2, 4]),
];

#[test]
fn the_apostrophe_rule_with_and_without_the_folded_copy() {
    let mut v = apostrophe_view();
    let all: Vec<u32> = (0..v.len() as u32).collect();
    for prebuilt in [false, true] {
        if prebuilt {
            // The default folded copy (case and diacritics, apostrophes
            // kept, plus the side table), as the benchmark builds it.
            v.build_folded(FoldOptions::for_match(false, false));
        }
        for &(query, want) in APOSTROPHE_CASES {
            let label = format!("{query:?} (prebuilt folded copy: {prebuilt})");
            assert_eq!(ids(&v, query), want, "{label}");
            // The oracle agrees.
            let parsed = parse(query, &SearchOptions::default());
            let oracle: Vec<u32> = all
                .iter()
                .copied()
                .filter(|&id| {
                    let name = v.name(id);
                    let parent = match v.parent(id) {
                        None => String::new(),
                        Some(0) => "/".to_string(),
                        Some(p) => format!("/{}", v.name(p)),
                    };
                    let cand = Candidate::new(name, &parent, v.is_folder(id));
                    matches_in(&parsed.query, &cand, &EvalContext::default()).unwrap()
                })
                .collect();
            assert_eq!(oracle, want, "oracle for {label}");
        }
    }
}

// ---------------------------------------------------------------------------
// Large views: chunking and cancellation
// ---------------------------------------------------------------------------

const BIG: u32 = 200_000;

fn big_view() -> VecView {
    let mut v = VecView::new();
    let root = v.push("/", None, true, Metadata::default());
    for i in 1..BIG {
        let name = if i % 1000 == 0 {
            format!("Needle{i}.pdf")
        } else {
            format!("f{i}.txt")
        };
        v.push(&name, Some(root), false, Metadata::default());
    }
    v
}

#[test]
fn large_view_results_are_complete_and_ascending() {
    let v = big_view();
    let expected: Vec<u32> = (1..BIG).filter(|i| i % 1000 == 0).collect();
    assert_eq!(ids(&v, "needle"), expected);
    assert_eq!(ids(&v, "*.pdf"), expected);
    assert_eq!(ids(&v, "").len(), BIG as usize);
    let never = CancelToken::never();
    assert_eq!(
        compile("*.txt").count(&v, &never),
        Ok((BIG - 1 - 199) as usize)
    );
}

#[test]
fn bumped_generation_cancels_search() {
    let v = big_view();
    let generation = Generation::new();
    let token = generation.token();
    let q = compile("needle");
    assert!(q.search(&v, &token).is_ok());
    generation.bump();
    assert_eq!(q.search(&v, &token), Err(Cancelled));
    assert_eq!(q.count(&v, &token), Err(Cancelled));
    let all: Vec<u32> = (0..BIG).collect();
    assert_eq!(q.search_within(&v, &all, &token), Err(Cancelled));
    // A fresh token for the new generation searches normally.
    assert_eq!(q.search(&v, &generation.token()).unwrap().len(), 199);
}

// ---------------------------------------------------------------------------
// Randomized differential test against the eval oracle
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
    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len() as u64) as usize]
    }
}

const WORDS: &[&str] = &[
    "report",
    "Report",
    "résumé",
    "Re\u{301}sume\u{301}",
    "RÉSUMÉ",
    "resume",
    "notes",
    "src",
    "Src",
    "data",
    "photo",
    "école",
    "e\u{301}cole",
    "Straße",
    "STRASSE",
    "ΣΊΣΥΦΟΣ",
    "σίσυφος",
    "naïve",
    "café",
    "CAFÉ",
    "main",
    "docs",
    "My Docs",
    "2024",
    "v2",
    "a",
    "x",
    // Apostrophes (the apostrophe rule).
    "Plumb’s",
    "Plumb's",
    "O’Brien",
    "obrien",
    "plumb's",
    "plumbs",
    "Café’s",
    "it‘s",
    "rock ʼn roll",
    "'",
];
const SEPS: &[&str] = &["", " ", "-", "_", "."];
const EXTS: &[&str] = &[
    "", ".pdf", ".PDF", ".txt", ".Txt", ".rs", ".tar.gz", ".jpg", ".é",
];

struct World {
    view: VecView,
    rows: Vec<(String, bool, Metadata)>,
    parent_paths: Vec<String>,
    live: Vec<bool>,
}

impl World {
    fn candidate(&self, id: u32) -> Candidate<'_> {
        let (name, is_dir, m) = &self.rows[id as usize];
        Candidate {
            name,
            parent_path: &self.parent_paths[id as usize],
            is_dir: *is_dir,
            size: m.size,
            modified: m.modified,
            created: m.created,
            accessed: m.accessed,
            child_count: m.child_count,
            hidden: m.hidden,
            symlink: m.symlink,
            read_only: m.read_only,
        }
    }

    fn oracle(&self, parsed: &Parsed, ctx: &EvalContext, ids: &[u32]) -> Vec<u32> {
        ids.iter()
            .copied()
            .filter(|&id| self.live[id as usize])
            .filter(|&id| matches_in(&parsed.query, &self.candidate(id), ctx).unwrap())
            .collect()
    }

    fn all_ids(&self) -> Vec<u32> {
        (0..self.rows.len() as u32).collect()
    }
}

fn random_world(entries: usize, seed: u64) -> World {
    let mut rng = Rng(seed | 1);
    const DAY: i64 = 86_400;
    let now = EvalContext::DEFAULT_NOW;
    let date = |rng: &mut Rng| {
        if rng.below(8) == 0 {
            None
        } else {
            Some(now - rng.below(800) as i64 * DAY + rng.below(DAY as u64) as i64 - DAY / 2)
        }
    };
    const SIZES: &[Option<u64>] = &[
        None,
        Some(0),
        Some(100),
        Some(1024),
        Some(1500),
        Some(50_000),
        Some(2_000_000),
        Some(5_000_000_000),
    ];

    // (name, parent, is_folder, metadata)
    let mut rows: Vec<(String, Option<u32>, bool, Metadata)> = vec![
        ("/".to_string(), None, true, Metadata::default()),
        ("/Volumes/Ext".to_string(), None, true, Metadata::default()),
    ];
    let mut folders: Vec<u32> = vec![0, 1];
    while rows.len() < entries {
        let parent = folders[rng.below(folders.len() as u64) as usize];
        let is_folder = rng.below(4) == 0;
        let mut name = rng.pick(WORDS).to_string();
        if rng.below(2) == 0 {
            name.push_str(rng.pick(SEPS));
            name.push_str(rng.pick(WORDS));
        }
        if !is_folder {
            name.push_str(rng.pick(EXTS));
        }
        let meta = Metadata {
            size: if is_folder && rng.below(4) != 0 {
                None
            } else {
                SIZES[rng.below(SIZES.len() as u64) as usize]
            },
            modified: date(&mut rng),
            created: date(&mut rng),
            accessed: date(&mut rng),
            child_count: None,
            hidden: rng.below(7) == 0,
            symlink: rng.below(11) == 0,
            read_only: rng.below(5) == 0,
        };
        let id = rows.len() as u32;
        rows.push((name, Some(parent), is_folder, meta));
        if is_folder {
            folders.push(id);
        }
    }
    // Real child counts for most folders.
    let mut children = vec![0u32; rows.len()];
    for row in &rows {
        if let Some(p) = row.1 {
            children[p as usize] += 1;
        }
    }
    for (i, row) in rows.iter_mut().enumerate() {
        if row.2 && rng.below(10) != 0 {
            row.3.child_count = Some(children[i]);
        }
    }

    let mut view = VecView::new();
    let mut full_paths: Vec<String> = Vec::with_capacity(rows.len());
    let mut parent_paths: Vec<String> = Vec::with_capacity(rows.len());
    for (name, parent, is_folder, meta) in &rows {
        let id = view.push(name, *parent, *is_folder, *meta);
        assert_eq!(id as usize, full_paths.len());
        let parent_path = parent.map_or(String::new(), |p| full_paths[p as usize].clone());
        let full = Candidate::new(name, &parent_path, *is_folder).full_path();
        parent_paths.push(parent_path);
        full_paths.push(full);
    }
    let mut live = vec![true; rows.len()];
    for (id, alive) in live.iter_mut().enumerate().skip(2) {
        if rng.below(30) == 0 {
            *alive = false;
            view.mark_deleted(id as u32);
        }
    }
    World {
        view,
        rows: rows.into_iter().map(|(n, _, d, m)| (n, d, m)).collect(),
        parent_paths,
        live,
    }
}

const QUERIES: &[&str] = &[
    // Literals and modifiers.
    "",
    "report",
    "REPORT",
    "resume",
    "résumé",
    "Re\u{301}sume\u{301}",
    "diacritics:resume",
    "diacritics:résumé",
    "case:Report",
    "case:résumé",
    "nocase:SRC",
    "école",
    "strasse",
    "straße",
    "σίσυφος",
    "ΣΊΣΥΦΟΣ",
    "naive",
    "\"my docs\"",
    "ww:src",
    "ww:a",
    "noww:a",
    "wfn:src",
    "wfn:notes.txt",
    "start-with:rep",
    "end-with:.pdf",
    "end-with:e",
    "file:src",
    "folder:src",
    "folder:",
    "file:",
    // Wildcards.
    "*.pdf",
    "*.PDF",
    "case:*.pdf",
    "*.tar.gz",
    "*.gz",
    "*.é",
    "*.e",
    "rep*",
    "*2024*",
    "?????",
    "re?ort*",
    "*caf?*",
    "folder:*s",
    "wfn:*.txt",
    // Regex.
    "regex:^rep",
    "regex:\\.(pdf|txt)$",
    "case:regex:^R",
    "regex:é",
    "regex:^r.sum.",
    "!regex:\\d",
    "regex:(",
    "path:regex:/src/.*\\.rs$",
    "path:regex:^/Volumes",
    // Paths.
    "path:src",
    "path:docs",
    "src/",
    "/volumes/",
    "/Volumes/Ext/",
    "path:*.rs",
    "path:*/src/*",
    "**/src/**",
    "path:ww:src",
    "path:wfn:/Volumes/Ext",
    "path:start-with:/volumes",
    "path:end-with:.rs",
    "path:case:Src",
    "path:diacritics:école",
    "path:folder:data",
    "nopath:src",
    // Functions.
    "ext:pdf",
    "ext:pdf;txt",
    "ext:é",
    "size:>1kb",
    "size:unknown",
    "size:empty",
    "size:large|size:huge",
    "dm:today",
    "dm:thisyear",
    "dm:lastmonth",
    "dc:thisweek",
    "da:2026",
    "parent:/Volumes/Ext",
    "parent:/",
    "depth:2",
    "depth:>3",
    "len:<6",
    "len:4..8",
    "empty:",
    "attrib:H",
    "attrib:RL",
    "attrib:D",
    "attrib:X",
    // Boolean structure.
    "rep pdf",
    "report|src",
    "!report",
    "<report|notes> !*.pdf",
    "folder: !src",
    "ext:pdf size:>1kb | ext:txt",
    "path:src *.rs | regex:^caf",
    "!<file: dm:thisyear> ww:docs",
    "count:3 report",
    "size:abc report",
    // Apostrophes (the apostrophe rule).
    "plumbs",
    "plumb's",
    "PLUMB'S",
    "plumb's report",
    "plumbs !plumb's",
    "o'brien",
    "obrien",
    "ww:o",
    "ww:o'brien",
    "wfn:o'brien",
    "plumb?s",
    "plumb's*",
    "o?brien*",
    "path:o'brien",
    "path:obrien",
    "o'brien/",
    "plumb’s",
    "cafes",
    "its",
    "diacritics:plumb's",
    "diacritics:plumb’s",
    "case:Plumbs",
    "regex:plumb.?s",
    "ww:plumb",
    "ww:plumbs",
    "ww:plumb's",
    "wfn:plumbs",
    "start-with:plumbs",
    "end-with:'s",
    "plumbs*",
    "*'s",
    "'",
    "’",
    "''",
    "*'*",
    "'*",
    "file:'",
    "path:'",
    "diacritics:'",
    "' report",
    "path:plumbs",
    "plumb's/",
];

fn option_sets() -> Vec<(&'static str, SearchOptions)> {
    let d = SearchOptions::default();
    vec![
        ("default", d),
        (
            "match_case",
            SearchOptions {
                match_case: true,
                ..d
            },
        ),
        (
            "match_diacritics",
            SearchOptions {
                match_diacritics: true,
                ..d
            },
        ),
        (
            "match_path",
            SearchOptions {
                match_path: true,
                ..d
            },
        ),
        (
            "whole_word",
            SearchOptions {
                whole_word: true,
                ..d
            },
        ),
    ]
}

fn check_corpus(
    world: &World,
    queries: &[&str],
    opts_label: &str,
    opts: &SearchOptions,
    ctx: EvalContext,
    failures: &mut Vec<String>,
) -> usize {
    let all = world.all_ids();
    let never = CancelToken::never();
    let mut non_empty = 0;
    for &query in queries {
        let (parsed, compiled) = compile_with(query, opts, ctx);
        let expected = world.oracle(&parsed, &ctx, &all);
        let got = compiled.search(&world.view, &never).unwrap();
        if !expected.is_empty() {
            non_empty += 1;
        }
        if got != expected {
            let missing: Vec<_> = expected
                .iter()
                .filter(|i| !got.contains(i))
                .take(3)
                .collect();
            let extra: Vec<_> = got
                .iter()
                .filter(|i| !expected.contains(i))
                .take(3)
                .collect();
            failures.push(format!(
                "{query:?} [{opts_label}]: got {} ids, oracle {}; missing e.g. {:?}, extra e.g. {:?}",
                got.len(),
                expected.len(),
                missing
                    .iter()
                    .map(|&&i| world.candidate(i).full_path())
                    .collect::<Vec<_>>(),
                extra
                    .iter()
                    .map(|&&i| world.candidate(i).full_path())
                    .collect::<Vec<_>>(),
            ));
            continue;
        }
        let count = compiled.count(&world.view, &never).unwrap();
        if count != expected.len() {
            failures.push(format!(
                "{query:?} [{opts_label}]: count {count} != {}",
                expected.len()
            ));
        }
        // Refinement over an arbitrary subset agrees with the oracle too.
        let subset: Vec<u32> = all.iter().copied().filter(|i| i % 3 != 1).collect();
        let within = compiled
            .search_within(&world.view, &subset, &never)
            .unwrap();
        let expected_within: Vec<u32> = expected.iter().copied().filter(|i| i % 3 != 1).collect();
        if within != expected_within {
            failures.push(format!("{query:?} [{opts_label}]: search_within differs"));
        }
    }
    non_empty
}

fn assert_no_failures(failures: &[String]) {
    assert!(
        failures.is_empty(),
        "{} mismatches against the eval oracle:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn differential_against_eval_oracle_across_options() {
    let world = random_world(2000, 0x9E37_79B9_7F4A_7C15);
    let ctx = EvalContext::default();
    let mut failures = Vec::new();
    for (label, opts) in option_sets() {
        let non_empty = check_corpus(&world, QUERIES, label, &opts, ctx, &mut failures);
        assert!(
            non_empty >= QUERIES.len() / 2,
            "[{label}] only {non_empty} queries matched anything; the generator is too sparse"
        );
    }
    assert_no_failures(&failures);
}

#[test]
fn differential_with_the_prebuilt_folded_copy() {
    // The default folded copy serves name terms that fold case and
    // diacritics: a term with an apostrophe reads the main copy, one
    // without reads the side table for names with an apostrophe. Every
    // other term (`case:`, `diacritics:`) must fall back to folding per
    // entry and still agree with the oracle.
    // Only the option sets whose name terms use the copy are run here.
    let mut world = random_world(2000, 0x51_7CC1_B727_220A);
    world
        .view
        .build_folded(FoldOptions::for_match(false, false));
    let ctx = EvalContext::default();
    let mut failures = Vec::new();
    for (label, opts) in option_sets() {
        if label == "default" || label == "whole_word" {
            check_corpus(&world, QUERIES, label, &opts, ctx, &mut failures);
        }
    }
    assert_no_failures(&failures);
}

#[test]
fn differential_second_seed_and_time_zone() {
    let world = random_world(3000, 0xD1B5_4A32_D192_ED03);
    let ctx = EvalContext {
        now: EvalContext::DEFAULT_NOW + 3 * 3600,
        utc_offset_secs: -5 * 3600,
    };
    let mut failures = Vec::new();
    check_corpus(
        &world,
        QUERIES,
        "default, UTC-5",
        &SearchOptions::default(),
        ctx,
        &mut failures,
    );
    assert_no_failures(&failures);
}

#[test]
fn differential_global_regex_toggle() {
    let world = random_world(1500, 42);
    let opts = SearchOptions {
        regex: true,
        ..Default::default()
    };
    let queries = [
        "^rep",
        "\\.(pdf|txt)$",
        "é",
        "^r.sum.",
        "noregex:*.pdf",
        "noregex:report",
        "path:/src/",
        "case:^R ext:pdf",
        "^caf|^nai",
        "(",
        "!\\d folder:",
    ];
    let mut failures = Vec::new();
    check_corpus(
        &world,
        &queries,
        "regex",
        &opts,
        EvalContext::default(),
        &mut failures,
    );
    assert_no_failures(&failures);
}

#[test]
fn refinement_of_a_growing_query_matches_fresh_search() {
    let world = random_world(2000, 7);
    let never = CancelToken::never();
    let typed = [
        "r", "re", "rep", "repo", "repor", "report", "report ", "report p",
    ];
    let mut previous = compile("").search(&world.view, &never).unwrap();
    for q in typed {
        let compiled = compile(q);
        let fresh = compiled.search(&world.view, &never).unwrap();
        let refined = compiled
            .search_within(&world.view, &previous, &never)
            .unwrap();
        assert_eq!(refined, fresh, "refining to {q:?}");
        previous = refined;
    }
}

// ---------------------------------------------------------------------------
// Full paths: results are identical however paths are matched
// ---------------------------------------------------------------------------

/// Folder and file words for [`path_world`]: case, accents (NFC and NFD),
/// apostrophes, repeated and overlapping words, so path needles cross
/// components in many ways.
const PATH_WORDS: &[&str] = &[
    "Resources",
    "resources",
    "res",
    "src",
    "Src",
    "main",
    "a",
    "ab",
    "b",
    "ba",
    "Résumé",
    "Re\u{301}sume\u{301}",
    "Joe’s",
    "joe's",
    "joes",
    "Library",
    "x.y",
    "e\u{301}",
    "'",
    "Plumb’s",
    "ΣΊΣΥΦΟΣ",
    "日本",
];

/// A deep random tree for path matching: four roots (`/`, `/Volumes/Ext`,
/// an empty name and a relative `rel`), folders mostly nested in the
/// latest folders so paths get long, an empty folder name now and then,
/// and some deleted entries.
fn path_world(entries: usize, seed: u64) -> World {
    let mut rng = Rng(seed | 1);
    let mut rows: Vec<(String, Option<u32>, bool, Metadata)> = vec![
        ("/".to_string(), None, true, Metadata::default()),
        ("/Volumes/Ext".to_string(), None, true, Metadata::default()),
        (String::new(), None, true, Metadata::default()),
        ("rel".to_string(), None, true, Metadata::default()),
    ];
    let mut folders: Vec<u32> = vec![0, 1, 2, 3];
    while rows.len() < entries {
        let parent = if rng.below(3) == 0 {
            folders[rng.below(folders.len() as u64) as usize]
        } else {
            // One of the latest folders, so the tree grows deep.
            let recent = folders.len().min(8);
            folders[folders.len() - 1 - rng.below(recent as u64) as usize]
        };
        let is_folder = rng.below(3) == 0;
        let name = if is_folder && rng.below(60) == 0 {
            String::new()
        } else {
            let mut name = rng.pick(PATH_WORDS).to_string();
            if rng.below(3) == 0 {
                name.push_str(rng.pick(&["", " ", "-", "."]));
                name.push_str(rng.pick(PATH_WORDS));
            }
            if !is_folder {
                name.push_str(rng.pick(&["", ".rs", ".TXT", ".pdf"]));
            }
            name
        };
        let id = rows.len() as u32;
        rows.push((name, Some(parent), is_folder, Metadata::default()));
        if is_folder {
            folders.push(id);
        }
    }
    let mut view = VecView::new();
    let mut full_paths: Vec<String> = Vec::with_capacity(rows.len());
    let mut parent_paths: Vec<String> = Vec::with_capacity(rows.len());
    for (name, parent, is_folder, meta) in &rows {
        view.push(name, *parent, *is_folder, *meta);
        let parent_path = parent.map_or(String::new(), |p| full_paths[p as usize].clone());
        full_paths.push(Candidate::new(name, &parent_path, *is_folder).full_path());
        parent_paths.push(parent_path);
    }
    let mut live = vec![true; rows.len()];
    for (id, alive) in live.iter_mut().enumerate().skip(4) {
        if rng.below(25) == 0 {
            *alive = false;
            view.mark_deleted(id as u32);
        }
    }
    World {
        view,
        rows: rows.into_iter().map(|(n, _, d, m)| (n, d, m)).collect(),
        parent_paths,
        live,
    }
}

/// Path queries: substrings within one name, across separators (several
/// components, the roots' own slashes), longer than any name, empty, with
/// apostrophes, and the constrained, wildcard and regex forms.
const PATH_QUERIES: &[&str] = &[
    "path:",
    "path:a",
    "path:res",
    "path:zqxj",
    "path:/",
    "path://",
    "path:/v",
    "path:/volumes/ext/",
    "path:ext/r",
    "path:resources/",
    "path:/resources/",
    "path:resources/src/main",
    "path:s/m",
    "path:b/a",
    "path:a/a/a",
    "path:/a",
    "path:a/",
    "path:rel/",
    "path:resources/resources/resources/resources",
    "path:résumé/",
    "path:resume/",
    "path:re\u{301}sume\u{301}/",
    "path:joes/",
    "path:joe's/",
    "path:'/",
    "path:/'",
    "path:''",
    "path:日本/",
    "path:σίσυφος/",
    "path:x.y/",
    "path:e/",
    "path:.rs",
    "resources/",
    "src/main",
    "/volumes/",
    "ext/",
    "path:case:Resources/",
    "path:diacritics:résumé/",
    "path:file:res/",
    "path:folder:src/",
    "path:res !path:src/",
    "path:src/ | path:joes/",
    "path:src/ path:a/",
    "path:ww:src/",
    "path:start-with:/volumes",
    "path:end-with:/main.rs",
    "path:wfn:/volumes/ext",
    "path:*/src/*",
    "**/resources/**",
    "path:regex:/src/[^/]*$",
    "parent:/volumes/ext",
    "depth:3",
];

#[test]
fn full_path_differential_against_eval_oracle() {
    let world = path_world(3000, 0xA076_1D64_78BD_642F);
    let ctx = EvalContext::default();
    let mut failures = Vec::new();
    for (label, opts) in option_sets() {
        let non_empty = check_corpus(&world, PATH_QUERIES, label, &opts, ctx, &mut failures);
        assert!(
            non_empty >= PATH_QUERIES.len() / 2,
            "[{label}] only {non_empty} path queries matched anything"
        );
    }
    assert_no_failures(&failures);
}

#[test]
fn full_path_differential_with_the_prebuilt_folded_copy() {
    let mut world = path_world(3000, 0xE703_7ED1_A0B4_28DB);
    world
        .view
        .build_folded(FoldOptions::for_match(false, false));
    let mut failures = Vec::new();
    for (label, opts) in option_sets() {
        check_corpus(
            &world,
            PATH_QUERIES,
            label,
            &opts,
            EvalContext::default(),
            &mut failures,
        );
    }
    assert_no_failures(&failures);
}

#[test]
fn full_path_differential_over_many_parallel_chunks() {
    // Several 16,384-entry chunks, so threads share the per-search folder
    // state while they fill it.
    let mut world = path_world(90_000, 0x8EBC_6AF0_9C88_C6E3);
    world
        .view
        .build_folded(FoldOptions::for_match(false, false));
    let queries = [
        "path:resources/",
        "path:zqxj",
        "path:s/m",
        "path:a/a/a",
        "path:joe's/",
        "path:/volumes/ext/",
        "path:res !path:src/",
    ];
    let mut failures = Vec::new();
    check_corpus(
        &world,
        &queries,
        "default, parallel",
        &SearchOptions::default(),
        EvalContext::default(),
        &mut failures,
    );
    assert_no_failures(&failures);
}

#[test]
fn full_path_is_match_agrees_with_search() {
    let world = path_world(1500, 0x2545_F491_4F6C_DD1D);
    let never = CancelToken::never();
    for &query in PATH_QUERIES {
        let compiled = compile(query);
        let found = compiled.search(&world.view, &never).unwrap();
        let single: Vec<u32> = world
            .all_ids()
            .into_iter()
            .filter(|&id| world.live[id as usize] && compiled.is_match(&world.view, id))
            .collect();
        assert_eq!(single, found, "{query:?}");
    }
}
