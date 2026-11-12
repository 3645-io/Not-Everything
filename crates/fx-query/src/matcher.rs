//! The compiled parallel query matcher.
//!
//! [`CompiledQuery::compile`] lowers a parsed query tree ([`Parsed`]) into
//! matcher nodes once; [`CompiledQuery::search`] then tests every live entry
//! of a [`SearchView`] in parallel with rayon and returns the matching IDs
//! in ascending order. The reference evaluator ([`crate::eval`]) is the
//! specification: for every live entry the result equals
//! `eval::matches_in(&parsed.query, &candidate, &ctx)`.
//!
//! How terms are lowered:
//!
//! - **Literals** fold the needle once and keep one
//!   [`memchr::memmem::Finder`] for it, run over the folded name (from the
//!   view's prebuilt folded copy when it holds the term's folding, else
//!   folded per entry into a buffer each search thread reuses, so an
//!   ASCII name, nearly every one, costs no allocation). The folding
//!   follows the apostrophe rule ([`FoldOptions::for_term`]): a term with
//!   an apostrophe reads the default copy, where apostrophes are kept as
//!   `'`; a term without one reads the copy's side table for the names
//!   that have an apostrophe and the default copy for the others (see
//!   [`crate::fold`]).
//!   `start-with:`, `end-with:`, `ww:` and `wfn:` check the positions the
//!   finder reports.
//! - **Wildcards** fold their literal pieces once. A name-only `*.ext`
//!   ([`WildcardPattern::extension_hint`]) becomes a direct suffix compare
//!   on the folded name; other patterns run a precompiled step simulation.
//! - **Regex terms** are compiled once with [`RegexTerm::compile`]; an
//!   invalid expression matches nothing, like the oracle.
//! - **Functions** precompute what they can (lowercased `ext:` lists,
//!   resolved date periods) and otherwise use [`crate::eval::filter_matches`].
//! - **Kind filters** (`file:`, `folder:`) are checked before the text;
//!   a term for files and folders alike never reads the entry's kind.
//! - `And` children are ordered cheapest first (kind, extension, metadata
//!   and literals before regexes and path terms), which is safe because
//!   every node is a pure function of the entry.
//!
//! Paths are never stored, and most path terms never build one. A plain
//! path literal (`path:resources/`) is decided per folder once per search
//! and the answer propagates down the tree: an entry then costs one memo
//! lookup and a search of its name (plus, for a needle with `/`, the end
//! of its folder's path). Other terms that need the full path build it
//! lazily, only for entries that pass the cheaper checks before it, from
//! the folder's cached (and, per folding, cached folded) path plus the
//! entry's name. The crate's private `paths` module holds both, and says
//! why they agree with building every path (also in the crate README).
//!
//! A root entry (no parent) has an empty parent path, so its name is its
//! full path (`"/"`, `"/Volumes/Ext"`); a child's parent path is its
//! parent's full path, joined as [`Candidate::full_path`] does.
//!
//! The scan runs over fixed chunks of [`DEFAULT_CHUNK_ENTRIES`] entries and
//! checks the [`CancelToken`] before each chunk, so a superseded search
//! stops within one chunk per thread (well inside [`CHECK_INTERVAL`]).
//! `count:N` is exposed through [`CompiledQuery::limit`] and not applied:
//! the limit belongs after sorting.

use std::error::Error;
use std::fmt;
use std::rc::Rc;

use memchr::memmem::Finder;
use rayon::prelude::*;

use crate::eval::{Candidate, EvalContext, filter_matches};
use crate::fold::{FoldOptions, FoldedColumn, fold_into, fold_str};
use crate::paths::{PathCache, PathMemo, PathNeedle, join};
use crate::regex_term::RegexTerm;
use crate::substring::{CHECK_INTERVAL, CancelToken, Cancelled, DEFAULT_CHUNK_ENTRIES, NameColumn};
use crate::syntax::functions::{Cmp, DateField, Filter, ValueTest};
use crate::syntax::wildcard::Piece;
use crate::syntax::{
    Node, Parsed, Pattern, SearchOptions, Term, TermKind, TermOptions, WildcardPattern,
};

/// Entries per parallel chunk. Cancellation is checked before each chunk.
const CHUNK_ENTRIES: usize = DEFAULT_CHUNK_ENTRIES;
const _: () = assert!(CHUNK_ENTRIES <= CHECK_INTERVAL);

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

/// A columnar, read-only view of indexed entries, addressed by `u32` ID
/// (`0..len()`).
///
/// Only `len`, `name`, `parent`, `is_folder` and `is_live` are required.
/// The metadata methods default to "unknown" (`None`) or "not set"
/// (`false`), as in [`Candidate`]. [`SearchView::folded`] may hand out a
/// prebuilt folded copy of the name; without it the matcher folds names
/// per entry.
pub trait SearchView: Sync {
    /// Number of entry slots, deleted ones included.
    fn len(&self) -> usize;
    /// Whether the view has no entry slots.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// The entry's name (last path component; a root's name is its path).
    fn name(&self, id: u32) -> &str;
    /// The folder holding the entry, or `None` for a root.
    fn parent(&self, id: u32) -> Option<u32>;
    /// Whether the entry is a folder.
    fn is_folder(&self, id: u32) -> bool;
    /// Whether the entry exists; deleted entries are skipped by searches.
    fn is_live(&self, id: u32) -> bool;
    /// The name folded with [`fold_str`] under `opts`, if the view keeps
    /// a folded copy holding that form (see [`FoldedColumn::folded_name`]).
    fn folded(&self, id: u32, opts: FoldOptions) -> Option<&str> {
        let _ = (id, opts);
        None
    }
    /// Size in bytes, if known.
    fn size(&self, id: u32) -> Option<u64> {
        let _ = id;
        None
    }
    /// Modification time in Unix seconds, if known.
    fn modified(&self, id: u32) -> Option<i64> {
        let _ = id;
        None
    }
    /// Creation time in Unix seconds, if known.
    fn created(&self, id: u32) -> Option<i64> {
        let _ = id;
        None
    }
    /// Last access time in Unix seconds, if known.
    fn accessed(&self, id: u32) -> Option<i64> {
        let _ = id;
        None
    }
    /// Number of direct children of a folder, if known.
    fn child_count(&self, id: u32) -> Option<u32> {
        let _ = id;
        None
    }
    /// The entry is hidden.
    fn hidden(&self, id: u32) -> bool {
        let _ = id;
        false
    }
    /// The entry is a symbolic link.
    fn symlink(&self, id: u32) -> bool {
        let _ = id;
        false
    }
    /// The entry is read-only (locked).
    fn read_only(&self, id: u32) -> bool {
        let _ = id;
        false
    }
}

/// Metadata of one [`VecView`] row: the metadata fields of [`Candidate`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Metadata {
    /// Size in bytes, if known.
    pub size: Option<u64>,
    /// Modification time in Unix seconds, if known.
    pub modified: Option<i64>,
    /// Creation time in Unix seconds, if known.
    pub created: Option<i64>,
    /// Last access time in Unix seconds, if known.
    pub accessed: Option<i64>,
    /// Number of direct children of a folder, if known.
    pub child_count: Option<u32>,
    /// The entry is hidden.
    pub hidden: bool,
    /// The entry is a symbolic link.
    pub symlink: bool,
    /// The entry is read-only (locked).
    pub read_only: bool,
}

/// No parent, in [`VecView`]'s parent column.
const NO_PARENT: u32 = u32::MAX;

/// A simple owned [`SearchView`] for tests and benchmarks.
///
/// Names live in one NUL-separated buffer with `u32` start offsets (the
/// [`NameColumn`] layout), next to parent, folder, liveness and metadata
/// columns. [`VecView::build_folded`] adds a [`FoldedColumn`].
#[derive(Debug, Clone, Default)]
pub struct VecView {
    buf: Vec<u8>,
    offsets: Vec<u32>,
    parents: Vec<u32>,
    folders: Vec<bool>,
    live: Vec<bool>,
    meta: Vec<Metadata>,
    folded: Option<(FoldOptions, FoldedColumn)>,
}

impl VecView {
    /// An empty view.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends an entry and returns its ID (IDs count up from 0). Drops a
    /// folded copy built earlier.
    ///
    /// # Panics
    ///
    /// If `name` contains NUL, `parent` is not an earlier ID, or the view
    /// outgrows `u32` IDs or offsets.
    pub fn push(
        &mut self,
        name: &str,
        parent: Option<u32>,
        is_folder: bool,
        meta: Metadata,
    ) -> u32 {
        assert!(!name.contains('\0'), "names cannot contain NUL");
        let id = u32::try_from(self.offsets.len()).expect("too many entries");
        assert!(id != NO_PARENT, "too many entries");
        if let Some(p) = parent {
            assert!(p < id, "parent {p} must be an earlier entry");
        }
        let offset = u32::try_from(self.buf.len()).expect("name buffer exceeds u32 offsets");
        self.offsets.push(offset);
        self.buf.extend_from_slice(name.as_bytes());
        self.buf.push(0);
        self.parents.push(parent.unwrap_or(NO_PARENT));
        self.folders.push(is_folder);
        self.live.push(true);
        self.meta.push(meta);
        self.folded = None;
        id
    }

    /// Marks an entry deleted; searches skip it.
    pub fn mark_deleted(&mut self, id: u32) {
        self.live[id as usize] = false;
    }

    /// Builds the folded search copy of the names for `opts`, used by
    /// terms that fold the same way (by default, case and diacritics, with
    /// apostrophes kept and a side table for terms without one).
    pub fn build_folded(&mut self, opts: FoldOptions) {
        let column = self.column();
        let folded = FoldedColumn::build(&column, opts);
        self.folded = Some((opts, folded));
    }

    /// The names as a [`NameColumn`].
    pub fn column(&self) -> NameColumn<'_> {
        NameColumn::new(&self.buf, &self.offsets).expect("a VecView always has a valid layout")
    }

    /// The metadata of an entry.
    pub fn metadata(&self, id: u32) -> &Metadata {
        &self.meta[id as usize]
    }

    fn name_bytes(&self, id: u32) -> &[u8] {
        let i = id as usize;
        let start = self.offsets[i] as usize;
        let end = match self.offsets.get(i + 1) {
            Some(&next) => next as usize - 1,
            None => self.buf.len() - 1,
        };
        &self.buf[start..end]
    }
}

impl SearchView for VecView {
    fn len(&self) -> usize {
        self.offsets.len()
    }
    fn name(&self, id: u32) -> &str {
        // Pushed from `&str`, so always valid UTF-8.
        std::str::from_utf8(self.name_bytes(id)).unwrap_or("")
    }
    fn parent(&self, id: u32) -> Option<u32> {
        let p = self.parents[id as usize];
        (p != NO_PARENT).then_some(p)
    }
    fn is_folder(&self, id: u32) -> bool {
        self.folders[id as usize]
    }
    fn is_live(&self, id: u32) -> bool {
        self.live[id as usize]
    }
    fn folded(&self, id: u32, opts: FoldOptions) -> Option<&str> {
        let (_, col) = self.folded.as_ref()?;
        std::str::from_utf8(col.folded_name(id as usize, opts)?).ok()
    }
    fn size(&self, id: u32) -> Option<u64> {
        self.meta[id as usize].size
    }
    fn modified(&self, id: u32) -> Option<i64> {
        self.meta[id as usize].modified
    }
    fn created(&self, id: u32) -> Option<i64> {
        self.meta[id as usize].created
    }
    fn accessed(&self, id: u32) -> Option<i64> {
        self.meta[id as usize].accessed
    }
    fn child_count(&self, id: u32) -> Option<u32> {
        self.meta[id as usize].child_count
    }
    fn hidden(&self, id: u32) -> bool {
        self.meta[id as usize].hidden
    }
    fn symlink(&self, id: u32) -> bool {
        self.meta[id as usize].symlink
    }
    fn read_only(&self, id: u32) -> bool {
        self.meta[id as usize].read_only
    }
}

// ---------------------------------------------------------------------------
// Compiled query
// ---------------------------------------------------------------------------

/// A query that could not be compiled. Every query the parser produces
/// compiles today (an invalid regex matches nothing, as in the oracle);
/// the type leaves room for limits on query size.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CompileError {
    /// The query uses a feature the matcher does not implement.
    Unsupported(&'static str),
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CompileError::Unsupported(feature) => {
                write!(f, "the matcher does not support {feature}")
            }
        }
    }
}

impl Error for CompileError {}

/// A query tree lowered into matcher nodes, ready to run over any
/// [`SearchView`] (see the [module docs](self)).
#[derive(Debug)]
pub struct CompiledQuery {
    root: M,
    limit: Option<usize>,
    /// Plain path literals, each with a folder memo per search.
    path_memos: usize,
}

impl CompiledQuery {
    /// Lowers `parsed` into matcher nodes. Terms carry their resolved
    /// options already, so `opts` (the global toggles the query was parsed
    /// with) only documents the pairing; date functions resolve against
    /// `ctx`.
    pub fn compile(
        parsed: &Parsed,
        opts: &SearchOptions,
        ctx: EvalContext,
    ) -> Result<CompiledQuery, CompileError> {
        let _ = opts;
        let mut root = lower(&parsed.query, &ctx);
        let mut path_memos = 0;
        root.number_path_memos(&mut path_memos);
        Ok(CompiledQuery {
            root,
            limit: parsed.count,
            path_memos,
        })
    }

    /// The `count:N` result limit, if the query set one. Not applied by
    /// the search methods: it belongs after sorting.
    pub fn limit(&self) -> Option<usize> {
        self.limit
    }

    /// Whether the live entry `id` matches (liveness is not checked).
    pub fn is_match<V: SearchView + ?Sized>(&self, view: &V, id: u32) -> bool {
        let mut cache = PathCache::default();
        self.root.eval(&mut Entry::new(view, id, &mut cache, None))
    }

    /// The folder memos of one search over `view`.
    fn memos<V: SearchView + ?Sized>(&self, view: &V) -> Vec<PathMemo> {
        (0..self.path_memos)
            .map(|_| PathMemo::new(view.len()))
            .collect()
    }

    /// Every live matching entry of `view`, in ascending ID order.
    pub fn search<V: SearchView + ?Sized>(
        &self,
        view: &V,
        cancel: &CancelToken<'_>,
    ) -> Result<Vec<u32>, Cancelled> {
        check(cancel)?;
        let n = view.len();
        let memos = self.memos(view);
        let parts: Vec<Vec<u32>> = (0..n.div_ceil(CHUNK_ENTRIES))
            .into_par_iter()
            .map_init(PathCache::default, |cache, chunk| {
                check(cancel)?;
                let start = chunk * CHUNK_ENTRIES;
                let end = (start + CHUNK_ENTRIES).min(n);
                let mut out = Vec::new();
                for id in start..end {
                    let id = id as u32;
                    if self.test(view, id, cache, &memos) {
                        out.push(id);
                    }
                }
                Ok(out)
            })
            .collect::<Result<_, Cancelled>>()?;
        Ok(parts.concat())
    }

    /// The number of live matching entries; equals `search(..).len()`.
    pub fn count<V: SearchView + ?Sized>(
        &self,
        view: &V,
        cancel: &CancelToken<'_>,
    ) -> Result<usize, Cancelled> {
        check(cancel)?;
        let n = view.len();
        let memos = self.memos(view);
        (0..n.div_ceil(CHUNK_ENTRIES))
            .into_par_iter()
            .map_init(PathCache::default, |cache, chunk| {
                check(cancel)?;
                let start = chunk * CHUNK_ENTRIES;
                let end = (start + CHUNK_ENTRIES).min(n);
                Ok((start..end)
                    .filter(|&id| self.test(view, id as u32, cache, &memos))
                    .count())
            })
            .try_reduce(|| 0, |a, b| Ok(a + b))
    }

    /// The live matching entries among `candidates`, in candidate order
    /// (ascending when the candidates are). IDs outside the view are
    /// skipped. Used to refine an earlier result when the query grows.
    pub fn search_within<V: SearchView + ?Sized>(
        &self,
        view: &V,
        candidates: &[u32],
        cancel: &CancelToken<'_>,
    ) -> Result<Vec<u32>, Cancelled> {
        check(cancel)?;
        let n = view.len();
        let memos = self.memos(view);
        let parts: Vec<Vec<u32>> = candidates
            .par_chunks(CHUNK_ENTRIES)
            .map_init(PathCache::default, |cache, chunk| {
                check(cancel)?;
                Ok(chunk
                    .iter()
                    .copied()
                    .filter(|&id| (id as usize) < n && self.test(view, id, cache, &memos))
                    .collect())
            })
            .collect::<Result<_, Cancelled>>()?;
        Ok(parts.concat())
    }

    fn test<V: SearchView + ?Sized>(
        &self,
        view: &V,
        id: u32,
        cache: &mut PathCache,
        memos: &[PathMemo],
    ) -> bool {
        if !view.is_live(id) {
            return false;
        }
        cache.trim();
        self.root
            .eval(&mut Entry::new(view, id, cache, Some(memos)))
    }
}

fn check(cancel: &CancelToken<'_>) -> Result<(), Cancelled> {
    if cancel.is_cancelled() {
        Err(Cancelled)
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Lazy paths
// ---------------------------------------------------------------------------

/// One entry under test, with its paths built on first use.
struct Entry<'a, V: SearchView + ?Sized> {
    view: &'a V,
    id: u32,
    cache: &'a mut PathCache,
    /// The search's folder memos; `None` for a one-off test, where plain
    /// path literals build the path instead.
    memos: Option<&'a [PathMemo]>,
    parent_path: Option<Rc<str>>,
    path: Option<String>,
}

impl<'a, V: SearchView + ?Sized> Entry<'a, V> {
    fn new(view: &'a V, id: u32, cache: &'a mut PathCache, memos: Option<&'a [PathMemo]>) -> Self {
        Entry {
            view,
            id,
            cache,
            memos,
            parent_path: None,
            path: None,
        }
    }

    fn name(&self) -> &'a str {
        self.view.name(self.id)
    }

    fn is_dir(&self) -> bool {
        self.view.is_folder(self.id)
    }

    /// Whether the entry is of `kind`. Files and folders alike
    /// ([`TermKind::Any`], most terms) never read the entry's kind, which
    /// costs a row read in a view such as the engine's.
    fn kind_ok(&self, kind: TermKind) -> bool {
        match kind {
            TermKind::Any => true,
            TermKind::FilesOnly => !self.is_dir(),
            TermKind::FoldersOnly => self.is_dir(),
        }
    }

    fn parent_path(&mut self) -> &str {
        if self.parent_path.is_none() {
            let pp = match self.view.parent(self.id) {
                Some(p) => self.cache.raw(self.view, p),
                None => Rc::from(""),
            };
            self.parent_path = Some(pp);
        }
        self.parent_path.as_deref().unwrap_or("")
    }

    fn path(&mut self) -> &str {
        if self.path.is_none() {
            let name = self.name();
            let full = join(self.parent_path(), name);
            self.path = Some(full);
        }
        self.path.as_deref().unwrap_or("")
    }

    /// `test` of the name or the full path, folded under `fold`. The full
    /// path is the folder's cached folded path plus the folded name. A name
    /// is read from the view's folded copy when it has one, and is
    /// otherwise folded into the thread's buffer, so testing an ASCII name
    /// allocates nothing (a search re-run folds every name).
    fn with_hay(
        &mut self,
        path: bool,
        fold: FoldOptions,
        test: impl FnOnce(Hay<'_>) -> bool,
    ) -> bool {
        let (view, id) = (self.view, self.id);
        if path {
            return test(Hay::Text(&self.cache.folded_path(
                view,
                id,
                view.parent(id),
                fold,
            )));
        }
        if let Some(folded) = view.folded(id, fold) {
            return test(Hay::Text(folded));
        }
        let name = view.name(id);
        let buf = &mut self.cache.fold_buf;
        if fold_into(name, fold, buf) {
            test(Hay::Folded(buf))
        } else {
            test(Hay::Text(name))
        }
    }
}

/// A folded name or full path under test. A plain substring or `*.ext`
/// test reads its bytes; tests that need characters read it as text, which
/// costs a UTF-8 check for a name folded into the thread's buffer (a check
/// that costs about as much as folding a short name, so the plain tests,
/// most of them, skip it).
#[derive(Debug, Clone, Copy)]
enum Hay<'h> {
    /// Text that needed no folding, or was folded elsewhere.
    Text(&'h str),
    /// What [`fold_into`] wrote, which is always UTF-8.
    Folded(&'h [u8]),
}

impl<'h> Hay<'h> {
    fn bytes(self) -> &'h [u8] {
        match self {
            Hay::Text(text) => text.as_bytes(),
            Hay::Folded(bytes) => bytes,
        }
    }

    fn text(self) -> &'h str {
        match self {
            Hay::Text(text) => text,
            // Never fails (see `Folded`); an empty text would only miss a
            // match, never panic in a search.
            Hay::Folded(bytes) => std::str::from_utf8(bytes).unwrap_or_default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Matcher nodes
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum M {
    True,
    False,
    Not(Box<M>),
    And(Vec<M>),
    Or(Vec<M>),
    Lit(Box<LitTerm>),
    Wild(Box<WildTerm>),
    Regex {
        term: Box<RegexTerm>,
        kind: TermKind,
        path: bool,
    },
    /// `ext:` with the list lowercased per `char`.
    Ext(Vec<String>),
    /// A date function with its periods resolved at compile time.
    Date(DateField, DateTest),
    /// Any other function, through [`filter_matches`].
    Filter(Filter, EvalContext),
}

#[derive(Debug)]
struct LitTerm {
    kind: TermKind,
    path: bool,
    fold: FoldOptions,
    opts: TermOptions,
    needle: String,
    finder: Finder<'static>,
    /// For a plain path literal, its folder memo in the search's list.
    memo: Option<usize>,
}

#[derive(Debug)]
struct WildTerm {
    kind: TermKind,
    path: bool,
    fold: FoldOptions,
    shape: WildShape,
}

#[derive(Debug)]
enum WildShape {
    /// `*.ext` on a name: the folded name ends with this `.ext` and has no
    /// `/` before it.
    Suffix(String),
    /// Any other pattern, as folded steps.
    Steps(Vec<Step>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Char(char),
    One,
    Seq,
    Cross,
}

#[derive(Debug)]
enum DateTest {
    Cmp(Cmp, Option<(i64, i64)>),
    Range(Option<(i64, i64)>),
}

impl M {
    fn eval<V: SearchView + ?Sized>(&self, e: &mut Entry<'_, V>) -> bool {
        match self {
            M::True => true,
            M::False => false,
            M::Not(inner) => !inner.eval(e),
            M::And(children) => children.iter().all(|c| c.eval(e)),
            M::Or(children) => children.iter().any(|c| c.eval(e)),
            M::Lit(t) => e.kind_ok(t.kind) && t.eval(e),
            M::Wild(t) => e.kind_ok(t.kind) && e.with_hay(t.path, t.fold, |hay| t.matches(hay)),
            M::Regex { term, kind, path } => {
                e.kind_ok(*kind)
                    && if *path {
                        term.is_match(e.path())
                    } else {
                        term.is_match(e.name())
                    }
            }
            M::Ext(exts) => ext_matches(exts, extension(e.name())),
            M::Date(field, test) => {
                let (view, id) = (e.view, e.id);
                let stamp = match field {
                    DateField::Modified => view.modified(id),
                    DateField::Created => view.created(id),
                    DateField::Accessed => view.accessed(id),
                };
                stamp.is_some_and(|t| test.matches(t))
            }
            M::Filter(filter, ctx) => {
                let (view, id) = (e.view, e.id);
                let needs_parent = matches!(filter, Filter::Parent(_) | Filter::Depth(_));
                let name = e.name();
                let parent_path = if needs_parent { e.parent_path() } else { "" };
                let candidate = Candidate {
                    name,
                    parent_path,
                    is_dir: view.is_folder(id),
                    size: view.size(id),
                    modified: None,
                    created: None,
                    accessed: None,
                    child_count: view.child_count(id),
                    hidden: view.hidden(id),
                    symlink: view.symlink(id),
                    read_only: view.read_only(id),
                };
                filter_matches(filter, &candidate, ctx).unwrap_or(false)
            }
        }
    }

    /// Gives every plain path literal its own folder memo index, counting
    /// up from `next`.
    fn number_path_memos(&mut self, next: &mut usize) {
        match self {
            M::Lit(t) if t.is_plain_path() => {
                t.memo = Some(*next);
                *next += 1;
            }
            M::Not(inner) => inner.number_path_memos(next),
            M::And(children) | M::Or(children) => {
                for child in children {
                    child.number_path_memos(next);
                }
            }
            _ => {}
        }
    }

    /// A rough relative cost, for ordering `And` children.
    fn cost(&self) -> u32 {
        match self {
            M::True | M::False => 0,
            M::Ext(_) | M::Date(..) => 1,
            M::Filter(f, _) => match f {
                Filter::Parent(_) | Filter::Depth(_) => 8,
                _ => 1,
            },
            M::Wild(t) => match (&t.shape, t.path) {
                (_, true) => 12,
                (WildShape::Suffix(_), false) => 2,
                (WildShape::Steps(_), false) => 6,
            },
            M::Lit(t) => match (t.path, t.opts.whole_word) {
                (true, _) => 10,
                (false, false) => 3,
                (false, true) => 4,
            },
            M::Regex { path, .. } => {
                if *path {
                    14
                } else {
                    8
                }
            }
            M::Not(inner) => inner.cost(),
            M::And(children) | M::Or(children) => {
                children.iter().map(M::cost).fold(0, u32::saturating_add)
            }
        }
    }
}

impl DateTest {
    fn new(test: &ValueTest<crate::syntax::functions::DateValue>, ctx: &EvalContext) -> Self {
        let span = |v: &crate::syntax::functions::DateValue| v.span(ctx.now, ctx.utc_offset_secs);
        match test {
            ValueTest::Cmp(cmp, value) => DateTest::Cmp(*cmp, span(value)),
            ValueTest::Range(range) => {
                DateTest::Range(match (span(&range.start), span(&range.end)) {
                    (Some((start, _)), Some((_, end))) => Some((start, end)),
                    _ => None,
                })
            }
        }
    }

    /// Same rules as the reference evaluator's date comparison.
    fn matches(&self, t: i64) -> bool {
        match *self {
            DateTest::Cmp(cmp, Some((start, end))) => match cmp {
                Cmp::Eq => start <= t && t < end,
                Cmp::ExactEq => t == start,
                Cmp::Ne => !(start <= t && t < end),
                Cmp::Lt => t < start,
                Cmp::Le => t < end,
                Cmp::Gt => t >= end,
                Cmp::Ge => t >= start,
            },
            DateTest::Range(Some((start, end))) => start <= t && t < end,
            DateTest::Cmp(_, None) | DateTest::Range(None) => false,
        }
    }
}

impl LitTerm {
    /// Whether the term matches entry `e` (its kind aside).
    fn eval<V: SearchView + ?Sized>(&self, e: &mut Entry<'_, V>) -> bool {
        if let (Some(k), Some(memos)) = (self.memo, e.memos) {
            let needle = PathNeedle {
                needle: &self.needle,
                finder: &self.finder,
                fold: self.fold,
            };
            return memos[k].is_hit(e.view, &needle, e.id, &mut e.cache.scratch);
        }
        e.with_hay(self.path, self.fold, |hay| self.matches(hay))
    }

    /// Whether the term is a plain substring test over the full path,
    /// which a folder memo can decide.
    fn is_plain_path(&self) -> bool {
        let o = &self.opts;
        self.path
            && !self.needle.is_empty()
            && !(o.whole_word || o.start_with || o.end_with || o.whole_filename)
    }

    fn matches(&self, hay: Hay<'_>) -> bool {
        let opts = &self.opts;
        let constrained =
            opts.whole_word || opts.start_with || opts.end_with || opts.whole_filename;
        if !constrained && !self.needle.is_empty() {
            return self.finder.find(hay.bytes()).is_some();
        }
        self.matches_text(hay.text())
    }

    /// [`matches`](Self::matches) for an empty needle or a constrained one
    /// (`start-with:`, `end-with:`, `ww:`, `wfn:`), which look at
    /// characters around the match.
    fn matches_text(&self, hay: &str) -> bool {
        let opts = &self.opts;
        if self.needle.is_empty() {
            return find_constrained(hay, &self.needle, opts);
        }
        let n = self.needle.len();
        let must_start = opts.start_with || opts.whole_filename;
        let must_end = opts.end_with || opts.whole_filename;
        // A valid UTF-8 needle only matches valid UTF-8 at char boundaries.
        if must_start {
            return hay.starts_with(self.needle.as_str()) && self.accept(hay, 0, must_end);
        }
        if must_end {
            return hay.len() >= n
                && hay.ends_with(self.needle.as_str())
                && self.accept(hay, hay.len() - n, true);
        }
        let bytes = hay.as_bytes();
        let mut pos = 0;
        while let Some(i) = self.finder.find(&bytes[pos..]) {
            let at = pos + i;
            if self.accept(hay, at, false) {
                return true;
            }
            pos = at + 1;
        }
        false
    }

    /// Whether a match of the needle at byte `at` satisfies the end and
    /// whole-word constraints.
    fn accept(&self, hay: &str, at: usize, must_end: bool) -> bool {
        let end = at + self.needle.len();
        if must_end && end != hay.len() {
            return false;
        }
        if self.opts.whole_word {
            let first_word = self.needle.chars().next().is_some_and(is_word_char);
            let last_word = self.needle.chars().next_back().is_some_and(is_word_char);
            if first_word && hay[..at].chars().next_back().is_some_and(is_word_char) {
                return false;
            }
            if last_word && hay[end..].chars().next().is_some_and(is_word_char) {
                return false;
            }
        }
        true
    }
}

impl WildTerm {
    fn matches(&self, hay: Hay<'_>) -> bool {
        match &self.shape {
            WildShape::Suffix(suffix) => {
                // `/` is ASCII, so no other character's bytes contain it.
                let hay = hay.bytes();
                hay.ends_with(suffix.as_bytes())
                    && memchr::memchr(b'/', &hay[..hay.len() - suffix.len()]).is_none()
            }
            WildShape::Steps(steps) => run_steps(steps, hay.text()),
        }
    }
}

// ---------------------------------------------------------------------------
// Lowering
// ---------------------------------------------------------------------------

fn lower(node: &Node, ctx: &EvalContext) -> M {
    match node {
        Node::MatchAll => M::True,
        Node::Not(inner) => match lower(inner, ctx) {
            M::True => M::False,
            M::False => M::True,
            M::Not(x) => *x,
            other => M::Not(Box::new(other)),
        },
        Node::And(children) => {
            let mut lowered = Vec::with_capacity(children.len());
            for child in children {
                match lower(child, ctx) {
                    M::True => {}
                    M::False => return M::False,
                    M::And(inner) => lowered.extend(inner),
                    other => lowered.push(other),
                }
            }
            lowered.sort_by_key(M::cost);
            match lowered.len() {
                0 => M::True,
                1 => lowered.pop().unwrap_or(M::True),
                _ => M::And(lowered),
            }
        }
        Node::Or(children) => {
            let mut lowered = Vec::with_capacity(children.len());
            for child in children {
                match lower(child, ctx) {
                    M::False => {}
                    M::True => return M::True,
                    M::Or(inner) => lowered.extend(inner),
                    other => lowered.push(other),
                }
            }
            lowered.sort_by_key(M::cost);
            match lowered.len() {
                0 => M::False,
                1 => lowered.pop().unwrap_or(M::False),
                _ => M::Or(lowered),
            }
        }
        Node::Term(term) => lower_term(term),
        Node::Function(filter) => lower_filter(filter, ctx),
    }
}

fn lower_term(term: &Term) -> M {
    let opts = term.options;
    let fold = |text: &str| FoldOptions::for_term(text, opts.match_case, opts.match_diacritics);
    match &term.pattern {
        Pattern::Literal(text) => {
            let fold = fold(text);
            let needle = fold_str(text, fold).into_owned();
            let finder = Finder::new(needle.as_bytes()).into_owned();
            M::Lit(Box::new(LitTerm {
                kind: opts.kind,
                path: opts.match_path || text.contains('/'),
                fold,
                opts,
                needle,
                finder,
                memo: None,
            }))
        }
        Pattern::Wildcard(pattern) => {
            let path = opts.match_path || term.text.contains('/');
            let fold = fold(&literal_text(pattern));
            let folded = fold_pattern(pattern, fold);
            let shape = match folded.extension_hint() {
                Some(ext) if !path => WildShape::Suffix(format!(".{ext}")),
                _ => WildShape::Steps(steps(&folded)),
            };
            M::Wild(Box::new(WildTerm {
                kind: opts.kind,
                path,
                fold,
                shape,
            }))
        }
        Pattern::Regex(pattern) => match RegexTerm::compile(pattern, &opts) {
            Ok(term) => {
                let kind = term.options().kind;
                let path = term.matches_path();
                M::Regex {
                    term: Box::new(term),
                    kind,
                    path,
                }
            }
            // An invalid expression matches nothing, as in the oracle.
            Err(_) => M::False,
        },
    }
}

fn lower_filter(filter: &Filter, ctx: &EvalContext) -> M {
    match filter {
        Filter::Ext(exts) => M::Ext(exts.iter().map(|e| lowercase(e)).collect()),
        Filter::Date(field, test) => M::Date(*field, DateTest::new(test, ctx)),
        other => M::Filter(other.clone(), *ctx),
    }
}

// ---------------------------------------------------------------------------
// Helpers mirroring the reference evaluator
// ---------------------------------------------------------------------------

/// Lowercases one `char` at a time (no context-dependent mappings).
fn lowercase(s: &str) -> String {
    s.chars().flat_map(char::to_lowercase).collect()
}

/// The text after the last `.` of a name, or `""` when there is none or
/// the only `.` starts the name.
fn extension(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 => &name[i + 1..],
        _ => "",
    }
}

/// Whether `ext` lowercased per `char` is one of the lowercased `exts`,
/// without allocating for ASCII extensions.
fn ext_matches(exts: &[String], ext: &str) -> bool {
    if ext.is_ascii() {
        exts.iter().any(|e| {
            e.len() == ext.len()
                && e.bytes()
                    .zip(ext.bytes())
                    .all(|(a, b)| a == b.to_ascii_lowercase())
        })
    } else {
        let lower = lowercase(ext);
        exts.contains(&lower)
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric()
}

/// The reference evaluator's constrained substring search, used for the
/// empty needle (which the finder cannot position).
fn find_constrained(hay: &str, needle: &str, opts: &TermOptions) -> bool {
    let must_start = opts.start_with || opts.whole_filename;
    let must_end = opts.end_with || opts.whole_filename;
    let needle_first_word = needle.chars().next().is_some_and(is_word_char);
    let needle_last_word = needle.chars().next_back().is_some_and(is_word_char);
    let starts = hay
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(hay.len()));
    for i in starts {
        if must_start && i != 0 {
            break;
        }
        if !hay[i..].starts_with(needle) {
            continue;
        }
        let end = i + needle.len();
        if must_end && end != hay.len() {
            continue;
        }
        if opts.whole_word {
            let before = hay[..i].chars().next_back();
            let after = hay[end..].chars().next();
            if needle_first_word && before.is_some_and(is_word_char) {
                continue;
            }
            if needle_last_word && after.is_some_and(is_word_char) {
                continue;
            }
        }
        return true;
    }
    false
}

/// The literal pieces of a wildcard pattern, joined, which decide its
/// folding as in the reference evaluator.
fn literal_text(pattern: &WildcardPattern) -> String {
    pattern
        .pieces()
        .iter()
        .filter_map(|piece| match piece {
            Piece::Literal(lit) => Some(lit.as_str()),
            _ => None,
        })
        .collect()
}

/// Folds the literal pieces of a wildcard pattern, keeping its wildcards,
/// exactly as the reference evaluator does.
fn fold_pattern(pattern: &WildcardPattern, fold: FoldOptions) -> WildcardPattern {
    let mut text = String::new();
    let mut star_run: Option<bool> = None;
    for piece in pattern.pieces() {
        match piece {
            Piece::Literal(lit) => {
                let lit = fold_str(lit, fold);
                if !lit.is_empty() {
                    text.push_str(&lit);
                    star_run = None;
                }
            }
            Piece::AnyOne => {
                text.push('?');
                star_run = None;
            }
            Piece::AnySeq | Piece::AnySeqCrossing => {
                let crossing = matches!(piece, Piece::AnySeqCrossing);
                match star_run {
                    Some(true) => {}
                    Some(false) if crossing => text.push('*'),
                    Some(false) => {}
                    None => text.push_str(if crossing { "**" } else { "*" }),
                }
                star_run = Some(star_run.unwrap_or(false) || crossing);
            }
        }
    }
    WildcardPattern::new(&text)
}

/// The steps of an already folded pattern, matched case-sensitively.
fn steps(pattern: &WildcardPattern) -> Vec<Step> {
    let mut steps = Vec::new();
    for piece in pattern.pieces() {
        match piece {
            Piece::Literal(s) => steps.extend(s.chars().map(Step::Char)),
            Piece::AnyOne => steps.push(Step::One),
            Piece::AnySeq => steps.push(Step::Seq),
            Piece::AnySeqCrossing => steps.push(Step::Cross),
        }
    }
    steps
}

/// Positions tracked on the stack before falling back to the heap.
const INLINE_STEPS: usize = 64;

/// Simulates the steps over the whole of `text`, tracking every reachable
/// position (linear in `text × steps`, as [`WildcardPattern::matches`]).
fn run_steps(steps: &[Step], text: &str) -> bool {
    let m = steps.len();
    if m < INLINE_STEPS {
        let mut current = [false; INLINE_STEPS + 1];
        let mut next = [false; INLINE_STEPS + 1];
        simulate(steps, text, &mut current[..=m], &mut next[..=m])
    } else {
        let mut current = vec![false; m + 1];
        let mut next = vec![false; m + 1];
        simulate(steps, text, &mut current, &mut next)
    }
}

fn simulate(steps: &[Step], text: &str, current: &mut [bool], next: &mut [bool]) -> bool {
    let m = steps.len();
    let (mut current, mut next) = (current, next);
    current[0] = true;
    close(steps, current);
    for c in text.chars() {
        next.fill(false);
        let mut any = false;
        for (j, step) in steps.iter().enumerate() {
            if !current[j] {
                continue;
            }
            let target = match *step {
                Step::Char(p) if p == c => j + 1,
                Step::One if c != '/' => j + 1,
                Step::Seq if c != '/' => j,
                Step::Cross => j,
                _ => continue,
            };
            next[target] = true;
            any = true;
        }
        if !any {
            return false;
        }
        close(steps, next);
        std::mem::swap(&mut current, &mut next);
    }
    current[m]
}

fn close(steps: &[Step], set: &mut [bool]) {
    for (j, step) in steps.iter().enumerate() {
        if set[j] && matches!(step, Step::Seq | Step::Cross) {
            set[j + 1] = true;
        }
    }
}
