//! Match highlighting: which parts of a result's name and folder
//! to draw highlighted.
//!
//! A [`Highlighter`] prepares a parsed query once (folded needles and their
//! finders, folded wildcard patterns, compiled regexes); then
//! [`Highlighter::highlight`] takes one result row (the full path of the
//! folder holding the entry, its name, and whether it is a folder) and
//! returns [`Highlights`]: ranges in the *original* name and folder text.
//!
//! # Offsets
//!
//! Ranges are in **UTF-16 code units** of the strings passed in, which is
//! what `NSString`, `NSRange` and `NSAttributedString` count, so the UI
//! can apply them as they are. They are sorted, and overlapping or
//! touching ranges are merged into one. [`replace_prefix`] moves them
//! onto a display string whose start was replaced, such as a home folder
//! shown as `~`.
//!
//! # What is highlighted
//!
//! - **Literal terms**: every occurrence, overlapping ones included, of
//!   every term that is not under a `!` (in every branch of an `|`), found
//!   as the matcher finds it: case, diacritic and apostrophe folding as in
//!   [`crate::fold`] (so a term without an apostrophe spans one in the
//!   name), mapped back through the folding to the original text. A folded
//!   match therefore covers a decomposed letter's marks, a letter that
//!   folding changed in length (`İ` to `i`) and a dropped apostrophe.
//!   `ww:`, `start-with:`, `end-with:` and `wfn:` keep only the occurrences
//!   they accept.
//! - **Regex terms**: each match of the expression (leftmost-first,
//!   non-overlapping, empty matches skipped), on the text in NFC as the
//!   matcher tests it, mapped back the same way. An expression that does
//!   not compile is skipped.
//! - **Wildcard terms**: where the pattern matches the whole name (or
//!   path), its literal pieces: the first one at the start unless the
//!   pattern starts with `*`, the last one at the end unless it ends with
//!   `*`, the others at their first place after the piece before. Patterns
//!   with `?` are skipped: which characters it stood for is not mapped.
//! - **Functions** (`ext:`, `size:`, ...) and terms under `!` highlight
//!   nothing. `file:` and `folder:` terms highlight only entries of their
//!   kind.
//! - **Path terms** (`path:`, a `/` in the term, or the Path option) are
//!   found in the full path, the folder joined with the name as
//!   [`Candidate::full_path`](crate::eval::Candidate::full_path) does, and
//!   split between the folder and the name; a separator the join adds is
//!   never part of either.
//!
//! The highlighter finds occurrences in the strings it is given: rows
//! whose folder is shown differently (an absolute path for an index whose
//! root is not `/`) get the occurrences in that text.
//!
//! # Mapping folded text back
//!
//! ASCII text maps byte for byte (folding only lowercases and drops `'`).
//! Other text is cut into segments, a character with the combining marks
//! after it, folded one by one; if the folded segments put together equal
//! the folded whole (true unless characters compose across segments, as
//! Hangul jamo do), each folded byte maps to its segment. Otherwise the cut
//! is coarser, before ASCII characters only, and failing that the whole
//! text is one segment.

use std::ops::Range;

use memchr::memmem::Finder;
use unicode_normalization::char::canonical_combining_class;

use crate::eval::{fold_pattern, literal_text};
use crate::fold::{FoldOptions, fold_str};
use crate::options::QueryOptions;
use crate::paths::join;
use crate::regex_term::RegexTerm;
use crate::syntax::wildcard::Piece;
use crate::syntax::{Node, Parsed, Pattern, Term, TermKind, TermOptions, WildcardPattern};

/// The ranges to highlight in one result row, in UTF-16 code units of the
/// strings passed to [`Highlighter::highlight`], sorted and merged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Highlights {
    /// Ranges in the entry's name.
    pub name: Vec<Range<usize>>,
    /// Ranges in the full path of the folder holding the entry.
    pub folder: Vec<Range<usize>>,
}

impl Highlights {
    /// Whether nothing is highlighted.
    pub fn is_empty(&self) -> bool {
        self.name.is_empty() && self.folder.is_empty()
    }
}

/// A query prepared for highlighting (see the [module docs](self)).
#[derive(Debug)]
pub struct Highlighter {
    terms: Vec<HlTerm>,
    /// Whether any term reads the full path.
    any_path: bool,
}

#[derive(Debug)]
struct HlTerm {
    kind: TermKind,
    path: bool,
    how: How,
}

#[derive(Debug)]
enum How {
    Literal {
        fold: FoldOptions,
        opts: TermOptions,
        needle: String,
        finder: Finder<'static>,
    },
    Wildcard {
        fold: FoldOptions,
        pattern: WildcardPattern,
    },
    Regex(Box<RegexTerm>),
}

impl How {
    /// The folding of the text the term is found in.
    fn fold(&self) -> FoldOptions {
        match self {
            How::Literal { fold, .. } | How::Wildcard { fold, .. } => *fold,
            // Regexes see the text in NFC only.
            How::Regex(_) => FoldOptions::default(),
        }
    }
}

impl Highlighter {
    /// Prepares the positive terms of `parsed` (see the
    /// [module docs](self) for which terms highlight what).
    pub fn new(parsed: &Parsed) -> Highlighter {
        let mut terms = Vec::new();
        collect(&parsed.query, &mut terms);
        let any_path = terms.iter().any(|t| t.path);
        Highlighter { terms, any_path }
    }

    /// Prepares the search field's `input` under its `options`, parsed as
    /// the engine searches it ([`QueryOptions::parse`]): with
    /// `options.regex`, the whole input is one expression.
    pub fn for_query(input: &str, options: QueryOptions) -> Highlighter {
        Highlighter::new(&options.parse(input))
    }

    /// Whether no term can highlight anything (an empty query, or only
    /// functions, negated terms and skipped patterns).
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// The ranges to highlight for one row: `folder` is the full path of
    /// the folder holding the entry (`""` for a root, whose name is its
    /// path), `name` the entry's name and `is_folder` its kind (for `file:`
    /// and `folder:` terms). Ranges are in UTF-16 code units of `folder`
    /// and `name`.
    pub fn highlight(&self, folder: &str, name: &str, is_folder: bool) -> Highlights {
        if self.terms.is_empty() {
            return Highlights::default();
        }
        let full = self.any_path.then(|| join(folder, name));
        let mut maps: Vec<(bool, FoldOptions, Folded)> = Vec::new();
        let mut in_name = Vec::new();
        let mut in_path = Vec::new();
        for t in &self.terms {
            if !kind_ok(t.kind, is_folder) {
                continue;
            }
            let fold = t.how.fold();
            let k = match maps.iter().position(|(p, f, _)| *p == t.path && *f == fold) {
                Some(k) => k,
                None => {
                    let text = match &full {
                        Some(full) if t.path => full.as_str(),
                        _ => name,
                    };
                    maps.push((t.path, fold, Folded::new(text, fold)));
                    maps.len() - 1
                }
            };
            let out = if t.path { &mut in_path } else { &mut in_name };
            find(&t.how, &maps[k].2, out);
        }
        let mut folder_bytes = Vec::new();
        if let Some(full) = &full {
            // The name ends the full path; the folder starts it.
            let name_start = full.len() - name.len();
            for r in in_path {
                if r.start < folder.len() {
                    folder_bytes.push(r.start..r.end.min(folder.len()));
                }
                if r.end > name_start {
                    in_name.push(r.start.max(name_start) - name_start..r.end - name_start);
                }
            }
        }
        Highlights {
            name: to_utf16(name, merge(in_name)),
            folder: to_utf16(folder, merge(folder_bytes)),
        }
    }
}

/// Moves `ranges` (UTF-16 code units, as [`Highlights`] gives them) onto a
/// text whose first `prefix` units were replaced by `replacement` units,
/// such as `/Users/me/Docs` shown as `~/Docs` (`prefix` 9, `replacement`
/// 1): a range inside the prefix highlights the whole replacement, one
/// after it moves by the difference, one across it covers the replacement
/// and its own part after the prefix. The result is sorted and merged.
pub fn replace_prefix(
    ranges: &[Range<usize>],
    prefix: usize,
    replacement: usize,
) -> Vec<Range<usize>> {
    let moved = |at: usize| at - prefix + replacement;
    merge(
        ranges
            .iter()
            .filter(|r| !r.is_empty())
            .map(|r| {
                if r.start >= prefix {
                    moved(r.start)..moved(r.end)
                } else if r.end <= prefix {
                    0..replacement
                } else {
                    0..moved(r.end)
                }
            })
            .collect(),
    )
}

/// The highlightable terms of `node` that are not under a `!`.
fn collect(node: &Node, out: &mut Vec<HlTerm>) {
    match node {
        Node::And(children) | Node::Or(children) => {
            for child in children {
                collect(child, out);
            }
        }
        Node::Term(term) => out.extend(prepare(term)),
        Node::Not(_) | Node::Function(_) | Node::MatchAll => {}
    }
}

/// One term prepared as the matcher prepares it, or `None` when it cannot
/// highlight anything.
fn prepare(term: &Term) -> Option<HlTerm> {
    let opts = term.options;
    match &term.pattern {
        Pattern::Literal(text) => {
            let fold = FoldOptions::for_term(text, opts.match_case, opts.match_diacritics);
            let needle = fold_str(text, fold).into_owned();
            if needle.is_empty() {
                return None;
            }
            let finder = Finder::new(needle.as_bytes()).into_owned();
            Some(HlTerm {
                kind: opts.kind,
                path: opts.match_path || text.contains('/'),
                how: How::Literal {
                    fold,
                    opts,
                    needle,
                    finder,
                },
            })
        }
        Pattern::Wildcard(pattern) => {
            if pattern.pieces().iter().any(|p| matches!(p, Piece::AnyOne)) {
                return None;
            }
            let fold = FoldOptions::for_term(
                &literal_text(pattern),
                opts.match_case,
                opts.match_diacritics,
            );
            let pattern = fold_pattern(pattern, fold);
            if !pattern
                .pieces()
                .iter()
                .any(|p| matches!(p, Piece::Literal(_)))
            {
                return None;
            }
            Some(HlTerm {
                kind: opts.kind,
                path: opts.match_path || term.text.contains('/'),
                how: How::Wildcard { fold, pattern },
            })
        }
        Pattern::Regex(pattern) => {
            let regex = RegexTerm::compile(pattern, &opts).ok()?;
            Some(HlTerm {
                kind: regex.options().kind,
                path: regex.matches_path(),
                how: How::Regex(Box::new(regex)),
            })
        }
    }
}

fn kind_ok(kind: TermKind, is_folder: bool) -> bool {
    match kind {
        TermKind::Any => true,
        TermKind::FilesOnly => !is_folder,
        TermKind::FoldersOnly => is_folder,
    }
}

/// Appends to `out` the original byte ranges where `how` matches the
/// folded text `folded`.
fn find(how: &How, folded: &Folded, out: &mut Vec<Range<usize>>) {
    let hay = folded.text.as_str();
    match how {
        How::Literal {
            opts,
            needle,
            finder,
            ..
        } => {
            let bytes = hay.as_bytes();
            let mut pos = 0;
            while pos <= bytes.len() {
                let Some(i) = finder.find(&bytes[pos..]) else {
                    break;
                };
                let at = pos + i;
                if accepts(hay, needle, at, opts) {
                    out.push(folded.original(at..at + needle.len()));
                }
                // Overlapping occurrences too; a valid needle only starts at
                // a char boundary, so stepping one byte is safe.
                pos = at + 1;
            }
        }
        How::Regex(term) => {
            for m in term.as_regex().find_iter(hay) {
                if !m.is_empty() {
                    out.push(folded.original(m.range()));
                }
            }
        }
        How::Wildcard { pattern, .. } => {
            if !pattern.matches(hay, true) {
                return;
            }
            let pieces = pattern.pieces();
            let last = pieces.len() - 1;
            let mut pos = 0;
            for (k, piece) in pieces.iter().enumerate() {
                let Piece::Literal(lit) = piece else {
                    continue;
                };
                let at = if k == 0 {
                    0
                } else if k == last {
                    hay.len() - lit.len()
                } else {
                    match hay[pos..].find(lit.as_str()) {
                        Some(i) => pos + i,
                        None => break,
                    }
                };
                if !lit.is_empty() {
                    out.push(folded.original(at..at + lit.len()));
                }
                pos = at + lit.len();
            }
        }
    }
}

/// Whether an occurrence of `needle` at byte `at` of `hay` satisfies the
/// term's whole-word, start, end and whole-name constraints, as the
/// matcher decides it.
fn accepts(hay: &str, needle: &str, at: usize, opts: &TermOptions) -> bool {
    let end = at + needle.len();
    if (opts.start_with || opts.whole_filename) && at != 0 {
        return false;
    }
    if (opts.end_with || opts.whole_filename) && end != hay.len() {
        return false;
    }
    if opts.whole_word {
        let word = |c: char| c.is_alphanumeric();
        let first_word = needle.chars().next().is_some_and(word);
        let last_word = needle.chars().next_back().is_some_and(word);
        if first_word && hay[..at].chars().next_back().is_some_and(word) {
            return false;
        }
        if last_word && hay[end..].chars().next().is_some_and(word) {
            return false;
        }
    }
    true
}

/// Text folded with a map back to the original: for each folded byte, the
/// original byte range of the segment it came from.
#[derive(Debug, Default)]
struct Folded {
    text: String,
    start: Vec<u32>,
    end: Vec<u32>,
}

impl Folded {
    /// Folds `s` under `fold`, keeping the map (see the
    /// [module docs](self#mapping-folded-text-back)).
    fn new(s: &str, fold: FoldOptions) -> Folded {
        let mut f = Folded::default();
        if s.is_ascii() {
            for (i, &b) in s.as_bytes().iter().enumerate() {
                f.push_ascii(b, i, fold);
            }
            return f;
        }
        let whole = fold_str(s, fold);
        f.segments(s, fold, |c| canonical_combining_class(c) == 0);
        if f.text == whole {
            return f;
        }
        f = Folded::default();
        f.segments(s, fold, |c| c.is_ascii());
        if f.text == whole {
            return f;
        }
        f = Folded::default();
        f.push(&whole, 0, s.len());
        f
    }

    /// Folds `s` in segments, one starting at each character `cut`
    /// accepts (and one at the start).
    fn segments(&mut self, s: &str, fold: FoldOptions, cut: impl Fn(char) -> bool) {
        let mut start = 0;
        for (j, c) in s.char_indices().skip(1) {
            if cut(c) {
                self.segment(&s[start..j], start, fold);
                start = j;
            }
        }
        self.segment(&s[start..], start, fold);
    }

    /// Folds one segment starting at original byte `start`.
    fn segment(&mut self, segment: &str, start: usize, fold: FoldOptions) {
        match segment.as_bytes() {
            [] => {}
            &[b] => self.push_ascii(b, start, fold),
            _ => self.push(&fold_str(segment, fold), start, start + segment.len()),
        }
    }

    /// Appends one ASCII byte folded as [`fold_str`] folds it.
    fn push_ascii(&mut self, b: u8, at: usize, fold: FoldOptions) {
        if b == b'\'' && fold.diacritics && !fold.keep_apostrophes {
            return;
        }
        let b = if fold.case { b.to_ascii_lowercase() } else { b };
        self.text.push(char::from(b));
        self.start.push(at as u32);
        self.end.push(at as u32 + 1);
    }

    /// Appends folded text that came from the original bytes
    /// `start..end`.
    fn push(&mut self, folded: &str, start: usize, end: usize) {
        self.text.push_str(folded);
        for _ in 0..folded.len() {
            self.start.push(start as u32);
            self.end.push(end as u32);
        }
    }

    /// The original byte range of the non-empty folded range `r`.
    fn original(&self, r: Range<usize>) -> Range<usize> {
        self.start[r.start] as usize..self.end[r.end - 1] as usize
    }
}

/// Sorts `ranges` and merges the ones that overlap or touch.
fn merge(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.retain(|r| !r.is_empty());
    ranges.sort_by_key(|r| (r.start, r.end));
    let mut out: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for r in ranges {
        match out.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => out.push(r),
        }
    }
    out
}

/// Converts sorted, disjoint byte ranges of `s` to UTF-16 code units.
fn to_utf16(s: &str, ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    if s.is_ascii() || ranges.is_empty() {
        return ranges;
    }
    let mut chars = s.chars();
    let (mut byte, mut unit) = (0usize, 0usize);
    let mut at = |target: usize| {
        while byte < target {
            let Some(c) = chars.next() else {
                break;
            };
            byte += c.len_utf8();
            unit += c.len_utf16();
        }
        unit
    };
    ranges
        .into_iter()
        .map(|r| {
            let start = at(r.start);
            start..at(r.end)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folded_maps_point_at_their_segments() {
        let both = FoldOptions::for_match(false, false);
        let f = Folded::new("Re\u{301}s", both);
        assert_eq!(f.text, "res");
        assert_eq!(f.original(0..1), 0..1);
        assert_eq!(f.original(1..2), 1..4);
        assert_eq!(f.original(0..3), 0..5);
        let f = Folded::new("it's", both.without_apostrophes());
        assert_eq!(f.text, "its");
        assert_eq!(f.original(0..3), 0..4);
        // Jamo compose across segments: the coarser cut is used.
        let f = Folded::new("\u{1112}\u{1161}x", both);
        assert_eq!(f.text, "\u{d558}x");
        assert_eq!(f.original(0..3), 0..6);
        assert_eq!(f.original(3..4), 6..7);
    }

    #[test]
    fn merge_and_utf16() {
        assert_eq!(merge(vec![3..5, 0..1, 1..2, 4..9, 7..7]), vec![0..2, 3..9]);
        assert_eq!(to_utf16("é📄x", vec![0..2, 6..7]), vec![0..1, 3..4]);
    }
}
