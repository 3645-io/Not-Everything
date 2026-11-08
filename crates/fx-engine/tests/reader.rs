//! The read-only store reader used by fx-cli, with fakes, on every
//! platform.
//!
//! Contract these tests rely on (in addition to `tests/common/mod.rs`):
//!
//! - `fx_engine::StoreReader::open(path) -> Result<StoreReader, ReadError>`
//!   loads an fx-store checkpoint (as written by `Engine::checkpoint`) and
//!   never writes, renames or creates anything: it works on a read-only
//!   file in a read-only folder. It starts no change source.
//! - `fx_engine::ReadError` implements `std::error::Error`. A missing store
//!   is `ReadError::NotFound { .. }` (any payload); a store fx-store rejects
//!   (corrupt, wrong magic, truncated) is `ReadError::Rejected { .. }`,
//!   wrapping the `fx_store::LoadError`.
//! - `fx_engine::ReadOptions` has public `bool` fields `match_case`,
//!   `whole_word`, `match_path`, `match_diacritics` and `regex`, implements
//!   `Default` (all off, like `fx_query::SearchOptions::default()`), and is
//!   mapped onto the fx-query toggles of the same names.
//! - `StoreReader::search(&self, query, &ReadOptions, SortKey, Direction,
//!   offset: usize, limit: usize) -> Result<P, SearchError>` where `P` has
//!   public fields `total: usize` (all matches) and `ids: Vec<u32>` (entry
//!   IDs at positions `offset .. offset + limit` of the sorted matches).
//!   With default options it returns the same results as `Engine::search`
//!   over the same index.
//! - `fx_engine::SearchError` implements `std::error::Error`. A query that
//!   cannot be compiled is an `Err` (the variant is not pinned); a
//!   malformed query never panics. A regular expression that does not
//!   compile (the whole query with the `regex` toggle, or any `regex:`
//!   term) is an `Err` with a non-empty message, never an empty result;
//!   query syntax the parser recovers from (an unclosed quote, ...) is
//!   `Ok`.
//! - Row accessors for an ID returned by `search`: `full_path(id)` and
//!   `name(id)` return an `Option` of a string (`String` or `&str`);
//!   `is_folder(id) -> bool`; `size(id) -> Option<u64>` (`None` for
//!   folders); `modified(id) -> Option<i64>` and `created(id) ->
//!   Option<i64>` in Unix seconds. The full path is built from parent
//!   pointers like the macOS UI does: names joined with `/` from the root
//!   entry down, with a root entry named `/` giving `/a/b` (not `//a/b`).

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::*;
use fx_engine::{Engine, EngineConfig, ReadError, ReadOptions, SearchError, StoreReader};
use fx_index::Index;
use fx_model::{Entry, EntryFlags, EntryId, EntryKind, FileKey, StartOutcome, VolumeId};
use fx_sort::{Direction, SortKey};

/// A unique scratch directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "fx-engine-reader-{name}-{}-{nanos}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).expect("create scratch dir");
        TempDir(dir)
    }

    fn store(&self) -> PathBuf {
        self.0.join("store").join("index.fxdb")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fkey(id: u64) -> FileKey {
    FileKey::new(VolumeId(1), id)
}

/// The fixture: (key, parent key, name, is folder, size, modified, created).
/// The root entry is named `/`, as when the whole volume is indexed.
const FIXTURE: &[(u64, u64, &str, bool, u64, i64, i64)] = &[
    (1, 0, "/", true, 0, 1_600_000_000, 1_500_000_000),
    (2, 1, "docs", true, 0, 1_600_000_010, 1_500_000_010),
    (
        3,
        2,
        "report.txt",
        false,
        4_000,
        1_600_000_300,
        1_500_000_300,
    ),
    (4, 2, "Notes.txt", false, 120, 1_600_000_100, 1_500_000_100),
    (5, 2, "café.md", false, 9_999, 1_600_000_200, 1_500_000_200),
    (6, 1, "src", true, 0, 1_600_000_020, 1_500_000_020),
    (7, 6, "main.rs", false, 2_048, 1_600_000_400, 1_500_000_400),
    (
        8,
        6,
        "report_old.txt",
        false,
        50,
        1_600_000_050,
        1_500_000_050,
    ),
    (9, 6, "docs", true, 0, 1_600_000_030, 1_500_000_030),
    (
        10,
        9,
        "readme.txt",
        false,
        700,
        1_600_000_500,
        1_500_000_500,
    ),
    (
        11,
        1,
        "transport.log",
        false,
        333,
        1_600_000_600,
        1_500_000_600,
    ),
];

fn fixture_index() -> Index {
    let mut index = Index::new();
    for &(k, parent, name, folder, size, modified, created) in FIXTURE {
        index
            .upsert(Entry {
                key: fkey(k),
                parent: (parent != 0).then(|| fkey(parent)),
                name: name.to_string(),
                kind: if folder {
                    EntryKind::Dir
                } else {
                    EntryKind::File
                },
                flags: EntryFlags::empty(),
                size,
                modified,
                created,
            })
            .expect("upsert into a fresh index");
    }
    index
}

/// Starts an engine over the fixture (root `/`) that checkpoints to
/// `store`, and saves one checkpoint. No changes are ever pushed, so the
/// engine keeps serving the fixture as given.
fn engine_with_store(store: &Path) -> Engine {
    let tree = FakeTree::new();
    let source = FakeSource::new(StartOutcome::Resumed);
    let config = EngineConfig::new("/")
        .with_batch_window(Duration::from_millis(50))
        .with_resume_from(source.current_cursor())
        .with_store_path(store)
        .with_checkpoint_idle(Duration::from_secs(3600));
    let engine =
        Engine::start(fixture_index(), config, source, tree.clone(), tree).expect("engine starts");
    engine.checkpoint().expect("checkpoint saved");
    assert!(store.is_file(), "the checkpoint exists at {store:?}");
    engine
}

/// A store holding the fixture, written by a dropped engine (dropping
/// saves no further checkpoint).
fn fixture_store(dir: &TempDir) -> PathBuf {
    let store = dir.store();
    drop(engine_with_store(&store));
    store
}

/// The full path of an engine snapshot entry, folded like the UI does.
fn engine_path(snap: &fx_engine::EngineSnapshot, id: u32) -> String {
    let raw = snap.index().path(EntryId(id));
    match raw.strip_prefix('/') {
        Some(rest) if rest.starts_with('/') => rest.to_string(),
        _ => raw,
    }
}

fn reader_paths(reader: &StoreReader, ids: &[u32]) -> Vec<String> {
    ids.iter()
        .map(|&id| {
            reader
                .full_path(id)
                .as_deref()
                .unwrap_or_else(|| panic!("id {id} from search has a path"))
                .to_string()
        })
        .collect()
}

fn open(store: &Path) -> StoreReader {
    match StoreReader::open(store) {
        Ok(reader) => reader,
        Err(e) => panic!("store at {store:?} opens: {e}"),
    }
}

/// Full paths of every match, in `sort` order, with `opts`.
fn find(reader: &StoreReader, query: &str, opts: &ReadOptions) -> Vec<String> {
    let page = reader
        .search(
            query,
            opts,
            SortKey::Path,
            Direction::Ascending,
            0,
            usize::MAX,
        )
        .unwrap_or_else(|_| panic!("query {query:?} runs"));
    assert_eq!(page.total, page.ids.len(), "query {query:?}: whole window");
    reader_paths(reader, &page.ids)
}

fn defaults() -> ReadOptions {
    ReadOptions::default()
}

#[test]
fn reader_matches_engine_search_for_queries_sorts_and_pages() {
    let dir = TempDir::new("same");
    let store = dir.store();
    let engine = engine_with_store(&store);
    let snap = engine.snapshot();
    let reader = open(&store);

    let queries = [
        "",
        "report",
        "txt",
        "docs",
        "*.txt",
        "ext:txt",
        "!report",
        "notes | main",
    ];
    let sorts = [
        SortKey::Name,
        SortKey::Path,
        SortKey::Size,
        SortKey::Modified,
    ];
    let dirs = [Direction::Ascending, Direction::Descending];
    let windows = [(0, usize::MAX), (0, 2), (1, 3), (5, 100), (50, 10)];
    for query in queries {
        for sort in sorts {
            for direction in dirs {
                for (offset, limit) in windows {
                    let want = engine
                        .search(
                            query,
                            fx_engine::QueryOptions::default(),
                            sort,
                            direction,
                            offset,
                            limit,
                        )
                        .expect("engine search");
                    let got = reader
                        .search(query, &defaults(), sort, direction, offset, limit)
                        .unwrap_or_else(|_| panic!("reader search {query:?}"));
                    let what = format!("{query:?} {sort:?} {direction:?} {offset}+{limit}");
                    assert_eq!(got.total, want.total, "total for {what}");
                    let want_paths: Vec<String> =
                        want.ids.iter().map(|&id| engine_path(&snap, id)).collect();
                    assert_eq!(
                        reader_paths(&reader, &got.ids),
                        want_paths,
                        "rows for {what}"
                    );
                }
            }
        }
    }
    engine.shutdown();
}

#[test]
fn search_returns_total_and_sorted_paged_ids() {
    let dir = TempDir::new("paging");
    let reader = open(&fixture_store(&dir));

    let all = reader
        .search(
            "txt",
            &defaults(),
            SortKey::Name,
            Direction::Ascending,
            0,
            usize::MAX,
        )
        .expect("search");
    assert_eq!(all.total, 4);
    let names: Vec<String> = all
        .ids
        .iter()
        .map(|&id| reader.name(id).as_deref().expect("name").to_string())
        .collect();
    assert_eq!(
        names,
        ["Notes.txt", "readme.txt", "report.txt", "report_old.txt"]
    );

    let page = reader
        .search(
            "txt",
            &defaults(),
            SortKey::Name,
            Direction::Ascending,
            1,
            2,
        )
        .expect("search");
    assert_eq!(page.total, 4, "total counts every match, not the window");
    assert_eq!(page.ids, all.ids[1..3].to_vec());

    let past_end = reader
        .search(
            "txt",
            &defaults(),
            SortKey::Name,
            Direction::Ascending,
            10,
            5,
        )
        .expect("search");
    assert_eq!(past_end.total, 4);
    assert!(past_end.ids.is_empty());

    let by_size = reader
        .search(
            "txt",
            &defaults(),
            SortKey::Size,
            Direction::Descending,
            0,
            usize::MAX,
        )
        .expect("search");
    let sizes: Vec<Option<u64>> = by_size.ids.iter().map(|&id| reader.size(id)).collect();
    assert_eq!(sizes, [Some(4_000), Some(700), Some(120), Some(50)]);

    let everything = reader
        .search(
            "",
            &defaults(),
            SortKey::Path,
            Direction::Ascending,
            0,
            usize::MAX,
        )
        .expect("search");
    assert_eq!(everything.total, FIXTURE.len(), "an empty query lists all");
}

#[test]
fn rows_give_full_path_name_kind_size_and_times() {
    let dir = TempDir::new("rows");
    let reader = open(&fixture_store(&dir));

    let page = reader
        .search(
            "",
            &defaults(),
            SortKey::Path,
            Direction::Ascending,
            0,
            usize::MAX,
        )
        .expect("search");
    let paths = reader_paths(&reader, &page.ids);
    assert_eq!(
        paths,
        [
            "/",
            "/docs",
            "/docs/café.md",
            "/docs/Notes.txt",
            "/docs/report.txt",
            "/src",
            "/src/docs",
            "/src/docs/readme.txt",
            "/src/main.rs",
            "/src/report_old.txt",
            "/transport.log",
        ]
    );

    let by_path: BTreeMap<String, u32> = paths.into_iter().zip(page.ids).collect();
    let report = by_path["/docs/report.txt"];
    assert_eq!(reader.name(report).as_deref(), Some("report.txt"));
    assert!(!reader.is_folder(report));
    assert_eq!(reader.size(report), Some(4_000));
    assert_eq!(reader.modified(report), Some(1_600_000_300));
    assert_eq!(reader.created(report), Some(1_500_000_300));

    let nested = by_path["/src/docs"];
    assert_eq!(reader.name(nested).as_deref(), Some("docs"));
    assert!(reader.is_folder(nested));
    assert_eq!(reader.size(nested), None, "folder sizes are not tracked");
    assert_eq!(reader.modified(nested), Some(1_600_000_030));

    let readme = by_path["/src/docs/readme.txt"];
    assert_eq!(reader.size(readme), Some(700));
    assert_eq!(reader.created(readme), Some(1_500_000_500));
}

#[test]
fn toggles_map_to_the_query_options() {
    let dir = TempDir::new("toggles");
    let reader = open(&fixture_store(&dir));

    // Match case.
    assert_eq!(find(&reader, "notes", &defaults()), ["/docs/Notes.txt"]);
    let case = ReadOptions {
        match_case: true,
        ..defaults()
    };
    assert!(find(&reader, "notes", &case).is_empty());
    assert_eq!(find(&reader, "Notes", &case), ["/docs/Notes.txt"]);

    // Whole word: "port" is inside "report" and "transport" only.
    assert_eq!(
        find(&reader, "port", &defaults()),
        ["/docs/report.txt", "/src/report_old.txt", "/transport.log"]
    );
    let word = ReadOptions {
        whole_word: true,
        ..defaults()
    };
    assert!(find(&reader, "port", &word).is_empty());
    assert_eq!(find(&reader, "report.txt", &word), ["/docs/report.txt"]);

    // Match path: "docs" then also matches everything under a docs folder.
    assert_eq!(find(&reader, "docs", &defaults()), ["/docs", "/src/docs"]);
    let path = ReadOptions {
        match_path: true,
        ..defaults()
    };
    assert_eq!(
        find(&reader, "docs", &path),
        [
            "/docs",
            "/docs/café.md",
            "/docs/Notes.txt",
            "/docs/report.txt",
            "/src/docs",
            "/src/docs/readme.txt",
        ]
    );

    // Diacritics: off folds "é" to "e".
    assert_eq!(find(&reader, "cafe", &defaults()), ["/docs/café.md"]);
    let marks = ReadOptions {
        match_diacritics: true,
        ..defaults()
    };
    assert!(find(&reader, "cafe", &marks).is_empty());
    assert_eq!(find(&reader, "café", &marks), ["/docs/café.md"]);

    // Regex.
    let pattern = r"^rep.*\.txt$";
    assert!(find(&reader, pattern, &defaults()).is_empty());
    let regex = ReadOptions {
        regex: true,
        ..defaults()
    };
    assert_eq!(
        find(&reader, pattern, &regex),
        ["/docs/report.txt", "/src/report_old.txt"]
    );
}

#[test]
fn malformed_queries_never_panic() {
    let dir = TempDir::new("malformed");
    let reader = open(&fixture_store(&dir));
    let regex = ReadOptions {
        regex: true,
        ..defaults()
    };
    let plain = defaults();
    let cases: [(&str, &ReadOptions); 7] = [
        ("\"unclosed", &plain),
        ("<report", &plain),
        ("report>", &plain),
        ("a |", &plain),
        ("size:", &plain),
        ("dm:notadate", &plain),
        ("[unclosed(", &regex),
    ];
    for (query, opts) in cases {
        // Returning at all (Ok or Err) is the contract: no panic.
        let _ = reader.search(query, opts, SortKey::Name, Direction::Ascending, 0, 10);
    }
}

#[test]
fn an_invalid_regex_is_a_search_error() {
    let dir = TempDir::new("badregex");
    let reader = open(&fixture_store(&dir));
    let regex = ReadOptions {
        regex: true,
        ..defaults()
    };
    let plain = defaults();
    let cases: [(&str, &ReadOptions); 6] = [
        ("(", &regex),
        ("[unclosed(", &regex),
        ("regex:[bad", &plain),
        ("report regex:[bad", &plain),
        ("report | regex:[bad", &plain),
        ("!regex:[bad", &plain),
    ];
    for (query, opts) in cases {
        let result: Result<_, SearchError> =
            reader.search(query, opts, SortKey::Name, Direction::Ascending, 0, 10);
        match result {
            Err(e) => {
                let as_error: &dyn std::error::Error = &e;
                assert!(
                    !as_error.to_string().trim().is_empty(),
                    "query {query:?} ({opts:?}): the error says why"
                );
            }
            Ok(page) => panic!(
                "query {query:?} ({opts:?}) has an invalid regex and must be an error, \
                 got {} results",
                page.total
            ),
        }
    }

    // Valid regexes and recovered syntax still search.
    assert_eq!(
        find(&reader, "regex:^report\\.txt$", &plain),
        ["/docs/report.txt"]
    );
    assert_eq!(
        find(&reader, "^report\\.txt$", &regex),
        ["/docs/report.txt"]
    );
    for query in ["\"unclosed", "(", "[unclosed("] {
        assert!(
            reader
                .search(query, &plain, SortKey::Name, Direction::Ascending, 0, 10)
                .is_ok(),
            "{query:?} is not a regex without the toggle, so it searches"
        );
    }
}

#[test]
fn missing_store_is_not_found() {
    let dir = TempDir::new("missing");
    let store = dir.store();
    match StoreReader::open(&store) {
        Err(ReadError::NotFound { .. }) => {}
        Err(other) => panic!("a missing store is NotFound, got {other:?}"),
        Ok(_) => panic!("a missing store does not open"),
    }
    assert!(!store.exists(), "opening creates no store");
    assert!(
        !store.parent().unwrap().exists(),
        "opening creates no folder"
    );
}

#[test]
fn corrupt_store_is_rejected_and_left_alone() {
    let dir = TempDir::new("corrupt");
    let store = dir.store();
    fs::create_dir_all(store.parent().unwrap()).unwrap();

    let garbage = b"this is not an fx-store checkpoint at all, just some bytes".to_vec();
    fs::write(&store, &garbage).unwrap();
    let err = match StoreReader::open(&store) {
        Err(e) => e,
        Ok(_) => panic!("garbage does not open"),
    };
    assert!(
        matches!(err, ReadError::Rejected { .. }),
        "garbage is Rejected, got {err:?}"
    );
    let err: &dyn std::error::Error = &err;
    assert!(!err.to_string().is_empty());
    assert_eq!(fs::read(&store).unwrap(), garbage, "the file is untouched");

    // A real checkpoint cut short is rejected too.
    let other = TempDir::new("corrupt-src");
    let good = fs::read(fixture_store(&other)).unwrap();
    let truncated = good[..good.len() / 2].to_vec();
    fs::write(&store, &truncated).unwrap();
    assert!(matches!(
        StoreReader::open(&store),
        Err(ReadError::Rejected { .. })
    ));
    assert_eq!(
        fs::read(&store).unwrap(),
        truncated,
        "the file is untouched"
    );

    // An empty file as well.
    fs::write(&store, b"").unwrap();
    assert!(matches!(
        StoreReader::open(&store),
        Err(ReadError::Rejected { .. })
    ));
}

/// Names and contents of every file under `dir`.
fn listing(dir: &Path) -> BTreeMap<PathBuf, (Vec<u8>, Option<SystemTime>)> {
    let mut out = BTreeMap::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let meta = fs::metadata(&path).unwrap();
        let bytes = if meta.is_file() {
            fs::read(&path).unwrap()
        } else {
            Vec::new()
        };
        out.insert(path, (bytes, meta.modified().ok()));
    }
    out
}

#[test]
fn reading_leaves_the_store_unchanged() {
    let dir = TempDir::new("unchanged");
    let store = fixture_store(&dir);
    let folder = store.parent().unwrap().to_path_buf();
    let before = listing(&folder);

    {
        let reader = open(&store);
        for query in ["", "report", "docs"] {
            let page = reader
                .search(
                    query,
                    &defaults(),
                    SortKey::Modified,
                    Direction::Descending,
                    0,
                    3,
                )
                .expect("search");
            for &id in &page.ids {
                let _ = reader.full_path(id);
                let _ = reader.size(id);
            }
        }
        let again = open(&store);
        drop(again);
    }

    assert_eq!(
        listing(&folder),
        before,
        "the store folder holds the same files with the same bytes and times"
    );
}

#[cfg(unix)]
#[test]
fn opens_a_read_only_store_in_a_read_only_folder() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new("readonly");
    let store = fixture_store(&dir);
    let folder = store.parent().unwrap().to_path_buf();
    fs::set_permissions(&store, fs::Permissions::from_mode(0o444)).unwrap();
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o555)).unwrap();
    let probe = folder.join("write-probe");
    if fs::write(&probe, b"").is_ok() {
        // As root the folder stays writable, so the test could not show that
        // opening needs no write access.
        let _ = fs::remove_file(&probe);
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&store, fs::Permissions::from_mode(0o644)).unwrap();
        eprintln!("skipping: running with privileges that ignore read-only modes");
        return;
    }

    let result = StoreReader::open(&store).map(|reader| {
        reader
            .search(
                "report",
                &defaults(),
                SortKey::Name,
                Direction::Ascending,
                0,
                10,
            )
            .map(|page| page.total)
            .ok()
    });

    // Restore permissions so the scratch dir can be removed.
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(&store, fs::Permissions::from_mode(0o644)).unwrap();

    match result {
        Ok(total) => assert_eq!(total, Some(2), "report.txt and report_old.txt"),
        Err(e) => panic!("a read-only store opens: {e}"),
    }
}
