//! Integration tests of `EngineSource` over a real `fx_engine::Engine`, plus
//! the synthetic per-page cost used by `--autoscroll`.
//!
//! Contract these tests rely on:
//!
//! - fx-ui-macos has `fx-fs` (workspace crate, MIT) as a dev-dependency, for
//!   the `ChangeSource` / `VolumeEnumerator` / `PathStat` traits the fakes
//!   implement.
//! - The engine runs over a canonical temporary directory with a no-op
//!   `ChangeSource`. On macOS the enumerator and path stat are the real
//!   `fx_fs_macos::enumerator::MacScanner` and
//!   `fx_fs_macos::path_stat::MacPathStat`; elsewhere a small `std::fs`
//!   walker that, like the macOS scanner, does not emit the root itself.
//! - `EngineSource::new(Arc<Engine>, root)`: `row(page.generation, id)` for
//!   an id from `search` gives the entry's absolute `full_path` (the root
//!   path joined with the entry's path under it, `/`-separated), its own
//!   `name`, `size` (`Some(bytes)` for files, `None` for folders), a known
//!   `modified` time and `is_folder`. The root itself is a row whose
//!   `full_path` is the root path.
//! - `status().indexing` is true while the first scan runs and false once
//!   it has been published; `status().entries` then counts every entry.
//! - With the `measure` feature, `AppArgs::parse` accepts
//!   `--page-cost-ms <ms>` into `AppArgs::page_cost_ms: Option<u64>`
//!   (default `None`), and `SyntheticSource::with_page_cost(Duration)` makes
//!   every `search` take at least that long without changing its results.
//!
//! Tests only create and remove their own temporary directories.

#![cfg(unix)]

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fx_engine::{Engine, EngineConfig, QueryOptions, SearchError};
use fx_fs::{ChangeSource, VolumeEnumerator};
use fx_model::{Change, Entry, ResumeCursor, StartOutcome};
use fx_sort::{Direction, SortKey};
#[cfg(feature = "measure")]
use fx_ui_macos::app_args::AppArgs;
#[cfg(feature = "measure")]
use fx_ui_macos::source::SyntheticSource;
use fx_ui_macos::source::{EngineSource, ResultsSource, Row, SearchQuery};

/// How long the first scan and its publication may take in debug builds.
const SETUP: Duration = Duration::from_secs(30);

// ------------------------------------------------------------- helpers

/// A unique scratch directory, removed on drop. `path` is canonical (on
/// macOS under `/private/var/...`).
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(name: &str) -> TempDir {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "fx-ui-source-{name}-{}-{nanos}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).expect("create scratch dir");
        let path = fs::canonicalize(&dir).expect("canonicalize scratch dir");
        TempDir { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// A change source that never reports anything.
struct QuietSource;

impl ChangeSource for QuietSource {
    fn start(&mut self, _from: Option<&ResumeCursor>) -> io::Result<StartOutcome> {
        Ok(StartOutcome::NoCursorSupport)
    }

    fn next_batch(&mut self, deadline: Instant) -> io::Result<Vec<Change>> {
        let now = Instant::now();
        if deadline > now {
            thread::sleep((deadline - now).min(Duration::from_millis(20)));
        }
        Ok(Vec::new())
    }

    fn cursor(&self) -> ResumeCursor {
        ResumeCursor::None { snapshot_epoch: 0 }
    }
}

/// Holds enumeration back until the test opens it (or a timeout passes, so
/// a failing test never hangs on the engine's drop).
#[derive(Clone, Default)]
struct Gate {
    inner: Arc<(Mutex<bool>, Condvar)>,
}

impl Gate {
    fn open(&self) {
        let (lock, cv) = &*self.inner;
        *lock.lock().unwrap_or_else(|p| p.into_inner()) = true;
        cv.notify_all();
    }

    fn wait(&self) {
        let (lock, cv) = &*self.inner;
        let guard = lock.lock().unwrap_or_else(|p| p.into_inner());
        let _ = cv
            .wait_timeout_while(guard, SETUP, |open| !*open)
            .unwrap_or_else(|p| p.into_inner());
    }
}

/// An enumerator that waits for its gate before walking.
struct Gated<V> {
    inner: V,
    gate: Gate,
}

impl<V: VolumeEnumerator> VolumeEnumerator for Gated<V> {
    fn enumerate(&self, root: &Path, sink: &mut dyn FnMut(Entry)) -> io::Result<()> {
        self.gate.wait();
        self.inner.enumerate(root, sink)
    }
}

/// A `std::fs` walker and lstat for hosts without the macOS scanner. Keys
/// are handed out per path and kept, so the walker and the stat agree.
#[cfg(not(target_os = "macos"))]
#[derive(Clone, Default)]
struct StdFs {
    keys: Arc<Mutex<HashMap<PathBuf, u64>>>,
}

#[cfg(not(target_os = "macos"))]
impl StdFs {
    fn key(&self, path: &Path) -> fx_model::FileKey {
        let mut keys = self.keys.lock().unwrap_or_else(|p| p.into_inner());
        let next = keys.len() as u64 + 1;
        let id = *keys.entry(path.to_path_buf()).or_insert(next);
        fx_model::FileKey::new(fx_model::VolumeId(1), id)
    }

    fn entry(&self, path: &Path) -> io::Result<Entry> {
        let md = fs::symlink_metadata(path)?;
        let kind = if md.file_type().is_symlink() {
            fx_model::EntryKind::Symlink
        } else if md.is_dir() {
            fx_model::EntryKind::Dir
        } else {
            fx_model::EntryKind::File
        };
        let secs = |t: io::Result<SystemTime>| {
            t.ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
        };
        Ok(Entry {
            key: self.key(path),
            parent: path.parent().map(|p| self.key(p)),
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "/".to_string()),
            kind,
            flags: fx_model::EntryFlags::empty(),
            size: if kind == fx_model::EntryKind::Dir {
                0
            } else {
                md.len()
            },
            modified: secs(md.modified()),
            created: secs(md.created()),
        })
    }

    fn walk(&self, dir: &Path, sink: &mut dyn FnMut(Entry)) -> io::Result<()> {
        let mut children: Vec<PathBuf> = fs::read_dir(dir)?
            .map(|e| e.map(|e| e.path()))
            .collect::<io::Result<_>>()?;
        children.sort();
        for child in children {
            let entry = self.entry(&child)?;
            let is_dir = entry.kind == fx_model::EntryKind::Dir;
            sink(entry);
            if is_dir {
                self.walk(&child, sink)?;
            }
        }
        Ok(())
    }
}

#[cfg(not(target_os = "macos"))]
impl VolumeEnumerator for StdFs {
    fn enumerate(&self, root: &Path, sink: &mut dyn FnMut(Entry)) -> io::Result<()> {
        self.walk(root, sink)
    }
}

#[cfg(not(target_os = "macos"))]
impl fx_fs::PathStat for StdFs {
    fn stat(&self, path: &Path) -> io::Result<Option<Entry>> {
        match self.entry(path) {
            Ok(e) => Ok(Some(e)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
}

fn open_engine(root: &Path, gate: &Gate) -> Engine {
    open_engine_with(
        EngineConfig::new(root).with_batch_window(Duration::from_millis(50)),
        gate,
    )
}

#[cfg(target_os = "macos")]
fn open_engine_with(config: EngineConfig, gate: &Gate) -> Engine {
    use fx_fs_macos::enumerator::MacScanner;
    use fx_fs_macos::path_stat::MacPathStat;
    use fx_fs_macos::scan::ScanOptions;
    Engine::open(
        config,
        QuietSource,
        MacPathStat::new(),
        Gated {
            inner: MacScanner::new(ScanOptions::default()),
            gate: gate.clone(),
        },
    )
    .expect("engine starts")
}

#[cfg(not(target_os = "macos"))]
fn open_engine_with(config: EngineConfig, gate: &Gate) -> Engine {
    let fs = StdFs::default();
    Engine::open(
        config,
        QuietSource,
        fs.clone(),
        Gated {
            inner: fs,
            gate: gate.clone(),
        },
    )
    .expect("engine starts")
}

fn write_file(path: &Path, bytes: usize) {
    fs::write(path, vec![b'x'; bytes]).expect("write test file");
}

/// The tree every engine test indexes:
///
/// ```text
/// <root>/alpha.txt              5 bytes
/// <root>/docs/                  folder
/// <root>/docs/beta.log         12 bytes
/// <root>/docs/nested/           folder
/// <root>/docs/nested/gamma.bin  3 bytes
/// ```
fn populate(root: &Path) {
    fs::create_dir_all(root.join("docs/nested")).expect("create folders");
    write_file(&root.join("alpha.txt"), 5);
    write_file(&root.join("docs/beta.log"), 12);
    write_file(&root.join("docs/nested/gamma.bin"), 3);
}

/// `root` as a string plus `/rel`.
fn abs(root: &Path, rel: &str) -> String {
    let root = root.to_str().expect("UTF-8 temp path");
    if rel.is_empty() {
        root.to_string()
    } else {
        format!("{}/{rel}", root.trim_end_matches('/'))
    }
}

/// Waits until `cond` holds, failing with `what` after [`SETUP`].
fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(start.elapsed() < SETUP, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(5));
    }
}

/// Every row of `query`'s results in Path order, by full path.
fn rows_by_path(source: &EngineSource, query: &str) -> HashMap<String, Row> {
    let page = source
        .search(query, SortKey::Path, Direction::Ascending, 0, 10_000)
        .expect("search not cancelled");
    assert_eq!(page.ids.len(), page.total, "one page holds every match");
    let mut rows = HashMap::new();
    for &id in &page.ids {
        let row = source
            .row(page.generation, id)
            .unwrap_or_else(|| panic!("row {id} of generation {:?}", page.generation));
        let previous = rows.insert(row.full_path.clone(), row.clone());
        assert!(
            previous.is_none(),
            "duplicate full path {:?}",
            row.full_path
        );
    }
    rows
}

/// Starts an engine over a populated temp dir and waits for the first scan
/// to be published, checking the indexing state on the way.
fn scanned(name: &str) -> (TempDir, EngineSource) {
    let dir = TempDir::new(name);
    populate(&dir.path);
    let gate = Gate::default();
    let engine = Arc::new(open_engine(&dir.path, &gate));
    let source = EngineSource::new(Arc::clone(&engine), &dir.path);
    let updates = source.subscribe();

    assert!(
        source.status().indexing,
        "indexing while the first scan is held back"
    );

    gate.open();
    let generation = updates
        .recv_timeout(SETUP)
        .expect("the first scan publishes a generation");
    assert!(generation.get() > 0, "a published generation is past 0");
    wait_until("the first scan to finish", || !source.status().indexing);
    (dir, source)
}

// ------------------------------------------------------------- tests

#[test]
fn indexing_goes_from_true_to_false_after_the_first_scan() {
    let (_dir, source) = scanned("indexing");
    let status = source.status();
    assert!(!status.indexing);
    // The root plus two folders and three files.
    assert_eq!(status.entries, 6, "{status:?}");
    assert!(source.generation().get() > 0);
}

#[test]
fn rows_carry_absolute_paths_names_sizes_and_kinds() {
    let (dir, source) = scanned("rows");
    let root = &dir.path;
    let rows = rows_by_path(&source, "");

    let mut paths: Vec<&String> = rows.keys().collect();
    paths.sort();
    let mut expected = [
        abs(root, ""),
        abs(root, "alpha.txt"),
        abs(root, "docs"),
        abs(root, "docs/beta.log"),
        abs(root, "docs/nested"),
        abs(root, "docs/nested/gamma.bin"),
    ];
    expected.sort();
    assert_eq!(
        paths,
        expected.iter().collect::<Vec<_>>(),
        "full paths are the absolute on-disk paths"
    );

    let files = [
        ("alpha.txt", "alpha.txt", 5u64, ""),
        ("docs/beta.log", "beta.log", 12, "docs"),
        ("docs/nested/gamma.bin", "gamma.bin", 3, "docs/nested"),
    ];
    for (rel, name, size, folder) in files {
        let row = &rows[&abs(root, rel)];
        assert_eq!(row.name, name, "{row:?}");
        assert!(!row.is_folder, "{row:?}");
        assert_eq!(row.size, Some(size), "{row:?}");
        assert!(row.modified.is_some_and(|t| t > 0), "{row:?}");
        assert_eq!(row.folder(), abs(root, folder), "{row:?}");
        assert!(Path::new(&row.full_path).is_file(), "{row:?}");
    }

    let folders = [("docs", "docs", ""), ("docs/nested", "nested", "docs")];
    for (rel, name, folder) in folders {
        let row = &rows[&abs(root, rel)];
        assert_eq!(row.name, name, "{row:?}");
        assert!(row.is_folder, "{row:?}");
        assert_eq!(row.size, None, "folders have no size: {row:?}");
        assert_eq!(row.folder(), abs(root, folder), "{row:?}");
        assert!(Path::new(&row.full_path).is_dir(), "{row:?}");
    }

    let root_row = &rows[&abs(root, "")];
    assert!(root_row.is_folder, "{root_row:?}");
    assert_eq!(
        root_row.name,
        root.file_name().unwrap().to_str().unwrap(),
        "the root row is named after the root folder"
    );
    assert_eq!(
        root_row.folder(),
        root.parent().unwrap().to_str().unwrap(),
        "{root_row:?}"
    );
}

#[test]
fn query_results_resolve_to_the_matching_absolute_paths() {
    let (dir, source) = scanned("query");
    let root = &dir.path;

    let rows = rows_by_path(&source, "beta");
    assert_eq!(rows.len(), 1, "{rows:?}");
    let row = &rows[&abs(root, "docs/beta.log")];
    assert_eq!(row.name, "beta.log");
    assert_eq!(row.size, Some(12));

    let rows = rows_by_path(&source, "nested");
    assert_eq!(rows.len(), 1, "{rows:?}");
    let row = &rows[&abs(root, "docs/nested")];
    assert!(row.is_folder);
    assert_eq!(row.size, None);
}

#[test]
fn every_sort_key_gives_the_same_rows() {
    let (_dir, source) = scanned("sorts");
    let by_path = rows_by_path(&source, "");
    for sort in [SortKey::Name, SortKey::Size, SortKey::Modified] {
        for dir in [Direction::Ascending, Direction::Descending] {
            let page = source
                .search("", sort, dir, 0, 10_000)
                .expect("search not cancelled");
            assert_eq!(page.total, by_path.len(), "{sort:?} {dir:?}");
            for &id in &page.ids {
                let row = source.row(page.generation, id).expect("row exists");
                assert_eq!(
                    by_path.get(&row.full_path),
                    Some(&row),
                    "{sort:?} {dir:?}: same row under every order"
                );
            }
        }
    }
}

/// "Your files first" through the app's source: with the flag the matches
/// under the priority folder come first, each part in the plain order;
/// without it the order is the plain sort. Goes through `Arc<dyn
/// ResultsSource>`, as the window does.
#[test]
fn your_files_first_groups_the_engine_results() {
    let dir = TempDir::new("yours");
    populate(&dir.path);
    let config = EngineConfig::new(&dir.path)
        .with_batch_window(Duration::from_millis(50))
        .with_priority_folders([dir.path.join("docs/nested")], Vec::<PathBuf>::new());
    let gate = Gate::default();
    gate.open();
    let engine = Arc::new(open_engine_with(config, &gate));
    let source: Arc<dyn ResultsSource> = Arc::new(EngineSource::new(engine, &dir.path));
    wait_until("the first scan to finish", || !source.status().indexing);

    let yours = [
        abs(&dir.path, "docs/nested"),
        abs(&dir.path, "docs/nested/gamma.bin"),
    ];
    let full_paths = |page: &fx_ui_macos::source::ResultPage| -> Vec<String> {
        page.ids
            .iter()
            .map(|&id| source.row(page.generation, id).expect("row").full_path)
            .collect()
    };
    for sort in [
        SortKey::Name,
        SortKey::Path,
        SortKey::Size,
        SortKey::Modified,
    ] {
        for dir in [Direction::Ascending, Direction::Descending] {
            let plain = source.search("", sort, dir, 0, 100).expect("search");
            let off = source
                .search_grouped("", sort, dir, false, 0, 100)
                .expect("search");
            assert_eq!(off, plain, "{sort:?} {dir:?}: off is the plain sort");
            let grouped = source
                .search_grouped("", sort, dir, true, 0, 100)
                .expect("search");
            assert_eq!(grouped.total, plain.total);
            let plain_paths = full_paths(&plain);
            let mut want: Vec<String> = plain_paths
                .iter()
                .filter(|p| yours.contains(p))
                .cloned()
                .collect();
            want.extend(plain_paths.iter().filter(|p| !yours.contains(p)).cloned());
            assert_eq!(full_paths(&grouped), want, "{sort:?} {dir:?}");
        }
    }
}

/// The selection lookup follows "Your files first": `positions_grouped`
/// with the flag gives the rows of
/// the grouped list, without it the rows of `positions`. Goes through
/// `Arc<dyn ResultsSource>`, as the window does.
#[test]
fn grouped_positions_are_rows_of_the_grouped_list() {
    let dir = TempDir::new("yours-positions");
    populate(&dir.path);
    let config = EngineConfig::new(&dir.path)
        .with_batch_window(Duration::from_millis(50))
        .with_priority_folders([dir.path.join("docs/nested")], Vec::<PathBuf>::new());
    let gate = Gate::default();
    gate.open();
    let engine = Arc::new(open_engine_with(config, &gate));
    let source: Arc<dyn ResultsSource> = Arc::new(EngineSource::new(engine, &dir.path));
    wait_until("the first scan to finish", || !source.status().indexing);

    let mut asked: Vec<String> = ["docs/nested/gamma.bin", "alpha.txt", "docs", "docs/nested"]
        .iter()
        .map(|rel| abs(&dir.path, rel))
        .collect();
    asked.extend([
        abs(&dir.path, ""),
        abs(&dir.path, "docs/beta.log"),
        abs(&dir.path, "missing.txt"),
        "/elsewhere/alpha.txt".to_string(),
    ]);
    for query in ["", "a", "zzz"] {
        for sort in [
            SortKey::Name,
            SortKey::Path,
            SortKey::Size,
            SortKey::Modified,
        ] {
            for dir in [Direction::Ascending, Direction::Descending] {
                let listed = source
                    .search_grouped(query, sort, dir, true, 0, 100)
                    .expect("search");
                let listed: Vec<String> = listed
                    .ids
                    .iter()
                    .map(|&id| source.row(listed.generation, id).expect("row").full_path)
                    .collect();
                let found = source
                    .positions_grouped(query, sort, dir, true, &asked)
                    .expect("not cancelled");
                for (k, path) in asked.iter().enumerate() {
                    let want = listed.iter().position(|p| p == path);
                    assert_eq!(found.rows[k], want, "{query:?} {sort:?} {dir:?} {path}");
                }
                let off = source
                    .positions_grouped(query, sort, dir, false, &asked)
                    .expect("not cancelled");
                let plain = source
                    .positions(query, sort, dir, &asked)
                    .expect("not cancelled");
                assert_eq!(off, plain, "{query:?} {sort:?} {dir:?}");
            }
        }
    }
    // By name, gamma.bin (yours) leads the grouped list; alpha.txt leads
    // the plain one.
    let gamma = [abs(&dir.path, "docs/nested/gamma.bin")];
    let grouped = source
        .positions_grouped("", SortKey::Name, Direction::Ascending, true, &gamma)
        .expect("not cancelled");
    let plain = source
        .positions("", SortKey::Name, Direction::Ascending, &gamma)
        .expect("not cancelled");
    assert_eq!(grouped.rows, [Some(0)]);
    assert_ne!(plain.rows, [Some(0)], "the group moves gamma.bin up");
}

/// Full paths of every match of `query`, read `page` rows at a time.
fn paged_paths<S: ResultsSource>(
    source: &S,
    query: &str,
    sort: SortKey,
    dir: Direction,
    page: usize,
) -> Vec<String> {
    let mut paths = Vec::new();
    loop {
        let found = source
            .search(query, sort, dir, paths.len(), page)
            .expect("search not cancelled");
        if found.ids.is_empty() {
            assert_eq!(paths.len(), found.total, "{query:?} {sort:?} {dir:?}");
            return paths;
        }
        for id in found.ids {
            let row = source.row(found.generation, id).expect("row exists");
            paths.push(row.full_path);
        }
    }
}

/// Asserts that `positions` finds each of `asked` at the row paging finds
/// it at, and nothing for paths outside the results.
fn assert_positions_match_paging<S: ResultsSource>(source: &S, query: &str, asked: &[String]) {
    for sort in [
        SortKey::Name,
        SortKey::Path,
        SortKey::Size,
        SortKey::Modified,
    ] {
        for dir in [Direction::Ascending, Direction::Descending] {
            let paged = paged_paths(source, query, sort, dir, 2);
            let found = source
                .positions(query, sort, dir, asked)
                .expect("not cancelled");
            assert_eq!(found.rows.len(), asked.len());
            for (k, path) in asked.iter().enumerate() {
                let want = paged.iter().position(|p| p == path);
                assert_eq!(found.rows[k], want, "{query:?} {sort:?} {dir:?} {path}");
            }
        }
    }
}

#[test]
fn positions_find_rows_by_their_absolute_paths() {
    let (dir, source) = scanned("positions");
    let root = &dir.path;
    let mut asked: Vec<String> = ["docs/nested/gamma.bin", "", "alpha.txt", "docs"]
        .iter()
        .map(|rel| abs(root, rel))
        .collect();
    asked.extend([
        abs(root, "docs/beta.log"),
        abs(root, "docs/nested"),
        abs(root, "missing.txt"),
        abs(root, "docs/"),
        "/elsewhere/alpha.txt".to_string(),
        "alpha.txt".to_string(),
        String::new(),
    ]);
    for query in ["", "a", "beta", "zzz"] {
        assert_positions_match_paging(&source, query, &asked);
    }
    let found = source
        .positions("", SortKey::Name, Direction::Ascending, &asked)
        .expect("not cancelled");
    assert_eq!(found.generation, source.generation());
    assert_eq!(found.rows.iter().filter(|r| r.is_some()).count(), 6);
}

#[test]
fn the_bare_engine_finds_rows_by_their_index_paths() {
    let (dir, source) = scanned("positions-bare");
    let engine: &Engine = source.engine();
    let name = dir.path.file_name().unwrap().to_str().unwrap();
    let asked: Vec<String> = ["", "/alpha.txt", "/docs/nested/gamma.bin", "/nope"]
        .iter()
        .map(|rel| format!("{name}{rel}"))
        .collect();
    assert_positions_match_paging(engine, "", &asked);
    // The trait method, not the engine's own (which takes index paths).
    let found = ResultsSource::positions(engine, "", SortKey::Path, Direction::Ascending, &asked)
        .expect("not cancelled");
    assert_eq!(found.rows, [Some(0), Some(1), Some(5), None]);
}

#[cfg(feature = "measure")]
#[test]
fn synthetic_positions_match_reading_the_pages() {
    let source = SyntheticSource::new(3_000, 11);
    let every: Vec<String> = paged_paths(&source, "", SortKey::Name, Direction::Ascending, 500);
    let mut asked: Vec<String> = every.iter().step_by(37).cloned().collect();
    asked.reverse();
    asked.push("/nope/missing".to_string());
    asked.push("relative/path".to_string());
    for query in ["", "a", "e"] {
        assert_positions_match_paging(&source, query, &asked);
    }
}

// ------------------------------------------- synthetic per-page cost

#[cfg(feature = "measure")]
#[test]
fn page_cost_option_parses_and_defaults_to_none() {
    let args = AppArgs::parse(["--synthetic", "2000000", "--autoscroll"]).expect("parses");
    assert_eq!(args.page_cost_ms, None);

    let args = AppArgs::parse([
        "--synthetic",
        "2000000",
        "--autoscroll",
        "--page-cost-ms",
        "3",
    ])
    .expect("parses");
    assert_eq!(args.page_cost_ms, Some(3));
    assert_eq!(args.synthetic, Some(2_000_000));
    assert!(args.autoscroll);

    let args = AppArgs::parse(["--page-cost-ms", "0"]).expect("zero cost parses");
    assert_eq!(args.page_cost_ms, Some(0));
}

#[cfg(feature = "measure")]
#[test]
fn page_cost_option_rejects_bad_values() {
    assert!(AppArgs::parse(["--page-cost-ms"]).is_err(), "missing value");
    assert!(
        AppArgs::parse(["--page-cost-ms", ""]).is_err(),
        "empty value"
    );
    assert!(AppArgs::parse(["--page-cost-ms", "abc"]).is_err());
    assert!(AppArgs::parse(["--page-cost-ms", "-1"]).is_err());
    assert!(AppArgs::parse(["--page-cost-ms", "1.5"]).is_err());
}

#[cfg(feature = "measure")]
#[test]
fn synthetic_page_cost_delays_each_search_without_changing_results() {
    let plain = SyntheticSource::new(2_000, 7);
    let costly = SyntheticSource::new(2_000, 7).with_page_cost(Duration::from_millis(30));

    for (query, sort, dir, offset) in [
        ("", SortKey::Name, Direction::Ascending, 0),
        ("", SortKey::Size, Direction::Descending, 100),
        ("a", SortKey::Path, Direction::Ascending, 5),
    ] {
        let expected = plain
            .search(query, sort, dir, offset, 50)
            .expect("search not cancelled");
        let start = Instant::now();
        let page = costly
            .search(query, sort, dir, offset, 50)
            .expect("search not cancelled");
        assert!(
            start.elapsed() >= Duration::from_millis(30),
            "each page costs at least the simulated time"
        );
        assert_eq!(page, expected, "{query:?} {sort:?} {dir:?}");
        for &id in &page.ids {
            assert_eq!(
                costly.row(page.generation, id),
                plain.row(expected.generation, id)
            );
        }
    }
}

#[cfg(feature = "measure")]
#[test]
fn synthetic_source_has_no_index_to_save() {
    assert_eq!(SyntheticSource::new(10, 1).save_for_quit(), Ok(()));
}

/// `EngineSource::save_for_quit` (called when the app quits) leaves a store
/// that already holds the latest snapshot as it is: after the save that
/// follows the first scan, with no change since, quitting writes nothing.
#[cfg(target_os = "macos")]
#[test]
fn quit_after_the_saved_first_scan_leaves_the_store_unchanged() {
    use std::os::unix::fs::MetadataExt;

    use fx_fs_macos::enumerator::MacScanner;
    use fx_fs_macos::path_stat::MacPathStat;
    use fx_fs_macos::scan::ScanOptions;

    let dir = TempDir::new("save-root");
    populate(&dir.path);
    let stores = TempDir::new("save-store");
    let store = stores.path.join("index.fxstore");
    // No idle or max-delay save during the test: only the save after the
    // first scan.
    let config = EngineConfig::new(&dir.path)
        .with_batch_window(Duration::from_millis(50))
        .with_store_path(&store)
        .with_checkpoint_idle(Duration::from_secs(3600))
        .with_checkpoint_max_delay(Duration::from_secs(3600));
    let engine = Engine::open(
        config,
        QuietSource,
        MacPathStat::new(),
        MacScanner::new(ScanOptions::default()),
    )
    .expect("engine starts");
    let source = EngineSource::new(Arc::new(engine), &dir.path);
    wait_until("the first scan to finish", || !source.status().indexing);
    wait_until("the save after the first scan", || store.exists());

    // Every write renames a new file over the store: a new inode and
    // modification time.
    let stamp = |path: &Path| {
        let meta = fs::metadata(path).expect("the store exists");
        (
            meta.ino(),
            meta.modified().expect("modification time"),
            meta.len(),
        )
    };
    let before = stamp(&store);
    assert!(before.2 > 0, "the saved store is not empty");
    let bytes = fs::read(&store).expect("the store is readable");
    assert_eq!(source.save_for_quit(), Ok(()));
    assert_eq!(stamp(&store), before, "quitting rewrote the store");
    assert_eq!(fs::read(&store).expect("the store is readable"), bytes);
}

/// The engine reports a regular expression that does not compile
/// as an error. Until the window shows it, the engine sources answer as
/// before, with no rows.
#[test]
fn an_invalid_regex_term_finds_no_rows() {
    let (dir, source) = scanned("bad-regex");
    let (name, asc) = (SortKey::Name, Direction::Ascending);
    let page = source
        .search("alpha regex:(", name, asc, 0, 10)
        .expect("not cancelled");
    assert_eq!((page.total, page.ids.len()), (0, 0));
    assert_eq!(page.generation, source.generation());
    let asked = [abs(&dir.path, "alpha.txt")];
    let found = source
        .positions("alpha regex:(", name, asc, &asked)
        .expect("not cancelled");
    assert_eq!(found.rows, vec![None]);
    let engine: &Engine = source.engine();
    let bare = ResultsSource::search(engine, "regex:(", name, asc, 0, 10).expect("not cancelled");
    assert_eq!(bare.total, 0);
    // A valid expression still finds its rows.
    let page = source
        .search("regex:^alpha", name, asc, 0, 10)
        .expect("not cancelled");
    assert_eq!(page.total, 1);
}

/// The window's search takes the search field's options (the Aa,
/// Path and .* chips) and reports an invalid regular expression as a typed
/// error, which the window shows instead of an empty page.
#[test]
fn the_windows_search_applies_the_chips() {
    let (dir, source) = scanned("chips");
    let ask = |text, options| SearchQuery {
        text,
        options,
        sort: SortKey::Name,
        direction: Direction::Ascending,
        your_files_first: false,
    };
    let names = |q: &SearchQuery<'_>| -> Vec<String> {
        let page = source.search_query(q, 0, 100).expect("searches");
        page.ids
            .iter()
            .filter_map(|&id| source.row(page.generation, id))
            .map(|r| r.name)
            .collect()
    };
    let plain = QueryOptions::default();
    assert_eq!(names(&ask("ALPHA", plain)), ["alpha.txt"]);
    let case = QueryOptions {
        match_case: true,
        ..plain
    };
    assert!(names(&ask("ALPHA", case)).is_empty(), "Aa matches case");
    assert_eq!(names(&ask("alpha", case)), ["alpha.txt"]);
    // Path: a folder's name finds what it holds.
    let path = QueryOptions {
        match_path: true,
        ..plain
    };
    assert!(names(&ask("nested", plain)).contains(&"nested".to_owned()));
    assert!(!names(&ask("nested", plain)).contains(&"gamma.bin".to_owned()));
    assert!(names(&ask("nested", path)).contains(&"gamma.bin".to_owned()));
    // .*: the whole search is one expression, bars included.
    let regex = QueryOptions {
        regex: true,
        ..plain
    };
    assert_eq!(
        names(&ask("^(alpha|beta)\\.", regex)),
        ["alpha.txt", "beta.log"]
    );
    match source.search_query(&ask("alpha(", regex), 0, 100) {
        Err(SearchError::InvalidRegex { pattern, .. }) => assert_eq!(pattern, "alpha("),
        other => panic!("expected an invalid regex, got {other:?}"),
    }
    // A `regex:` term that doesn't compile fails the same way.
    assert!(matches!(
        source.search_query(&ask("regex:(", plain), 0, 100),
        Err(SearchError::InvalidRegex { .. })
    ));
    // Positions use the options too.
    let asked = [abs(&dir.path, "docs/nested/gamma.bin")];
    let found = source
        .positions_query(&ask("nested", path), &asked)
        .expect("not cancelled");
    assert!(found.rows[0].is_some());
    let found = source
        .positions_query(&ask("nested", plain), &asked)
        .expect("not cancelled");
    assert_eq!(found.rows, vec![None]);
    // The bare engine answers the same way.
    let engine: &Engine = source.engine();
    let bare = ResultsSource::search_query(engine, &ask("ALPHA", case), 0, 10);
    assert!(bare.expect("searches").ids.is_empty());
    assert!(matches!(
        ResultsSource::search_query(engine, &ask("(", regex), 0, 10),
        Err(SearchError::InvalidRegex { .. })
    ));
}
