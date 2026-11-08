//! Fix 6: the selection stays on the same files when the results refresh.
//!
//! The bug: on a live update (a new engine generation re-runs the query),
//! the table kept the selected row numbers, so a file added or removed
//! above the selection moved the highlight onto another file. The fix keeps
//! the selected files by path in the platform-agnostic
//! `ResultsController` and finds their new rows with one
//! `ResultsSource::positions` call (`positions_grouped`, so with "Your
//! files first" on the rows are those of the grouped list; see
//! `tests/controller.rs`).
//!
//! Contracts checked, with a fake source whose files change per generation:
//!
//! - `remember_selection(&[Range<usize>])` records the files of the selected
//!   rows of the page shown (rows already cached cost no search). More than
//!   `MAX_KEPT_SELECTION` (1,000) rows are not kept.
//! - After a page of the same query and sort from a newer generation is
//!   applied, `reselect(first_in_view)` returns the new rows of the
//!   remembered files (ascending), dropping files that were deleted or no
//!   longer match, and `scroll_to`: the first selected file's new row when it
//!   was in view, else `None`. A selection that was not kept comes back as
//!   an empty `Reselection` (cleared). `None` means "leave the selection as
//!   it is": nothing was selected, or the page shown is the generation the
//!   rows were found in.
//! - A new query or sort forgets the selection.
//! - Copy, open, reveal and drag never resolve row positions against a
//!   newer generation than the page shown: `paths_of_selection(ranges,
//!   selected)` returns the remembered files when they are the whole
//!   selection, and `paths_for_ranges` reads rows from the cached windows
//!   of the page shown or from a search of that same generation; otherwise
//!   it resolves nothing, sets `CopiedPaths::outdated` and requests a
//!   re-run.

// Single-range selections (`&[0..n]`) are the point of several tests.
#![allow(clippy::single_range_in_vec_init)]

use std::ops::Range;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use fx_engine::Generation;
use fx_query::Cancelled;
use fx_sort::{Direction, SortKey};
use fx_ui_macos::columns::Column;
use fx_ui_macos::controller::{MAX_KEPT_SELECTION, Reselection, ResultsController};
use fx_ui_macos::source::{ResultPage, ResultsSource, Row, RowPositions, SourceStatus};

// ---------------------------------------------------------------------------
// Fake source
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct File {
    id: u32,
    path: String,
    size: u64,
}

impl File {
    fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap()
    }
}

struct State {
    generation: u64,
    files: Vec<File>,
    next_id: u32,
    searches: usize,
    position_calls: usize,
    subscribers: Vec<Sender<Generation>>,
}

/// Files under `/f/`. The Size sort orders by size, then name; every other
/// key by name, then path. A query `size>N` matches files over `N` bytes;
/// any other query is a case-blind substring of the name. IDs are stable
/// until `renumber`.
#[derive(Clone)]
struct ChangingSource {
    state: Arc<Mutex<State>>,
}

impl ChangingSource {
    fn new(names: &[&str]) -> ChangingSource {
        let source = ChangingSource {
            state: Arc::new(Mutex::new(State {
                generation: 1,
                files: Vec::new(),
                next_id: 0,
                searches: 0,
                position_calls: 0,
                subscribers: Vec::new(),
            })),
        };
        for name in names {
            source.add(name);
        }
        source
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    fn add(&self, name: &str) {
        let mut s = self.lock();
        let id = s.next_id;
        s.next_id += 1;
        s.files.push(File {
            id,
            path: format!("/f/{name}"),
            size: 0,
        });
    }

    fn remove(&self, name: &str) {
        let path = format!("/f/{name}");
        let mut s = self.lock();
        let before = s.files.len();
        s.files.retain(|f| f.path != path);
        assert_eq!(s.files.len() + 1, before, "{name} existed");
    }

    fn set_size(&self, name: &str, size: u64) {
        let path = format!("/f/{name}");
        let mut s = self.lock();
        let file = s.files.iter_mut().find(|f| f.path == path).expect("exists");
        file.size = size;
    }

    /// Gives every file a new ID, as a different index of the same files
    /// would (for example the full-access engine swapped in by Fix 1).
    fn renumber(&self) {
        let mut s = self.lock();
        let base = s.next_id + 10_000;
        let n = s.files.len() as u32;
        for (k, f) in s.files.iter_mut().enumerate() {
            f.id = base + (n - k as u32);
        }
        s.next_id = base + n + 1;
    }

    /// Publishes the next generation and returns it.
    fn bump(&self) -> Generation {
        let mut s = self.lock();
        s.generation += 1;
        let g = Generation(s.generation);
        s.subscribers.retain(|tx| tx.send(g).is_ok());
        g
    }

    fn searches(&self) -> usize {
        self.lock().searches
    }

    fn position_calls(&self) -> usize {
        self.lock().position_calls
    }

    /// The matches of `query`, in display order.
    fn sorted(s: &State, query: &str, sort: SortKey, dir: Direction) -> Vec<File> {
        let needle = query.to_lowercase();
        let over: Option<u64> = query.strip_prefix("size>").map(|n| n.parse().unwrap());
        let mut files: Vec<File> = s
            .files
            .iter()
            .filter(|f| match over {
                Some(n) => f.size > n,
                None => f.name().to_lowercase().contains(&needle),
            })
            .cloned()
            .collect();
        let size = |f: &File| if sort == SortKey::Size { f.size } else { 0 };
        files.sort_by(|a, b| {
            (size(a), a.name().to_lowercase(), &a.path).cmp(&(
                size(b),
                b.name().to_lowercase(),
                &b.path,
            ))
        });
        if dir == Direction::Descending {
            files.reverse();
        }
        files
    }
}

impl ResultsSource for ChangingSource {
    fn search(
        &self,
        query: &str,
        sort: SortKey,
        dir: Direction,
        offset: usize,
        limit: usize,
    ) -> Result<ResultPage, Cancelled> {
        let mut s = self.lock();
        s.searches += 1;
        let files = ChangingSource::sorted(&s, query, sort, dir);
        Ok(ResultPage {
            generation: Generation(s.generation),
            total: files.len(),
            ids: files
                .iter()
                .skip(offset)
                .take(limit)
                .map(|f| f.id)
                .collect(),
        })
    }

    fn cancel(&self) {}

    fn row(&self, _generation: Generation, id: u32) -> Option<Row> {
        let s = self.lock();
        let f = s.files.iter().find(|f| f.id == id)?;
        Some(Row {
            name: f.name().to_string(),
            full_path: f.path.clone(),
            size: Some(1),
            modified: Some(0),
            is_folder: false,
        })
    }

    fn positions(
        &self,
        query: &str,
        sort: SortKey,
        dir: Direction,
        paths: &[String],
    ) -> Result<RowPositions, Cancelled> {
        let mut s = self.lock();
        s.position_calls += 1;
        let files = ChangingSource::sorted(&s, query, sort, dir);
        Ok(RowPositions {
            generation: Generation(s.generation),
            rows: paths
                .iter()
                .map(|p| files.iter().position(|f| &f.path == p))
                .collect(),
        })
    }

    fn generation(&self) -> Generation {
        Generation(self.lock().generation)
    }

    fn status(&self) -> SourceStatus {
        SourceStatus {
            indexing: false,
            entries: self.lock().files.len(),
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

type Controller = ResultsController<ChangingSource>;

/// A controller showing the first page of `query`, with no throttle.
fn shown(source: &ChangingSource, query: &str, row_window: usize) -> Controller {
    let mut c = ResultsController::new(source.clone())
        .with_row_window(row_window)
        .with_throttle(Duration::ZERO);
    let req = c.set_query(query);
    let page = c.run(req).expect("not cancelled");
    assert!(c.apply(req, page), "first page applied");
    c
}

/// A live update: the source publishes a generation, the controller re-runs
/// the same query and applies the page, as the window's drain does.
fn live_update(c: &mut Controller) {
    let g = c.source().bump();
    let req = c
        .on_generation(g, Instant::now())
        .expect("a new generation re-runs the query");
    let page = c.run(req).expect("not cancelled");
    assert_eq!(page.generation, g);
    assert!(c.apply(req, page), "re-run page applied");
}

fn name_at(c: &mut Controller, row: usize) -> String {
    c.row(row).expect("row shown").name.clone()
}

fn names_at(c: &mut Controller, rows: &[usize]) -> Vec<String> {
    rows.iter().map(|&r| name_at(c, r)).collect()
}

fn one(row: usize) -> Range<usize> {
    row..row + 1
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn a_file_added_above_the_selection_keeps_the_same_file_selected() {
    let source = ChangingSource::new(&["b.txt", "c.txt", "d.txt", "e.txt"]);
    let mut c = shown(&source, "", 64);
    assert_eq!(name_at(&mut c, 1), "c.txt");
    c.remember_selection(&[one(1)]);

    source.add("a.txt");
    live_update(&mut c);

    // The bug: the old row number now shows another file.
    assert_eq!(name_at(&mut c, 1), "b.txt");
    let r = c.reselect(true).expect("the selection is re-mapped");
    assert_eq!(r.rows, vec![2]);
    assert_eq!(names_at(&mut c, &r.rows), ["c.txt"]);
    assert_eq!(
        r.scroll_to,
        Some(2),
        "the first selected file stays in view"
    );

    // The kept selection follows the file through further updates.
    source.add("a0.txt");
    live_update(&mut c);
    let r = c.reselect(false).expect("re-mapped again");
    assert_eq!(r.rows, vec![3]);
    assert_eq!(names_at(&mut c, &r.rows), ["c.txt"]);
    assert_eq!(r.scroll_to, None, "out of view: the scroll is left alone");
}

#[test]
fn a_file_removed_above_the_selection_keeps_the_same_file_selected() {
    let source = ChangingSource::new(&["a.txt", "b.txt", "c.txt", "d.txt"]);
    let mut c = shown(&source, "", 64);
    c.remember_selection(&[one(3)]);

    source.remove("a.txt");
    live_update(&mut c);

    let r = c.reselect(true).expect("re-mapped");
    assert_eq!(r.rows, vec![2]);
    assert_eq!(names_at(&mut c, &r.rows), ["d.txt"]);
    assert_eq!(r.scroll_to, Some(2));
}

#[test]
fn a_deleted_selected_file_is_dropped_from_the_selection() {
    let source = ChangingSource::new(&["b.txt", "c.txt", "d.txt", "e.txt"]);
    let mut c = shown(&source, "", 64);
    c.remember_selection(&[one(1), one(3)]);

    source.remove("c.txt");
    live_update(&mut c);

    let r = c.reselect(true).expect("re-mapped");
    assert_eq!(names_at(&mut c, &r.rows), ["e.txt"]);
    assert_eq!(r.rows, vec![2]);
    assert_eq!(
        r.scroll_to, None,
        "the first selected file is gone, so the scroll is left alone"
    );

    // Deleting the only selected file clears the selection.
    source.remove("e.txt");
    live_update(&mut c);
    let r = c.reselect(true).expect("re-mapped");
    assert_eq!(r, Reselection::default());

    // Nothing is selected any more: later updates leave it alone.
    source.add("f.txt");
    live_update(&mut c);
    assert_eq!(c.reselect(true), None);
}

#[test]
fn a_file_that_no_longer_matches_is_dropped_from_the_selection() {
    let source = ChangingSource::new(&["a", "b", "c", "d"]);
    for (name, size) in [("a", 200), ("b", 300), ("c", 50), ("d", 400)] {
        source.set_size(name, size);
    }
    let mut c = shown(&source, "size>100", 64);
    assert_eq!(c.row_count(), 3);
    c.remember_selection(&[1..3]);
    assert_eq!(names_at(&mut c, &[1, 2]), ["b", "d"]);

    // Same path, but it no longer matches the query.
    source.set_size("b", 10);
    live_update(&mut c);

    let r = c.reselect(true).expect("re-mapped");
    assert_eq!(r.rows, vec![1]);
    assert_eq!(names_at(&mut c, &r.rows), ["d"]);
    assert_eq!(
        r.scroll_to, None,
        "the first selected file no longer matches"
    );
}

#[test]
fn a_multi_selection_keeps_every_file_at_its_new_row() {
    let names: Vec<String> = (0..20).step_by(2).map(|n| format!("f{n:02}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let source = ChangingSource::new(&refs);
    let mut c = shown(&source, "", 64);
    // Rows 1, 3, 4 and 7: f02, f06, f08 and f14.
    c.remember_selection(&[one(1), 3..5, one(7)]);
    let selected = names_at(&mut c, &[1, 3, 4, 7]);
    assert_eq!(selected, ["f02", "f06", "f08", "f14"]);

    // Files appear above and between the selected ones; an unselected file
    // goes away.
    source.add("f01");
    source.add("f05");
    source.add("f07");
    source.remove("f12");
    live_update(&mut c);

    let r = c.reselect(true).expect("re-mapped");
    assert_eq!(names_at(&mut c, &r.rows), selected);
    assert!(r.rows.windows(2).all(|w| w[0] < w[1]), "ascending: {r:?}");
    assert_eq!(r.rows, vec![2, 5, 7, 9]);
    assert_eq!(r.scroll_to, Some(2), "f02's new row");
}

#[test]
fn selected_files_that_swap_places_come_back_in_row_order() {
    // Sorted by size, the two selected files swap places: the rows come
    // back ascending, and the scroll target is still the file that was
    // selected first (the one on the lowest row before the update).
    let source = ChangingSource::new(&["a", "b", "c", "d"]);
    for (name, size) in [("a", 1), ("b", 2), ("c", 3), ("d", 4)] {
        source.set_size(name, size);
    }
    let mut c = shown(&source, "", 64);
    let req = c.set_sort(Column::Size);
    let page = c.run(req).unwrap();
    assert!(c.apply(req, page));
    c.remember_selection(&[one(0), one(3)]);
    assert_eq!(names_at(&mut c, &[0, 3]), ["a", "d"]);

    source.set_size("a", 9);
    source.set_size("d", 0);
    live_update(&mut c);

    let r = c.reselect(true).expect("re-mapped");
    assert_eq!(r.rows, vec![0, 3]);
    assert_eq!(names_at(&mut c, &r.rows), ["d", "a"]);
    assert_eq!(r.scroll_to, Some(3), "a, selected first, is now on row 3");
}

#[test]
fn a_selection_of_up_to_1000_rows_is_kept() {
    assert_eq!(MAX_KEPT_SELECTION, 1_000);
    let names: Vec<String> = (0..2_000).map(|n| format!("n{n:05}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let source = ChangingSource::new(&refs);
    let mut c = shown(&source, "", 256);
    c.remember_selection(&[10..10 + MAX_KEPT_SELECTION]);

    source.add("a-first");
    live_update(&mut c);

    let calls = source.position_calls();
    let r = c.reselect(false).expect("re-mapped");
    assert_eq!(
        source.position_calls(),
        calls + 1,
        "one lookup for them all"
    );
    let want: Vec<usize> = (11..11 + MAX_KEPT_SELECTION).collect();
    assert_eq!(r.rows, want);
    assert_eq!(name_at(&mut c, 11), "n00010");
}

#[test]
fn a_selection_over_1000_rows_is_cleared() {
    let names: Vec<String> = (0..2_000).map(|n| format!("n{n:05}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let source = ChangingSource::new(&refs);
    let mut c = shown(&source, "", 256);
    let searches = source.searches();
    c.remember_selection(&[0..MAX_KEPT_SELECTION + 1]);
    assert_eq!(source.searches(), searches, "a large selection is not read");

    source.add("a-first");
    live_update(&mut c);

    let calls = source.position_calls();
    assert_eq!(c.reselect(true), Some(Reselection::default()), "cleared");
    assert_eq!(source.position_calls(), calls, "no lookup");
    // Select All over the whole result set is cleared the same way.
    let all = c.row_count();
    c.remember_selection(&[0..all]);
    source.add("a-second");
    live_update(&mut c);
    assert_eq!(c.reselect(true), Some(Reselection::default()));
}

#[test]
fn rows_renumbered_entirely_keep_the_same_files() {
    let names: Vec<String> = (0..50).map(|n| format!("m{n:03}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let source = ChangingSource::new(&refs);
    let mut c = shown(&source, "", 16);
    c.remember_selection(&[one(2), 20..22, one(49)]);
    let selected = names_at(&mut c, &[2, 20, 21, 49]);
    let old_ids: Vec<u32> = c
        .source()
        .search("", SortKey::Name, Direction::Ascending, 0, 50)
        .unwrap()
        .ids;

    // Another index of the same files: every ID changes, and many more
    // files show up between them.
    source.renumber();
    for n in 0..200 {
        source.add(&format!("m{:03}-{n:03}", n % 50));
    }
    live_update(&mut c);
    let new_ids = c
        .source()
        .search("", SortKey::Name, Direction::Ascending, 0, 250)
        .unwrap()
        .ids;
    assert!(
        new_ids.iter().all(|id| !old_ids.contains(id)),
        "all renumbered"
    );

    let r = c.reselect(true).expect("re-mapped");
    assert_eq!(names_at(&mut c, &r.rows), selected);
    assert_eq!(r.rows.len(), 4);
    assert_eq!(r.scroll_to, Some(r.rows[0]));
}

#[test]
fn the_first_selected_file_is_scrolled_to_only_when_it_was_in_view() {
    let source = ChangingSource::new(&["b", "c", "d"]);
    let mut c = shown(&source, "", 64);
    c.remember_selection(&[one(2)]);
    source.add("a");
    live_update(&mut c);
    assert_eq!(c.reselect(false).expect("re-mapped").scroll_to, None);

    c.remember_selection(&[one(3)]);
    source.add("a0");
    live_update(&mut c);
    let r = c.reselect(true).expect("re-mapped");
    assert_eq!(r.rows, vec![4]);
    assert_eq!(r.scroll_to, Some(4));
}

#[test]
fn nothing_selected_or_the_same_generation_leaves_the_selection_alone() {
    let source = ChangingSource::new(&["a", "b", "c"]);
    let mut c = shown(&source, "", 64);

    // Nothing selected.
    source.add("0");
    live_update(&mut c);
    assert_eq!(c.reselect(true), None);

    // Selected, but the page shown is still the generation it was read from.
    c.remember_selection(&[one(1)]);
    let calls = source.position_calls();
    assert_eq!(c.reselect(true), None);
    assert_eq!(source.position_calls(), calls, "no lookup");

    // An empty selection forgets the files.
    c.remember_selection(&[]);
    source.add("00");
    live_update(&mut c);
    assert_eq!(c.reselect(true), None);
}

#[test]
fn a_new_query_or_sort_forgets_the_selection() {
    let source = ChangingSource::new(&["a", "b", "c"]);
    let mut c = shown(&source, "", 64);
    c.remember_selection(&[one(1)]);
    let req = c.set_query("b");
    let page = c.run(req).unwrap();
    assert!(c.apply(req, page));
    assert_eq!(c.reselect(true), None, "a new query starts unselected");

    c.remember_selection(&[one(0)]);
    let req = c.set_sort(Column::Size);
    let page = c.run(req).unwrap();
    assert!(c.apply(req, page));
    assert_eq!(c.reselect(true), None, "a new sort starts unselected");

    c.remember_selection(&[one(0)]);
    c.forget_selection();
    source.add("ab");
    live_update(&mut c);
    assert_eq!(c.reselect(true), None, "forgotten");
}

#[test]
fn rows_already_shown_are_remembered_without_a_search() {
    let names: Vec<String> = (0..1_000).map(|n| format!("r{n:04}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let source = ChangingSource::new(&refs);
    let mut c = shown(&source, "", 32);
    for i in 0..32 {
        c.row(i);
    }
    let searches = source.searches();
    c.remember_selection(&[one(3), 10..20]);
    assert_eq!(source.searches(), searches, "cached rows need no search");

    // Rows far below the cached windows cost one search per range.
    c.remember_selection(&[one(3), 600..610, 900..905]);
    assert!(source.searches() <= searches + 2);
    source.add("a");
    live_update(&mut c);
    let r = c.reselect(false).expect("re-mapped");
    let mut want = vec![4];
    want.extend(601..611);
    want.extend(901..906);
    assert_eq!(r.rows, want);
}

#[test]
fn rows_read_from_a_newer_generation_are_not_guessed() {
    let names: Vec<String> = (0..500).map(|n| format!("s{n:04}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let source = ChangingSource::new(&refs);
    let mut c = shown(&source, "", 16);

    // A file is added and published before the window has heard of it;
    // the user selects a row that is not cached, so its file can only be
    // read from the newer generation, where row 300 is another file.
    source.add("a");
    let g = source.bump();
    c.remember_selection(&[one(300)]);

    let req = c
        .on_generation(g, Instant::now())
        .or_else(|| c.tick(Instant::now()))
        .expect("a re-run is requested");
    let page = c.run(req).unwrap();
    assert!(c.apply(req, page));
    assert_eq!(
        c.reselect(true),
        Some(Reselection::default()),
        "cleared rather than guessed"
    );
}

// ---------------------------------------------------------------------------
// Copy, open, reveal and drag between a refresh and its page
// ---------------------------------------------------------------------------

#[test]
fn copying_the_selection_before_the_refresh_lands_gives_the_selected_files() {
    let source = ChangingSource::new(&["b.txt", "c.txt", "d.txt", "e.txt"]);
    let mut c = shown(&source, "", 64);
    c.remember_selection(&[one(1), one(3)]);

    // Another index of the files is published (every ID changes and files
    // appear above the selection) before the window shows its page.
    source.renumber();
    source.add("a.txt");
    source.add("c0.txt");
    source.bump();
    let now_at_1 = c
        .source()
        .search("", SortKey::Name, Direction::Ascending, 1, 1)
        .unwrap();
    let file_now_at_1 = c.source().row(Generation(2), now_at_1.ids[0]).unwrap();
    assert_eq!(file_now_at_1.name, "b.txt", "row 1 holds another file now");

    let out = c.paths_of_selection(&[one(1), one(3)], 2);
    assert_eq!(out.paths, ["/f/c.txt", "/f/e.txt"]);
    assert!(!out.truncated && !out.outdated);

    // Open acts on the first rows only: the first remembered files.
    let out = c.paths_of_selection(&[one(1)], 2);
    assert_eq!(out.paths, ["/f/c.txt"]);
}

#[test]
fn rows_are_never_resolved_against_a_newer_generation() {
    let names: Vec<String> = (0..500).map(|n| format!("s{n:04}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let source = ChangingSource::new(&refs);
    let mut c = shown(&source, "", 16);
    let out = c.paths_for_ranges(&[one(300)]);
    assert_eq!(out.paths, ["/f/s0300"]);

    // A file appears above everything and is published; the window still
    // shows generation 1.
    source.add("a");
    source.bump();

    // Row 300 is not cached; the source would answer with s0299 there now.
    let out = c.paths_for_ranges(&[one(300)]);
    assert!(out.outdated, "{out:?}");
    assert!(out.paths.is_empty(), "nothing from the newer generation");
    assert!(
        c.tick(Instant::now()).is_some(),
        "a re-run is requested so a retry works"
    );

    // Rows of the page shown that are cached still read as shown.
    let out = c.paths_for_ranges(&[one(3), 5..7]);
    assert_eq!(out.paths, ["/f/s0003", "/f/s0005", "/f/s0006"]);
    assert!(!out.outdated);

    // A selection the controller does not keep (or that is not the whole
    // selection) falls back to the same generation-checked rows.
    c.remember_selection(&[0..MAX_KEPT_SELECTION + 1]);
    let out = c.paths_of_selection(&[0..MAX_KEPT_SELECTION + 1], MAX_KEPT_SELECTION + 1);
    assert!(out.outdated && out.paths.is_empty(), "{out:?}");
    c.remember_selection(&[one(2)]);
    let out = c.paths_of_selection(&[one(2), one(400)], 2);
    assert!(out.outdated && out.paths.is_empty(), "{out:?}");
}

#[test]
fn rows_of_the_generation_shown_resolve_by_search_when_not_cached() {
    let names: Vec<String> = (0..500).map(|n| format!("s{n:04}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let source = ChangingSource::new(&refs);
    let mut c = shown(&source, "", 16);
    let searches = source.searches();
    let out = c.paths_for_ranges(&[2..4, 250..252]);
    assert_eq!(out.paths, ["/f/s0002", "/f/s0003", "/f/s0250", "/f/s0251"]);
    assert!(!out.outdated);
    assert_eq!(
        source.searches(),
        searches + 1,
        "cached rows need no search"
    );
}
