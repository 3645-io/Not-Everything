//! The reference evaluator: a naive, obviously correct oracle for query
//! trees, used by the conformance tests. It is not the fast path; the
//! optimised matchers ([`crate::matcher`]) are checked against this.
//!
//! How a literal term is tested against a [`Candidate`]:
//!
//! - The haystack is the name, or the full path (`parent_path + '/' + name`)
//!   when the term has `match_path` set or its text contains `/`. A `/` in a
//!   term is the macOS stand-in for Everything's rule that a term with a path
//!   separator matches against the path.
//! - Both sides are folded with [`fold_str`] before comparing: always
//!   normalized, so an NFD name (`e` + U+0301, as APFS may store it) and an
//!   NFC query (`é`) compare equal; without `match_case`, lowercased one
//!   `char` at a time with std's Unicode lowercase mapping (no expansion, so
//!   `ß` stays `ß`, and no context-dependent rules such as the Greek final
//!   sigma, so `Σ` always becomes `σ`); without `match_diacritics` (the
//!   default), accents stripped, so `resume` matches `Résumé.pdf` while
//!   `diacritics:resume` does not. With accents stripped, the apostrophes
//!   `'`, `’`, `‘` and `ʼ` follow the apostrophe rule
//!   ([`FoldOptions::for_term`]): a term without an apostrophe is compared
//!   with the name's apostrophes dropped, so `plumbs` matches
//!   `Plumb’s.pdf`; a term with one keeps them, every kind written as `'`,
//!   so `plumb's` matches `Plumb’s.pdf` but not `plumbs.pdf`, and `'`
//!   finds names containing any apostrophe. See [`crate::fold`].
//! - The term must occur as a substring, narrowed by its modifiers:
//!   `whole_word` (`ww:`) needs a word boundary, meaning a character that
//!   is not a letter or digit, or the edge of the string, on each side of
//!   the match where the term itself starts or ends with a letter or digit;
//!   `start_with` and `end_with` anchor the match to the start or end;
//!   `whole_filename` (`wfn:`) needs the text to equal the whole name (or
//!   path).
//! - `kind` (`file:`, `folder:`) limits the term to files or folders.
//!
//! A wildcard term picks its haystack the same way, then must match all of
//! it through [`WildcardPattern::matches`] (`*` and `?` stop at `/`, `**`
//! crosses it), with the same folding applied to the haystack and to the
//! pattern's literal pieces. Since names are normalized to NFC, `?` matches
//! one accented character even when the name stores it decomposed.
//!
//! Search functions keep their own comparisons: `ext:` and `parent:`
//! lowercase per `char` without normalizing or stripping accents, and
//! `len:` counts the `char`s of the name as stored.
//!
//! Search functions ([`Node::Function`]) test the candidate's metadata
//! through [`filter_matches`]. Date values such as `today` depend on the
//! clock and time zone in an [`EvalContext`]: [`matches_in`] takes one,
//! and [`matches()`] uses the fixed [`EvalContext::default`] so results never
//! depend on the machine. Metadata the candidate lacks (`None`) never makes
//! a function match, and an unknown size matches only `size:unknown`.
//! Folders have no size, so `size:` tests other than `size:unknown` never
//! match a folder even when it carries a size. `attrib:` letters without a
//! macOS meaning ([`Attrib::Other`]) never match.
//!
//! A regex term ([`Pattern::Regex`]) is compiled with
//! [`RegexTerm::compile`] and tested, unanchored, against the name, or the
//! full path when the term matches paths (a `/` in the expression does not
//! switch to the path). It is case-insensitive unless the term matches
//! case; the diacritics option does not apply. An invalid expression
//! matches nothing. See [`crate::regex_term`].
//!
//! Every query feature is supported now, so the evaluator never returns
//! [`Unsupported`]; the error type stays for API stability. `And` and `Or`
//! evaluate every child, so an unsupported term would be reported whatever
//! the other children return.

use std::error::Error;
use std::fmt;

use crate::fold::{FoldOptions, fold_str};
use crate::regex_term::RegexTerm;
use crate::syntax::functions::{Attrib, Cmp, DateField, DateValue, Filter, SizeTest, ValueTest};
use crate::syntax::wildcard::Piece;
use crate::syntax::{Node, Pattern, Term, TermKind, TermOptions, WildcardPattern};

/// One file-system entry to test a query against.
///
/// Only `name`, `parent_path` and `is_dir` are needed for name terms; the
/// metadata fields feed search functions and default to "unknown"
/// (`None`) or "not set" (`false`):
///
/// ```
/// use fx_query::eval::Candidate;
///
/// let c = Candidate {
///     name: "a.txt",
///     parent_path: "/Users/me",
///     is_dir: false,
///     ..Default::default()
/// };
/// assert_eq!(c.size, None);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Candidate<'a> {
    /// The entry's name (last path component).
    pub name: &'a str,
    /// The full path of the folder holding the entry, without a trailing
    /// `/` (except for the root, `/`).
    pub parent_path: &'a str,
    /// Whether the entry is a folder.
    pub is_dir: bool,
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

impl<'a> Candidate<'a> {
    /// A candidate with no metadata.
    pub fn new(name: &'a str, parent_path: &'a str, is_dir: bool) -> Self {
        Candidate {
            name,
            parent_path,
            is_dir,
            ..Default::default()
        }
    }

    /// A file with no metadata.
    pub fn file(name: &'a str, parent_path: &'a str) -> Self {
        Candidate::new(name, parent_path, false)
    }

    /// A folder with no metadata.
    pub fn folder(name: &'a str, parent_path: &'a str) -> Self {
        Candidate::new(name, parent_path, true)
    }
}

/// The clock and time zone that date functions are evaluated against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EvalContext {
    /// The current time, in Unix seconds.
    pub now: i64,
    /// The local time zone's offset east of UTC, in seconds (UTC+1 is
    /// 3600). Applied to the whole query, so daylight-saving changes are
    /// not modelled.
    pub utc_offset_secs: i32,
}

impl EvalContext {
    /// The fixed clock of [`EvalContext::default`]: 2026-11-03T10:00:00Z.
    pub const DEFAULT_NOW: i64 = 1_793_700_000;
}

impl Default for EvalContext {
    /// A fixed context, so results never depend on the machine's clock:
    /// `now` is [`EvalContext::DEFAULT_NOW`] and the time zone is UTC.
    fn default() -> Self {
        EvalContext {
            now: Self::DEFAULT_NOW,
            utc_offset_secs: 0,
        }
    }
}

impl Candidate<'_> {
    /// The full path: `parent_path`, a `/` unless the parent already ends
    /// with one (or is empty), then `name`.
    pub fn full_path(&self) -> String {
        let mut path = String::with_capacity(self.parent_path.len() + 1 + self.name.len());
        path.push_str(self.parent_path);
        if !self.parent_path.is_empty() && !self.parent_path.ends_with('/') {
            path.push('/');
        }
        path.push_str(self.name);
        path
    }
}

/// A query feature the reference evaluator does not implement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unsupported {
    /// Short name of the feature, such as `"regex"`.
    pub feature: &'static str,
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the reference evaluator does not support {}",
            self.feature
        )
    }
}

impl Error for Unsupported {}

/// Tests whether `candidate` matches the query tree `node`, with date
/// functions evaluated against the fixed [`EvalContext::default`].
pub fn matches(node: &Node, candidate: &Candidate<'_>) -> Result<bool, Unsupported> {
    matches_in(node, candidate, &EvalContext::default())
}

/// Tests whether `candidate` matches the query tree `node`, with date
/// functions evaluated against `ctx`.
pub fn matches_in(
    node: &Node,
    candidate: &Candidate<'_>,
    ctx: &EvalContext,
) -> Result<bool, Unsupported> {
    match node {
        Node::MatchAll => Ok(true),
        Node::Not(inner) => Ok(!matches_in(inner, candidate, ctx)?),
        Node::And(children) => {
            let mut all = true;
            for child in children {
                all &= matches_in(child, candidate, ctx)?;
            }
            Ok(all)
        }
        Node::Or(children) => {
            let mut any = false;
            for child in children {
                any |= matches_in(child, candidate, ctx)?;
            }
            Ok(any)
        }
        Node::Term(term) => term_matches(term, candidate),
        Node::Function(filter) => filter_matches(filter, candidate, ctx),
    }
}

/// Tests a single search function. Metadata the candidate does not carry
/// never matches (see [`crate::syntax::functions`]).
pub fn filter_matches(
    filter: &Filter,
    candidate: &Candidate<'_>,
    ctx: &EvalContext,
) -> Result<bool, Unsupported> {
    Ok(match filter {
        Filter::Ext(exts) => {
            let ext = lowercase(extension(candidate.name));
            exts.iter().any(|e| lowercase(e) == ext)
        }
        Filter::Size(SizeTest::Unknown) => candidate.size.is_none(),
        // Folders have no size: only `size:unknown` can match them.
        Filter::Size(test) => {
            !candidate.is_dir && candidate.size.is_some_and(|s| test.matches_bytes(s))
        }
        Filter::Date(field, test) => {
            let stamp = match field {
                DateField::Modified => candidate.modified,
                DateField::Created => candidate.created,
                DateField::Accessed => candidate.accessed,
            };
            match stamp {
                Some(t) => date_matches(test, t, ctx),
                None => false,
            }
        }
        Filter::Parent(path) => {
            lowercase(trim_trailing_slash(candidate.parent_path))
                == lowercase(trim_trailing_slash(path))
        }
        Filter::Depth(test) => {
            let depth = candidate
                .full_path()
                .split('/')
                .filter(|part| !part.is_empty())
                .count();
            u32::try_from(depth).is_ok_and(|d| test.matches_point(d))
        }
        Filter::Len(test) => {
            u32::try_from(candidate.name.chars().count()).is_ok_and(|n| test.matches_point(n))
        }
        Filter::Empty => candidate.is_dir && candidate.child_count == Some(0),
        Filter::Attrib(attrs) => attrs.iter().all(|a| match a {
            Attrib::Hidden => candidate.hidden,
            Attrib::ReadOnly => candidate.read_only,
            Attrib::Symlink => candidate.symlink,
            Attrib::Directory => candidate.is_dir,
            // Letters without a macOS meaning never match.
            Attrib::Other(_) => false,
        }),
    })
}

/// Tests a timestamp against a date comparison. The value is a period
/// `[start, end)`: `=` means inside it, `==` at its first second, `<`
/// before it, `<=` before its end, `>` from its end on, `>=` from its
/// start on, and `a..b` from the start of `a` to the end of `b`. A value
/// that does not resolve (an invalid date) matches nothing.
fn date_matches(test: &ValueTest<DateValue>, t: i64, ctx: &EvalContext) -> bool {
    let span = |v: &DateValue| v.span(ctx.now, ctx.utc_offset_secs);
    match test {
        ValueTest::Cmp(cmp, value) => {
            let Some((start, end)) = span(value) else {
                return false;
            };
            match cmp {
                Cmp::Eq => start <= t && t < end,
                Cmp::ExactEq => t == start,
                Cmp::Ne => !(start <= t && t < end),
                Cmp::Lt => t < start,
                Cmp::Le => t < end,
                Cmp::Gt => t >= end,
                Cmp::Ge => t >= start,
            }
        }
        ValueTest::Range(range) => match (span(&range.start), span(&range.end)) {
            (Some((start, _)), Some((_, end))) => start <= t && t < end,
            _ => false,
        },
    }
}

/// The text after the last `.` of a name, or `""` when there is none or
/// the only `.` starts the name (`.bashrc`).
fn extension(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 => &name[i + 1..],
        _ => "",
    }
}

fn trim_trailing_slash(path: &str) -> &str {
    match path.strip_suffix('/') {
        Some(rest) if !rest.is_empty() => rest,
        _ => path,
    }
}

/// Tests a single term.
pub fn term_matches(term: &Term, candidate: &Candidate<'_>) -> Result<bool, Unsupported> {
    let opts = &term.options;
    let kind_ok = kind_matches(opts.kind, candidate);
    match &term.pattern {
        Pattern::Literal(text) => {
            // Evaluate the text even when the kind fails, so the result
            // does not depend on evaluation order.
            let text_ok = literal_matches(text, opts, candidate);
            Ok(kind_ok && text_ok)
        }
        Pattern::Wildcard(pattern) => {
            let text_ok = wildcard_matches(pattern, &term.text, opts, candidate);
            Ok(kind_ok && text_ok)
        }
        Pattern::Regex(pattern) => Ok(regex_matches(pattern, opts, candidate)),
    }
}

fn kind_matches(kind: TermKind, candidate: &Candidate<'_>) -> bool {
    match kind {
        TermKind::Any => true,
        TermKind::FilesOnly => !candidate.is_dir,
        TermKind::FoldersOnly => candidate.is_dir,
    }
}

/// A regex term, compiled afresh for every candidate (this is the oracle,
/// not the fast path). Modifiers peeled off the front of the pattern by
/// [`RegexTerm::compile`] decide the haystack and the kind; an invalid
/// pattern matches nothing.
fn regex_matches(pattern: &str, opts: &TermOptions, candidate: &Candidate<'_>) -> bool {
    let Ok(term) = RegexTerm::compile(pattern, opts) else {
        return false;
    };
    let text_ok = if term.matches_path() {
        term.is_match(&candidate.full_path())
    } else {
        term.is_match(candidate.name)
    };
    kind_matches(term.options().kind, candidate) && text_ok
}

/// A wildcard term must match the whole haystack (name, or full path under
/// the same rule as literals), so `whole_word`, `start_with`, `end_with`
/// and `whole_filename` add nothing to it.
fn wildcard_matches(
    pattern: &WildcardPattern,
    text: &str,
    opts: &TermOptions,
    candidate: &Candidate<'_>,
) -> bool {
    let fold = term_fold(&literal_text(pattern), opts);
    let folded = fold_pattern(pattern, fold);
    if opts.match_path || text.contains('/') {
        folded.matches(&fold_str(&candidate.full_path(), fold), true)
    } else {
        folded.matches(&fold_str(candidate.name, fold), true)
    }
}

fn literal_matches(text: &str, opts: &TermOptions, candidate: &Candidate<'_>) -> bool {
    let fold = term_fold(text, opts);
    let needle = fold_str(text, fold);
    if opts.match_path || text.contains('/') {
        find_constrained(&fold_str(&candidate.full_path(), fold), &needle, opts)
    } else {
        find_constrained(&fold_str(candidate.name, fold), &needle, opts)
    }
}

/// The folding a term with literal text `text` asks for: its options, with
/// apostrophes kept when the text has one and dropped when it has none
/// (the apostrophe rule).
fn term_fold(text: &str, opts: &TermOptions) -> FoldOptions {
    FoldOptions::for_term(text, opts.match_case, opts.match_diacritics)
}

/// The literal pieces of a wildcard pattern, joined.
pub(crate) fn literal_text(pattern: &WildcardPattern) -> String {
    pattern
        .pieces()
        .iter()
        .filter_map(|piece| match piece {
            Piece::Literal(lit) => Some(lit.as_str()),
            _ => None,
        })
        .collect()
}

/// Folds the literal pieces of a wildcard pattern, keeping its wildcards.
///
/// The pattern is rebuilt from its pieces rather than from the term text so
/// the result does not depend on how the text was quoted. Folding never
/// introduces `*` or `?`. A literal that folds to nothing (a lone combining
/// mark) is dropped without merging the `*` on either side into a `**`.
pub(crate) fn fold_pattern(pattern: &WildcardPattern, fold: FoldOptions) -> WildcardPattern {
    let mut text = String::new();
    // Whether the text ends in a star run, and whether that run crosses `/`.
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
                    // `*` then `*` is still `*`; anything with `**` crosses.
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

/// Lowercases one `char` at a time (no context-dependent mappings).
fn lowercase(s: &str) -> String {
    s.chars().flat_map(char::to_lowercase).collect()
}

/// Letters and digits make up words; anything else (space, punctuation,
/// `_`) or the edge of the string is a word boundary.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric()
}

/// Whether `needle` occurs in `hay` at a position satisfying the
/// whole-word, start, end and whole-name constraints.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> TermOptions {
        TermOptions::default()
    }

    #[test]
    fn full_path_handles_root_and_empty_parent() {
        let c = |p| Candidate::file("a", p);
        assert_eq!(c("/").full_path(), "/a");
        assert_eq!(c("/x").full_path(), "/x/a");
        assert_eq!(c("").full_path(), "a");
    }

    #[test]
    fn filters_need_their_metadata() {
        use crate::syntax::functions::{CalendarDate, DateKeyword, Range};
        let ctx = EvalContext::default();
        let bare = Candidate::file("a.TXT", "/Users/me/");
        let full = Candidate {
            size: Some(1500),
            modified: Some(EvalContext::DEFAULT_NOW),
            child_count: Some(0),
            hidden: true,
            ..bare
        };
        let check = |f: Filter, c: &Candidate<'_>| filter_matches(&f, c, &ctx).unwrap();

        let big = Filter::Size(SizeTest::Bytes(ValueTest::Cmp(Cmp::Gt, 1024)));
        assert!(check(big.clone(), &full));
        assert!(!check(big, &bare));
        let ne = Filter::Size(SizeTest::Bytes(ValueTest::Cmp(Cmp::Ne, 1)));
        assert!(!check(ne, &bare));
        assert!(check(Filter::Size(SizeTest::Unknown), &bare));
        assert!(!check(Filter::Size(SizeTest::Unknown), &full));

        let today = Filter::Date(
            DateField::Modified,
            ValueTest::Cmp(Cmp::Eq, DateValue::Keyword(DateKeyword::Today)),
        );
        assert!(check(today.clone(), &full));
        assert!(!check(today, &bare));
        let y2026 = DateValue::Calendar(CalendarDate::year(2026));
        let y2025 = DateValue::Calendar(CalendarDate::year(2025));
        let not_2026 = Filter::Date(DateField::Modified, ValueTest::Cmp(Cmp::Ne, y2026));
        assert!(!check(not_2026.clone(), &full));
        assert!(!check(not_2026, &bare));
        let span = Filter::Date(
            DateField::Modified,
            ValueTest::Range(Range {
                start: y2025,
                end: y2026,
            }),
        );
        assert!(check(span, &full));
        let created = Filter::Date(DateField::Created, ValueTest::Cmp(Cmp::Ge, y2025));
        assert!(!check(created, &full));

        assert!(check(Filter::Ext(vec!["txt".into()]), &bare));
        assert!(!check(Filter::Ext(vec![String::new()]), &bare));
        assert!(check(
            Filter::Ext(vec![String::new()]),
            &Candidate::file(".bashrc", "/")
        ));
        assert!(check(Filter::Parent("/users/ME".into()), &bare));
        assert!(check(Filter::Depth(ValueTest::Cmp(Cmp::Eq, 3)), &bare));
        assert!(check(Filter::Len(ValueTest::Cmp(Cmp::Eq, 5)), &bare));
        assert!(!check(Filter::Empty, &full));
        assert!(check(
            Filter::Empty,
            &Candidate {
                is_dir: true,
                ..full
            }
        ));
        assert!(!check(
            Filter::Empty,
            &Candidate {
                is_dir: true,
                ..bare
            }
        ));
        assert!(check(Filter::Attrib(vec![Attrib::Hidden]), &full));
        assert!(!check(Filter::Attrib(vec![Attrib::Hidden]), &bare));
    }

    #[test]
    fn fold_pattern_keeps_wildcards_and_star_kinds() {
        let both = FoldOptions::for_match(false, false);
        let folded = |p: &str| fold_pattern(&WildcardPattern::new(p), both);
        assert_eq!(folded("RÉS*.PDF"), WildcardPattern::new("res*.pdf"));
        assert_eq!(folded("?É**x"), WildcardPattern::new("?e**x"));
        // A literal that folds away must not turn `*` `*` into `**`.
        assert_eq!(folded("a*\u{301}*b"), WildcardPattern::new("a*b"));
        assert_eq!(folded("a*\u{301}**b"), WildcardPattern::new("a**b"));
        assert_eq!(folded("a**\u{301}*b"), WildcardPattern::new("a**b"));
        let keep = FoldOptions::for_match(true, true);
        assert_eq!(
            fold_pattern(&WildcardPattern::new("A*\u{301}*B"), keep),
            WildcardPattern::new("A*\u{301}*B")
        );
    }

    #[test]
    fn constraints() {
        let ww = TermOptions {
            whole_word: true,
            ..opts()
        };
        assert!(find_constrained("my foo.txt", "foo", &ww));
        assert!(!find_constrained("myfoo.txt", "foo", &ww));
        assert!(find_constrained("myfoo foo", "foo", &ww));
        assert!(find_constrained("my_foo_bar", "foo", &ww));
        assert!(find_constrained("a.txt", ".txt", &ww));
        let sw = TermOptions {
            start_with: true,
            ..opts()
        };
        assert!(find_constrained("foobar", "foo", &sw));
        assert!(!find_constrained("barfoo", "foo", &sw));
        let ew = TermOptions {
            end_with: true,
            ..opts()
        };
        assert!(find_constrained("barfoo", "foo", &ew));
        assert!(find_constrained("foofoo", "foo", &ew));
        let wf = TermOptions {
            whole_filename: true,
            ..opts()
        };
        assert!(find_constrained("foo", "foo", &wf));
        assert!(!find_constrained("foox", "foo", &wf));
        assert!(find_constrained("", "", &wf));
    }
}
