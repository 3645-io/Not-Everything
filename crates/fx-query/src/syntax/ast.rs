//! The query tree produced by [`parse`](super::parse), plus the options and
//! diagnostics attached to it.

use std::fmt;
use std::ops::Range;

use super::functions::Filter;
use super::wildcard::{WildcardPattern, has_wildcard};

/// Global search toggles, as set in the UI or on the command line.
///
/// Every flag defaults to `false`. They seed the [`TermOptions`] of every
/// term; search modifiers inside the query can then override them per term.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct SearchOptions {
    /// Match upper and lower case exactly.
    pub match_case: bool,
    /// A term must match whole words only.
    pub whole_word: bool,
    /// Terms are tested against the full path instead of the name only.
    pub match_path: bool,
    /// Accented letters must match exactly (`é` does not match `e`).
    pub match_diacritics: bool,
    /// Terms are regular expressions.
    pub regex: bool,
}

/// Which kinds of entries a term can match.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum TermKind {
    /// Files and folders.
    #[default]
    Any,
    /// Files only (everything that is not a folder).
    FilesOnly,
    /// Folders only.
    FoldersOnly,
}

/// Per-term flags, resolved from [`SearchOptions`] and the term's modifiers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct TermOptions {
    /// Match upper and lower case exactly.
    pub match_case: bool,
    /// The term must match whole words only.
    pub whole_word: bool,
    /// The term is tested against the full path instead of the name.
    pub match_path: bool,
    /// Accented letters must match exactly.
    pub match_diacritics: bool,
    /// The term is a regular expression.
    pub regex: bool,
    /// Which kinds of entries the term can match.
    pub kind: TermKind,
    /// The term must match the whole name (or whole path), not a part of it.
    pub whole_filename: bool,
    /// The name (or path) must start with the term.
    pub start_with: bool,
    /// The name (or path) must end with the term.
    pub end_with: bool,
}

impl TermOptions {
    /// Term options seeded from the global toggles, with no modifiers applied.
    pub fn from_search(opts: &SearchOptions) -> Self {
        TermOptions {
            match_case: opts.match_case,
            whole_word: opts.whole_word,
            match_path: opts.match_path,
            match_diacritics: opts.match_diacritics,
            regex: opts.regex,
            kind: TermKind::Any,
            whole_filename: false,
            start_with: false,
            end_with: false,
        }
    }
}

impl From<&SearchOptions> for TermOptions {
    fn from(opts: &SearchOptions) -> Self {
        TermOptions::from_search(opts)
    }
}

/// How a term's text is matched.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Pattern {
    /// Plain text, tested as a substring (subject to the term's options).
    Literal(String),
    /// Text with `*`, `?` or `**`; must match the whole name, or the whole
    /// path when the term matches paths.
    Wildcard(WildcardPattern),
    /// A regular expression, kept as raw text (from `regex:` or the global
    /// regex toggle); compiled by [`crate::regex_term::RegexTerm`].
    Regex(String),
}

impl Pattern {
    /// The pattern for a term's `text` (quotes removed) under resolved
    /// `options`: the raw text as a regex when the term is a regex,
    /// otherwise a wildcard if the text has a `*` or `?` (there is no
    /// escape character, and quotes do not disable wildcards), otherwise a
    /// literal.
    pub fn for_text(text: &str, options: &TermOptions) -> Self {
        if options.regex {
            Pattern::Regex(text.to_owned())
        } else if has_wildcard(text) {
            Pattern::Wildcard(WildcardPattern::new(text))
        } else {
            Pattern::Literal(text.to_owned())
        }
    }
}

/// One search term: a word (possibly quoted) between operators.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Term {
    /// The term's text with quotes and search modifiers removed.
    pub text: String,
    /// How the text is matched.
    pub pattern: Pattern,
    /// Resolved per-term flags.
    pub options: TermOptions,
    /// Byte range of the term in the input, quotes included and any `!`
    /// or search-modifier prefix excluded.
    pub span: Range<usize>,
}

/// A node of the query tree.
///
/// `And` and `Or` are always flat (no `And` directly inside an `And`, no
/// `Or` directly inside an `Or`) and always hold at least two children.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Node {
    /// Every child must match.
    And(Vec<Node>),
    /// At least one child must match.
    Or(Vec<Node>),
    /// The child must not match.
    Not(Box<Node>),
    /// A single term.
    Term(Term),
    /// A search function such as `size:>1mb`, testing metadata rather
    /// than the name's text.
    Function(Filter),
    /// Matches every entry (the empty query).
    MatchAll,
}

/// What went wrong in a recovered part of the query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DiagnosticKind {
    /// A `"` with no closing quote; the quote runs to the end of input.
    UnclosedQuote,
    /// A `<` with no matching `>`; the group closes at the end of input.
    UnclosedGroup,
    /// A `>` with no open group; it is ignored.
    StrayClose,
    /// A `|` with nothing on one side; the empty operand is dropped.
    EmptyOrOperand,
    /// A `!` with nothing after it; it is dropped.
    DanglingNot,
    /// A `<>` group with nothing inside; it is dropped.
    EmptyGroup,
    /// Groups nested deeper than the parser allows; the `<` is ignored.
    TooDeep,
    /// A search modifier (`case:`, `ww:`, ...) with no term, phrase or
    /// group right after it; it is ignored.
    BareModifier,
    /// A search function (`size:`, `depth:`, ...) with no argument; the
    /// function is ignored.
    MissingArgument,
    /// A search function with a malformed argument (`size:abc`,
    /// `size:>`); the function is ignored.
    InvalidArgument,
}

impl DiagnosticKind {
    /// A short human-readable description.
    pub fn message(self) -> &'static str {
        match self {
            DiagnosticKind::UnclosedQuote => "unclosed quote",
            DiagnosticKind::UnclosedGroup => "unclosed '<' group",
            DiagnosticKind::StrayClose => "'>' without a matching '<'",
            DiagnosticKind::EmptyOrOperand => "'|' is missing an operand",
            DiagnosticKind::DanglingNot => "'!' is not followed by a term or group",
            DiagnosticKind::EmptyGroup => "empty '<>' group",
            DiagnosticKind::TooDeep => "groups are nested too deeply",
            DiagnosticKind::BareModifier => "search modifier has nothing to apply to",
            DiagnosticKind::MissingArgument => "search function needs an argument",
            DiagnosticKind::InvalidArgument => "search function argument is not valid",
        }
    }
}

/// A problem the parser recovered from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Diagnostic {
    /// What went wrong.
    pub kind: DiagnosticKind,
    /// Byte range of the offending input; always on char boundaries.
    pub span: Range<usize>,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} at bytes {}..{}",
            self.kind.message(),
            self.span.start,
            self.span.end
        )
    }
}

/// The result of [`parse`](super::parse): a query tree and any problems the
/// parser recovered from. Parsing never fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    /// The (possibly recovered) query tree.
    pub query: Node,
    /// Recovered problems, in the order they were found.
    pub diagnostics: Vec<Diagnostic>,
    /// The result limit set by `count:`, if any.
    pub count: Option<usize>,
}
