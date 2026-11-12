//! A read-only view of a saved checkpoint, for the command line.
//!
//! [`StoreReader::open`] maps an fx-store checkpoint (as written by
//! [`Engine::checkpoint`](crate::Engine::checkpoint)), verifies it and
//! rebuilds the index and sort orders, like [`Engine::open`](crate::Engine::open)
//! does, but it never writes, renames, creates or removes anything (not even
//! stale temp files), starts no change source and spawns no threads. It
//! works on a read-only file in a read-only folder.
//!
//! [`StoreReader::search`] runs the same query and sort path as
//! [`Engine::search`](crate::Engine::search) over the loaded snapshot, with
//! the search toggles of [`ReadOptions`].

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fx_model::{EntryId, EntryKind};
use fx_query::{
    Cancelled, CompiledQuery, Diagnostic, Node, Pattern, RegexError, RegexTerm, SearchOptions,
    SearchView,
};
use fx_sort::{Direction, Order, SortKey};
use fx_store::{LoadError, MappedIndex};

use crate::query;
use crate::snapshot::{ENGINE_SORT_KEYS, EngineSnapshot};

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
/// Opening maps the store read-only, verifies it and rebuilds the index and
/// sort orders in memory; nothing on disk is ever written, renamed, created
/// or removed (not even stale temp files), no change source is started and
/// no thread is spawned. Searches run the same query and sort path as
/// [`Engine::search`](crate::Engine::search), with the toggles of
/// [`ReadOptions`].
#[derive(Debug)]
pub struct StoreReader {
    snap: EngineSnapshot,
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
    /// tables and every column checksum), rebuilding the index and the
    /// saved sort orders. Orders that are missing or do not fit the index
    /// are rebuilt in memory.
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
        let snap = index.snapshot();
        let orders: Vec<Order> = ENGINE_SORT_KEYS
            .iter()
            .map(|&key| {
                let saved = order_code(key)
                    .and_then(|code| mapped.order(code))
                    .map(|ids| Order {
                        key,
                        ids: ids.to_vec(),
                    })
                    .filter(|order| order.validate(&*snap).is_ok());
                saved.unwrap_or_else(|| Order::build(&*snap, key))
            })
            .collect();
        Ok(StoreReader {
            snap: EngineSnapshot {
                index: snap,
                orders: Arc::new(orders),
                cursor: None,
                generation: 0,
                // The reader doesn't know where volumes are mounted, and
                // finding offline entries would read every key: each hit is
                // checked instead, so offline entries stay hidden here too.
                hide_offline: true,
                // The command line keeps the plain sort.
                priority: None,
                orders_pending: false,
            },
            utc_offset: None,
        })
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
        let order = self.snap.order_or_build(sort);
        let page = fx_sort::emit(&order, &matches, direction, offset, limit);
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
