//! The platform-agnostic results controller, driven by a fake
//! `ResultsSource` with injected time.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use fx_engine::Generation;
use fx_query::Cancelled;
use fx_sort::{Direction, SortKey};
use fx_ui_macos::columns::{Column, SortState};
use fx_ui_macos::controller::{DEFAULT_ROW_WINDOW, DEFAULT_THROTTLE, RequestId, ResultsController};
#[cfg(feature = "measure")]
use fx_ui_macos::source::SyntheticSource;
use fx_ui_macos::source::{ResultPage, ResultsSource, Row, RowPositions, SourceStatus};
use fx_ui_macos::status_view::{StatusInput, status_view};

// ---------------------------------------------------------------------------
// Fake source
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Entry {
    name: String,
    full_path: String,
    size: Option<u64>,
    modified: Option<i64>,
    is_folder: bool,
}

fn file(full_path: &str, size: u64, modified: i64) -> Entry {
    Entry {
        name: full_path.rsplit('/').next().unwrap().to_string(),
        full_path: full_path.to_string(),
        size: Some(size),
        modified: Some(modified),
        is_folder: false,
    }
}

fn folder(full_path: &str) -> Entry {
    Entry {
        name: full_path.rsplit('/').next().unwrap().to_string(),
        full_path: full_path.to_string(),
        size: None,
        modified: Some(1_500_000_000),
        is_folder: true,
    }
}

#[derive(Debug, Clone)]
struct SearchCall {
    query: String,
    sort: SortKey,
    direction: Direction,
    your_files_first: bool,
}

#[derive(Default)]
struct State {
    entries: Vec<Entry>,
    generation: u64,
    indexing: bool,
    searches: Vec<SearchCall>,
    cancels: usize,
    row_generations: Vec<Generation>,
    subscribers: Vec<Sender<Generation>>,
}

#[derive(Clone, Default)]
struct FakeSource {
    state: Arc<Mutex<State>>,
}

impl FakeSource {
    fn new(entries: Vec<Entry>) -> FakeSource {
        let fake = FakeSource::default();
        {
            let mut s = fake.lock();
            s.entries = entries;
            s.generation = 1;
        }
        fake
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    /// Publishes a new generation and returns it.
    fn bump(&self) -> Generation {
        let mut s = self.lock();
        s.generation += 1;
        let g = Generation(s.generation);
        s.subscribers.retain(|tx| tx.send(g).is_ok());
        g
    }

    fn set_indexing(&self, indexing: bool) {
        self.lock().indexing = indexing;
    }

    fn search_count(&self) -> usize {
        self.lock().searches.len()
    }

    fn last_search(&self) -> SearchCall {
        self.lock().searches.last().cloned().expect("a search ran")
    }
}

fn sort_value(e: &Entry, key: SortKey) -> (i128, String, String) {
    let name = (e.name.to_lowercase(), e.name.clone());
    match key {
        SortKey::Name => (0, name.0, name.1),
        SortKey::Path => (0, e.full_path.to_lowercase(), e.full_path.clone()),
        SortKey::Size => (e.size.unwrap_or(0) as i128, name.0, name.1),
        SortKey::Modified | SortKey::Created => (e.modified.unwrap_or(0) as i128, name.0, name.1),
    }
}

/// The fake's "your files": everything under `/Users/me/`.
fn is_yours(e: &Entry) -> bool {
    e.full_path.starts_with("/Users/me/")
}

/// Every match of `query`, in display order: with `your_files_first`, your
/// files first, each part in the sorted order.
fn sorted_ids(
    s: &State,
    query: &str,
    sort: SortKey,
    dir: Direction,
    your_files_first: bool,
) -> Vec<u32> {
    let needle = query.to_lowercase();
    let mut ids: Vec<u32> = s
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| needle.is_empty() || e.name.to_lowercase().contains(&needle))
        .map(|(i, _)| i as u32)
        .collect();
    ids.sort_by_key(|&id| sort_value(&s.entries[id as usize], sort));
    if dir == Direction::Descending {
        ids.reverse();
    }
    if your_files_first {
        // A stable sort keeps each part in the sorted order.
        ids.sort_by_key(|&id| !is_yours(&s.entries[id as usize]));
    }
    ids
}

impl ResultsSource for FakeSource {
    fn search(
        &self,
        query: &str,
        sort: SortKey,
        dir: Direction,
        offset: usize,
        limit: usize,
    ) -> Result<ResultPage, Cancelled> {
        self.search_grouped(query, sort, dir, false, offset, limit)
    }

    fn search_grouped(
        &self,
        query: &str,
        sort: SortKey,
        dir: Direction,
        your_files_first: bool,
        offset: usize,
        limit: usize,
    ) -> Result<ResultPage, Cancelled> {
        let mut s = self.lock();
        s.searches.push(SearchCall {
            query: query.to_string(),
            sort,
            direction: dir,
            your_files_first,
        });
        let ids = sorted_ids(&s, query, sort, dir, your_files_first);
        let total = ids.len();
        let ids: Vec<u32> = ids.into_iter().skip(offset).take(limit).collect();
        Ok(ResultPage {
            generation: Generation(s.generation),
            total,
            ids,
        })
    }

    fn cancel(&self) {
        self.lock().cancels += 1;
    }

    fn row(&self, generation: Generation, id: u32) -> Option<Row> {
        let mut s = self.lock();
        s.row_generations.push(generation);
        let e = s.entries.get(id as usize)?.clone();
        Some(Row {
            name: e.name,
            full_path: e.full_path,
            size: e.size,
            modified: e.modified,
            is_folder: e.is_folder,
        })
    }

    fn positions(
        &self,
        query: &str,
        sort: SortKey,
        dir: Direction,
        paths: &[String],
    ) -> Result<RowPositions, Cancelled> {
        self.positions_grouped(query, sort, dir, false, paths)
    }

    fn positions_grouped(
        &self,
        query: &str,
        sort: SortKey,
        dir: Direction,
        your_files_first: bool,
        paths: &[String],
    ) -> Result<RowPositions, Cancelled> {
        let s = self.lock();
        let ids = sorted_ids(&s, query, sort, dir, your_files_first);
        let rows = paths
            .iter()
            .map(|p| {
                ids.iter()
                    .position(|&id| s.entries[id as usize].full_path == *p)
            })
            .collect();
        Ok(RowPositions {
            generation: Generation(s.generation),
            rows,
        })
    }

    fn generation(&self) -> Generation {
        Generation(self.lock().generation)
    }

    fn status(&self) -> SourceStatus {
        let s = self.lock();
        SourceStatus {
            indexing: s.indexing,
            entries: s.entries.len(),
        }
    }

    fn subscribe(&self) -> Receiver<Generation> {
        let (tx, rx) = channel();
        self.lock().subscribers.push(tx);
        rx
    }
}

/// Six entries; by name ascending: alpha.txt, Alphabet.md, alpine,
/// beta.rs, delta.png, gamma.log.
fn sample() -> FakeSource {
    FakeSource::new(vec![
        file("/var/log/gamma.log", 5, 1_600_000_500),
        file("/Users/me/src/beta.rs", 2000, 1_600_000_400),
        folder("/Users/me/alpine"),
        file("/Users/me/docs/Alphabet.md", 300, 1_600_000_200),
        file("/Users/me/alpha.txt", 10, 1_600_000_100),
        file("/Users/me/pics/delta.png", 70_000, 1_600_000_300),
    ])
}

fn numbered(count: usize) -> FakeSource {
    FakeSource::new(
        (0..count)
            .map(|i| file(&format!("/data/file{i:05}.bin"), i as u64, 1_600_000_000))
            .collect(),
    )
}

fn refresh<S: ResultsSource>(ctl: &mut ResultsController<S>, req: RequestId) {
    let page = ctl.run(req).expect("search should not be cancelled");
    assert!(ctl.apply(req, page), "a fresh page should be applied");
}

fn name_at<S: ResultsSource>(ctl: &mut ResultsController<S>, i: usize) -> Option<String> {
    ctl.row(i).map(|r| r.name.clone())
}

fn names<S: ResultsSource>(ctl: &mut ResultsController<S>) -> Vec<String> {
    (0..ctl.row_count())
        .map(|i| name_at(ctl, i).expect("row in range"))
        .collect()
}

const MS: Duration = Duration::from_millis(1);

// ---------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------

fn assert_results_source<T: ResultsSource>() {}

#[test]
fn the_engine_is_a_results_source() {
    assert_results_source::<fx_engine::Engine>();
}

#[cfg(feature = "measure")]
#[test]
fn the_synthetic_source_is_a_results_source() {
    assert_results_source::<SyntheticSource>();
}

#[cfg(feature = "measure")]
#[test]
fn synthetic_source_lists_every_row() {
    let source = SyntheticSource::new(1000, 7);
    let status = source.status();
    assert_eq!(status.entries, 1000);
    assert!(!status.indexing);

    let page = source
        .search("", SortKey::Name, Direction::Ascending, 0, 10)
        .expect("not cancelled");
    assert_eq!(page.total, 1000);
    assert_eq!(page.ids.len(), 10);
    for &id in &page.ids {
        let row = source
            .row(page.generation, id)
            .expect("listed id has a row");
        assert!(!row.name.is_empty());
        assert!(
            row.full_path.ends_with(&row.name),
            "{:?} should end with {:?}",
            row.full_path,
            row.name
        );
    }
}

#[cfg(feature = "measure")]
#[test]
fn synthetic_source_filters_by_query() {
    let source = SyntheticSource::new(1000, 7);
    let page = source
        .search("", SortKey::Name, Direction::Ascending, 0, 1)
        .expect("not cancelled");
    let name = source.row(page.generation, page.ids[0]).unwrap().name;
    let hits = source
        .search(&name, SortKey::Name, Direction::Ascending, 0, 1000)
        .expect("not cancelled");
    assert!(hits.total >= 1 && hits.total < 1000, "total {}", hits.total);
    assert!(hits.ids.contains(&page.ids[0]));
}

// ---------------------------------------------------------------------------
// Query → total and rows
// ---------------------------------------------------------------------------

#[test]
fn defaults() {
    assert_eq!(DEFAULT_THROTTLE, Duration::from_millis(100));
    assert_eq!(DEFAULT_ROW_WINDOW, 256);
}

#[test]
fn query_gives_total_and_rows_in_name_order() {
    let fake = sample();
    let mut ctl = ResultsController::new(fake.clone());
    let req = ctl.set_query("alp");
    refresh(&mut ctl, req);

    let call = fake.last_search();
    assert_eq!(call.query, "alp");
    assert_eq!(call.sort, SortKey::Name);
    assert_eq!(call.direction, Direction::Ascending);

    assert_eq!(ctl.row_count(), 3);
    assert_eq!(names(&mut ctl), vec!["alpha.txt", "Alphabet.md", "alpine"]);
    let row = ctl.row(1).expect("row 1");
    assert_eq!(row.full_path, "/Users/me/docs/Alphabet.md");
    assert_eq!(row.size, Some(300));
    assert_eq!(row.modified, Some(1_600_000_200));
    assert!(!row.is_folder);
    let alpine = ctl.row(2).expect("row 2");
    assert!(alpine.is_folder);
    assert_eq!(alpine.size, None);
    assert!(ctl.row(3).is_none(), "rows past the total are absent");
}

#[test]
fn empty_query_lists_everything() {
    let fake = sample();
    let mut ctl = ResultsController::new(fake.clone());
    let req = ctl.set_query("");
    refresh(&mut ctl, req);
    assert_eq!(ctl.row_count(), 6);
    assert_eq!(
        names(&mut ctl),
        vec![
            "alpha.txt",
            "Alphabet.md",
            "alpine",
            "beta.rs",
            "delta.png",
            "gamma.log"
        ]
    );
}

#[test]
fn rows_are_read_from_the_page_generation() {
    let fake = sample();
    let mut ctl = ResultsController::new(fake.clone());
    let req = ctl.set_query("");
    let page = ctl.run(req).expect("not cancelled");
    let generation = page.generation;
    assert!(ctl.apply(req, page));
    fake.lock().row_generations.clear();
    let _ = names(&mut ctl);
    let seen = fake.lock().row_generations.clone();
    assert!(!seen.is_empty());
    assert!(seen.iter().all(|&g| g == generation), "{seen:?}");
}

#[test]
fn set_query_cancels_in_flight_searches_and_bumps_the_request() {
    let fake = sample();
    let mut ctl = ResultsController::new(fake.clone());
    let r1 = ctl.set_query("a");
    let before = fake.lock().cancels;
    let r2 = ctl.set_query("al");
    assert_ne!(r1, r2);
    assert!(
        fake.lock().cancels > before,
        "set_query must cancel searches"
    );
}

// ---------------------------------------------------------------------------
// Sorting
// ---------------------------------------------------------------------------

#[test]
fn set_sort_reruns_and_toggles_direction() {
    let fake = sample();
    let mut ctl = ResultsController::new(fake.clone());
    let req = ctl.set_query("");
    refresh(&mut ctl, req);

    let r = ctl.set_sort(Column::Size);
    let page = ctl.run(r).expect("not cancelled");
    let call = fake.last_search();
    assert_eq!(
        (call.sort, call.direction),
        (SortKey::Size, Direction::Ascending)
    );
    assert!(ctl.apply(r, page));
    assert_eq!(name_at(&mut ctl, 0).as_deref(), Some("alpine"));
    assert_eq!(name_at(&mut ctl, 5).as_deref(), Some("delta.png"));

    let r = ctl.set_sort(Column::Size);
    let page = ctl.run(r).expect("not cancelled");
    let call = fake.last_search();
    assert_eq!(
        (call.sort, call.direction),
        (SortKey::Size, Direction::Descending)
    );
    assert!(ctl.apply(r, page));
    assert_eq!(name_at(&mut ctl, 0).as_deref(), Some("delta.png"));
    assert_eq!(name_at(&mut ctl, 5).as_deref(), Some("alpine"));

    let r = ctl.set_sort(Column::Modified);
    let page = ctl.run(r).expect("not cancelled");
    let call = fake.last_search();
    assert_eq!(
        (call.sort, call.direction),
        (SortKey::Modified, Direction::Ascending)
    );
    assert!(ctl.apply(r, page));
    assert_eq!(name_at(&mut ctl, 0).as_deref(), Some("alpine"));
    assert_eq!(name_at(&mut ctl, 1).as_deref(), Some("alpha.txt"));
}

#[test]
fn starts_from_the_sort_it_is_given() {
    let fake = sample();
    let given = SortState {
        column: Column::Size,
        direction: Direction::Descending,
    };
    let mut ctl = ResultsController::new(fake.clone()).with_sort(given);
    assert_eq!(ctl.sort(), given);

    let req = ctl.set_query("");
    assert_eq!(ctl.search_request(req).sort, SortKey::Size);
    assert_eq!(ctl.search_request(req).direction, Direction::Descending);
    refresh(&mut ctl, req);
    let call = fake.last_search();
    assert_eq!(
        (call.sort, call.direction),
        (SortKey::Size, Direction::Descending)
    );
    assert_eq!(name_at(&mut ctl, 0).as_deref(), Some("delta.png"));
    assert_eq!(name_at(&mut ctl, 5).as_deref(), Some("alpine"));

    // A header click goes on from the given sort.
    let _ = ctl.set_sort(Column::Size);
    assert_eq!(
        ctl.sort(),
        SortState {
            column: Column::Size,
            direction: Direction::Ascending,
        }
    );
}

#[test]
fn without_a_given_sort_it_starts_at_name_ascending() {
    let ctl = ResultsController::new(sample());
    assert_eq!(ctl.sort(), SortState::default());
}

#[test]
fn sort_is_kept_across_queries() {
    let fake = sample();
    let mut ctl = ResultsController::new(fake.clone());
    let _ = ctl.set_query("");
    let _ = ctl.set_sort(Column::Path);
    let r = ctl.set_query("a");
    let _ = ctl.run(r).expect("not cancelled");
    let call = fake.last_search();
    assert_eq!(call.query, "a");
    assert_eq!(
        (call.sort, call.direction),
        (SortKey::Path, Direction::Ascending)
    );
}

// ---------------------------------------------------------------------------
// Stale results
// ---------------------------------------------------------------------------

#[test]
fn stale_request_results_are_ignored() {
    let fake = sample();
    let mut ctl = ResultsController::new(fake.clone());
    let r1 = ctl.set_query("alp");
    let p1 = ctl.run(r1).expect("not cancelled");
    let r2 = ctl.set_query("beta");
    assert!(!ctl.apply(r1, p1), "a superseded request must be dropped");

    refresh(&mut ctl, r2);
    assert_eq!(ctl.row_count(), 1);
    assert_eq!(name_at(&mut ctl, 0).as_deref(), Some("beta.rs"));
}

#[test]
fn stale_generation_pages_are_ignored() {
    let fake = sample();
    let t0 = Instant::now();
    let mut ctl = ResultsController::new(fake.clone());
    let req = ctl.set_query("");
    let old = ctl.run(req).expect("not cancelled");
    assert_eq!(old.generation, Generation(1));

    let g2 = fake.bump();
    let r = ctl
        .on_generation(g2, t0)
        .expect("a newer generation re-runs");
    let fresh = ctl.run(r).expect("not cancelled");
    assert_eq!(fresh.generation, g2);
    assert!(ctl.apply(r, fresh));
    assert_eq!(ctl.row_count(), 6);

    let stale = ResultPage {
        generation: Generation(1),
        total: 1,
        ids: vec![0],
    };
    assert!(!ctl.apply(r, stale), "an older generation must be dropped");
    assert_eq!(ctl.row_count(), 6);
}

// ---------------------------------------------------------------------------
// New generations and throttling
// ---------------------------------------------------------------------------

#[test]
fn old_or_current_generations_do_not_rerun() {
    let fake = sample();
    let t0 = Instant::now();
    let mut ctl = ResultsController::new(fake.clone());
    let req = ctl.set_query("");
    refresh(&mut ctl, req);
    assert!(ctl.on_generation(Generation(1), t0).is_none());
    assert!(ctl.on_generation(Generation(0), t0 + 500 * MS).is_none());
    assert!(ctl.tick(t0 + 1000 * MS).is_none());
}

#[test]
fn new_generation_reruns_the_current_query_with_throttling() {
    let fake = sample();
    let t0 = Instant::now();
    let mut ctl = ResultsController::new(fake.clone());
    let req = ctl.set_query("alp");
    refresh(&mut ctl, req);
    assert_eq!(ctl.row_count(), 3);

    // The scan adds an entry and publishes generation 2.
    fake.lock().entries.push(file("/Users/me/alps.jpg", 1, 1));
    let g2 = fake.bump();
    let r = ctl
        .on_generation(g2, t0)
        .expect("first new generation re-runs");
    assert_ne!(r, req);
    refresh(&mut ctl, r);
    assert_eq!(fake.last_search().query, "alp");
    assert_eq!(ctl.row_count(), 4);

    // Generation 3 arrives within the interval: deferred, not dropped.
    fake.lock().entries.push(file("/Users/me/alpaca.png", 1, 1));
    let g3 = fake.bump();
    let searches = fake.search_count();
    assert!(ctl.on_generation(g3, t0 + 50 * MS).is_none());
    assert!(ctl.tick(t0 + 80 * MS).is_none());
    assert_eq!(fake.search_count(), searches, "throttled: no search yet");

    let r = ctl
        .tick(t0 + 150 * MS)
        .expect("the deferred re-run fires once the interval has passed");
    refresh(&mut ctl, r);
    assert_eq!(ctl.row_count(), 5);

    // Nothing is pending any more.
    assert!(ctl.tick(t0 + 400 * MS).is_none());

    // A later generation outside the interval re-runs at once.
    let g4 = fake.bump();
    assert!(ctl.on_generation(g4, t0 + 400 * MS).is_some());
}

#[test]
fn throttle_interval_is_configurable() {
    let fake = sample();
    let t0 = Instant::now();
    let mut ctl = ResultsController::new(fake.clone()).with_throttle(Duration::from_millis(500));
    let req = ctl.set_query("");
    refresh(&mut ctl, req);

    let r = ctl.on_generation(fake.bump(), t0).expect("re-runs");
    refresh(&mut ctl, r);
    assert!(ctl.on_generation(fake.bump(), t0 + 200 * MS).is_none());
    assert!(ctl.tick(t0 + 400 * MS).is_none());
    assert!(ctl.tick(t0 + 600 * MS).is_some());
}

// ---------------------------------------------------------------------------
// The window off screen
// ---------------------------------------------------------------------------

#[test]
fn a_hidden_window_remembers_new_generations_without_searching() {
    let fake = sample();
    let t0 = Instant::now();
    let mut ctl = ResultsController::new(fake.clone());
    let req = ctl.set_query("alp");
    refresh(&mut ctl, req);
    assert!(
        ctl.set_visible(false, t0).is_none(),
        "hiding never searches"
    );

    let searches = fake.search_count();
    assert!(ctl.on_generation(fake.bump(), t0).is_none());
    assert!(ctl.has_pending_rerun(), "the change is remembered");
    assert!(!ctl.wants_tick(), "no timer while hidden");
    assert!(ctl.tick(t0 + 1000 * MS).is_none(), "no re-run while hidden");
    assert!(ctl.on_generation(fake.bump(), t0 + 2000 * MS).is_none());
    assert_eq!(fake.search_count(), searches, "hidden: no search at all");
}

#[test]
fn showing_the_window_reruns_once_at_once() {
    let fake = sample();
    let t0 = Instant::now();
    let mut ctl = ResultsController::new(fake.clone());
    let req = ctl.set_query("alp");
    refresh(&mut ctl, req);
    // A re-run at t0, so the throttle interval has not passed at t0 + 30 ms.
    let r = ctl
        .on_generation(fake.bump(), t0)
        .expect("visible: re-runs");
    refresh(&mut ctl, r);
    assert!(ctl.set_visible(false, t0 + 10 * MS).is_none());
    fake.lock().entries.push(file("/Users/me/alps.jpg", 1, 1));
    assert!(ctl.on_generation(fake.bump(), t0 + 20 * MS).is_none());

    let r = ctl
        .set_visible(true, t0 + 30 * MS)
        .expect("showing the window re-runs without waiting out the throttle");
    refresh(&mut ctl, r);
    assert_eq!(ctl.row_count(), 4);
    assert!(!ctl.has_pending_rerun());
    assert!(
        ctl.set_visible(true, t0 + 40 * MS).is_none(),
        "already visible"
    );
    assert!(ctl.tick(t0 + 1000 * MS).is_none());
}

#[test]
fn showing_the_window_without_a_newer_generation_does_not_rerun() {
    let fake = sample();
    let t0 = Instant::now();
    let mut ctl = ResultsController::new(fake.clone());
    let req = ctl.set_query("");
    refresh(&mut ctl, req);
    assert!(ctl.set_visible(false, t0).is_none());
    assert!(ctl.on_generation(Generation(1), t0).is_none(), "not newer");
    assert!(ctl.set_visible(true, t0 + 500 * MS).is_none());
}

#[test]
fn the_throttle_applies_again_once_visible() {
    let fake = sample();
    let t0 = Instant::now();
    let mut ctl = ResultsController::new(fake.clone());
    let req = ctl.set_query("");
    refresh(&mut ctl, req);
    assert!(ctl.set_visible(false, t0).is_none());
    assert!(ctl.on_generation(fake.bump(), t0).is_none());
    let r = ctl
        .set_visible(true, t0 + 10 * MS)
        .expect("re-runs when shown");
    refresh(&mut ctl, r);
    assert!(
        ctl.on_generation(fake.bump(), t0 + 50 * MS).is_none(),
        "inside the interval"
    );
    assert!(ctl.wants_tick());
    assert!(ctl.tick(t0 + 80 * MS).is_none());
    assert!(ctl.tick(t0 + 120 * MS).is_some());
}

#[test]
fn new_page_invalidates_cached_rows() {
    let fake = sample();
    let t0 = Instant::now();
    let mut ctl = ResultsController::new(fake.clone());
    let req = ctl.set_query("");
    refresh(&mut ctl, req);
    assert_eq!(name_at(&mut ctl, 0).as_deref(), Some("alpha.txt"));

    fake.lock().entries.push(file("/a/aardvark", 1, 1));
    let r = ctl.on_generation(fake.bump(), t0).expect("re-runs");
    refresh(&mut ctl, r);
    assert_eq!(ctl.row_count(), 7);
    assert_eq!(name_at(&mut ctl, 0).as_deref(), Some("aardvark"));
}

// ---------------------------------------------------------------------------
// Row window cache
// ---------------------------------------------------------------------------

#[test]
fn rows_are_fetched_in_cached_windows() {
    let fake = numbered(2000);
    let mut ctl = ResultsController::new(fake.clone());
    let req = ctl.set_query("");
    refresh(&mut ctl, req);
    assert_eq!(ctl.row_count(), 2000);

    let before = fake.search_count();
    for i in 0..50 {
        assert_eq!(name_at(&mut ctl, i), Some(format!("file{i:05}.bin")));
    }
    assert!(
        fake.search_count() - before <= 1,
        "visible rows near the top should come from one window"
    );

    assert_eq!(name_at(&mut ctl, 1500).as_deref(), Some("file01500.bin"));
    let after_jump = fake.search_count();
    for i in 1490..1510 {
        assert_eq!(name_at(&mut ctl, i), Some(format!("file{i:05}.bin")));
    }
    assert!(
        fake.search_count() - after_jump <= 1,
        "rows around a fetched row are cached"
    );
    assert_eq!(name_at(&mut ctl, 1999).as_deref(), Some("file01999.bin"));
    assert!(name_at(&mut ctl, 2000).is_none());
}

#[test]
fn no_window_search_while_a_newer_generation_is_known() {
    let fake = numbered(2000);
    let t0 = Instant::now();
    let mut ctl = ResultsController::new(fake.clone()).with_row_window(64);
    let req = ctl.set_query("");
    refresh(&mut ctl, req);

    // Generation 2 is known but its re-run has not landed: uncached rows
    // read as blank without searching, rather than mixing generations.
    let r = ctl.on_generation(fake.bump(), t0).expect("re-runs");
    let before = fake.search_count();
    for i in 500..600 {
        assert!(name_at(&mut ctl, i).is_none(), "row {i} from generation 2");
    }
    assert_eq!(fake.search_count(), before, "no window searches meanwhile");
    // Cached rows of the page shown still read.
    assert_eq!(name_at(&mut ctl, 0).as_deref(), Some("file00000.bin"));

    // The re-run's page is from the newest generation: nothing is left to
    // re-run, and every row reads again.
    refresh(&mut ctl, r);
    assert!(!ctl.has_pending_rerun());
    assert!(ctl.tick(t0 + 1000 * MS).is_none());
    assert_eq!(name_at(&mut ctl, 550).as_deref(), Some("file00550.bin"));
}

// ---------------------------------------------------------------------------
// Status text
// ---------------------------------------------------------------------------

#[test]
fn status_text_while_indexing_and_after() {
    let fake = numbered(1234);
    fake.set_indexing(true);
    let t0 = Instant::now();
    let mut ctl = ResultsController::new(fake.clone());
    let req = ctl.set_query("");
    refresh(&mut ctl, req);
    assert_eq!(ctl.status_text(), "Indexing\u{2026} 1,234 objects");

    fake.set_indexing(false);
    let r = ctl.on_generation(fake.bump(), t0).expect("re-runs");
    refresh(&mut ctl, r);
    assert_eq!(ctl.status_text(), "1,234 objects");
}

#[test]
fn status_input_from_the_controller() {
    // The status bar's counts and index state come from the
    // controller and its source (`fx_ui_macos::status_view`).
    let fake = sample();
    fake.set_indexing(true);
    let mut ctl = ResultsController::new(fake.clone());
    let req = ctl.set_query("");
    refresh(&mut ctl, req);
    let input = StatusInput::from_controller(&ctl);
    assert!(!input.has_query);
    assert_eq!((input.results, input.objects), (6, 6));
    assert!(input.indexing);
    assert_eq!(status_view(&input).counts.text(), "6 objects");

    // Spaces alone are no query.
    let req = ctl.set_query("   ");
    refresh(&mut ctl, req);
    assert!(!StatusInput::from_controller(&ctl).has_query);

    fake.set_indexing(false);
    let req = ctl.set_query("alp");
    refresh(&mut ctl, req);
    let input = StatusInput::from_controller(&ctl);
    assert!(input.has_query && !input.indexing);
    assert_eq!((input.results, input.objects), (ctl.row_count(), 6));
    assert!(input.results > 0 && input.results < 6);
    assert_eq!(
        status_view(&input).counts.text(),
        format!("{} results · 6 objects", input.results)
    );
}

#[test]
fn status_text_small_count_has_no_separator() {
    let fake = sample();
    let mut ctl = ResultsController::new(fake.clone());
    let req = ctl.set_query("");
    refresh(&mut ctl, req);
    assert_eq!(ctl.status_text(), "6 objects");
}

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

#[test]
fn selection_paths_are_full_paths_in_row_order() {
    let fake = sample();
    let mut ctl = ResultsController::new(fake.clone());
    let req = ctl.set_query("");
    refresh(&mut ctl, req);
    assert_eq!(
        ctl.selection_paths(&[0, 3]),
        vec![
            "/Users/me/alpha.txt".to_string(),
            "/Users/me/src/beta.rs".to_string()
        ]
    );
    assert_eq!(
        ctl.selection_paths(&[2]),
        vec!["/Users/me/alpine".to_string()]
    );
}

#[test]
fn selection_paths_skip_rows_out_of_range() {
    let fake = sample();
    let mut ctl = ResultsController::new(fake.clone());
    let req = ctl.set_query("");
    refresh(&mut ctl, req);
    assert_eq!(
        ctl.selection_paths(&[1, 99]),
        vec!["/Users/me/docs/Alphabet.md".to_string()]
    );
    assert!(ctl.selection_paths(&[]).is_empty());
}

// ---------------------------------------------------------------------------
// Your files first
// ---------------------------------------------------------------------------

/// `sample()` by size, ascending: the plain order puts `/var/log`'s
/// gamma.log second; your files first puts it last.
const BY_SIZE_PLAIN: [&str; 6] = [
    "alpine",
    "gamma.log",
    "alpha.txt",
    "Alphabet.md",
    "beta.rs",
    "delta.png",
];
const BY_SIZE_YOURS_FIRST: [&str; 6] = [
    "alpine",
    "alpha.txt",
    "Alphabet.md",
    "beta.rs",
    "delta.png",
    "gamma.log",
];

#[test]
fn your_files_first_is_off_unless_set_and_every_search_carries_it() {
    let fake = sample();
    assert!(!ResultsController::new(fake.clone()).your_files_first());

    let mut ctl = ResultsController::new(fake.clone())
        .with_your_files_first(true)
        .with_row_window(2);
    assert!(ctl.your_files_first());
    let _ = ctl.set_sort(Column::Size);
    let req = ctl.set_query("");
    assert!(ctl.search_request(req).your_files_first);
    refresh(&mut ctl, req);
    assert!(fake.last_search().your_files_first);
    // Row windows past the first page use the same order.
    assert_eq!(names(&mut ctl), BY_SIZE_YOURS_FIRST);
    assert!(fake.search_count() > 1, "more windows were fetched");
    assert!(fake.lock().searches.iter().all(|c| c.your_files_first));
    // So does copying a selection.
    let copied = ctl.paths_for_ranges(std::slice::from_ref(&(4..6)));
    assert_eq!(
        copied.paths,
        ["/Users/me/pics/delta.png", "/var/log/gamma.log"]
    );
    assert!(fake.last_search().your_files_first);
}

#[test]
fn toggling_your_files_first_reruns_the_search() {
    let fake = sample();
    let mut ctl = ResultsController::new(fake.clone()).with_your_files_first(true);
    let _ = ctl.set_sort(Column::Size);
    let first = ctl.set_query("");
    refresh(&mut ctl, first);
    assert_eq!(names(&mut ctl), BY_SIZE_YOURS_FIRST);

    let cancels = fake.lock().cancels;
    let off = ctl
        .set_your_files_first(false)
        .expect("a change re-runs the search");
    assert!(off > first);
    assert!(
        fake.lock().cancels > cancels,
        "the search in flight is cancelled"
    );
    assert!(!ctl.your_files_first());
    assert!(!ctl.search_request(off).your_files_first);
    // A page asked for before the change is not shown.
    assert!(ctl.run(first).is_err(), "the old request is superseded");
    let stale = fake
        .search_grouped("", SortKey::Size, Direction::Ascending, true, 0, 10)
        .unwrap();
    assert!(!ctl.apply(first, stale));

    refresh(&mut ctl, off);
    let call = fake.last_search();
    assert!(!call.your_files_first);
    assert_eq!((call.query.as_str(), call.sort), ("", SortKey::Size));
    assert_eq!(
        names(&mut ctl),
        BY_SIZE_PLAIN,
        "with it off: the plain sort"
    );

    let searches = fake.search_count();
    assert!(
        ctl.set_your_files_first(false).is_none(),
        "no change, no search"
    );
    assert_eq!(fake.search_count(), searches);

    let on = ctl
        .set_your_files_first(true)
        .expect("a change re-runs the search");
    refresh(&mut ctl, on);
    assert!(fake.last_search().your_files_first);
    assert_eq!(names(&mut ctl), BY_SIZE_YOURS_FIRST);
}

#[cfg(feature = "measure")]
#[test]
fn sources_without_groups_answer_with_the_plain_sort() {
    let source = SyntheticSource::new(500, 3);
    for on in [false, true] {
        assert_eq!(
            source
                .search_grouped("a", SortKey::Size, Direction::Descending, on, 5, 20)
                .unwrap(),
            source
                .search("a", SortKey::Size, Direction::Descending, 5, 20)
                .unwrap()
        );
    }
}

/// With your files first, a kept
/// selection is looked up in the grouped order, so it lands on the rows the
/// grouped list shows, not on the files' rows in the plain sort.
#[test]
fn a_kept_selection_follows_the_grouped_order() {
    let fake = sample();
    let mut ctl = ResultsController::new(fake.clone()).with_your_files_first(true);
    let _ = ctl.set_sort(Column::Size);
    let req = ctl.set_query("");
    refresh(&mut ctl, req);
    assert_eq!(names(&mut ctl), BY_SIZE_YOURS_FIRST);
    // gamma.log: last in the grouped list, second in the plain one.
    ctl.remember_selection(std::slice::from_ref(&(5..6)));

    // A new file of yours, smaller than any other: one row more above
    // gamma.log in either order.
    fake.lock()
        .entries
        .push(file("/Users/me/tiny.txt", 1, 1_600_000_600));
    let g = fake.bump();
    let rerun = ctl
        .on_generation(g, Instant::now() + Duration::from_secs(1))
        .expect("a live update re-runs the search");
    refresh(&mut ctl, rerun);
    let r = ctl.reselect(true).expect("the selection is restored");
    assert_eq!(r.rows, [6], "the grouped row, not the plain row 2");
    assert_eq!(r.scroll_to, Some(6));
    assert_eq!(name_at(&mut ctl, 6).as_deref(), Some("gamma.log"));

    // Off, the same lookup gives the plain row.
    let off = ctl
        .set_your_files_first(false)
        .expect("a change re-runs the search");
    refresh(&mut ctl, off);
    assert_eq!(name_at(&mut ctl, 2).as_deref(), Some("gamma.log"));
    ctl.remember_selection(std::slice::from_ref(&(2..3)));
    let g = fake.bump();
    let rerun = ctl
        .on_generation(g, Instant::now() + Duration::from_secs(2))
        .expect("a live update re-runs the search");
    refresh(&mut ctl, rerun);
    assert_eq!(ctl.reselect(true).expect("restored").rows, [2]);
}
