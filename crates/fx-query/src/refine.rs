//! The refinement cache for search-as-you-type.
//!
//! A [`Searcher`] wraps [`CompiledQuery`] searches for one generation of a
//! [`SearchView`] (one index snapshot). Each call to [`Searcher::search`]
//! takes exactly one of three paths, counted in [`SearchStats`]:
//!
//! 1. **Cache hit.** The same query text with the same [`SearchOptions`] is
//!    in the LRU of recent result sets ([`DEFAULT_CAPACITY`] entries by
//!    default): its `Arc<[u32]>` comes back as is, so backspace is instant.
//! 2. **Refinement.** The new query provably narrows a cached one: every
//!    hit of the new query is a hit of the cached query. The new query
//!    then runs with [`CompiledQuery::search_within`] over the cached hits
//!    only. When several cached queries qualify, the smallest result set
//!    is used.
//! 3. **Full scan.** Anything else runs [`CompiledQuery::search`] over the
//!    whole view.
//!
//! # When a query narrows another
//!
//! The rule is deliberately conservative and works on the parsed tree, not
//! the raw text. Both queries must use the same [`SearchOptions`] and be a
//! single positive AND of plain literal terms (or the empty query): no OR,
//! no NOT, no wildcard, no regex, no function. A plain literal is a
//! substring test with none of `whole_word`, `whole_filename`, `start_with`
//! or `end_with`. Then every old term must be implied by some new term:
//!
//! - both fold the same way (same `match_case` and `match_diacritics`) and
//!   test the same haystack (both the name, or both the full path);
//! - the new term's kind filter is the old one's, or the old one has none;
//! - the old term's folded text is a substring of the new term's folded
//!   text.
//!
//! The literal matcher tests "the folded needle is a substring of the
//! folded haystack", and substring is transitive, so a name matching the
//! new term matches the old term too. This covers typing more letters
//! (`re` → `rep`), appending terms (`rep` → `rep txt`) and merging terms
//! (`re port` → `report`). Comparing folded text rather than raw text keeps
//! the rule sound for input that normalizes (`re` → `re` + U+0301 is not a
//! narrowing when diacritics are matched). Whole-word terms are excluded
//! because they do not narrow: whole-word `rep` matches `rep notes.txt`,
//! whole-word `re` does not.
//!
//! # Invalidation and cancellation
//!
//! Cached results are only valid for the view they were computed on.
//! [`Searcher::set_epoch`] clears the cache when the caller's view epoch
//! (for example the index snapshot generation) changes, and
//! [`Searcher::invalidate`] clears it unconditionally. The caller must do
//! one of the two whenever it passes a different view.
//!
//! A cancelled search returns [`Cancelled`] and leaves the cache and the
//! counters exactly as they were.
//!
//! Every query the parser produces compiles today; should
//! [`CompiledQuery::compile`] ever refuse one, the search returns an empty
//! result (the query matches nothing, as an invalid regex does), counted as
//! a full scan and not cached.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::eval::EvalContext;
use crate::fold::{FoldOptions, fold_str};
use crate::matcher::{CompiledQuery, SearchView};
use crate::substring::{CancelToken, Cancelled};
use crate::syntax::{Node, Pattern, SearchOptions, Term, TermKind, parse};

/// Recent result sets a [`Searcher::new`] keeps.
pub const DEFAULT_CAPACITY: usize = 8;

/// How the searches of a [`Searcher`] were answered. Every successful
/// search bumps exactly one counter; cancelled searches bump none.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchStats {
    /// Searches that scanned the whole view.
    pub full_scans: u64,
    /// Searches that filtered an earlier, wider result set.
    pub refined: u64,
    /// Searches answered from the LRU of recent results.
    pub cache_hits: u64,
}

/// A literal term in the form the narrowing rule compares.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PlainLiteral {
    /// The term text folded under `fold`.
    needle: String,
    fold: FoldOptions,
    /// Tested against the full path rather than the name.
    path: bool,
    kind: TermKind,
}

impl PlainLiteral {
    /// Whether every entry matching `self` also matches `older`.
    fn implies(&self, older: &PlainLiteral) -> bool {
        self.fold == older.fold
            && self.path == older.path
            && (older.kind == TermKind::Any || older.kind == self.kind)
            && self.needle.contains(older.needle.as_str())
    }
}

/// The plain literal for `term`, or `None` when the term is anything more
/// than a folded substring test.
fn plain_literal(term: &Term) -> Option<PlainLiteral> {
    let o = &term.options;
    let Pattern::Literal(text) = &term.pattern else {
        return None;
    };
    if o.regex || o.whole_word || o.whole_filename || o.start_with || o.end_with {
        return None;
    }
    // The literal matcher's folding (the apostrophe rule), so a term
    // without an apostrophe (compared with the names' apostrophes dropped)
    // never serves as the base for one with an apostrophe, or the other
    // way round.
    let fold = FoldOptions::for_term(text, o.match_case, o.match_diacritics);
    Some(PlainLiteral {
        needle: fold_str(text, fold).into_owned(),
        fold,
        // The same haystack rule as the literal matcher and the oracle.
        path: o.match_path || text.contains('/'),
        kind: o.kind,
    })
}

/// The conjuncts of a positive AND of plain literals (empty for the empty
/// query), or `None` for any other query.
fn plain_conjuncts(node: &Node) -> Option<Vec<PlainLiteral>> {
    match node {
        Node::MatchAll => Some(Vec::new()),
        Node::Term(term) => plain_literal(term).map(|lit| vec![lit]),
        Node::And(children) => children
            .iter()
            .map(|child| match child {
                Node::Term(term) => plain_literal(term),
                _ => None,
            })
            .collect(),
        Node::Or(_) | Node::Not(_) | Node::Function(_) => None,
    }
}

/// Whether a query with conjuncts `new` provably narrows one with `old`.
fn narrows(new: &[PlainLiteral], old: &[PlainLiteral]) -> bool {
    old.iter().all(|o| new.iter().any(|n| n.implies(o)))
}

/// One cached result set.
#[derive(Debug)]
struct CacheEntry {
    text: String,
    opts: SearchOptions,
    /// `Some` when the query can serve as a refinement base.
    conjuncts: Option<Vec<PlainLiteral>>,
    hits: Arc<[u32]>,
}

/// Search-as-you-type over one view generation: an LRU of recent result
/// sets plus refinement of narrowing queries (see the [module docs](self)).
#[derive(Debug)]
pub struct Searcher {
    /// Most recently used first.
    entries: VecDeque<CacheEntry>,
    capacity: usize,
    epoch: Option<u64>,
    ctx: EvalContext,
    stats: SearchStats,
}

impl Default for Searcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Searcher {
    /// A searcher keeping [`DEFAULT_CAPACITY`] recent result sets, with
    /// date functions evaluated against [`EvalContext::default`].
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    /// A searcher keeping `capacity` recent result sets (0 disables both
    /// the cache and refinement).
    pub fn with_capacity(capacity: usize) -> Self {
        Searcher {
            entries: VecDeque::with_capacity(capacity),
            capacity,
            epoch: None,
            ctx: EvalContext::default(),
            stats: SearchStats::default(),
        }
    }

    /// The clock and time zone date functions are evaluated against.
    pub fn context(&self) -> EvalContext {
        self.ctx
    }

    /// Sets the clock and time zone for date functions. Clears the cache
    /// when they change, since cached `dm:today` results may no longer
    /// hold.
    pub fn set_context(&mut self, ctx: EvalContext) {
        if ctx != self.ctx {
            self.ctx = ctx;
            self.invalidate();
        }
    }

    /// The view epoch last given to [`Searcher::set_epoch`], if any.
    pub fn epoch(&self) -> Option<u64> {
        self.epoch
    }

    /// Records the epoch of the view the next searches run on, clearing
    /// the cache when it differs from the current one.
    pub fn set_epoch(&mut self, epoch: u64) {
        if self.epoch != Some(epoch) {
            self.epoch = Some(epoch);
            self.invalidate();
        }
    }

    /// Drops every cached result set (and so every refinement base). The
    /// counters are kept.
    pub fn invalidate(&mut self) {
        self.entries.clear();
    }

    /// The counters so far.
    pub fn stats(&self) -> SearchStats {
        self.stats
    }

    /// Number of cached result sets.
    pub fn cached(&self) -> usize {
        self.entries.len()
    }

    /// The live entries of `view` matching `text` parsed under `opts`, in
    /// ascending ID order: exactly what a fresh [`CompiledQuery::search`]
    /// returns, answered from the cache or by refinement when possible.
    ///
    /// Returns [`Cancelled`] once `cancel`'s generation moves on, leaving
    /// the cache and counters untouched.
    pub fn search<V: SearchView + ?Sized>(
        &mut self,
        text: &str,
        opts: &SearchOptions,
        view: &V,
        cancel: &CancelToken<'_>,
    ) -> Result<Arc<[u32]>, Cancelled> {
        if let Some(pos) = self
            .entries
            .iter()
            .position(|e| e.text == text && e.opts == *opts)
        {
            if cancel.is_cancelled() {
                return Err(Cancelled);
            }
            let entry = self.entries.remove(pos).expect("position is in range");
            let hits = Arc::clone(&entry.hits);
            self.entries.push_front(entry);
            self.stats.cache_hits += 1;
            return Ok(hits);
        }

        let parsed = parse(text, opts);
        let Ok(query) = CompiledQuery::compile(&parsed, opts, self.ctx) else {
            if cancel.is_cancelled() {
                return Err(Cancelled);
            }
            self.stats.full_scans += 1;
            return Ok(Arc::from(Vec::new()));
        };
        let conjuncts = plain_conjuncts(&parsed.query);

        let base = conjuncts.as_deref().and_then(|new| {
            self.entries
                .iter()
                .filter(|e| e.opts == *opts)
                .filter(|e| e.conjuncts.as_deref().is_some_and(|old| narrows(new, old)))
                .min_by_key(|e| e.hits.len())
                .map(|e| Arc::clone(&e.hits))
        });

        let hits: Arc<[u32]> = match &base {
            Some(base) => query.search_within(view, base, cancel)?.into(),
            None => query.search(view, cancel)?.into(),
        };
        if base.is_some() {
            self.stats.refined += 1;
        } else {
            self.stats.full_scans += 1;
        }

        if self.capacity > 0 {
            self.entries.truncate(self.capacity - 1);
            self.entries.push_front(CacheEntry {
                text: text.to_owned(),
                opts: *opts,
                conjuncts,
                hits: Arc::clone(&hits),
            });
        }
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conj(q: &str, opts: &SearchOptions) -> Option<Vec<PlainLiteral>> {
        plain_conjuncts(&parse(q, opts).query)
    }

    fn narrows_text(new: &str, old: &str, opts: &SearchOptions) -> bool {
        match (conj(new, opts), conj(old, opts)) {
            (Some(n), Some(o)) => narrows(&n, &o),
            _ => false,
        }
    }

    #[test]
    fn narrowing_rule() {
        let d = SearchOptions::default();
        assert!(narrows_text("rep", "re", &d));
        assert!(narrows_text("rep txt", "rep", &d));
        assert!(narrows_text("report", "re port", &d));
        assert!(narrows_text("REP", "re", &d));
        assert!(narrows_text("re", "", &d));
        assert!(!narrows_text("re", "rep", &d));
        assert!(!narrows_text("re|x", "re", &d));
        assert!(!narrows_text("re !x", "re", &d));
        assert!(!narrows_text("re*", "re", &d));
        assert!(!narrows_text("repo/", "repo", &d));
        assert!(!narrows_text("folder:rep", "file:re", &d));
        assert!(narrows_text("folder:rep", "re", &d));
        assert!(!narrows_text("re", "folder:re", &d));
        let ww = SearchOptions {
            whole_word: true,
            ..d
        };
        assert!(!narrows_text("rep", "re", &ww));
        let diac = SearchOptions {
            match_diacritics: true,
            ..d
        };
        assert!(!narrows_text("re\u{301}", "re", &diac));
        assert!(narrows_text("re\u{301}", "re", &d));
    }
}
