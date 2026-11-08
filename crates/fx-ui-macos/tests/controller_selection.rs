//! Bounded selection resolution and single-generation paging in
//! `ResultsController`, driven by a counting fake `ResultsSource`.
//!
//! Contracts checked:
//!
//! - `ResultsController::paths_for_ranges(&[Range<usize>]) -> CopiedPaths`
//!   resolves each contiguous selected range with one `search(offset, len)`
//!   (a range may be split into chunks at a documented cap), so a 1M-row
//!   selection costs O(ranges) searches, not O(rows / row window). Paths come
//!   back in the order given, clipped to the rows shown, and at most
//!   `MAX_COPY_PATHS` of them; `CopiedPaths::truncated` says when the cap cut
//!   the copy short, so the status bar can say so.
//! - A row window fetched from a generation newer than the page shown is
//!   never mixed into the view: it is dropped and a re-run is requested (fired
//!   by `tick`), and once the re-run is applied, `row_count` and every row come
//!   from the one new generation, with no repeated, skipped or blank rows.

// Single-range selections (`&[0..n]`) are the point of these tests, and the
// cap's bounds are checked as plain assertions on purpose.
#![allow(clippy::single_range_in_vec_init, clippy::assertions_on_constants)]

use std::ops::Range;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use fx_engine::Generation;
use fx_query::Cancelled;
use fx_sort::{Direction, SortKey};
use fx_ui_macos::controller::{CopiedPaths, MAX_COPY_PATHS, RequestId, ResultsController};
use fx_ui_macos::source::{ResultPage, ResultsSource, Row, RowPositions, SourceStatus};

/// Extra searches allowed on top of one per range, for chunking long ranges.
/// A chunk cap of at least 32,768 rows keeps a 1M-row copy inside this.
const CHUNK_SLACK: usize = 32;

// ---------------------------------------------------------------------------
// Counting fake source
// ---------------------------------------------------------------------------

/// Entries at generation `g`: `base + 10 * (g - 1)` of them; the entry at
/// sorted position `p` has ID `p` and path `/g{g}/f{p:08}`, so every row says
/// which generation it came from and where it sits.
struct State {
    generation: u64,
    searches: Vec<(usize, usize)>,
    subscribers: Vec<Sender<Generation>>,
}

#[derive(Clone)]
struct PagedSource {
    base: usize,
    state: Arc<Mutex<State>>,
}

impl PagedSource {
    fn new(base: usize) -> PagedSource {
        PagedSource {
            base,
            state: Arc::new(Mutex::new(State {
                generation: 1,
                searches: Vec::new(),
                subscribers: Vec::new(),
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    fn total_at(&self, g: u64) -> usize {
        self.base + 10 * (g as usize - 1)
    }

    /// Publishes the next generation and returns it.
    fn bump(&self) -> Generation {
        let mut s = self.lock();
        s.generation += 1;
        let g = Generation(s.generation);
        s.subscribers.retain(|tx| tx.send(g).is_ok());
        g
    }

    /// The `(offset, limit)` of every search since the last call.
    fn take_searches(&self) -> Vec<(usize, usize)> {
        std::mem::take(&mut self.lock().searches)
    }
}

fn path_at(g: u64, p: usize) -> String {
    format!("/g{g}/f{p:08}")
}

/// `(generation, position)` encoded in a fake row's path.
fn decode(row: &Row) -> (u64, usize) {
    let rest = row.full_path.strip_prefix("/g").expect("fake path");
    let (g, p) = rest.split_once("/f").expect("fake path");
    (g.parse().unwrap(), p.parse().unwrap())
}

impl ResultsSource for PagedSource {
    fn search(
        &self,
        _query: &str,
        _sort: SortKey,
        _dir: Direction,
        offset: usize,
        limit: usize,
    ) -> Result<ResultPage, Cancelled> {
        let mut s = self.lock();
        s.searches.push((offset, limit));
        let g = s.generation;
        let total = self.total_at(g);
        let start = offset.min(total);
        let end = offset.saturating_add(limit).min(total);
        Ok(ResultPage {
            generation: Generation(g),
            total,
            ids: (start..end).map(|p| p as u32).collect(),
        })
    }

    fn cancel(&self) {}

    fn row(&self, generation: Generation, id: u32) -> Option<Row> {
        let p = id as usize;
        if generation.0 == 0 || p >= self.total_at(generation.0) {
            return None;
        }
        Some(Row {
            name: format!("f{p:08}"),
            full_path: path_at(generation.0, p),
            size: Some(p as u64),
            modified: Some(0),
            is_folder: false,
        })
    }

    /// A path names its generation: only the current one's are found.
    fn positions(
        &self,
        _query: &str,
        _sort: SortKey,
        _dir: Direction,
        paths: &[String],
    ) -> Result<RowPositions, Cancelled> {
        let g = self.lock().generation;
        let total = self.total_at(g);
        let rows = paths
            .iter()
            .map(|path| {
                let rest = path.strip_prefix("/g")?;
                let (pg, p) = rest.split_once("/f")?;
                let p: usize = p.parse().ok()?;
                (pg.parse::<u64>().ok()? == g && p < total).then_some(p)
            })
            .collect();
        Ok(RowPositions {
            generation: Generation(g),
            rows,
        })
    }

    fn generation(&self) -> Generation {
        Generation(self.lock().generation)
    }

    fn status(&self) -> SourceStatus {
        let g = self.lock().generation;
        SourceStatus {
            indexing: false,
            entries: self.total_at(g),
        }
    }

    fn subscribe(&self) -> Receiver<Generation> {
        let (tx, rx) = channel();
        self.lock().subscribers.push(tx);
        rx
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A controller over `source` showing the first page of the empty query,
/// with no throttle so `tick` fires any requested re-run at once.
fn shown(source: PagedSource, row_window: usize) -> ResultsController<PagedSource> {
    let mut c = ResultsController::new(source)
        .with_row_window(row_window)
        .with_throttle(Duration::ZERO);
    let req = c.set_query("");
    let page = c.run(req).expect("not cancelled");
    assert!(c.apply(req, page), "first page applied");
    c
}

fn copy(c: &mut ResultsController<PagedSource>, ranges: &[Range<usize>]) -> CopiedPaths {
    c.paths_for_ranges(ranges)
}

/// Runs re-run `req` the way the window's worker does and applies it.
fn apply_rerun(c: &mut ResultsController<PagedSource>, req: RequestId) {
    let page = c
        .search_request(req)
        .execute(c.source())
        .expect("not cancelled");
    c.apply(req, page);
}

/// Fires and applies requested re-runs until none is pending.
fn settle(c: &mut ResultsController<PagedSource>) {
    for _ in 0..16 {
        match c.tick(Instant::now()) {
            Some(req) => apply_rerun(c, req),
            None => return,
        }
    }
    panic!("re-runs never settled");
}

/// Asserts every position of `expected` matches `paths`, reporting the first
/// mismatch only (the lists can hold a million entries).
fn assert_paths(paths: &[String], expected: impl Iterator<Item = String>) {
    let mut n = 0;
    for (k, want) in expected.enumerate() {
        assert!(k < paths.len(), "only {} paths, expected more", paths.len());
        assert_eq!(paths[k], want, "path {k}");
        n = k + 1;
    }
    assert_eq!(paths.len(), n, "more paths than expected");
}

/// Full scan of the rows shown: every row is present, from one generation
/// whose total is `row_count`, and at its own position.
fn assert_consistent_scan(c: &mut ResultsController<PagedSource>) {
    let total = c.row_count();
    let mut generation: Option<u64> = None;
    for i in 0..total {
        let row = c
            .row(i)
            .unwrap_or_else(|| panic!("blank row {i} inside total {total}"))
            .clone();
        let (g, p) = decode(&row);
        assert_eq!(p, i, "row {i} shows position {p}: repeated or skipped");
        assert_eq!(
            *generation.get_or_insert(g),
            g,
            "row {i} from another generation"
        );
    }
    if let Some(g) = generation {
        assert_eq!(
            c.source().total_at(g),
            total,
            "total from another generation"
        );
    }
}

// ---------------------------------------------------------------------------
// paths_for_ranges
// ---------------------------------------------------------------------------

#[test]
fn a_one_million_row_range_costs_o_ranges_searches() {
    let rows = 1_000_000;
    let source = PagedSource::new(rows);
    let mut c = shown(source.clone(), 16);
    source.take_searches();

    let out = copy(&mut c, &[0..rows]);

    let expected = rows.min(MAX_COPY_PATHS);
    assert_eq!(out.truncated, expected < rows);
    assert_paths(&out.paths, (0..expected).map(|p| path_at(1, p)));
    let searches = source.take_searches().len();
    assert!(
        searches <= 1 + CHUNK_SLACK,
        "{searches} searches for one range (row by row would be ~{})",
        rows / 16
    );
}

#[test]
fn many_ranges_cost_one_search_each_not_one_per_window() {
    let source = PagedSource::new(2_000_000);
    let mut c = shown(source.clone(), 16);
    source.take_searches();
    let ranges: Vec<Range<usize>> = (0..1_000).map(|k| k * 2_000..k * 2_000 + 1_000).collect();

    let out = copy(&mut c, &ranges);

    let all = ranges.iter().flat_map(|r| r.clone());
    let expected = 1_000_000usize.min(MAX_COPY_PATHS);
    assert_eq!(out.truncated, expected < 1_000_000);
    assert_paths(&out.paths, all.take(expected).map(|p| path_at(1, p)));
    let searches = source.take_searches().len();
    assert!(
        searches <= ranges.len() + CHUNK_SLACK,
        "{searches} searches for {} ranges",
        ranges.len()
    );
}

#[test]
fn ranges_keep_their_order_and_are_clipped_to_the_rows_shown() {
    let source = PagedSource::new(100);
    let mut c = shown(source.clone(), 8);
    source.take_searches();

    let ranges = [50..53, 10..12, 5..5, 98..120, 200..300];
    let out = copy(&mut c, &ranges);

    let expected = [50, 51, 52, 10, 11, 98, 99].map(|p| path_at(1, p));
    assert_eq!(out.paths, expected);
    assert!(!out.truncated);
    let searches = source.take_searches().len();
    assert!(
        searches <= 3,
        "{searches} searches for 3 non-empty ranges in range"
    );
}

#[test]
fn an_empty_selection_copies_nothing_without_searching() {
    let source = PagedSource::new(100);
    let mut c = shown(source.clone(), 8);
    source.take_searches();

    let out = copy(&mut c, &[]);

    assert!(out.paths.is_empty());
    assert!(!out.truncated);
    assert!(source.take_searches().is_empty());
}

#[test]
fn a_copy_past_the_cap_is_truncated_and_says_so() {
    assert!(MAX_COPY_PATHS > 0);
    assert!(
        MAX_COPY_PATHS <= 10_000_000,
        "clipboard copies must be bounded to a reasonable size"
    );
    let rows = MAX_COPY_PATHS + 3;
    let source = PagedSource::new(rows);
    let mut c = shown(source, 64);

    let out = copy(&mut c, &[0..2, 2..rows]);

    assert!(out.truncated);
    assert_eq!(out.paths.len(), MAX_COPY_PATHS);
    assert_eq!(out.paths[0], path_at(1, 0));
    assert_eq!(
        out.paths[MAX_COPY_PATHS - 1],
        path_at(1, MAX_COPY_PATHS - 1)
    );

    // Exactly at the cap is not truncated.
    let out = copy(&mut c, &[0..MAX_COPY_PATHS]);
    assert!(!out.truncated);
    assert_eq!(out.paths.len(), MAX_COPY_PATHS);
}

// ---------------------------------------------------------------------------
// Single-generation paging
// ---------------------------------------------------------------------------

#[test]
fn a_newer_generation_window_is_dropped_and_a_rerun_requested() {
    let source = PagedSource::new(1_000);
    let mut c = shown(source.clone(), 64);
    assert_eq!(c.row_count(), 1_000);
    for i in 0..100 {
        let (g, p) = decode(c.row(i).expect("row shown"));
        assert_eq!((g, p), (1, i));
    }

    // Generation 2 is published (1,010 rows) before the controller hears of it.
    source.bump();

    // Row 900 is not cached: its window comes back from generation 2 and must
    // not be shown under generation 1's total.
    if let Some(row) = c.row(900) {
        assert_eq!(
            decode(row),
            (1, 900),
            "generation 2 row under generation 1 total"
        );
    }
    assert_eq!(c.row_count(), 1_000, "total moved without its rows");
    // Rows already cached from generation 1 still read as generation 1.
    assert_eq!(decode(c.row(10).expect("cached row")), (1, 10));

    // A re-run was requested; once applied, total and rows are generation 2.
    let req = c
        .tick(Instant::now())
        .expect("a re-run is requested after dropping a newer window");
    let page = c
        .search_request(req)
        .execute(c.source())
        .expect("not cancelled");
    assert_eq!(page.generation, Generation(2));
    assert!(c.apply(req, page));
    assert_eq!(c.row_count(), 1_010);
    assert_consistent_scan(&mut c);
    assert_eq!(decode(c.row(1_009).expect("last row")), (2, 1_009));
}

#[test]
fn a_window_fetched_while_a_rerun_is_in_flight_is_not_mixed() {
    let source = PagedSource::new(500);
    let mut c = shown(source.clone(), 32);
    let g = source.bump();
    // The controller hears of generation 2 and hands out a re-run, which a
    // worker would run; the table asks for an uncached row meanwhile.
    let req = c
        .on_generation(g, Instant::now())
        .expect("re-run for a new generation");
    if let Some(row) = c.row(400) {
        assert_eq!(
            decode(row),
            (1, 400),
            "generation 2 row under generation 1 total"
        );
    }
    assert_eq!(c.row_count(), 500);

    apply_rerun(&mut c, req);
    settle(&mut c);
    assert_eq!(c.row_count(), 510);
    assert_consistent_scan(&mut c);
}

#[test]
fn rows_never_mix_generations_while_snapshots_keep_arriving() {
    let source = PagedSource::new(3_000);
    let mut c = shown(source.clone(), 50);

    // Scroll down, then back up, while new generations are published.
    let order: Vec<usize> = (0..3_000).chain((0..3_000).rev()).collect();
    for (step, &i) in order.iter().enumerate() {
        if step % 137 == 0 {
            source.bump();
        }
        if step % 401 == 0 {
            settle(&mut c);
        }
        let total = c.row_count();
        if i >= total {
            continue;
        }
        if let Some((g, p)) = c.row(i).map(decode) {
            assert_eq!(p, i, "row {i} shows position {p}");
            assert_eq!(
                c.source().total_at(g),
                total,
                "row {i} from generation {g} under a total of {total}"
            );
        }
    }

    // The window hears of the latest generation through its subscription.
    if let Some(req) = c.on_generation(source.generation(), Instant::now()) {
        apply_rerun(&mut c, req);
    }
    settle(&mut c);
    let latest = source.generation().0;
    assert_eq!(c.row_count(), source.total_at(latest));
    assert_consistent_scan(&mut c);
}
