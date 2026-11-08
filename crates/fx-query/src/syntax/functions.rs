//! Search functions: `name:value` words that filter on metadata rather than
//! on the name's text, such as `size:>1mb`, `dm:today` or `ext:txt;md`.
//!
//! This module holds the parsed form, [`Filter`], and the comparison types
//! it is built from: [`Cmp`] for the operator after the `:` and [`Range`]
//! for `a..b`. The parser turns a word into a [`Node::Function`] holding a
//! `Filter`; the reference evaluator ([`crate::eval`]) tests it against a
//! candidate's metadata.
//!
//! Rules every filter follows, in our own words after voidtools' public
//! Search Functions page and docs/PLAN.md:
//!
//! - Metadata a candidate does not carry never matches a function test: a
//!   candidate without a modification time matches no `dm:` query, not even
//!   a negated comparison such as `dm:!=2026`.
//! - A candidate of unknown size matches only `size:unknown`
//!   ([`SizeTest::Unknown`]), and `size:unknown` matches only candidates of
//!   unknown size.
//! - Ranges include both ends; for dates, `a..b` runs from the start of the
//!   period `a` to the end of the period `b`.
//!
//! Readings for the entry functions (our own words after the public
//! Search Functions and Searching pages):
//!
//! - Function names are case-insensitive. A word is a function only when
//!   its `name:` is typed outside quotes; `"size:1kb"` is literal text, as
//!   is any unknown `name:` prefix.
//! - `size:`, `depth:`, `len:` (and the date functions) take one value
//!   after an optional operator `=`, `==`, `!=`, `<`, `<=`, `>`, `>=`, or a
//!   range `a..b` (see [`parse_value_test`]). Right after such a
//!   function's `:`, `<` and `>` are operators, not group brackets, so
//!   `<size:<1kb ext:txt>` works. Once the value has started, `>` closes a
//!   group again (`<size:>1mb>`).
//! - `size:` values are buckets at the precision they are written with
//!   ([`super::size`]): `size:1kb` matches 1024..=2047 and `size:==1kb`
//!   exactly 1024. Constants `empty`, `tiny`, `small`, `medium`, `large`,
//!   `huge`, `gigantic` are byte bands and `unknown` tests for an unknown
//!   size.
//! - Folders have no size: no `size:` test matches a folder, whatever
//!   size metadata it carries, except `size:unknown` on a folder whose size
//!   is not known.
//! - `ext:` takes a `;` list, case-insensitive, compared with the text
//!   after the name's last `.` (a leading `.` alone, as in `.bashrc`, is
//!   not an extension). An empty `ext:` matches names without an
//!   extension; empty list items are skipped.
//! - `parent:` / `infolder:` match the direct children of exactly that
//!   folder (case-insensitive, trailing `/` ignored); `parent:/` lists the
//!   root's children.
//! - `depth:` counts the full path's components (`/Volumes` is 1);
//!   `len:` counts the name's `char`s, not bytes.
//! - `empty:` matches folders whose child count is known to be 0; it takes
//!   no argument.
//! - `count:N` sets [`Parsed::count`](super::Parsed::count) (the last one
//!   wins) and adds nothing to the tree, so it filters nothing.
//! - `dm:` / `datemodified:`, `dc:` / `datecreated:` and `da:` /
//!   `dateaccessed:` take date values ([`super::date`]): ISO 8601 dates
//!   and keywords such as `today` or `lastweek`, each standing for a whole
//!   period. `=` means inside the period, `!=` outside it, `<` before its
//!   start, `<=` before its end, `>` from its end on, `>=` from its start
//!   on and `==` its first instant; `a..b` runs from the start of `a` to
//!   the end of `b`.
//! - `attrib:` letters: `D` folder, `H` hidden, `L` symlink, `R` read-only
//!   (locked), case-insensitive; every listed letter must hold. Other ASCII
//!   letters (Windows' `A`, `S`, ...) parse silently but never match.
//! - A malformed or missing argument (`size:abc`, `size:>`, `count:-1`,
//!   `depth:`) leaves a diagnostic and the word is dropped, as if it were
//!   not typed, so partial input while typing never fails and other terms
//!   still apply.
//!
//! [`Node::Function`]: super::Node::Function

use std::fmt;

use super::date::parse_date_value;
use super::size::{SizeBucket, SizeConstant, parse_size};

pub use super::date::{
    CalendarDate, DateKeyword, DateValue, civil_to_unix, days_in_month, is_leap_year,
};

/// The comparison operator of a function value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Cmp {
    /// `:` or `=`: equal. For dates this means "inside the period", so
    /// `dm:2026` matches any time in 2026.
    #[default]
    Eq,
    /// `==`: exactly equal. For dates this means "at the period's first
    /// instant".
    ExactEq,
    /// `!=`: not equal (for dates, outside the period).
    Ne,
    /// `<`: less than (for dates, before the period starts).
    Lt,
    /// `<=`: less than or equal (for dates, before the period ends).
    Le,
    /// `>`: greater than (for dates, after the period ends).
    Gt,
    /// `>=`: greater than or equal (for dates, from the period's start on).
    Ge,
}

impl Cmp {
    /// The canonical operator text: empty for [`Cmp::Eq`], otherwise `==`,
    /// `!=`, `<`, `<=`, `>` or `>=`.
    pub fn symbol(self) -> &'static str {
        match self {
            Cmp::Eq => "",
            Cmp::ExactEq => "==",
            Cmp::Ne => "!=",
            Cmp::Lt => "<",
            Cmp::Le => "<=",
            Cmp::Gt => ">",
            Cmp::Ge => ">=",
        }
    }
}

/// An inclusive range `start..end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Range<T> {
    /// The lower end, included.
    pub start: T,
    /// The upper end, included.
    pub end: T,
}

/// A comparison against one value, or a range of values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueTest<T> {
    /// `op value`, such as `>1mb`.
    Cmp(Cmp, T),
    /// `a..b`.
    Range(Range<T>),
}

impl<T: Ord + Copy> ValueTest<T> {
    /// Tests a point value (a size, a depth, a length). [`Cmp::Eq`] and
    /// [`Cmp::ExactEq`] both mean equality here.
    pub fn matches_point(&self, v: T) -> bool {
        match *self {
            ValueTest::Cmp(cmp, x) => match cmp {
                Cmp::Eq | Cmp::ExactEq => v == x,
                Cmp::Ne => v != x,
                Cmp::Lt => v < x,
                Cmp::Le => v <= x,
                Cmp::Gt => v > x,
                Cmp::Ge => v >= x,
            },
            ValueTest::Range(r) => r.start <= v && v <= r.end,
        }
    }
}

impl<T: fmt::Display> fmt::Display for ValueTest<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValueTest::Cmp(cmp, v) => write!(f, "{}{v}", cmp.symbol()),
            ValueTest::Range(r) => write!(f, "{}..{}", r.start, r.end),
        }
    }
}

/// What `size:` tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SizeTest {
    /// `size:unknown`: the size is not known (folders not yet summed,
    /// dataless files).
    Unknown,
    /// A comparison or range in bytes.
    Bytes(ValueTest<u64>),
    /// Outside an inclusive byte range: `!=` against a size that stands
    /// for more than one byte count, such as `size:!=1kb`.
    Outside(Range<u64>),
}

impl SizeTest {
    /// Tests a known size in bytes. [`SizeTest::Unknown`] never matches a
    /// known size.
    pub fn matches_bytes(&self, size: u64) -> bool {
        match self {
            SizeTest::Unknown => false,
            SizeTest::Bytes(t) => t.matches_point(size),
            SizeTest::Outside(r) => size < r.start || size > r.end,
        }
    }
}

impl fmt::Display for SizeTest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SizeTest::Unknown => f.write_str("unknown"),
            SizeTest::Bytes(t) => t.fmt(f),
            SizeTest::Outside(r) => write!(f, "!={}..{}", r.start, r.end),
        }
    }
}

/// Which timestamp a date function tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DateField {
    /// `dm:` / `datemodified:`.
    Modified,
    /// `dc:` / `datecreated:`.
    Created,
    /// `da:` / `dateaccessed:`.
    Accessed,
}

impl DateField {
    /// The short function name: `dm`, `dc` or `da`.
    pub fn name(self) -> &'static str {
        match self {
            DateField::Modified => "dm",
            DateField::Created => "dc",
            DateField::Accessed => "da",
        }
    }
}

/// A file attribute tested by `attrib:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Attrib {
    /// `H`: hidden.
    Hidden,
    /// `R`: read-only (locked).
    ReadOnly,
    /// `L`: a symbolic link.
    Symlink,
    /// `D`: a folder.
    Directory,
    /// Any other letter (stored in uppercase), such as Windows' `A`
    /// (archive) or `S` (system). It parses but has no meaning on macOS,
    /// so it never matches.
    Other(char),
}

impl Attrib {
    /// The attribute's letter.
    pub fn letter(self) -> char {
        match self {
            Attrib::Hidden => 'H',
            Attrib::ReadOnly => 'R',
            Attrib::Symlink => 'L',
            Attrib::Directory => 'D',
            Attrib::Other(c) => c,
        }
    }

    /// The attribute for a letter, case-insensitively.
    pub fn from_letter(c: char) -> Option<Self> {
        match c.to_ascii_uppercase() {
            'H' => Some(Attrib::Hidden),
            'R' => Some(Attrib::ReadOnly),
            'L' => Some(Attrib::Symlink),
            'D' => Some(Attrib::Directory),
            _ => None,
        }
    }
}

/// A parsed search function.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Filter {
    /// `ext:a;b`: the name's extension is one of these (lowercase, without
    /// the dot; an empty string stands for "no extension"). Macros such as
    /// `pic:` expand to this.
    Ext(Vec<String>),
    /// `size:`.
    Size(SizeTest),
    /// `dm:`, `dc:` or `da:`.
    Date(DateField, ValueTest<DateValue>),
    /// `parent:path`: the entry sits directly in this folder (compared
    /// case-insensitively, trailing `/` ignored).
    Parent(String),
    /// `depth:`: the number of components in the entry's full path
    /// (`/Volumes` is 1, `/Users/me/a.txt` is 3).
    Depth(ValueTest<u32>),
    /// `len:`: the name's length in characters.
    Len(ValueTest<u32>),
    /// `empty:`: a folder known to have no children.
    Empty,
    /// `attrib:`: every listed attribute is set.
    Attrib(Vec<Attrib>),
}

impl Filter {
    /// The function's canonical name, such as `size` or `dm`.
    pub fn name(&self) -> &'static str {
        match self {
            Filter::Ext(_) => "ext",
            Filter::Size(_) => "size",
            Filter::Date(field, _) => field.name(),
            Filter::Parent(_) => "parent",
            Filter::Depth(_) => "depth",
            Filter::Len(_) => "len",
            Filter::Empty => "empty",
            Filter::Attrib(_) => "attrib",
        }
    }

    /// The function's value in canonical text: an operator symbol (see
    /// [`Cmp::symbol`]) and value, `a..b` for ranges, sizes in bytes,
    /// dates in ISO 8601, extensions joined by `;`, attributes as letters.
    pub fn args(&self) -> String {
        match self {
            Filter::Ext(exts) => exts.join(";"),
            Filter::Size(t) => t.to_string(),
            Filter::Date(_, t) => t.to_string(),
            Filter::Parent(p) => p.clone(),
            Filter::Depth(t) | Filter::Len(t) => t.to_string(),
            Filter::Empty => String::new(),
            Filter::Attrib(attrs) => attrs.iter().map(|a| a.letter()).collect(),
        }
    }
}

impl fmt::Display for Filter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.name(), self.args())
    }
}

/// What a function word parses to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FunctionWord {
    /// A filter to put in the query tree.
    Filter(Filter),
    /// `count:N`: a result limit. It filters nothing, so it adds no node.
    Count(usize),
}

/// Why a function's argument was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArgError {
    /// The function needs an argument and has none (`size:`).
    Missing,
    /// The argument is malformed (`size:abc`, `size:>`).
    Invalid,
}

/// Whether the function `name` (case-insensitive, without the `:`) takes
/// the comparison grammar, so that `<` and `>` right after its `:` are
/// comparison operators rather than group brackets. The lexer uses this to
/// keep `size:<1mb` in one word.
pub fn takes_comparison(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "size"
            | "depth"
            | "len"
            | "dm"
            | "dc"
            | "da"
            | "datemodified"
            | "datecreated"
            | "dateaccessed"
    )
}

/// Parses the function word `name:arg`. `name` is matched
/// case-insensitively; `arg` is the text after the `:` with quotes
/// removed. Returns `None` if `name` is not a known function, so the word
/// stays literal text.
pub fn parse_function(name: &str, arg: &str) -> Option<Result<FunctionWord, ArgError>> {
    let filter = |r: Result<Filter, ArgError>| Some(r.map(FunctionWord::Filter));
    match name.to_ascii_lowercase().as_str() {
        "ext" => filter(Ok(parse_ext(arg))),
        "size" => filter(parse_size_test(arg).map(Filter::Size)),
        "parent" | "infolder" => filter(if arg.is_empty() {
            Err(ArgError::Missing)
        } else {
            Ok(Filter::Parent(arg.to_owned()))
        }),
        "depth" => filter(parse_value_test(arg, parse_u32).map(Filter::Depth)),
        "len" => filter(parse_value_test(arg, parse_u32).map(Filter::Len)),
        "empty" => filter(if arg.is_empty() {
            Ok(Filter::Empty)
        } else {
            Err(ArgError::Invalid)
        }),
        "attrib" => filter(parse_attrib(arg)),
        "dm" | "datemodified" => filter(parse_date_test(DateField::Modified, arg)),
        "dc" | "datecreated" => filter(parse_date_test(DateField::Created, arg)),
        "da" | "dateaccessed" => filter(parse_date_test(DateField::Accessed, arg)),
        "count" => Some(if arg.is_empty() {
            Err(ArgError::Missing)
        } else if arg.bytes().all(|b| b.is_ascii_digit()) {
            arg.parse::<usize>()
                .map(FunctionWord::Count)
                .map_err(|_| ArgError::Invalid)
        } else {
            Err(ArgError::Invalid)
        }),
        _ => None,
    }
}

/// Parses the comparison grammar shared by `size:`, `depth:`, `len:` and
/// the date functions: an optional operator (`=`, `==`, `!=`, `<`, `<=`,
/// `>`, `>=`) and one value, or a range `a..b` with no operator (or `=`).
/// An empty argument is [`ArgError::Missing`]; an operator with no value,
/// an operator before a range, or a value `value` rejects is
/// [`ArgError::Invalid`].
pub fn parse_value_test<T>(
    arg: &str,
    value: impl Fn(&str) -> Result<T, ArgError>,
) -> Result<ValueTest<T>, ArgError> {
    if arg.is_empty() {
        return Err(ArgError::Missing);
    }
    let (cmp, explicit, rest) = split_cmp(arg);
    if rest.is_empty() {
        return Err(ArgError::Invalid);
    }
    if let Some((a, b)) = rest.split_once("..") {
        if matches!(explicit, Some(c) if c != Cmp::Eq) {
            return Err(ArgError::Invalid);
        }
        let start = value(a).map_err(|_| ArgError::Invalid)?;
        let end = value(b).map_err(|_| ArgError::Invalid)?;
        return Ok(ValueTest::Range(Range { start, end }));
    }
    let v = value(rest).map_err(|_| ArgError::Invalid)?;
    Ok(ValueTest::Cmp(cmp, v))
}

/// Splits a leading comparison operator off `arg`. Returns the operator
/// ([`Cmp::Eq`] when there is none), the operator as written (`None` when
/// there is none) and the rest.
fn split_cmp(arg: &str) -> (Cmp, Option<Cmp>, &str) {
    const OPS: [(&str, Cmp); 7] = [
        ("==", Cmp::ExactEq),
        ("!=", Cmp::Ne),
        ("<=", Cmp::Le),
        (">=", Cmp::Ge),
        ("<", Cmp::Lt),
        (">", Cmp::Gt),
        ("=", Cmp::Eq),
    ];
    for (sym, cmp) in OPS {
        if let Some(rest) = arg.strip_prefix(sym) {
            return (cmp, Some(cmp), rest);
        }
    }
    (Cmp::Eq, None, arg)
}

/// A non-negative decimal integer that fits in `u32`.
fn parse_u32(text: &str) -> Result<u32, ArgError> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ArgError::Invalid);
    }
    text.parse().map_err(|_| ArgError::Invalid)
}

/// `ext:` arguments: a `;` list, lowercased, a leading `.` dropped. Empty
/// items are skipped (`ext:txt;` while typing is `ext:txt`), and a list
/// with no items at all stands for "no extension".
fn parse_ext(arg: &str) -> Filter {
    let mut exts: Vec<String> = arg
        .split(';')
        .map(|e| e.strip_prefix('.').unwrap_or(e))
        .filter(|e| !e.is_empty())
        .map(|e| e.chars().flat_map(char::to_lowercase).collect())
        .collect();
    if exts.is_empty() {
        exts.push(String::new());
    }
    Filter::Ext(exts)
}

/// Date function arguments: the comparison grammar over date values (see
/// [`super::date`]). A comparison is kept against the whole period and
/// resolved at evaluation time, since keywords depend on the clock.
fn parse_date_test(field: DateField, arg: &str) -> Result<Filter, ArgError> {
    parse_value_test(arg, parse_date_value).map(|t| Filter::Date(field, t))
}

/// `attrib:` letters. Any ASCII letter parses (see [`Attrib::Other`]);
/// anything else is invalid.
fn parse_attrib(arg: &str) -> Result<Filter, ArgError> {
    if arg.is_empty() {
        return Err(ArgError::Missing);
    }
    arg.chars()
        .map(|c| {
            if !c.is_ascii_alphabetic() {
                Err(ArgError::Invalid)
            } else {
                Ok(Attrib::from_letter(c).unwrap_or(Attrib::Other(c.to_ascii_uppercase())))
            }
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Filter::Attrib)
}

/// `size:` arguments: `unknown`, or the comparison grammar over sizes and
/// size constants, each standing for a bucket of byte counts (see
/// [`super::size`]). A comparison against a bucket becomes a byte test:
/// `=` is inside the bucket, `==` exactly its first byte count, `!=`
/// outside it, `<` and `>=` against its start, `<=` and `>` against its
/// end, and `a..b` runs from the start of `a`'s bucket to the end of `b`'s.
fn parse_size_test(arg: &str) -> Result<SizeTest, ArgError> {
    if arg.eq_ignore_ascii_case("unknown") {
        return Ok(SizeTest::Unknown);
    }
    let bucket = |text: &str| -> Result<SizeBucket, ArgError> {
        if let Some(c) = SizeConstant::from_name(text) {
            let (start, end) = c.bounds();
            return Ok(SizeBucket { start, end });
        }
        parse_size(text).map_err(|_| ArgError::Invalid)
    };
    Ok(match parse_value_test(arg, bucket)? {
        ValueTest::Range(r) => SizeTest::Bytes(ValueTest::Range(Range {
            start: r.start.start,
            end: r.end.end,
        })),
        ValueTest::Cmp(cmp, b) => {
            let single = b.start == b.end;
            SizeTest::Bytes(match cmp {
                Cmp::Eq if single => ValueTest::Cmp(Cmp::Eq, b.start),
                Cmp::Eq => ValueTest::Range(Range {
                    start: b.start,
                    end: b.end,
                }),
                Cmp::Ne if single => ValueTest::Cmp(Cmp::Ne, b.start),
                Cmp::Ne => {
                    return Ok(SizeTest::Outside(Range {
                        start: b.start,
                        end: b.end,
                    }));
                }
                Cmp::ExactEq => ValueTest::Cmp(Cmp::ExactEq, b.start),
                Cmp::Lt => ValueTest::Cmp(Cmp::Lt, b.start),
                Cmp::Ge => ValueTest::Cmp(Cmp::Ge, b.start),
                Cmp::Le => ValueTest::Cmp(Cmp::Le, b.end),
                Cmp::Gt => ValueTest::Cmp(Cmp::Gt, b.end),
            })
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_functions() {
        let parse = |name: &str, arg: &str| parse_function(name, arg).unwrap();
        let today = ValueTest::Cmp(Cmp::Eq, DateValue::Keyword(DateKeyword::Today));
        assert_eq!(
            parse("DateCreated", "today"),
            Ok(FunctionWord::Filter(Filter::Date(
                DateField::Created,
                today
            )))
        );
        assert_eq!(parse("dm", ""), Err(ArgError::Missing));
        assert_eq!(parse("da", "2026-13"), Err(ArgError::Invalid));
        assert_eq!(parse("dm", ">"), Err(ArgError::Invalid));
        assert_eq!(parse("dm", ">2026..2027"), Err(ArgError::Invalid));
    }

    #[test]
    fn point_tests() {
        let t = ValueTest::Range(Range {
            start: 1u64,
            end: 3,
        });
        assert!(t.matches_point(1) && t.matches_point(3) && !t.matches_point(4));
        assert!(ValueTest::Cmp(Cmp::Ge, 2u32).matches_point(2));
        assert!(!ValueTest::Cmp(Cmp::Ne, 2u32).matches_point(2));
        assert_eq!(ValueTest::Cmp(Cmp::Gt, 1024u64).to_string(), ">1024");
    }
}
