//! Unicode case and diacritic folding, and the folded search copy of a name
//! column.
//!
//! Both sides of a comparison are folded the same way, then compared
//! exactly. [`fold_str`] folds one string; [`FoldedColumn`] holds the folded
//! copy of a whole [`NameColumn`] so the fast matchers can scan it with
//! plain byte search.
//!
//! What folding does, under [`FoldOptions`]:
//!
//! - **Normalization, always.** APFS may store names decomposed (NFD:
//!   `e` + U+0301) while a typed query is usually composed (NFC: `é`). The
//!   output is canonically normalized so both spellings fold to the same
//!   string. With `diacritics: false` that form is NFC.
//! - **`case: true`** lowercases one `char` at a time with std's Unicode
//!   lowercase mapping. There is no expansion (`ß` stays `ß`, so `strasse`
//!   does not match `Straße`), no Turkish special-casing beyond std (`İ`
//!   becomes `i` + U+0307, dotless `ı` stays `ı`, ASCII `I` becomes `i`) and
//!   no Greek final-sigma rule (`Σ` always becomes `σ`).
//! - **`diacritics: true`** strips accents: decompose to NFD, drop every
//!   attached combining mark (a mark with a nonzero canonical combining
//!   class), then recompose what is left. Spacing vowel signs (Mc) and
//!   enclosing marks (Me) are letters rather than accents and stay, so
//!   Devanagari `नाम` keeps its vowel sign.
//!   With the marks gone, recomposition only rejoins starter pairs such as
//!   Hangul jamo into syllables, so `?` still sees one Korean syllable as
//!   one character; no accent is ever put back. Letters without a
//!   decomposition keep their identity: `æ`, `ø`, `œ`, `ı` and `ß` are not
//!   turned into ASCII.
//!   It also drops the apostrophes `'` (U+0027), `’` (U+2019, what macOS
//!   types), `‘` (U+2018) and `ʼ` (U+02BC), so `plumb's` and `plumb’s`
//!   both fold to `plumbs`. They go before recomposition, as the marks do,
//!   so the output stays NFC.
//! - **`keep_apostrophes: true`** (only with `diacritics`) keeps the
//!   apostrophes instead, each written as `'`, so the four kinds count as
//!   one character: `plumb's` and `plumb’s` both fold to `plumb's`.
//!
//! # The apostrophe rule
//!
//! A search term without an apostrophe ignores the apostrophes in names
//! (`plumbs` finds "Plumb’s.pdf", "Plumb's.pdf" and "plumbs.pdf"); a term
//! with one finds only names that have one in that place (`plumb's` finds
//! the first two, not "plumbs.pdf"). With diacritics folded:
//!
//! - names are folded with their apostrophes kept
//!   ([`FoldOptions::for_match`]); that is the form of the in-memory
//!   folded copy ([`FoldedColumn`]);
//! - a term is folded with [`FoldOptions::for_term`]: apostrophes kept when
//!   its text has one, dropped when it has none, and the name is compared
//!   in the same folding. So a term without an apostrophe sees the name
//!   with its apostrophes dropped, exactly as an earlier rule did for every
//!   term: `ww:plumb` does not find "Plumb’s" (the word is `plumbs`) and
//!   `plumb?s` does not either (`?` never stands for a dropped apostrophe);
//! - the folded copy keeps that apostrophe-free form in a sparse side table
//!   for the (rare) names that have an apostrophe, so neither term needs
//!   per-entry folding ([`FoldedColumn::folded_name`]).
//!
//! Matching diacritics (`diacritics:`, the command line's `-a`) keeps the
//! exact text, so the four kinds stay different there.
//!
//! An ASCII string needs none of the Unicode work (ASCII is already NFC and
//! has no marks), so it takes a fast path: it is borrowed unchanged, or
//! lowercased byte by byte when folding case and stripped of `'` when
//! dropping apostrophes.

use std::borrow::Cow;
use std::mem::size_of;

use memchr::memchr;
use rayon::prelude::*;
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::{canonical_combining_class, is_combining_mark};

use crate::substring::NameColumn;

/// Which differences folding removes. All off still normalizes (NFC).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct FoldOptions {
    /// Fold case (Unicode lowercase, one `char` at a time).
    pub case: bool,
    /// Strip diacritics (combining marks after NFD decomposition) and drop
    /// apostrophes (see [`is_apostrophe`]), or write them as `'` with
    /// `keep_apostrophes`.
    pub diacritics: bool,
    /// With `diacritics`, keep apostrophes, each written as `'`, instead of
    /// dropping them. No effect without `diacritics`.
    pub keep_apostrophes: bool,
}

impl FoldOptions {
    /// The folding of names for a search: case is folded unless matching
    /// case, and diacritics are stripped unless matching diacritics, with
    /// every apostrophe written as `'` (see the
    /// [apostrophe rule](self#the-apostrophe-rule)). The in-memory folded
    /// copy of the names is built with it.
    pub fn for_match(match_case: bool, match_diacritics: bool) -> Self {
        FoldOptions {
            case: !match_case,
            diacritics: !match_diacritics,
            keep_apostrophes: !match_diacritics,
        }
    }

    /// The folding for a term whose literal text is `text` (for a wildcard
    /// term, its literal pieces joined), used for the term and for the
    /// names it is compared with: [`for_match`](Self::for_match) when the
    /// text has an apostrophe (or diacritics are matched), else the same
    /// with apostrophes dropped. So a term without an apostrophe ignores
    /// the apostrophes in names, and one with an apostrophe finds only
    /// names that have one in that place.
    ///
    /// ```
    /// use fx_query::fold::{FoldOptions, fold_str};
    ///
    /// let with = FoldOptions::for_term("plumb’s", false, false);
    /// assert!(with.keep_apostrophes);
    /// assert_eq!(fold_str("Plumb’s.pdf", with), "plumb's.pdf");
    /// assert_eq!(fold_str("plumbs.pdf", with), "plumbs.pdf");
    /// let without = FoldOptions::for_term("plumbs", false, false);
    /// assert_eq!(fold_str("Plumb’s.pdf", without), "plumbs.pdf");
    /// ```
    pub fn for_term(text: &str, match_case: bool, match_diacritics: bool) -> Self {
        let fold = Self::for_match(match_case, match_diacritics);
        if text.contains(is_apostrophe) {
            fold
        } else {
            fold.without_apostrophes()
        }
    }

    /// The same folding with apostrophes dropped rather than kept: the
    /// folding of a term without an apostrophe, and of the side table of a
    /// [`FoldedColumn`].
    pub fn without_apostrophes(self) -> Self {
        FoldOptions {
            keep_apostrophes: false,
            ..self
        }
    }

    /// Whether folding removes apostrophes altogether.
    fn drops_apostrophes(self) -> bool {
        self.diacritics && !self.keep_apostrophes
    }

    /// Whether folding writes every apostrophe as `'`, so a name folded
    /// this way can have a different form without them.
    fn keeps_apostrophes(self) -> bool {
        self.diacritics && self.keep_apostrophes
    }
}

/// Whether `c` is one of the apostrophes diacritic folding unifies (or
/// drops): `'` (U+0027), `’` (U+2019), `‘` (U+2018) or `ʼ` (U+02BC).
/// Look-alikes such as the ʻokina (U+02BB) and the prime (U+2032) are not
/// among them.
pub fn is_apostrophe(c: char) -> bool {
    matches!(c, '\'' | '\u{2019}' | '\u{2018}' | '\u{2bc}')
}

/// Folds `s` under `opts` (see the [module docs](self)).
///
/// Returns `Cow::Borrowed(s)` when folding leaves `s` unchanged, which is
/// always the case for ASCII input unless `case` is set and `s` has an
/// uppercase letter, or apostrophes are dropped and `s` has a `'`.
///
/// ```
/// use fx_query::fold::{FoldOptions, fold_str};
///
/// let both = FoldOptions::for_match(false, false);
/// assert_eq!(fold_str("Résumé.PDF", both), "resume.pdf");
/// // The decomposed (NFD) spelling folds the same way.
/// assert_eq!(fold_str("Re\u{301}sume\u{301}.PDF", both), "resume.pdf");
/// // Every apostrophe is written as `'`, or dropped without them.
/// assert_eq!(fold_str("Plumb’s.pdf", both), "plumb's.pdf");
/// assert_eq!(fold_str("Plumb’s.pdf", both.without_apostrophes()), "plumbs.pdf");
/// ```
pub fn fold_str(s: &str, opts: FoldOptions) -> Cow<'_, str> {
    let bytes = s.as_bytes();
    if let Some(ascii) = AsciiTraits::of(bytes) {
        let drop = opts.drops_apostrophes() && ascii.apostrophe;
        let lower = opts.case && ascii.upper;
        if !drop && !lower {
            return Cow::Borrowed(s);
        }
        let mut out = Vec::with_capacity(bytes.len());
        fold_ascii(bytes, opts, &mut out);
        return Cow::Owned(String::from_utf8(out).expect("ASCII folds to ASCII"));
    }
    let folded = fold_non_ascii(s, opts);
    if folded == s {
        Cow::Borrowed(s)
    } else {
        Cow::Owned(folded)
    }
}

/// Writes [`fold_str`] of `s` under `opts` into `out` (cleared first)
/// when folding changes `s`, and says whether it did; `false` means `s` is
/// its own folded form. What it writes is always UTF-8. An ASCII string
/// (nearly every name) is folded without allocating once `out` has room
/// for it, so a scan that folds name after name into one buffer pays for
/// no allocation per name.
pub(crate) fn fold_into(s: &str, opts: FoldOptions, out: &mut Vec<u8>) -> bool {
    out.clear();
    let bytes = s.as_bytes();
    if let Some(ascii) = AsciiTraits::of(bytes) {
        let drop = opts.drops_apostrophes() && ascii.apostrophe;
        let lower = opts.case && ascii.upper;
        if !drop && !lower {
            return false;
        }
        if drop {
            out.extend(bytes.iter().copied().filter(|&b| b != b'\''));
            if opts.case {
                out.make_ascii_lowercase();
            }
        } else {
            push_ascii_lowercase(bytes, out);
        }
    } else {
        let folded = fold_non_ascii(s, opts);
        if folded == s {
            return false;
        }
        out.extend_from_slice(folded.as_bytes());
    }
    true
}

/// One `u64` of the eight bytes of `bytes` from `at` (little-endian; the
/// tricks below work on each byte alone, so the order doesn't matter).
fn word_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("8 bytes"))
}

/// `0x01` in every byte.
const ONES: u64 = u64::from_ne_bytes([0x01; 8]);
/// `0x80` in every byte.
const HIGH: u64 = u64::from_ne_bytes([0x80; 8]);
/// For a byte below 0x80, adding this sets its high bit from `A` up...
const FROM_A: u64 = u64::from_ne_bytes([0x80 - b'A'; 8]);
/// ...and adding this from just after `Z` up. Neither carries into the
/// next byte.
const AFTER_Z: u64 = u64::from_ne_bytes([0x80 - b'Z' - 1; 8]);

/// The high bit of each byte of `w` that is a capital, when every byte of
/// `w` is ASCII (meaningless otherwise).
fn capitals(w: u64) -> u64 {
    w.wrapping_add(FROM_A) & !w.wrapping_add(AFTER_Z) & HIGH
}

/// Appends ASCII `bytes`, lowercased, to `out`, eight bytes at a time:
/// each capital's high-bit mark, moved down to 0x20, is the bit that
/// lowercases it.
fn push_ascii_lowercase(bytes: &[u8], out: &mut Vec<u8>) {
    let lower = |w: u64| (w | (capitals(w) >> 2)).to_le_bytes();
    let n = bytes.len();
    if n < 8 {
        out.extend(bytes.iter().map(u8::to_ascii_lowercase));
        return;
    }
    let start = out.len();
    out.reserve(n);
    for at in (0..n - 7).step_by(8) {
        out.extend_from_slice(&lower(word_at(bytes, at)));
    }
    // The last eight bytes again, over what is already written with the
    // same lowercase bytes.
    if n % 8 != 0 {
        out.truncate(start + n - 8);
        out.extend_from_slice(&lower(word_at(bytes, n - 8)));
    }
}

/// What the ASCII fast path of folding needs to know about a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AsciiTraits {
    /// It has a capital letter.
    upper: bool,
    /// It has an apostrophe (`'`).
    apostrophe: bool,
}

impl AsciiTraits {
    /// The traits of `bytes`, or `None` when a byte isn't ASCII.
    ///
    /// One pass over eight bytes at a time in a `u64`, without branches:
    /// for strings as short as names (17 bytes on average) that is several
    /// times cheaper than asking the three questions in turn, each with its
    /// own early exit, and cheaper than a vector loop's set-up. A string
    /// shorter than eight bytes is padded with zero bytes, which are ASCII,
    /// not capitals and not apostrophes.
    fn of(bytes: &[u8]) -> Option<AsciiTraits> {
        const QUOTE: u64 = u64::from_ne_bytes([b'\''; 8]);
        let (mut any, mut upper, mut quote) = (0u64, 0u64, 0u64);
        let mut word = |w: u64| {
            any |= w;
            upper |= capitals(w);
            // A zero byte of `w ^ QUOTE` is an apostrophe.
            let x = w ^ QUOTE;
            quote |= x.wrapping_sub(ONES) & !x;
        };
        let n = bytes.len();
        if n >= 8 {
            for at in (0..n - 7).step_by(8) {
                word(word_at(bytes, at));
            }
            // The last eight bytes again, overlapping: the answers are
            // unions, so a byte seen twice changes nothing.
            if n % 8 != 0 {
                word(word_at(bytes, n - 8));
            }
        } else {
            word(bytes.iter().rev().fold(0, |w, &b| (w << 8) | u64::from(b)));
        }
        // With a non-ASCII byte the other two answers are meaningless, but
        // they are not used then.
        (any & HIGH == 0).then_some(AsciiTraits {
            upper: upper & HIGH != 0,
            apostrophe: quote & HIGH != 0,
        })
    }
}

/// Appends ASCII `bytes` folded under `opts` to `out`: without `'` when
/// apostrophes are dropped, lowercased when folding case.
fn fold_ascii(bytes: &[u8], opts: FoldOptions, out: &mut Vec<u8>) {
    let start = out.len();
    if opts.drops_apostrophes() && memchr(b'\'', bytes).is_some() {
        out.extend(bytes.iter().copied().filter(|&b| b != b'\''));
    } else {
        out.extend_from_slice(bytes);
    }
    if opts.case {
        out[start..].make_ascii_lowercase();
    }
}

/// The slow path of [`fold_str`], for strings with a non-ASCII character.
fn fold_non_ascii(s: &str, opts: FoldOptions) -> String {
    let mut out = String::with_capacity(s.len());
    // Accents go; apostrophes go too, or become `'` when kept. Both happen
    // before recomposition, so starters that end up side by side (Hangul
    // jamo split by an apostrophe) still join and the output stays NFC.
    let keep = opts.keep_apostrophes;
    let strip = |c: char| {
        if is_stripped_mark(c) {
            None
        } else if is_apostrophe(c) {
            keep.then_some('\'')
        } else {
            Some(c)
        }
    };
    match (opts.case, opts.diacritics) {
        (false, false) => out.extend(s.nfc()),
        (true, false) => out.extend(s.chars().flat_map(char::to_lowercase).nfc()),
        (false, true) => out.extend(s.nfd().filter_map(strip).nfc()),
        // Strip before lowercasing: `İ` decomposes to `I` + U+0307, so the
        // dot is gone before `I` lowercases to a plain `i`.
        (true, true) => out.extend(s.nfd().filter_map(strip).flat_map(char::to_lowercase).nfc()),
    }
    out
}

/// Whether diacritic folding drops `c`: a combining mark that attaches to
/// the character before it, meaning a nonzero canonical combining class.
///
/// This covers every accent of the Latin, Greek and Cyrillic scripts (and
/// Hebrew points, Arabic vowel marks, nukta and virama), but keeps spacing
/// vowel signs (General_Category Mc), enclosing marks (Me) and the few
/// vowel signs of class 0, which are letters in their own right rather
/// than accents. [`is_combining_mark`] alone covers all of
/// General_Category M, which would delete Indic vowels.
fn is_stripped_mark(c: char) -> bool {
    canonical_combining_class(c) != 0 && is_combining_mark(c)
}

/// Folds name bytes from a column. Names are UTF-8 on macOS; invalid
/// sequences, which no supported file system produces, are replaced with
/// U+FFFD before folding.
fn fold_bytes(name: &[u8], opts: FoldOptions, out: &mut Vec<u8>) {
    if name.is_ascii() {
        fold_ascii(name, opts, out);
        return;
    }
    let text = String::from_utf8_lossy(name);
    out.extend_from_slice(fold_str(&text, opts).as_bytes());
}

/// Entries per parallel chunk when building a [`FoldedColumn`].
const BUILD_CHUNK_ENTRIES: usize = 16_384;

/// The folded search copy of a [`NameColumn`].
///
/// It owns a contiguous NUL-separated buffer and `u32` start offsets in the
/// same layout as a valid [`NameColumn`] (see [`FoldedColumn::column`]),
/// where entry `i` is [`fold_str`] of entry `i` of the source column. Folded
/// names can be longer or shorter than the originals, so the offsets are
/// recomputed; an all-ASCII column without `'` keeps its offsets and its
/// bytes (lowercased when folding case). The copy lives in memory only, so
/// changing what folding does never changes a saved index.
///
/// Built with apostrophes kept (the default folding,
/// [`FoldOptions::for_match`]), it also holds a sparse side table: for each
/// name whose folded form has an apostrophe, that name folded with
/// apostrophes dropped ([`FoldOptions::without_apostrophes`]), the form a
/// term without an apostrophe compares with (see the
/// [apostrophe rule](self#the-apostrophe-rule)). A one-bit-per-entry mark
/// says which names have one, so the lookup costs one bit test for the
/// others; the bitset is not allocated when no name has an apostrophe.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FoldedColumn {
    buf: Vec<u8>,
    offsets: Vec<u32>,
    opts: FoldOptions,
    side: SideTable,
}

/// The apostrophe-free forms of the names that have an apostrophe.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SideTable {
    /// One bit per entry, set when the entry has a side form; empty when no
    /// entry has one.
    marks: Vec<u64>,
    /// Entries with a side form, ascending.
    ids: Vec<u32>,
    /// The side forms, back to back, in `ids` order.
    buf: Vec<u8>,
    /// Start of each side form in `buf`, plus the end of the last one.
    bounds: Vec<u32>,
}

impl SideTable {
    fn get(&self, i: usize) -> Option<&[u8]> {
        let word = *self.marks.get(i / 64)?;
        if word & (1 << (i % 64)) == 0 {
            return None;
        }
        let k = self.ids.binary_search(&(i as u32)).ok()?;
        Some(&self.buf[self.bounds[k] as usize..self.bounds[k + 1] as usize])
    }

    fn heap_bytes(&self) -> usize {
        self.marks.capacity() * size_of::<u64>()
            + self.ids.capacity() * size_of::<u32>()
            + self.buf.capacity()
            + self.bounds.capacity() * size_of::<u32>()
    }
}

/// One parallel chunk of a [`FoldedColumn`] build: the folded names with
/// chunk-local offsets, and the side forms with their entry IDs.
struct Chunk {
    buf: Vec<u8>,
    offsets: Vec<u32>,
    side: Vec<(u32, Vec<u8>)>,
}

impl FoldedColumn {
    /// Folds every name of `column` under `opts`, in parallel on the
    /// current rayon pool. Entry order is kept. With apostrophes kept, the
    /// side table is built too.
    ///
    /// # Panics
    ///
    /// If the folded buffer would not fit `u32` offsets (over 4 GiB).
    pub fn build(column: &NameColumn<'_>, opts: FoldOptions) -> Self {
        let len = column.len();
        let chunks: Vec<Chunk> = (0..len.div_ceil(BUILD_CHUNK_ENTRIES))
            .into_par_iter()
            .map(|chunk| {
                let start = chunk * BUILD_CHUNK_ENTRIES;
                let end = (start + BUILD_CHUNK_ENTRIES).min(len);
                fold_range(column, start, end, opts)
            })
            .collect();

        let total: usize = chunks.iter().map(|c| c.buf.len()).sum();
        assert!(
            u32::try_from(total).is_ok(),
            "folded name buffer exceeds u32 offsets"
        );
        let mut buf = Vec::with_capacity(total);
        let mut offsets = Vec::with_capacity(len);
        let mut side_rows = Vec::new();
        for chunk in chunks {
            // Fits: `total` fits in u32 and `buf.len() <= total`.
            let base = buf.len() as u32;
            offsets.extend(chunk.offsets.iter().map(|&o| base + o));
            buf.extend_from_slice(&chunk.buf);
            side_rows.extend(chunk.side);
        }
        FoldedColumn {
            buf,
            offsets,
            opts,
            side: side_table(len, side_rows),
        }
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.offsets.len()
    }

    /// Whether there are no entries.
    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }

    /// The folding the copy was built with.
    pub fn options(&self) -> FoldOptions {
        self.opts
    }

    /// The folded names, each followed by NUL.
    pub fn buf(&self) -> &[u8] {
        &self.buf
    }

    /// Start offset of each folded name in [`buf`](Self::buf).
    pub fn offsets(&self) -> &[u32] {
        &self.offsets
    }

    /// The folded bytes of entry `i`, without the NUL terminator.
    ///
    /// # Panics
    ///
    /// If `i >= self.len()`.
    pub fn name_bytes(&self, i: usize) -> &[u8] {
        let start = self.offsets[i] as usize;
        let end = match self.offsets.get(i + 1) {
            Some(&next) => next as usize - 1,
            None => self.buf.len() - 1,
        };
        &self.buf[start..end]
    }

    /// The side-table form of entry `i`: the name folded with apostrophes
    /// dropped, when its folded form has an apostrophe and the copy keeps
    /// them; `None` otherwise (for those names the two forms are the same).
    pub fn without_apostrophes(&self, i: usize) -> Option<&[u8]> {
        self.side.get(i)
    }

    /// Entry `i` folded under `opts`, if the copy holds that form: the
    /// main form for the copy's own options; for those options without
    /// apostrophes (a term without one), the side form when the entry has
    /// one, else the main form. `None` for any other folding.
    ///
    /// # Panics
    ///
    /// If `i >= self.len()` and the copy holds the form.
    pub fn folded_name(&self, i: usize, opts: FoldOptions) -> Option<&[u8]> {
        if opts == self.opts {
            Some(self.name_bytes(i))
        } else if self.opts.keeps_apostrophes() && opts == self.opts.without_apostrophes() {
            Some(self.side.get(i).unwrap_or_else(|| self.name_bytes(i)))
        } else {
            None
        }
    }

    /// Number of entries in the side table (names with an apostrophe).
    pub fn side_table_len(&self) -> usize {
        self.side.ids.len()
    }

    /// Heap bytes of the side table: the mark bitset, the entry IDs and the
    /// apostrophe-free forms. 0 when no name has an apostrophe.
    pub fn side_table_bytes(&self) -> usize {
        self.side.heap_bytes()
    }

    /// Heap bytes of the whole copy: the folded names, their offsets and
    /// the side table.
    pub fn heap_bytes(&self) -> usize {
        self.buf.capacity() + self.offsets.capacity() * size_of::<u32>() + self.side.heap_bytes()
    }

    /// The folded copy as a [`NameColumn`], for the scans in
    /// [`crate::substring`].
    pub fn column(&self) -> NameColumn<'_> {
        NameColumn::new(&self.buf, &self.offsets)
            .expect("a folded column always has a valid layout")
    }
}

/// Builds the side table of a column of `len` entries from its rows, which
/// come in ascending entry order.
fn side_table(len: usize, rows: Vec<(u32, Vec<u8>)>) -> SideTable {
    if rows.is_empty() {
        return SideTable::default();
    }
    let mut table = SideTable {
        marks: vec![0; len.div_ceil(64)],
        ids: Vec::with_capacity(rows.len()),
        buf: Vec::with_capacity(rows.iter().map(|(_, b)| b.len()).sum()),
        bounds: Vec::with_capacity(rows.len() + 1),
    };
    table.bounds.push(0);
    for (id, bytes) in rows {
        let i = id as usize;
        table.marks[i / 64] |= 1 << (i % 64);
        table.ids.push(id);
        table.buf.extend_from_slice(&bytes);
        let end = u32::try_from(table.buf.len()).expect("side table exceeds u32 offsets");
        table.bounds.push(end);
    }
    table
}

/// Folds entries `start..end` into a chunk buffer with chunk-local offsets,
/// plus the side forms of the names with an apostrophe when `opts` keeps
/// them.
fn fold_range(column: &NameColumn<'_>, start: usize, end: usize, opts: FoldOptions) -> Chunk {
    let mut buf = Vec::new();
    let mut offsets = Vec::with_capacity(end - start);
    let mut side = Vec::new();
    let side_opts = opts.keeps_apostrophes().then(|| opts.without_apostrophes());
    for i in start..end {
        let offset = u32::try_from(buf.len()).expect("folded name buffer exceeds u32 offsets");
        offsets.push(offset);
        fold_bytes(column.name_bytes(i), opts, &mut buf);
        if let Some(side_opts) = side_opts
            && memchr(b'\'', &buf[offset as usize..]).is_some()
        {
            // Folded afresh rather than cut out of the main form: dropping
            // the apostrophe before recomposition keeps the form NFC.
            let mut bytes = Vec::new();
            fold_bytes(column.name_bytes(i), side_opts, &mut bytes);
            let id = u32::try_from(i).expect("entry IDs fit u32");
            side.push((id, bytes));
        }
        buf.push(0);
    }
    Chunk { buf, offsets, side }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOTH: FoldOptions = FoldOptions {
        case: true,
        diacritics: true,
        keep_apostrophes: false,
    };
    const KEEP: FoldOptions = FoldOptions {
        keep_apostrophes: true,
        ..BOTH
    };

    #[test]
    fn for_match_inverts_the_match_flags_and_keeps_apostrophes() {
        assert_eq!(FoldOptions::for_match(false, false), KEEP);
        assert_eq!(FoldOptions::for_match(true, true), FoldOptions::default());
    }

    #[test]
    fn the_four_apostrophes() {
        for c in ['\'', '\u{2019}', '\u{2018}', '\u{2bc}'] {
            assert!(is_apostrophe(c), "{:04x}", c as u32);
        }
        for c in ['"', '`', '\u{b4}', '\u{2bb}', '\u{2032}', '\u{201c}', 's'] {
            assert!(!is_apostrophe(c), "{:04x}", c as u32);
        }
    }

    #[test]
    fn for_term_keeps_apostrophes_only_for_a_text_with_one() {
        for (match_case, match_diacritics) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let plain = FoldOptions::for_match(match_case, match_diacritics);
            for text in ["", "plumbs", "a", "\u{301}", "x\u{301}", "ʻ"] {
                let fold = FoldOptions::for_term(text, match_case, match_diacritics);
                assert_eq!(fold, plain.without_apostrophes(), "{text:?}");
            }
            for text in ["'", "plumb's", "’s", "‘", "ʼn"] {
                let fold = FoldOptions::for_term(text, match_case, match_diacritics);
                assert_eq!(fold, plain, "{text:?}");
            }
            // Matching diacritics keeps apostrophes anyway.
            let lone = FoldOptions::for_term("'", match_case, match_diacritics);
            assert_eq!(lone.keep_apostrophes, !match_diacritics, "{plain:?}");
        }
        for text in ["'", "’", "‘’", "ʼ'", "'\u{301}", "plumb’s"] {
            assert_eq!(FoldOptions::for_term(text, false, false), KEEP, "{text:?}");
        }
        assert_eq!(FoldOptions::for_term("plumbs", false, false), BOTH);
    }

    #[test]
    fn kept_apostrophes_are_all_written_as_ascii() {
        assert_eq!(fold_str("Plumb’s ‘x’ ʼn", KEEP), "plumb's 'x' 'n");
        assert_eq!(fold_str("Cafe\u{301}’s", KEEP), "cafe's");
        assert!(matches!(fold_str("it's", KEEP), Cow::Borrowed(_)));
        assert_eq!(fold_str("IT'S", KEEP), "it's");
        // Without `diacritics` there is nothing to keep or drop.
        let case_only = FoldOptions {
            case: true,
            diacritics: false,
            keep_apostrophes: true,
        };
        assert_eq!(fold_str("Plumb’s", case_only), "plumb’s");
    }

    #[test]
    fn ascii_column_bytes_drop_apostrophes() {
        let mut out = b"x".to_vec();
        fold_bytes(b"It's 'Ok'", BOTH, &mut out);
        assert_eq!(out, b"xits ok");
        out.clear();
        fold_bytes(b"It's", KEEP, &mut out);
        assert_eq!(out, b"it's");
    }

    #[test]
    fn unchanged_non_ascii_is_borrowed() {
        assert!(matches!(fold_str("日本語", BOTH), Cow::Borrowed(_)));
        assert!(matches!(fold_str("é", BOTH), Cow::Owned(_)));
    }

    #[test]
    fn only_attached_marks_are_stripped() {
        let d = FoldOptions {
            case: false,
            diacritics: true,
            keep_apostrophes: false,
        };
        // Accents from several scripts go.
        for c in ['\u{300}', '\u{301}', '\u{308}', '\u{30a}', '\u{327}'] {
            assert!(is_stripped_mark(c), "{:04x}", c as u32);
        }
        // Spacing vowel signs (Mc) and enclosing marks (Me) stay, so
        // Devanagari words keep their vowels.
        for c in ['\u{903}', '\u{93e}', '\u{20dd}'] {
            assert!(is_combining_mark(c), "{:04x} is a mark", c as u32);
            assert!(!is_stripped_mark(c), "{:04x} is kept", c as u32);
        }
        assert_eq!(fold_str("नाम", d), "नाम");
        assert_eq!(fold_str("Hello", d), "Hello");
    }

    #[test]
    fn invalid_utf8_is_replaced_not_dropped() {
        let mut out = Vec::new();
        fold_bytes(b"A\xffB\xc3\x89", BOTH, &mut out);
        assert_eq!(out, "a\u{fffd}be".as_bytes());
    }

    #[test]
    fn folding_into_a_buffer_agrees_with_fold_str() {
        let names = [
            "",
            "plain.txt",
            "README.md",
            "it's",
            "IT'S",
            "Plumb’s ‘x’ ʼn",
            "Résumé.PDF",
            "Re\u{301}sume\u{301}.PDF",
            "Cafe\u{301}’s",
            "日本語",
            "नाम",
            "İstanbul",
            "a",
            "CONFIGURATION-BACKUP.JSON",
        ];
        let case_only = FoldOptions {
            case: true,
            diacritics: false,
            keep_apostrophes: true,
        };
        let mut out = b"stale bytes from an earlier name".to_vec();
        for opts in [BOTH, KEEP, case_only, FoldOptions::default()] {
            for name in names {
                let want = fold_str(name, opts);
                let changed = fold_into(name, opts, &mut out);
                assert_eq!(
                    changed,
                    matches!(want, Cow::Owned(_)),
                    "{name:?} under {opts:?}"
                );
                let got = if changed {
                    std::str::from_utf8(&out).expect("UTF-8")
                } else {
                    name
                };
                assert_eq!(got, want, "{name:?} under {opts:?}");
            }
        }
        // Every length around the eight-byte steps, capitals anywhere.
        let alphabet = "AbCdEfGhIjKlMnOpQrStUvWxYz@[`{0'";
        for len in 0..alphabet.len() {
            for start in 0..alphabet.len() - len {
                let name = &alphabet[start..start + len];
                for opts in [BOTH, KEEP] {
                    let want = fold_str(name, opts);
                    let got = if fold_into(name, opts, &mut out) {
                        std::str::from_utf8(&out).expect("UTF-8")
                    } else {
                        name
                    };
                    assert_eq!(got, want, "{name:?} under {opts:?}");
                }
            }
        }
    }

    #[test]
    fn ascii_traits_find_capitals_apostrophes_and_other_bytes() {
        let t = |s: &str| AsciiTraits::of(s.as_bytes());
        let traits = |upper, apostrophe| Some(AsciiTraits { upper, apostrophe });
        assert_eq!(t(""), traits(false, false));
        assert_eq!(t("plain-name_01.txt"), traits(false, false));
        assert_eq!(t("@[`{"), traits(false, false), "neighbours of A-Z");
        assert_eq!(t("A"), traits(true, false));
        assert_eq!(t("z.Z"), traits(true, false));
        assert_eq!(t("it's"), traits(false, true));
        assert_eq!(t("a long name of thirty-two bytes'"), traits(false, true));
        assert_eq!(t("IT'S"), traits(true, true));
        assert_eq!(t("é"), None);
        assert_eq!(t("a long ASCII name, then: \u{2019}"), None);
        assert_eq!(t("\u{7f}"), traits(false, false));
    }

    #[test]
    fn folding_into_a_buffer_reuses_it() {
        let mut out = Vec::new();
        assert!(fold_into("A LONG NAME WITH CAPITALS.TXT", BOTH, &mut out));
        let (ptr, cap) = (out.as_ptr(), out.capacity());
        for name in ["Short", "MID LENGTH.md", "X", "IT'S"] {
            assert!(fold_into(name, BOTH, &mut out), "{name:?}");
            assert_eq!(
                (out.as_ptr(), out.capacity()),
                (ptr, cap),
                "{name:?} reused the buffer"
            );
        }
        assert_eq!(out, b"its");
    }
}
