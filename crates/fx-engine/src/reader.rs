//! A read-only view of a saved checkpoint, for the command line.
//!
//! [`StoreReader::open`] maps an fx-store checkpoint (as written by
//! [`Engine::checkpoint`](crate::Engine::checkpoint)), verifies it and
//! rebuilds the index, like [`Engine::open`](crate::Engine::open) does, but
//! it never writes, renames, creates or removes anything (not even stale
//! temp files), starts no change source and spawns no threads. It works on
//! a read-only file in a read-only folder.
//!
//! [`StoreReader::search`] runs the same query and sort path as
//! [`Engine::search`](crate::Engine::search) over the loaded snapshot, with
//! the search toggles of [`ReadOptions`]. Unlike the engine, which checks
//! every saved sort order against the index when it opens a store, a search
//! here reads only the saved order it sorts by, and checks it only on the
//! entries it matched ([`matches_in_order`]): a command runs one search, so
//! checking all four orders over every entry would cost it most of its
//! time.

use std::cmp::Ordering;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fx_model::{EntryId, EntryKind};
use fx_query::{
    Cancelled, CompiledQuery, Diagnostic, Node, Pattern, RegexError, RegexTerm, SearchOptions,
    SearchView,
};
use fx_sort::{Direction, MatchSet, Order, SortKey, SortSource};
use fx_store::{LoadError, MappedIndex};

use crate::query;
use crate::snapshot::EngineSnapshot;

/// Why [`StoreReader::open`] failed.
#[derive(Debug)]
pub enum ReadError {
    /// There is no store file at the path.
    NotFound {
        /// The path that was looked at.
        path: PathBuf,
    },
    /// The file exists but could not be used: fx-store could not read it
    /// (an I/O error such as permission denied) or rejected it (corrupt,
    /// truncated, wrong magic or format version, inconsistent index).
    Rejected {
        /// The store path.
        path: PathBuf,
        /// Why fx-store rejected it. An index that fails to rebuild from
        /// valid columns is reported as [`LoadError::Malformed`].
        source: LoadError,
    },
}

impl ReadError {
    /// The store path the error is about.
    pub fn path(&self) -> &Path {
        match self {
            ReadError::NotFound { path } | ReadError::Rejected { path, .. } => path,
        }
    }
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::NotFound { path } => write!(f, "no index store at {}", path.display()),
            ReadError::Rejected { path, source } => {
                write!(f, "cannot use the index store {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for ReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ReadError::NotFound { .. } => None,
            ReadError::Rejected { source, .. } => Some(source),
        }
    }
}

/// Why a search failed: [`StoreReader::search`], or the engine's
/// [`search`](crate::Engine::search), [`positions`](crate::Engine::positions)
/// and their grouped forms.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SearchError {
    /// The query could not be compiled into a matcher.
    ///
    /// fx-query's parser never fails: it recovers from problems such as an
    /// unclosed quote and reports them as [`ReadPage::diagnostics`]. An
    /// invalid regular expression is [`SearchError::InvalidRegex`]. This
    /// variant covers the queries that still cannot be run.
    Query {
        /// What is wrong with the query.
        message: String,
    },
    /// A regular expression in the query does not compile: the whole query
    /// with the `regex` toggle, or a `regex:` term. Reported for the first
    /// such expression in query order (the app's matcher would make it
    /// match nothing).
    InvalidRegex {
        /// The pattern as written: the whole query with the `regex` toggle
        /// or [`QueryOptions::regex`](fx_query::QueryOptions::regex), else
        /// the `regex:` term's text.
        pattern: String,
        /// The regex engine's description of the problem.
        message: String,
    },
    /// [`Engine::cancel_searches`](crate::Engine::cancel_searches) was
    /// called while the search ran. [`StoreReader::search`] is never
    /// cancelled.
    Cancelled,
}

impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SearchError::Query { message } => write!(f, "invalid query: {message}"),
            SearchError::InvalidRegex { pattern, message } => {
                write!(f, "invalid regular expression {pattern:?}: {message}")
            }
            SearchError::Cancelled => write!(f, "the search was cancelled"),
        }
    }
}

impl From<Cancelled> for SearchError {
    fn from(_: Cancelled) -> SearchError {
        SearchError::Cancelled
    }
}

impl From<RegexError> for SearchError {
    fn from(e: RegexError) -> SearchError {
        SearchError::InvalidRegex {
            pattern: e.pattern,
            message: e.message,
        }
    }
}

/// The first regular expression in `node` (in query order) that does not
/// compile, compiled as the matcher compiles it: with the term's resolved
/// options.
pub(crate) fn first_regex_error(node: &Node) -> Option<RegexError> {
    match node {
        Node::And(children) | Node::Or(children) => children.iter().find_map(first_regex_error),
        Node::Not(child) => first_regex_error(child),
        Node::Term(term) => match &term.pattern {
            Pattern::Regex(pattern) => RegexTerm::compile(pattern, &term.options).err(),
            _ => None,
        },
        _ => None,
    }
}

impl std::error::Error for SearchError {}

/// Whether the entries of `matches` appear in `ids` once each and in
/// strictly ascending `key` order. Only matched entries are compared, so a
/// saved order costs a search time in proportion to its matches, not to the
/// index.
///
/// That is enough to sort by the order: [`fx_sort::emit`] reads only the
/// matched entries of an order, so it gives the same page from any order
/// that passes as from one sorted throughout. `key` is a total order
/// (entry IDs break ties), so ascending matched entries are exactly the
/// matches sorted. Counting them keeps an order that lacks a match from
/// dropping it; strictly ascending entries are never repeated.
pub(crate) fn matches_in_order<S: SortSource + ?Sized>(
    ids: &[u32],
    key: SortKey,
    src: &S,
    matches: &MatchSet,
) -> bool {
    let mut seen = 0usize;
    let mut prev: Option<EntryId> = None;
    for &id in ids {
        let id = EntryId(id);
        if !matches.contains(id) {
            continue;
        }
        if let Some(p) = prev {
            if key.compare(src, p, id) != Ordering::Less {
                return false;
            }
        }
        prev = Some(id);
        seen += 1;
    }
    seen == matches.len()
}

/// The global search toggles for [`StoreReader::search`]. Every flag
/// defaults to off, like [`fx_query::SearchOptions::default`]; modifiers
/// inside the query (`case:`, `ww:`, ...) still override them per term.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ReadOptions {
    /// Match upper and lower case exactly.
    pub match_case: bool,
    /// Terms match whole words only.
    pub whole_word: bool,
    /// Terms are tested against the full path instead of the name only.
    pub match_path: bool,
    /// Accented letters must match exactly (`é` does not match `e`).
    pub match_diacritics: bool,
    /// Terms are regular expressions.
    pub regex: bool,
}

impl From<ReadOptions> for SearchOptions {
    fn from(o: ReadOptions) -> SearchOptions {
        SearchOptions {
            match_case: o.match_case,
            whole_word: o.whole_word,
            match_path: o.match_path,
            match_diacritics: o.match_diacritics,
            regex: o.regex,
        }
    }
}

/// One window of results from [`StoreReader::search`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadPage {
    /// How many entries matched in total.
    pub total: usize,
    /// The matching entry IDs at positions `offset .. offset + limit`, in
    /// the requested sort order and direction. Look rows up with the
    /// reader's accessors ([`StoreReader::full_path`], ...).
    pub ids: Vec<u32>,
    /// Problems the query parser recovered from (for example an unclosed
    /// quote), in the order found. The results are those of the recovered
    /// query, as in the app.
    pub diagnostics: Vec<Diagnostic>,
    /// The `count:N` limit the query set, if any. Not applied to `total` or
    /// `ids`, matching [`Engine::search`](crate::Engine::search).
    pub count: Option<usize>,
}

/// A read-only index loaded from a checkpoint store.
///
/// Opening maps the store read-only, verifies it and rebuilds the index,
/// whose columns are served from the map; nothing on disk is ever written,
/// renamed, created or removed (not even stale temp files), no change
/// source is started and no thread is spawned. Searches run the same query
/// and sort path as [`Engine::search`](crate::Engine::search), with the
/// toggles of [`ReadOptions`], reading the saved sort orders from the map
/// as they need them.
#[derive(Debug)]
pub struct StoreReader {
    /// The index, with no orders: [`StoreReader::search`] takes them from
    /// `mapped` or builds them.
    snap: EngineSnapshot,
    /// The store's map, for its saved sort orders. Its checksums and its
    /// orders' permutation checks passed when it was opened.
    mapped: MappedIndex,
    utc_offset: Option<i32>,
}

/// The fx-store key code a sort order is saved under; the same mapping the
/// engine's checkpoints use.
fn order_code(key: SortKey) -> Option<u8> {
    match key {
        SortKey::Name => Some(1),
        SortKey::Path => Some(2),
        SortKey::Size => Some(3),
        SortKey::Modified => Some(4),
        _ => None,
    }
}

impl StoreReader {
    /// Opens and fully verifies the checkpoint store at `path` (header,
    /// tables, every column checksum, and that each saved sort order is a
    /// permutation of the live entries), rebuilding the index. Whether a
    /// saved order is sorted is checked later, by each search on the
    /// entries it matched; an order that is missing or out of order there
    /// is built in memory for that search.
    ///
    /// Nothing on disk is written, renamed, created or removed.
    ///
    /// Errors: [`ReadError::NotFound`] when there is no file at `path`;
    /// [`ReadError::Rejected`] when it cannot be read or fx-store rejects it.
    pub fn open(path: impl AsRef<Path>) -> Result<StoreReader, ReadError> {
        let path = path.as_ref();
        let rejected = |source: LoadError| ReadError::Rejected {
            path: path.to_path_buf(),
            source,
        };
        let mapped = MappedIndex::open(path).map_err(|e| match e {
            LoadError::NotFound { path } => ReadError::NotFound { path },
            other => rejected(other),
        })?;
        mapped.verify_checksums().map_err(rejected)?;
        let index = mapped.share_index().map_err(|e| {
            rejected(LoadError::Malformed {
                reason: format!("the index does not rebuild: {e}"),
            })
        })?;
        Ok(StoreReader {
            snap: EngineSnapshot {
                index: index.snapshot(),
                // Read from `mapped` by each search, for its own sort only.
                orders: Arc::new(Vec::new()),
                cursor: None,
                generation: 0,
                // The command line keeps the plain sort.
                priority: None,
            },
            mapped,
            utc_offset: None,
        })
    }

    /// The order to read `matches` in by `sort`: the store's saved order
    /// when the matched entries appear in it in `sort` order, else one built
    /// from the index (an order that is missing, or that a store saved
    /// under other sort rules would hold).
    fn order_for(&self, sort: SortKey, matches: &MatchSet) -> Order {
        let index = self.snap.index();
        let saved = order_code(sort)
            .and_then(|code| self.mapped.order(code))
            .filter(|ids| matches_in_order(ids, sort, index, matches));
        match saved {
            Some(ids) => Order {
                key: sort,
                ids: ids.to_vec(),
            },
            None => Order::build(index, sort),
        }
    }

    /// Evaluates date functions (`dm:today`, ...) at `offset` seconds east
    /// of UTC instead of the operating system's local offset (`None`, the
    /// default).
    pub fn set_utc_offset(&mut self, offset: Option<i32>) {
        self.utc_offset = offset;
    }

    /// The number of live entries in the store.
    pub fn live_count(&self) -> usize {
        self.snap.index().live_count()
    }

    /// Searches the store for `query` (fx-query syntax) with the toggles in
    /// `opts`, and returns the matches at positions `offset .. offset +
    /// limit` in `sort` order read in `direction`, with the total match
    /// count. Date functions use the current time and the local UTC offset
    /// (see [`set_utc_offset`](Self::set_utc_offset)).
    ///
    /// With default options the results are those of
    /// [`Engine::search`](crate::Engine::search) over the same index.
    /// Offsets past the end give an empty window; huge limits are fine.
    ///
    /// Errors: [`SearchError::InvalidRegex`] when a regular expression in
    /// the query does not compile (the whole query with `opts.regex`, or a
    /// `regex:` term), where [`Engine::search`](crate::Engine::search)
    /// would let it match nothing; [`SearchError::Query`] when the query
    /// cannot be compiled otherwise. A malformed query never panics.
    pub fn search(
        &self,
        query: &str,
        opts: &ReadOptions,
        sort: SortKey,
        direction: Direction,
        offset: usize,
        limit: usize,
    ) -> Result<ReadPage, SearchError> {
        let opts = SearchOptions::from(*opts);
        let ctx = query::eval_context(self.utc_offset);
        let parsed = fx_query::parse(query, &opts);
        if let Some(e) = first_regex_error(&parsed.query) {
            return Err(e.into());
        }
        let compiled =
            CompiledQuery::compile(&parsed, &opts, ctx).map_err(|e| SearchError::Query {
                message: e.to_string(),
            })?;
        // The generation is never bumped, so the search is never cancelled
        // and always returns `Ok`.
        let generation = fx_query::Generation::new();
        let hits = compiled
            .search(&self.snap, &generation.token())
            .unwrap_or_default();
        let matches = self.snap.match_set(&hits);
        // No matches need no order (an empty page either way).
        let page = if matches.is_empty() {
            fx_sort::Page::default()
        } else {
            let order = self.order_for(sort, &matches);
            fx_sort::emit(&order, &matches, direction, offset, limit)
        };
        Ok(ReadPage {
            total: page.total,
            ids: page.ids,
            diagnostics: parsed.diagnostics,
            count: parsed.count,
        })
    }

    /// Whether `id` names a live entry of the store.
    pub fn contains(&self, id: u32) -> bool {
        self.snap.live(id)
    }

    /// The entry's full path: its name and its ancestors' names joined
    /// with `/` from the root entry down, as the macOS UI shows it. A root
    /// entry named `/` gives `/a/b`, not `//a/b`. `None` for an ID that is
    /// not a live entry.
    ///
    /// The store does not record the folder holding a root that is not
    /// `/`; use [`full_path_under`](Self::full_path_under) to prefix it.
    pub fn full_path(&self, id: u32) -> Option<String> {
        self.full_path_under(id, None)
    }

    /// Like [`full_path`](Self::full_path), with `root_parent` (the folder
    /// holding the indexed root, such as `/Users/me` for a root entry named
    /// `proj`) prefixed to paths that do not already start with `/`.
    pub fn full_path_under(&self, id: u32, root_parent: Option<&str>) -> Option<String> {
        if !self.contains(id) {
            return None;
        }
        let raw = self.snap.index().path(EntryId(id));
        let path = raw
            .strip_prefix('/')
            .filter(|rest| rest.starts_with('/'))
            .unwrap_or(&raw);
        Some(match root_parent {
            Some(parent) if !path.starts_with('/') => {
                format!("{}/{path}", parent.trim_end_matches('/'))
            }
            _ => path.to_string(),
        })
    }

    /// The entry's own name (one path component); `None` for an ID that is
    /// not a live entry. Owned, like [`full_path`](Self::full_path).
    pub fn name(&self, id: u32) -> Option<String> {
        self.contains(id)
            .then(|| self.snap.index().name(EntryId(id)).to_owned())
    }

    /// Whether the entry is a folder (`false` for an ID that is not a live
    /// entry).
    pub fn is_folder(&self, id: u32) -> bool {
        self.contains(id) && self.snap.index().kind(EntryId(id)) == EntryKind::Dir
    }

    /// The file size in bytes; `None` for folders (whose sizes are not
    /// tracked) and for an ID that is not a live entry.
    pub fn size(&self, id: u32) -> Option<u64> {
        self.contains(id)
            .then(|| SearchView::size(&self.snap, id))
            .flatten()
    }

    /// The last modification time in Unix seconds; `None` for an ID that is
    /// not a live entry.
    pub fn modified(&self, id: u32) -> Option<i64> {
        self.contains(id)
            .then(|| self.snap.index().modified(EntryId(id)))
    }

    /// The creation time in Unix seconds (the store always holds the
    /// column); `None` for an ID that is not a live entry.
    pub fn created(&self, id: u32) -> Option<i64> {
        self.contains(id)
            .then(|| self.snap.index().created(EntryId(id)))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use fx_model::{EntryId, EntryKind};
    use fx_sort::{MatchSet, SortKey, SortSource};

    use super::matches_in_order;

    /// Live, parentless entries with these names, recording every entry
    /// whose name is read.
    struct Names {
        names: Vec<&'static str>,
        read: RefCell<Vec<u32>>,
    }

    impl Names {
        fn new(names: &[&'static str]) -> Names {
            Names {
                names: names.to_vec(),
                read: RefCell::new(Vec::new()),
            }
        }
    }

    impl SortSource for Names {
        fn id_bound(&self) -> usize {
            self.names.len()
        }
        fn is_live(&self, id: EntryId) -> bool {
            id.index() < self.names.len()
        }
        fn name(&self, id: EntryId) -> &str {
            self.read.borrow_mut().push(id.0);
            self.names[id.index()]
        }
        fn parent(&self, _: EntryId) -> EntryId {
            EntryId::NONE
        }
        fn size(&self, _: EntryId) -> u64 {
            0
        }
        fn modified(&self, _: EntryId) -> i64 {
            0
        }
        fn created(&self, _: EntryId) -> i64 {
            0
        }
        fn kind(&self, _: EntryId) -> EntryKind {
            EntryKind::File
        }
    }

    fn set(ids: &[u32]) -> MatchSet {
        let mut set = MatchSet::with_bound(16);
        for &id in ids {
            set.insert(EntryId(id));
        }
        set
    }

    // IDs 0..6 by name: a (3), b (0), c (4), d (1), e (5), f (2).
    const NAMES: [&str; 6] = ["b", "d", "f", "a", "c", "e"];

    #[test]
    fn a_saved_order_wrong_only_outside_the_matches_is_used() {
        let src = Names::new(&NAMES);
        // b, d and f (0, 1, 2) are in order; e (5) first is not.
        let ids = [5, 0, 4, 1, 3, 2];
        assert!(matches_in_order(
            &ids,
            SortKey::Name,
            &src,
            &set(&[0, 1, 2])
        ));
        assert!(!matches_in_order(
            &ids,
            SortKey::Name,
            &src,
            &set(&[0, 1, 2, 3, 4, 5])
        ));
    }

    #[test]
    fn matched_entries_out_of_order_reject_the_saved_order() {
        let src = Names::new(&NAMES);
        // f (2) before d (1).
        let ids = [3, 0, 4, 2, 5, 1];
        assert!(!matches_in_order(&ids, SortKey::Name, &src, &set(&[1, 2])));
        assert!(matches_in_order(&ids, SortKey::Name, &src, &set(&[0, 2])));
    }

    #[test]
    fn only_the_matched_entries_are_compared() {
        let src = Names::new(&NAMES);
        let ids = [3, 0, 4, 1, 5, 2];
        assert!(matches_in_order(&ids, SortKey::Name, &src, &set(&[0, 5])));
        let mut read = src.read.borrow().clone();
        read.sort_unstable();
        read.dedup();
        assert_eq!(read, vec![0, 5], "names read");
    }

    #[test]
    fn a_match_missing_from_the_order_or_repeated_rejects_it() {
        let src = Names::new(&NAMES);
        // d (1) is missing.
        assert!(!matches_in_order(
            &[3, 0, 4, 5, 2],
            SortKey::Name,
            &src,
            &set(&[0, 1])
        ));
        // b (0) twice, d (1) missing.
        assert!(!matches_in_order(
            &[3, 0, 0, 4, 5, 2],
            SortKey::Name,
            &src,
            &set(&[0, 1])
        ));
    }

    #[test]
    fn no_matches_fit_any_order() {
        let src = Names::new(&NAMES);
        assert!(matches_in_order(&[2, 1, 0], SortKey::Path, &src, &set(&[])));
        assert!(src.read.borrow().is_empty());
    }
}
