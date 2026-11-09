//! Where the results window gets its rows from.
//!
//! [`ResultsSource`] is the narrow interface the
//! [`ResultsController`](crate::controller::ResultsController) needs: one
//! sorted window of matching IDs per search, row details by ID from the
//! snapshot generation a page came from, the rows of known files by path
//! (to keep a selection on the same files), the current generation, a
//! status line and a channel of newly published generations.
//!
//! Implementations:
//!
//! - [`fx_engine::Engine`]: a thin adapter over `Engine::search_grouped`
//!   and `Engine::positions_grouped` (the priority group is "Your files
//!   first"), `snapshot`, `subscribe` and `health`. The engine does not
//!   expose its root path, so full paths are the index's paths (names
//!   joined from the root entry down); they are absolute only when the root
//!   is `/`.
//! - [`EngineSource`]: the engine plus its root path, giving absolute paths
//!   for any root, and pinning the snapshots its searches ran on so rows are
//!   read from the page's generation.
//! - `SyntheticSource` (`measure` builds only): millions of synthetic rows
//!   from `fx-bench`, for measuring the window without a real index.
//! - `Arc<T>` for any source `T`, so one source can be shared with worker
//!   threads.
//! - [`SwitchableSource`](crate::switchable::SwitchableSource): any of these
//!   behind a switch, so a limited launch can move to the full-access index
//!   without a relaunch.

#[cfg(feature = "measure")]
mod synthetic;

#[cfg(feature = "measure")]
pub use synthetic::SyntheticSource;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use fx_engine::{
    CheckpointError, Engine, EngineHealth, EngineSnapshot, Generation, QueryOptions, SearchError,
};
use fx_model::{EntryId, EntryKind};
use fx_query::Cancelled;
use fx_sort::{Direction, SortKey};

/// One window of sorted search results.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResultPage {
    /// The snapshot generation searched; `ids` are valid in it.
    pub generation: Generation,
    /// How many entries matched in total.
    pub total: usize,
    /// The matching IDs at the requested positions, in sort order.
    pub ids: Vec<u32>,
}

/// Where some files sit in one search's results, from
/// [`ResultsSource::positions`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RowPositions {
    /// The snapshot generation searched; the rows are positions in it.
    pub generation: Generation,
    /// For each path asked about, in the same order: its row (the offset
    /// at which [`ResultsSource::search`] returns it), or `None` when no
    /// entry has that path or it does not match.
    pub rows: Vec<Option<usize>>,
}

/// The details of one result row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    /// The entry's own name.
    pub name: String,
    /// The entry's full path, ending with `name`.
    pub full_path: String,
    /// Size in bytes; `None` for folders.
    pub size: Option<u64>,
    /// Last modification time in Unix seconds, if known.
    pub modified: Option<i64>,
    /// Whether the entry is a folder.
    pub is_folder: bool,
}

impl Row {
    /// The path of the folder holding the entry (the Path column): the full
    /// path without its last component. `"/"` for an entry directly under
    /// the root `/`, empty when there is no folder part.
    pub fn folder(&self) -> &str {
        match self.full_path.rfind('/') {
            Some(0) if self.full_path.len() > 1 => "/",
            Some(i) => &self.full_path[..i],
            None => "",
        }
    }
}

/// The source's state for the status bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SourceStatus {
    /// Whether a scan is still filling (or catching up) the index.
    pub indexing: bool,
    /// Number of entries in the index.
    pub entries: usize,
}

/// What the search window asks: the search field's text under its
/// options (the Aa, Path and .* chips), in `sort` order read in
/// `direction`, with "Your files first" or not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchQuery<'a> {
    /// The search field's text (an empty text matches everything).
    pub text: &'a str,
    /// The search field's options.
    pub options: QueryOptions,
    /// The sort key.
    pub sort: SortKey,
    /// The sort direction.
    pub direction: Direction,
    /// Whether the matches in your folders come first.
    pub your_files_first: bool,
}

impl SearchQuery<'_> {
    /// The query with the default options: every search chip off.
    fn plain(
        text: &str,
        sort: SortKey,
        direction: Direction,
        your_files_first: bool,
    ) -> SearchQuery<'_> {
        SearchQuery {
            text,
            options: QueryOptions::default(),
            sort,
            direction,
            your_files_first,
        }
    }
}

/// A searchable, sortable set of results. See the module documentation.
pub trait ResultsSource: Send + Sync {
    /// Searches the latest snapshot for `query` (an empty query matches
    /// everything) and returns the matches at positions
    /// `offset .. offset + limit` in `sort` order read in `dir`.
    ///
    /// Errors: [`Cancelled`] when [`cancel`](Self::cancel) was called while
    /// the search ran.
    fn search(
        &self,
        query: &str,
        sort: SortKey,
        dir: Direction,
        offset: usize,
        limit: usize,
    ) -> Result<ResultPage, Cancelled>;

    /// Like [`search`](Self::search), with "Your files first":
    /// when `your_files_first` is set, the matches in your folders
    /// ([`crate::your_files`]) come first in `sort` order read in `dir`, then
    /// the other matches in the same order; the total is unchanged.
    ///
    /// Sources that know no folders (the synthetic rows) answer with the
    /// plain sort, which is the default.
    fn search_grouped(
        &self,
        query: &str,
        sort: SortKey,
        dir: Direction,
        your_files_first: bool,
        offset: usize,
        limit: usize,
    ) -> Result<ResultPage, Cancelled> {
        let _ = your_files_first;
        self.search(query, sort, dir, offset, limit)
    }

    /// Cancels every search running now.
    fn cancel(&self);

    /// The details of entry `id` as of snapshot `generation` (the
    /// generation of the page `id` came from), or `None` if it is unknown.
    fn row(&self, generation: Generation, id: u32) -> Option<Row>;

    /// Where the files at `paths` (full paths, as [`Row::full_path`] gives
    /// them) sit in the results of `query` in `sort` order read in `dir`,
    /// in the latest snapshot. The results are walked once, however many
    /// paths are asked about. A source that wraps another forwards this
    /// call like [`search`](Self::search).
    ///
    /// Errors: [`Cancelled`] when [`cancel`](Self::cancel) was called while
    /// it ran.
    fn positions(
        &self,
        query: &str,
        sort: SortKey,
        dir: Direction,
        paths: &[String],
    ) -> Result<RowPositions, Cancelled>;

    /// Like [`positions`](Self::positions), in the results of
    /// [`search_grouped`](Self::search_grouped) with the same
    /// `your_files_first`, so a kept selection lands on the rows the grouped
    /// list shows. A source that wraps another forwards this call like
    /// [`search_grouped`](Self::search_grouped).
    ///
    /// Sources that know no folders (the synthetic rows) answer with the
    /// plain order, which is the default, as for `search_grouped`.
    fn positions_grouped(
        &self,
        query: &str,
        sort: SortKey,
        dir: Direction,
        your_files_first: bool,
        paths: &[String],
    ) -> Result<RowPositions, Cancelled> {
        let _ = your_files_first;
        self.positions(query, sort, dir, paths)
    }

    /// The latest published generation.
    fn generation(&self) -> Generation;

    /// Indexing state and entry count.
    fn status(&self) -> SourceStatus;

    /// A channel receiving every generation published from now on.
    fn subscribe(&self) -> Receiver<Generation>;

    /// Called when the app quits: saves what the stored index lacks, so
    /// the next launch loads it instead of replaying up to a day of
    /// changes (see `fx_engine::Engine::checkpoint_for_quit`). Sources
    /// without a saved index (the synthetic rows) do nothing.
    fn save_for_quit(&self) -> Result<(), String> {
        Ok(())
    }

    /// How long ago the index was last saved, for the daily save
    /// ([`crate::daily_save`]); `None` without a saved index.
    fn saved_ago(&self) -> Option<std::time::Duration> {
        None
    }

    /// Asks for a save of the index in the background and returns at once
    /// (the daily save). Sources without a saved index do nothing.
    fn request_save(&self) {}

    /// The search window went on screen (`true`) or off it. An engine
    /// collects changes in longer batches while nobody watches the results
    /// (`fx_engine::Engine::set_foreground`); other sources do nothing.
    fn set_window_visible(&self, _visible: bool) {}

    /// The search window's search: `query.text` under the search
    /// field's options, in the order of
    /// [`search_grouped`](Self::search_grouped), at positions
    /// `offset .. offset + limit`.
    ///
    /// Errors: [`SearchError::Cancelled`] as for [`search`](Self::search);
    /// [`SearchError::InvalidRegex`] when a regular expression in the query
    /// does not compile (the whole text with the `.*` option, or a `regex:`
    /// term). A query that otherwise can't run lists nothing.
    ///
    /// Sources without fx-query's search (the synthetic rows) can't apply
    /// the options and answer [`search_grouped`](Self::search_grouped),
    /// which is the default. A source that wraps another forwards this
    /// call, or the options are lost.
    fn search_query(
        &self,
        query: &SearchQuery<'_>,
        offset: usize,
        limit: usize,
    ) -> Result<ResultPage, SearchError> {
        Ok(self.search_grouped(
            query.text,
            query.sort,
            query.direction,
            query.your_files_first,
            offset,
            limit,
        )?)
    }

    /// Where the files at `paths` sit in the results of
    /// [`search_query`](Self::search_query) for `query`, as
    /// [`positions_grouped`](Self::positions_grouped) finds them; a query
    /// that can't run finds none of them. The default, for sources without
    /// options, is [`positions_grouped`](Self::positions_grouped); a source
    /// that wraps another forwards this call.
    fn positions_query(
        &self,
        query: &SearchQuery<'_>,
        paths: &[String],
    ) -> Result<RowPositions, Cancelled> {
        self.positions_grouped(
            query.text,
            query.sort,
            query.direction,
            query.your_files_first,
            paths,
        )
    }
}

impl<T: ResultsSource + ?Sized> ResultsSource for Arc<T> {
    fn search_query(
        &self,
        query: &SearchQuery<'_>,
        offset: usize,
        limit: usize,
    ) -> Result<ResultPage, SearchError> {
        (**self).search_query(query, offset, limit)
    }

    fn positions_query(
        &self,
        query: &SearchQuery<'_>,
        paths: &[String],
    ) -> Result<RowPositions, Cancelled> {
        (**self).positions_query(query, paths)
    }

    fn search(
        &self,
        query: &str,
        sort: SortKey,
        dir: Direction,
        offset: usize,
        limit: usize,
    ) -> Result<ResultPage, Cancelled> {
        (**self).search(query, sort, dir, offset, limit)
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
        (**self).search_grouped(query, sort, dir, your_files_first, offset, limit)
    }

    fn cancel(&self) {
        (**self).cancel();
    }

    fn row(&self, generation: Generation, id: u32) -> Option<Row> {
        (**self).row(generation, id)
    }

    fn positions(
        &self,
        query: &str,
        sort: SortKey,
        dir: Direction,
        paths: &[String],
    ) -> Result<RowPositions, Cancelled> {
        (**self).positions(query, sort, dir, paths)
    }

    fn positions_grouped(
        &self,
        query: &str,
        sort: SortKey,
        dir: Direction,
        your_files_first: bool,
        paths: &[String],
    ) -> Result<RowPositions, Cancelled> {
        (**self).positions_grouped(query, sort, dir, your_files_first, paths)
    }

    fn generation(&self) -> Generation {
        (**self).generation()
    }

    fn status(&self) -> SourceStatus {
        (**self).status()
    }

    fn subscribe(&self) -> Receiver<Generation> {
        (**self).subscribe()
    }

    fn save_for_quit(&self) -> Result<(), String> {
        (**self).save_for_quit()
    }

    fn saved_ago(&self) -> Option<std::time::Duration> {
        (**self).saved_ago()
    }

    fn request_save(&self) {
        (**self).request_save();
    }

    fn set_window_visible(&self, visible: bool) {
        (**self).set_window_visible(visible);
    }
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// Reads entry `id` from an engine snapshot. `root_parent` is the folder
/// holding the engine's root, prefixed to the index path; `None` when the
/// root is `/` or unknown.
fn engine_row(snap: &EngineSnapshot, id: u32, root_parent: Option<&str>) -> Option<Row> {
    let index = snap.index();
    let eid = EntryId(id);
    if id as usize >= index.id_bound() || !index.is_live(eid) {
        return None;
    }
    let is_folder = index.kind(eid) == EntryKind::Dir;
    Some(Row {
        name: index.name(eid).to_string(),
        full_path: join_full_path(&index.path(eid), root_parent),
        size: (!is_folder).then(|| index.size(eid)),
        modified: Some(index.modified(eid)),
        is_folder,
    })
}

/// The full path of an entry from its index path (names joined with `/`
/// from the root entry down) and the folder holding the root. A root named
/// `/` makes the index path start with `//`, which is folded to one slash.
fn join_full_path(index_path: &str, root_parent: Option<&str>) -> String {
    let index_path = index_path
        .strip_prefix('/')
        .filter(|rest| rest.starts_with('/'))
        .unwrap_or(index_path);
    match root_parent {
        Some(parent) if !index_path.starts_with('/') => {
            let parent = parent.trim_end_matches('/');
            format!("{parent}/{index_path}")
        }
        _ => index_path.to_string(),
    }
}

/// The index path of a full path: the inverse of [`join_full_path`] for
/// the same `root_parent`, or `None` for a path outside the root.
fn index_path_of(full_path: &str, root_parent: Option<&str>) -> Option<String> {
    match root_parent {
        Some(parent) => {
            let parent = parent.trim_end_matches('/');
            let rest = full_path.strip_prefix(parent)?.strip_prefix('/')?;
            (!rest.is_empty()).then(|| rest.to_string())
        }
        // Under a root named `/`, index paths start with `//`.
        None if full_path.len() > 1 && full_path.starts_with('/') => Some(format!("/{full_path}")),
        None => Some(full_path.to_string()),
    }
}

/// [`Engine::positions_grouped`] for `query`'s question and the full paths
/// `paths` under `root_parent` (see [`join_full_path`]); paths outside the
/// root are not looked up. A query that can't run (an invalid regular
/// expression) finds none of them.
fn engine_positions(
    engine: &Engine,
    query: &SearchQuery<'_>,
    paths: &[String],
    root_parent: Option<&str>,
) -> Result<RowPositions, Cancelled> {
    let index_paths: Vec<Option<String>> = paths
        .iter()
        .map(|p| index_path_of(p, root_parent))
        .collect();
    let asked: Vec<&str> = index_paths.iter().flatten().map(String::as_str).collect();
    let found = match engine.positions_grouped(
        query.text,
        query.options,
        query.sort,
        query.direction,
        query.your_files_first,
        &asked,
    ) {
        Ok(found) => found,
        Err(SearchError::Cancelled) => return Err(Cancelled),
        Err(_) => fx_engine::Positions {
            generation: Generation(engine.snapshot().generation()),
            rows: vec![None; asked.len()],
        },
    };
    let mut found_rows = found.rows.into_iter();
    let rows = index_paths
        .iter()
        .map(|p| p.as_ref().and_then(|_| found_rows.next().flatten()))
        .collect();
    Ok(RowPositions {
        generation: found.generation,
        rows,
    })
}

fn engine_status(engine: &Engine) -> SourceStatus {
    SourceStatus {
        indexing: engine.health() == EngineHealth::Recovering,
        entries: engine.snapshot().index().live_count(),
    }
}

/// An empty page of `engine`'s latest generation.
fn empty_page(engine: &Engine) -> ResultPage {
    ResultPage {
        generation: Generation(engine.snapshot().generation()),
        ..ResultPage::default()
    }
}

/// [`Engine::search_grouped`] for `query`'s question. An invalid regular
/// expression and a cancelled search are errors; a query that otherwise
/// can't run ([`SearchError::Query`]) gives an empty page.
fn engine_search(
    engine: &Engine,
    query: &SearchQuery<'_>,
    offset: usize,
    limit: usize,
) -> Result<ResultPage, SearchError> {
    match engine.search_grouped(
        query.text,
        query.options,
        query.sort,
        query.direction,
        query.your_files_first,
        offset,
        limit,
    ) {
        Ok(page) => Ok(ResultPage {
            generation: page.generation,
            total: page.total,
            ids: page.ids,
        }),
        Err(SearchError::Query { .. }) => Ok(empty_page(engine)),
        Err(e) => Err(e),
    }
}

/// The answer of a search without the window's options (the plain trait
/// methods): a regular expression that does not compile finds nothing, since
/// these methods have no way to report it.
fn plain_answer(
    engine: &Engine,
    answer: Result<ResultPage, SearchError>,
) -> Result<ResultPage, Cancelled> {
    match answer {
        Ok(page) => Ok(page),
        Err(SearchError::Cancelled) => Err(Cancelled),
        Err(_) => Ok(empty_page(engine)),
    }
}

/// The bare engine. Rows are read from the latest snapshot (the engine keeps
/// no older ones); entry IDs are stable across generations except across a
/// compaction, after which the controller's re-run fetches fresh IDs. Use
/// [`EngineSource`] for absolute paths under any root.
impl ResultsSource for Engine {
    fn search(
        &self,
        query: &str,
        sort: SortKey,
        dir: Direction,
        offset: usize,
        limit: usize,
    ) -> Result<ResultPage, Cancelled> {
        ResultsSource::search_grouped(self, query, sort, dir, false, offset, limit)
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
        let q = SearchQuery::plain(query, sort, dir, your_files_first);
        plain_answer(self, engine_search(self, &q, offset, limit))
    }

    fn search_query(
        &self,
        query: &SearchQuery<'_>,
        offset: usize,
        limit: usize,
    ) -> Result<ResultPage, SearchError> {
        engine_search(self, query, offset, limit)
    }

    fn cancel(&self) {
        self.cancel_searches();
    }

    fn row(&self, _generation: Generation, id: u32) -> Option<Row> {
        engine_row(&self.snapshot(), id, None)
    }

    fn positions(
        &self,
        query: &str,
        sort: SortKey,
        dir: Direction,
        paths: &[String],
    ) -> Result<RowPositions, Cancelled> {
        ResultsSource::positions_grouped(self, query, sort, dir, false, paths)
    }

    fn positions_grouped(
        &self,
        query: &str,
        sort: SortKey,
        dir: Direction,
        your_files_first: bool,
        paths: &[String],
    ) -> Result<RowPositions, Cancelled> {
        let q = SearchQuery::plain(query, sort, dir, your_files_first);
        engine_positions(self, &q, paths, None)
    }

    fn positions_query(
        &self,
        query: &SearchQuery<'_>,
        paths: &[String],
    ) -> Result<RowPositions, Cancelled> {
        engine_positions(self, query, paths, None)
    }

    fn generation(&self) -> Generation {
        Generation(self.snapshot().generation())
    }

    fn status(&self) -> SourceStatus {
        engine_status(self)
    }

    fn subscribe(&self) -> Receiver<Generation> {
        Engine::subscribe(self)
    }

    /// [`Engine::checkpoint_for_quit`]; an engine without a store has
    /// nothing to save.
    fn save_for_quit(&self) -> Result<(), String> {
        match self.checkpoint_for_quit() {
            Ok(_) | Err(CheckpointError::NoStorePath) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }

    /// The store's age, from its modification time.
    fn saved_ago(&self) -> Option<std::time::Duration> {
        self.saved_at()?.elapsed().ok()
    }

    /// [`Engine::request_checkpoint`]: the checkpoint thread saves.
    fn request_save(&self) {
        self.request_checkpoint();
    }

    /// [`Engine::set_foreground`]: longer batches while hidden.
    fn set_window_visible(&self, visible: bool) {
        self.set_foreground(visible);
    }
}

/// How many searched snapshots an [`EngineSource`] keeps for row reads.
const PINNED_SNAPSHOTS: usize = 4;

/// An [`Engine`] plus the root path it watches.
///
/// Full paths are absolute for any root. Each search pins the snapshot it
/// ran on (when it is still the latest right after the search), so
/// [`row`](ResultsSource::row) reads from the page's own generation; an
/// unknown generation falls back to the latest snapshot.
pub struct EngineSource {
    engine: Arc<Engine>,
    root_parent: Option<String>,
    pinned: Mutex<VecDeque<Arc<EngineSnapshot>>>,
}

impl EngineSource {
    /// Wraps `engine`, which watches the absolute path `root`.
    pub fn new(engine: Arc<Engine>, root: impl AsRef<Path>) -> EngineSource {
        let root_parent = root
            .as_ref()
            .parent()
            .map(Path::to_path_buf)
            .map(PathBuf::into_os_string)
            .map(|p| p.to_string_lossy().into_owned());
        EngineSource {
            engine,
            root_parent,
            pinned: Mutex::new(VecDeque::with_capacity(PINNED_SNAPSHOTS)),
        }
    }

    /// The wrapped engine.
    pub fn engine(&self) -> &Arc<Engine> {
        &self.engine
    }

    fn pinned(&self) -> std::sync::MutexGuard<'_, VecDeque<Arc<EngineSnapshot>>> {
        self.pinned
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn pin(&self, snap: Arc<EngineSnapshot>) {
        let mut pinned = self.pinned();
        if pinned.iter().any(|s| s.generation() == snap.generation()) {
            return;
        }
        if pinned.len() == PINNED_SNAPSHOTS {
            pinned.pop_front();
        }
        pinned.push_back(snap);
    }

    fn snapshot_for(&self, generation: Generation) -> Arc<EngineSnapshot> {
        let latest = self.engine.snapshot();
        if latest.generation() == generation.get() {
            return latest;
        }
        self.pinned()
            .iter()
            .find(|s| s.generation() == generation.get())
            .cloned()
            .unwrap_or(latest)
    }
}

impl ResultsSource for EngineSource {
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
        let q = SearchQuery::plain(query, sort, dir, your_files_first);
        plain_answer(&self.engine, self.search_query(&q, offset, limit))
    }

    /// The engine's search, pinning the snapshot it ran on.
    fn search_query(
        &self,
        query: &SearchQuery<'_>,
        offset: usize,
        limit: usize,
    ) -> Result<ResultPage, SearchError> {
        let page = engine_search(&self.engine, query, offset, limit)?;
        let snap = self.engine.snapshot();
        if snap.generation() == page.generation.get() {
            self.pin(snap);
        }
        Ok(page)
    }

    fn cancel(&self) {
        self.engine.cancel_searches();
    }

    fn row(&self, generation: Generation, id: u32) -> Option<Row> {
        engine_row(
            &self.snapshot_for(generation),
            id,
            self.root_parent.as_deref(),
        )
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
        let q = SearchQuery::plain(query, sort, dir, your_files_first);
        self.positions_query(&q, paths)
    }

    fn positions_query(
        &self,
        query: &SearchQuery<'_>,
        paths: &[String],
    ) -> Result<RowPositions, Cancelled> {
        engine_positions(&self.engine, query, paths, self.root_parent.as_deref())
    }

    fn generation(&self) -> Generation {
        Generation(self.engine.snapshot().generation())
    }

    fn status(&self) -> SourceStatus {
        engine_status(&self.engine)
    }

    fn subscribe(&self) -> Receiver<Generation> {
        self.engine.subscribe()
    }

    fn save_for_quit(&self) -> Result<(), String> {
        ResultsSource::save_for_quit(&*self.engine)
    }

    fn saved_ago(&self) -> Option<std::time::Duration> {
        ResultsSource::saved_ago(&*self.engine)
    }

    fn request_save(&self) {
        ResultsSource::request_save(&*self.engine);
    }

    fn set_window_visible(&self, visible: bool) {
        ResultsSource::set_window_visible(&*self.engine, visible);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_paths_under_the_volume_root() {
        assert_eq!(join_full_path("/", None), "/");
        assert_eq!(join_full_path("//Users/me/a.txt", None), "/Users/me/a.txt");
    }

    #[test]
    fn full_paths_under_a_folder_root() {
        assert_eq!(join_full_path("me", Some("/Users")), "/Users/me");
        assert_eq!(
            join_full_path("me/docs/a.txt", Some("/Users/")),
            "/Users/me/docs/a.txt"
        );
        assert_eq!(join_full_path("Users/a", Some("/")), "/Users/a");
    }

    #[test]
    fn index_paths_invert_full_paths() {
        for (index_path, parent) in [
            ("/", None),
            ("//Users/me/a.txt", None),
            ("vol/a", None),
            ("vol", None),
            ("me", Some("/Users")),
            ("me/docs/a.txt", Some("/Users/")),
            ("Users/a", Some("/")),
        ] {
            let full = join_full_path(index_path, parent);
            assert_eq!(
                index_path_of(&full, parent).as_deref(),
                Some(index_path),
                "{full} under {parent:?}"
            );
        }
        assert_eq!(index_path_of("/Elsewhere/a", Some("/Users")), None);
        assert_eq!(index_path_of("/Usersx/a", Some("/Users")), None);
        assert_eq!(index_path_of("/Users", Some("/Users")), None);
        assert_eq!(index_path_of("", Some("/Users")), None);
    }

    #[test]
    fn row_folder_is_the_path_without_the_name() {
        let row = |p: &str| Row {
            full_path: p.to_string(),
            ..Row::default()
        };
        assert_eq!(row("/Users/me/a.txt").folder(), "/Users/me");
        assert_eq!(row("/a.txt").folder(), "/");
        assert_eq!(row("/").folder(), "");
        assert_eq!(row("a.txt").folder(), "");
    }
}
