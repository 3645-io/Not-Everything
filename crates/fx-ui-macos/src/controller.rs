//! The platform-agnostic results controller.
//!
//! [`ResultsController`] is a synchronous state machine between the window
//! and a [`ResultsSource`]. The UI layer drives it:
//!
//! 1. Typing calls [`set_query`](ResultsController::set_query) and a header
//!    click calls [`set_sort`](ResultsController::set_sort); the **Your
//!    files first** setting calls
//!    [`set_your_files_first`](ResultsController::set_your_files_first)
//!    and a click on the search field's Aa, Path or .* chip calls
//!    [`set_options`](ResultsController::set_options). Each
//!    returns a new [`RequestId`] and cancels searches in flight. Every
//!    search the controller makes carries the options
//!    ([`ResultsSource::search_query`]).
//! 2. A new snapshot generation from the source goes to
//!    [`on_generation`](ResultsController::on_generation), which re-runs the
//!    current query at most once per throttle interval (default
//!    [`DEFAULT_THROTTLE`]); a generation arriving sooner is deferred, and
//!    [`tick`](ResultsController::tick) fires it once the interval has
//!    passed. Time is passed in, so tests control it.
//! 3. [`run`](ResultsController::run) executes a request (or
//!    [`search_request`](ResultsController::search_request) captures it for
//!    a worker thread) and [`apply`](ResultsController::apply) installs the
//!    page, dropping pages from superseded queries and from generations
//!    older than the one shown. A search whose regular expression does not
//!    compile goes to [`apply_error`](ResultsController::apply_error)
//!    instead, which shows no rows and
//!    [`invalid_regex`](ResultsController::invalid_regex) until the next
//!    page.
//! 4. The table reads [`row_count`](ResultsController::row_count) and
//!    [`row`](ResultsController::row); rows are fetched a window at a time
//!    (default [`DEFAULT_ROW_WINDOW`]) around the row asked for, from the
//!    generation of the page that listed them, and cached until the next
//!    page is applied. Every window shares the shown page's generation: a
//!    window that comes back from a newer generation is dropped (the row
//!    reads as `None`) and a re-run is requested through
//!    [`tick`](ResultsController::tick), so `row_count` and the rows never
//!    mix generations.
//! 5. [`paths_for_ranges`](ResultsController::paths_for_ranges) resolves
//!    rows with one search per contiguous range (split at [`COPY_CHUNK`]
//!    rows), up to [`MAX_COPY_PATHS`] paths, reading cached rows without a
//!    search and never resolving a row against a newer generation than the
//!    page shown. [`paths_of_selection`](ResultsController::paths_of_selection)
//!    (copy, open, reveal of the selection) returns the remembered files of
//!    item 6 when they are the whole selection, so it works even while a
//!    refresh is on its way.
//! 6. The selection stays on the same files when the results refresh. The
//!    window reports every selection change to
//!    [`remember_selection`](ResultsController::remember_selection), which
//!    keeps the selected files by path (up to [`MAX_KEPT_SELECTION`]; rows
//!    already shown cost no search). After a page of the same query and
//!    sort is applied, [`reselect`](ResultsController::reselect) finds the
//!    files' new rows with one [`ResultsSource::positions_grouped`] call (in
//!    the grouped order when **Your files first** is on) and says
//!    which rows to select and whether to scroll; the window only applies
//!    that. Files that were deleted or no longer match are dropped, a larger
//!    selection is cleared, and a new query or sort forgets the selection.
//!
//! An empty query lists everything.

#![forbid(unsafe_code)]

use std::ops::Range;
use std::time::{Duration, Instant};

use fx_engine::Generation;
use fx_engine::SearchError;
use fx_query::QueryOptions;
use fx_sort::{Direction, SortKey};

use crate::columns::{Column, SortState};
/// Counts with thousands separators, defined in [`crate::formats`].
pub use crate::formats::group_thousands;
use crate::source::{ResultPage, ResultsSource, Row, SearchQuery};

/// The default minimum interval between re-runs caused by new generations.
pub const DEFAULT_THROTTLE: Duration = Duration::from_millis(100);

/// The default number of rows fetched per window.
pub const DEFAULT_ROW_WINDOW: usize = 256;

/// How many row windows are cached at once.
const CACHED_WINDOWS: usize = 4;

/// Most paths one [`paths_for_ranges`](ResultsController::paths_for_ranges)
/// call returns, so a copy of a huge selection stays a bounded clipboard
/// string (about 10 MB at 100 bytes a path).
pub const MAX_COPY_PATHS: usize = 100_000;

/// Most rows one search of
/// [`paths_for_ranges`](ResultsController::paths_for_ranges) asks for; a
/// longer range is split into chunks of this size.
pub const COPY_CHUNK: usize = 32_768;

/// Most selected rows whose files
/// [`remember_selection`](ResultsController::remember_selection) keeps
/// across refreshes; a larger selection is cleared by the next refresh.
pub const MAX_KEPT_SELECTION: usize = 1_000;

/// The selection to show after a refresh, from
/// [`reselect`](ResultsController::reselect).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reselection {
    /// The rows to select, ascending; empty clears the selection.
    pub rows: Vec<usize>,
    /// The row to scroll into view: the new row of the file that was
    /// selected first, when it was in view before the refresh and is still
    /// there. `None` leaves the scroll as it is.
    pub scroll_to: Option<usize>,
}

/// What the controller knows about the files selected in the window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum KeptSelection {
    /// Nothing is selected.
    #[default]
    Nothing,
    /// A selection the next refresh clears: more than
    /// [`MAX_KEPT_SELECTION`] rows, or rows that could not be read from the
    /// page shown.
    Dropped,
    /// The selected files by full path, first selected (lowest row) first,
    /// and the generation whose rows they are selected at.
    Files {
        generation: Generation,
        paths: Vec<String>,
    },
}

/// The paths of a selection, from
/// [`paths_for_ranges`](ResultsController::paths_for_ranges) or
/// [`paths_of_selection`](ResultsController::paths_of_selection).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CopiedPaths {
    /// Full paths, in the order of the ranges given.
    pub paths: Vec<String>,
    /// The selection held more than [`MAX_COPY_PATHS`] rows (inside the
    /// rows shown), and only the first ones were resolved.
    pub truncated: bool,
    /// Some rows could only have been read from a newer generation than
    /// the page shown, where other files sit at those positions, so nothing
    /// was resolved (`paths` is empty). A re-run is requested; once its
    /// page is shown, asking again works.
    pub outdated: bool,
}

/// Identifies one search request; later requests have larger IDs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId(pub u64);

/// Everything needed to run one request away from the controller, for
/// example on a worker thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchRequest {
    /// The request this search answers.
    pub id: RequestId,
    /// The query text.
    pub query: String,
    /// The sort key.
    pub sort: SortKey,
    /// The sort direction.
    pub direction: Direction,
    /// Whether your files come first.
    pub your_files_first: bool,
    /// The search field's options: the Aa, Path and .* chips.
    pub options: QueryOptions,
    /// How many rows the first page holds.
    pub limit: usize,
}

impl SearchRequest {
    /// The question this request asks.
    pub fn query(&self) -> SearchQuery<'_> {
        SearchQuery {
            text: &self.query,
            options: self.options,
            sort: self.sort,
            direction: self.direction,
            your_files_first: self.your_files_first,
        }
    }

    /// Runs the search for the first page on `source`
    /// ([`ResultsSource::search_query`]).
    ///
    /// Errors: as for [`ResultsSource::search_query`]: the search was
    /// cancelled, or a regular expression in it does not compile.
    pub fn execute<S: ResultsSource + ?Sized>(
        &self,
        source: &S,
    ) -> Result<ResultPage, SearchError> {
        source.search_query(&self.query(), 0, self.limit)
    }
}

/// A cached window of rows: positions `offset .. offset + ids.len()`.
struct Window {
    generation: Generation,
    offset: usize,
    ids: Vec<u32>,
    rows: Vec<Option<Row>>,
}

impl Window {
    fn new(generation: Generation, offset: usize, ids: Vec<u32>) -> Window {
        let rows = vec![None; ids.len()];
        Window {
            generation,
            offset,
            ids,
            rows,
        }
    }

    fn contains(&self, i: usize) -> bool {
        i >= self.offset && i < self.offset + self.ids.len()
    }
}

/// The results state machine. See the module documentation.
pub struct ResultsController<S: ResultsSource> {
    source: S,
    query: String,
    sort: SortState,
    /// Whether your files come first under every sort.
    your_files_first: bool,
    throttle: Duration,
    row_window: usize,
    /// The newest request handed out.
    request: RequestId,
    /// The first request with the current query and sort; requests from
    /// here on all ask the same question.
    basis: RequestId,
    /// The request whose page is shown.
    applied_request: RequestId,
    /// The generation of the page shown.
    applied_generation: Option<Generation>,
    /// The newest generation known.
    seen: Generation,
    total: usize,
    last_rerun: Option<Instant>,
    pending: bool,
    windows: Vec<Window>,
    /// The files selected in the window.
    kept: KeptSelection,
    /// The search field's options (the chips).
    options: QueryOptions,
    /// The request shown failed: its regular expression does not compile.
    invalid_regex: bool,
}

impl<S: ResultsSource> ResultsController<S> {
    /// A controller over `source` with an empty query, Name ascending, the
    /// plain sort (your files not first), the default throttle and row
    /// window, and no page yet.
    pub fn new(source: S) -> ResultsController<S> {
        ResultsController {
            source,
            query: String::new(),
            sort: SortState::default(),
            your_files_first: false,
            throttle: DEFAULT_THROTTLE,
            row_window: DEFAULT_ROW_WINDOW,
            request: RequestId(0),
            basis: RequestId(0),
            applied_request: RequestId(0),
            applied_generation: None,
            seen: Generation(0),
            total: 0,
            last_rerun: None,
            pending: false,
            windows: Vec::new(),
            kept: KeptSelection::Nothing,
            options: QueryOptions::default(),
            invalid_regex: false,
        }
    }

    /// Sets the minimum interval between generation re-runs.
    pub fn with_throttle(mut self, throttle: Duration) -> ResultsController<S> {
        self.throttle = throttle;
        self
    }

    /// Starts with your files first (the app passes its setting).
    pub fn with_your_files_first(mut self, on: bool) -> ResultsController<S> {
        self.your_files_first = on;
        self
    }

    /// Sets how many rows are fetched per window (at least one).
    pub fn with_row_window(mut self, rows: usize) -> ResultsController<S> {
        self.row_window = rows.max(1);
        self
    }

    /// Starts from `sort` instead of Name ascending (the app passes the
    /// sort saved in the settings file).
    pub fn with_sort(mut self, sort: SortState) -> ResultsController<S> {
        self.sort = sort;
        self
    }

    /// Starts with the search field's `options` (the app passes the chips
    /// saved in the settings file).
    pub fn with_options(mut self, options: QueryOptions) -> ResultsController<S> {
        self.options = options;
        self
    }

    /// The source.
    pub fn source(&self) -> &S {
        &self.source
    }

    /// The current query text.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The current sort column and direction.
    pub fn sort(&self) -> SortState {
        self.sort
    }

    /// Whether your files come first.
    pub fn your_files_first(&self) -> bool {
        self.your_files_first
    }

    /// The search field's options (the Aa, Path and .* chips).
    pub fn options(&self) -> QueryOptions {
        self.options
    }

    /// Whether the search shown failed because a regular expression in it
    /// does not compile ([`apply_error`](Self::apply_error)); no rows are
    /// shown then.
    pub fn invalid_regex(&self) -> bool {
        self.invalid_regex
    }

    /// The newest request handed out.
    pub fn current_request(&self) -> RequestId {
        self.request
    }

    /// Sets the query text, cancels searches in flight and returns the
    /// request to run.
    pub fn set_query(&mut self, text: &str) -> RequestId {
        self.query.clear();
        self.query.push_str(text);
        self.new_question()
    }

    /// A click on `column`'s header (see [`SortState::click`]): cancels
    /// searches in flight and returns the request to run.
    pub fn set_sort(&mut self, column: Column) -> RequestId {
        self.sort.click(column);
        self.new_question()
    }

    /// A chip changed the search field's options to `options`: cancels
    /// searches in flight and returns the request that runs the current
    /// query under them. `None`, with nothing cancelled, when they are
    /// unchanged.
    pub fn set_options(&mut self, options: QueryOptions) -> Option<RequestId> {
        if options == self.options {
            return None;
        }
        self.options = options;
        Some(self.new_question())
    }

    /// The **Your files first** setting changed to `on`: cancels searches
    /// in flight and returns the request that re-runs the current query
    /// and sort. `None`, with nothing cancelled, when it is unchanged.
    pub fn set_your_files_first(&mut self, on: bool) -> Option<RequestId> {
        if on == self.your_files_first {
            return None;
        }
        self.your_files_first = on;
        Some(self.new_question())
    }

    fn new_question(&mut self) -> RequestId {
        self.source.cancel();
        self.pending = false;
        self.kept = KeptSelection::Nothing;
        self.request = RequestId(self.request.0 + 1);
        self.basis = self.request;
        self.request
    }

    /// A newly published generation `g`. Returns the re-run request when
    /// `g` is newer than any seen and the throttle interval has passed since
    /// the last re-run; a newer `g` inside the interval is deferred to
    /// [`tick`](Self::tick).
    pub fn on_generation(&mut self, g: Generation, now: Instant) -> Option<RequestId> {
        if g <= self.seen {
            return None;
        }
        self.seen = g;
        if self.throttle_passed(now) {
            Some(self.rerun(now))
        } else {
            self.pending = true;
            None
        }
    }

    /// Fires a deferred re-run once the throttle interval has passed.
    pub fn tick(&mut self, now: Instant) -> Option<RequestId> {
        if self.pending && self.throttle_passed(now) {
            Some(self.rerun(now))
        } else {
            None
        }
    }

    /// Whether a deferred re-run is waiting for [`tick`](Self::tick).
    pub fn has_pending_rerun(&self) -> bool {
        self.pending
    }

    fn throttle_passed(&self, now: Instant) -> bool {
        self.last_rerun
            .is_none_or(|t| now.saturating_duration_since(t) >= self.throttle)
    }

    /// A re-run of the same query and sort. Searches in flight are not
    /// cancelled: they answer the same question, and their pages are still
    /// accepted if newer than the one shown.
    fn rerun(&mut self, now: Instant) -> RequestId {
        self.last_rerun = Some(now);
        self.pending = false;
        self.request = RequestId(self.request.0 + 1);
        self.request
    }

    /// The search for `request` with the current query, options and sort.
    pub fn search_request(&self, request: RequestId) -> SearchRequest {
        SearchRequest {
            id: request,
            query: self.query.clone(),
            sort: self.sort.sort_key(),
            direction: self.sort.direction,
            your_files_first: self.your_files_first,
            options: self.options,
            limit: self.row_window,
        }
    }

    /// The question the controller asks now.
    fn question(&self) -> SearchQuery<'_> {
        SearchQuery {
            text: &self.query,
            options: self.options,
            sort: self.sort.sort_key(),
            direction: self.sort.direction,
            your_files_first: self.your_files_first,
        }
    }

    /// Runs `request` on the source and returns its first page. A request
    /// superseded by a newer query, options or sort is not run:
    /// [`SearchError::Cancelled`]. Other errors as for
    /// [`SearchRequest::execute`].
    pub fn run(&self, request: RequestId) -> Result<ResultPage, SearchError> {
        if request < self.basis {
            return Err(SearchError::Cancelled);
        }
        self.search_request(request).execute(&self.source)
    }

    /// Installs `page` as the answer to `request`. Returns `false`, changing
    /// nothing, when the request is from an older query or sort, older than
    /// the request shown, or the page's generation is older than the one
    /// shown.
    pub fn apply(&mut self, request: RequestId, page: ResultPage) -> bool {
        if request < self.basis || request < self.applied_request || request > self.request {
            return false;
        }
        if self
            .applied_generation
            .is_some_and(|shown| page.generation < shown)
        {
            return false;
        }
        self.applied_request = request;
        self.applied_generation = Some(page.generation);
        self.invalid_regex = false;
        self.seen = self.seen.max(page.generation);
        if page.generation >= self.seen {
            // The page shown is from the newest generation known: a deferred
            // re-run would ask the same question of the same snapshot.
            self.pending = false;
        }
        self.total = page.total;
        self.windows.clear();
        self.windows.push(Window::new(page.generation, 0, page.ids));
        true
    }

    /// Shows that `request` failed with `error` instead of a page: an
    /// invalid regular expression replaces the rows with no rows and sets
    /// [`invalid_regex`](Self::invalid_regex) until the next page is
    /// applied. Returns `false`, changing nothing, for a cancelled search,
    /// another error, or a request [`apply`](Self::apply) would drop.
    pub fn apply_error(&mut self, request: RequestId, error: &SearchError) -> bool {
        if !matches!(error, SearchError::InvalidRegex { .. })
            || request < self.basis
            || request < self.applied_request
            || request > self.request
        {
            return false;
        }
        self.applied_request = request;
        self.applied_generation = None;
        self.invalid_regex = true;
        self.pending = false;
        self.total = 0;
        self.windows.clear();
        true
    }

    /// Number of rows in the page shown.
    pub fn row_count(&self) -> usize {
        self.total
    }

    /// Row `i` of the results, or `None` past the end (or if it can't be
    /// read). Fetches the window of rows around `i` when it is not cached.
    pub fn row(&mut self, i: usize) -> Option<&Row> {
        if i >= self.total {
            return None;
        }
        let w = match self.windows.iter().position(|w| w.contains(i)) {
            Some(w) => w,
            None => self.fetch_window(i)?,
        };
        let window = &mut self.windows[w];
        let k = i - window.offset;
        if window.rows[k].is_none() {
            window.rows[k] = self.source.row(window.generation, window.ids[k]);
        }
        window.rows[k].as_ref()
    }

    /// Searches the current query and sort for `limit` rows from `offset`.
    fn search_at(&self, offset: usize, limit: usize) -> Result<ResultPage, SearchError> {
        self.source.search_query(&self.question(), offset, limit)
    }

    /// Notes that the source answered from generation `g`. A generation
    /// newer than the page shown means positions no longer match
    /// `row_count`: a re-run is requested, fired by [`tick`](Self::tick).
    fn note_answer_generation(&mut self, g: Generation) {
        if self.applied_generation.is_some_and(|shown| g > shown) {
            self.seen = self.seen.max(g);
            self.pending = true;
        }
    }

    /// Fetches the window around `i` and returns its index. A window from a
    /// generation other than the page shown is dropped (`None`), and a newer
    /// one requests a re-run. While a newer generation than the page shown
    /// is already known, no search is made: any window would be dropped, so
    /// the row reads as `None` until the re-run's page is applied.
    fn fetch_window(&mut self, i: usize) -> Option<usize> {
        if self
            .applied_generation
            .is_some_and(|shown| self.seen > shown)
        {
            // Make sure a re-run will come even if the one handed out (if
            // any) never lands; `apply` clears this once it is up to date.
            self.pending = true;
            return None;
        }
        let size = self.row_window;
        let offset = i
            .saturating_sub(size / 2)
            .min(self.total.saturating_sub(size));
        let page = self.search_at(offset, size).ok()?;
        if Some(page.generation) != self.applied_generation {
            self.note_answer_generation(page.generation);
            return None;
        }
        let window = Window::new(page.generation, offset, page.ids);
        if !window.contains(i) {
            return None;
        }
        if self.windows.len() >= CACHED_WINDOWS {
            // Keep the first page's window; drop the oldest other one.
            self.windows.remove(1);
        }
        self.windows.push(window);
        Some(self.windows.len() - 1)
    }

    /// Full paths of `rows` in the order given, skipping rows out of range.
    pub fn selection_paths(&mut self, rows: &[usize]) -> Vec<String> {
        rows.iter()
            .filter_map(|&i| self.row(i).map(|r| r.full_path.clone()))
            .collect()
    }

    /// Full paths of the rows in `ranges`, in the order given, clipped to
    /// the rows shown and capped at [`MAX_COPY_PATHS`].
    ///
    /// Rows are only ever read from the generation of the page shown. A
    /// stretch inside one cached window is read from it without a search;
    /// any other contiguous range costs one search (one per [`COPY_CHUNK`]
    /// rows for longer ranges), not one per row window, and the cached
    /// windows are left alone. If a search answers from a newer generation
    /// (a refresh is on its way, and other files may sit at those
    /// positions), nothing is returned: [`CopiedPaths::outdated`] is set and
    /// a re-run is requested as for row windows.
    pub fn paths_for_ranges(&mut self, ranges: &[Range<usize>]) -> CopiedPaths {
        let mut out = CopiedPaths::default();
        for range in ranges {
            let end = range.end.min(self.total);
            let mut start = range.start;
            while start < end {
                let remaining = MAX_COPY_PATHS - out.paths.len();
                if remaining == 0 {
                    out.truncated = true;
                    return out;
                }
                let len = (end - start).min(COPY_CHUNK).min(remaining);
                if self.is_cached(start, len) {
                    for i in start..start + len {
                        if let Some(row) = self.row(i) {
                            out.paths.push(row.full_path.clone());
                        }
                    }
                    start += len;
                    continue;
                }
                let Ok(page) = self.search_at(start, len) else {
                    return out;
                };
                if Some(page.generation) != self.applied_generation {
                    self.note_answer_generation(page.generation);
                    return CopiedPaths {
                        outdated: true,
                        ..CopiedPaths::default()
                    };
                }
                out.paths.extend(
                    page.ids
                        .iter()
                        .filter_map(|&id| self.source.row(page.generation, id))
                        .map(|row| row.full_path),
                );
                if page.ids.len() < len {
                    // The snapshot answered has fewer rows than shown.
                    break;
                }
                start += len;
            }
        }
        out
    }

    /// Whether rows `start .. start + len` all sit in one cached window.
    fn is_cached(&self, start: usize, len: usize) -> bool {
        self.windows
            .iter()
            .any(|w| w.offset <= start && start + len <= w.offset + w.ids.len())
    }

    /// Full paths of the selection, for copy, open, reveal and drag:
    /// `ranges` are the first selected rows of the page shown (as many as
    /// the action needs, in row order) and `selected` how many rows are
    /// selected in all.
    ///
    /// When the files remembered by
    /// [`remember_selection`](Self::remember_selection) are the whole
    /// selection, the first of them are returned without a search, so the
    /// right files come back even while a refresh is on its way (or after
    /// the source swapped its index). Otherwise the rows are resolved by
    /// [`paths_for_ranges`](Self::paths_for_ranges), which never reads them
    /// from another generation than the page shown.
    pub fn paths_of_selection(&mut self, ranges: &[Range<usize>], selected: usize) -> CopiedPaths {
        if let KeptSelection::Files { paths, .. } = &self.kept {
            let wanted: usize = ranges
                .iter()
                .map(|r| r.end.min(self.total).saturating_sub(r.start))
                .sum();
            if paths.len() == selected && wanted <= paths.len() {
                return CopiedPaths {
                    paths: paths[..wanted].to_vec(),
                    ..CopiedPaths::default()
                };
            }
        }
        self.paths_for_ranges(ranges)
    }

    /// Remembers the files on the selected `ranges` (rows of the page
    /// shown) by full path, so [`reselect`](Self::reselect) can keep them
    /// selected after the next refresh. The window calls it on every
    /// selection change.
    ///
    /// Rows in the cached windows are read without a search; any other
    /// range costs one search, whose rows are used only if it answers from
    /// the generation of the page shown. Otherwise (a newer generation was
    /// published but is not shown yet) the selection is cleared at the next
    /// refresh rather than guessed, and a re-run is requested. More than
    /// [`MAX_KEPT_SELECTION`] rows are not read: the next refresh clears
    /// them.
    pub fn remember_selection(&mut self, ranges: &[Range<usize>]) {
        let total = self.total;
        let mut ranges: Vec<Range<usize>> = ranges
            .iter()
            .map(|r| r.start..r.end.min(total))
            .filter(|r| !r.is_empty())
            .collect();
        ranges.sort_by_key(|r| r.start);
        let count: usize = ranges.iter().map(|r| r.len()).sum();
        self.kept = if count == 0 {
            KeptSelection::Nothing
        } else if count > MAX_KEPT_SELECTION {
            KeptSelection::Dropped
        } else {
            match (self.applied_generation, self.read_paths(&ranges)) {
                (Some(generation), Some(paths)) => KeptSelection::Files { generation, paths },
                _ => KeptSelection::Dropped,
            }
        };
    }

    /// Forgets the selected files, as when the selection is cleared.
    pub fn forget_selection(&mut self) {
        self.kept = KeptSelection::Nothing;
    }

    /// Full paths of the rows in `ranges` (inside the rows shown), read
    /// from the generation of the page shown; `None` if any of them cannot
    /// be read from it.
    fn read_paths(&mut self, ranges: &[Range<usize>]) -> Option<Vec<String>> {
        let mut paths = Vec::new();
        for range in ranges {
            let cached = range
                .clone()
                .all(|i| self.windows.iter().any(|w| w.contains(i)));
            if cached {
                for i in range.clone() {
                    paths.push(self.row(i)?.full_path.clone());
                }
                continue;
            }
            let page = self.search_at(range.start, range.len()).ok()?;
            if Some(page.generation) != self.applied_generation {
                self.note_answer_generation(page.generation);
                return None;
            }
            if page.ids.len() != range.len() {
                return None;
            }
            for id in page.ids {
                paths.push(self.source.row(page.generation, id)?.full_path);
            }
        }
        Some(paths)
    }

    /// The selection to show after a page of the same query and sort was
    /// applied, so the remembered files stay selected; `None` leaves the
    /// selection as it is (nothing is selected, or the page shown is the
    /// generation the selected rows already belong to).
    ///
    /// One [`ResultsSource::positions_grouped`] call finds the files' new
    /// rows, in the order the page lists them (grouped when your files come
    /// first).
    /// Files that were deleted or no longer match are dropped, and the
    /// others are remembered at their new rows. A selection that was not
    /// kept (see [`remember_selection`](Self::remember_selection)) comes
    /// back empty, which clears it. `first_in_view` says whether the first
    /// selected row was visible before the page was applied: only then is
    /// the first selected file's new row returned in
    /// [`Reselection::scroll_to`].
    ///
    /// If the lookup answers from a generation newer than the page shown,
    /// its rows are still used and a re-run is requested (fired by
    /// [`tick`](Self::tick)); once that generation's page is applied, the
    /// rows already match it and no second lookup is made.
    pub fn reselect(&mut self, first_in_view: bool) -> Option<Reselection> {
        let (generation, paths) = match std::mem::take(&mut self.kept) {
            KeptSelection::Nothing => return None,
            KeptSelection::Dropped => return Some(Reselection::default()),
            KeptSelection::Files { generation, paths } => (generation, paths),
        };
        if Some(generation) == self.applied_generation {
            self.kept = KeptSelection::Files { generation, paths };
            return None;
        }
        // In the order the page shown lists (grouped when "Your files
        // first" is on), so the rows match the rows on screen.
        let Ok(found) = self.source.positions_query(&self.question(), &paths) else {
            return Some(Reselection::default());
        };
        self.note_answer_generation(found.generation);
        let total = self.total;
        let shown = |row: &Option<usize>| row.filter(|&r| r < total);
        let scroll_to = if first_in_view {
            found.rows.first().and_then(shown)
        } else {
            None
        };
        let mut kept: Vec<(usize, String)> = paths
            .into_iter()
            .zip(&found.rows)
            .filter_map(|(path, row)| shown(row).map(|r| (r, path)))
            .collect();
        kept.sort_unstable_by_key(|(row, _)| *row);
        kept.dedup_by_key(|(row, _)| *row);
        let rows = kept.iter().map(|(row, _)| *row).collect();
        if !kept.is_empty() {
            self.kept = KeptSelection::Files {
                generation: found.generation,
                paths: kept.into_iter().map(|(_, path)| path).collect(),
            };
        }
        Some(Reselection { rows, scroll_to })
    }

    /// The status bar text: `"1,234 objects"`, or `"Indexing… 1,234
    /// objects"` while the source is indexing.
    pub fn status_text(&self) -> String {
        let count = group_thousands(self.total);
        let noun = if self.total == 1 { "object" } else { "objects" };
        if self.source.status().indexing {
            format!("Indexing\u{2026} {count} {noun}")
        } else {
            format!("{count} {noun}")
        }
    }
}
