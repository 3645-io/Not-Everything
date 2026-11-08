//! Search queries.
//!
//! This crate holds the query parser for a search syntax compatible with
//! voidtools Everything, the name matchers, the refinement cache that reuses
//! earlier results while the user types, and query cancellation.
//!
//! - [`syntax`]: the query tree, the lexer and the lenient
//!   operator parser, plus [`WildcardPattern`] for `*`, `?` and `**`
//!   terms and [`Modifier`] for search modifiers such as `case:` and
//!   `folder:`. [`parse`] never fails; problems in partial input come back
//!   as [`Diagnostic`]s next to a recovered tree. Search functions
//!   (`size:`, `dm:`, `ext:`, ...) are parsed into a [`Filter`].
//! - [`eval`]: a naive reference evaluator, the test oracle for
//!   the fast matchers.
//! - [`fold`]: Unicode case and diacritic folding with NFC/NFD
//!   normalization ([`fold_str`]) and the apostrophe rule: a term
//!   without an apostrophe ignores the names' apostrophes, a term with one
//!   needs one in that place. [`FoldedColumn`] is the folded search copy of
//!   a name column, with a side table for the names with an apostrophe.
//! - [`regex_term`]: regex terms compiled with the `regex` crate
//!   ([`RegexTerm`]): unanchored, case-insensitive unless matching case,
//!   tested against the name or the full path. The diacritics option does
//!   not apply to them.
//! - [`highlight`]: [`Highlighter`], the ranges of a result's name
//!   and folder to highlight for a query, in UTF-16 code units of the
//!   original text, mapped back through case, diacritic and apostrophe
//!   folding.
//! - [`matcher`]: [`CompiledQuery`], the query tree lowered into
//!   a parallel matcher over a columnar [`SearchView`] (memmem literals,
//!   `*.ext` as an extension compare, lazily built full paths, functions on
//!   metadata, cancellation), agreeing exactly with [`eval`]. [`VecView`]
//!   is a simple owned view for tests and benchmarks. Path terms reuse
//!   each folder's result or cached path instead of building every full
//!   path (the private `paths` module).
//! - [`refine`]: [`Searcher`], the refinement cache for
//!   search-as-you-type: an LRU of recent result sets (backspace is
//!   instant) and `search_within` over earlier hits when the query
//!   provably narrows, with view-epoch invalidation.
//! - [`options`]: [`QueryOptions`], the search field's chips
//!   (match case, match path, and the whole input as one regular
//!   expression), turned into the query text and tree to search with.
//! - [`substring`]: a parallel substring scan over one contiguous
//!   NUL-separated name buffer, with the generation-based cancellation
//!   every search uses for search-as-you-type.

pub mod eval;
pub mod fold;
pub mod highlight;
pub mod matcher;
pub mod options;
mod paths;
pub mod refine;
pub mod regex_term;
pub mod substring;
pub mod syntax;

pub use fold::{FoldOptions, FoldedColumn, fold_str};
pub use highlight::{Highlighter, Highlights};
pub use matcher::{CompileError, CompiledQuery, SearchView, VecView};
pub use options::{QueryOptions, whole_regex_text};
pub use refine::{SearchStats, Searcher};
pub use regex_term::{RegexError, RegexTerm};
pub use substring::{
    CHECK_INTERVAL, CancelToken, Cancelled, CaseMode, ColumnError, DEFAULT_CHUNK_ENTRIES,
    Generation, NameColumn, SubstringQuery, count, fold_ascii_lowercase, search, search_chunked,
};
pub use syntax::{
    Diagnostic, DiagnosticKind, Filter, Modifier, Node, Parsed, Pattern, SearchOptions, Term,
    TermKind, TermOptions, WildcardPattern, parse,
};
