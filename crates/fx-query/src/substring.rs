//! Parallel substring scan over one contiguous NUL-separated name buffer.
//!
//! Names live in a single buffer, each followed by a NUL byte, with a
//! parallel array of start offsets (the layout of `fx_bench::Names`). A
//! query builds one [`memmem::Finder`], and the scan runs it over the bytes
//! of whole chunks of entries instead of once per name. Each hit offset is
//! mapped back to its entry ID, and the scan then resumes at the start of the
//! next entry, so an entry is reported at most once. A needle never contains
//! NUL, so a hit can never span two names.
//!
//! The entry range is split into chunks aligned to entry boundaries, which
//! the current rayon pool scans in parallel; chunk results are concatenated
//! in order, so IDs come back ascending whatever the thread count or chunk
//! size.
//!
//! ASCII case-insensitive search runs against a folded copy of the buffer
//! ([`fold_ascii_lowercase`]), which has the same length, so the offsets are
//! reused unchanged.
//!
//! Cancellation: a [`Generation`] counter is bumped whenever a newer query
//! supersedes the running one. A [`CancelToken`] captures the generation at
//! the start of a query and is checked before each chunk and at least every
//! [`CHECK_INTERVAL`] entries.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use memchr::memmem;
use rayon::prelude::*;

/// The scan checks its cancel token at least once per this many entries.
pub const CHECK_INTERVAL: usize = 65_536;

/// Entries per parallel chunk used by [`search`] and [`count`].
///
/// Small enough that 5M names give a few hundred chunks for load balancing
/// across threads, large enough that per-chunk overhead is negligible.
pub const DEFAULT_CHUNK_ENTRIES: usize = 16_384;

// ---------------------------------------------------------------------------
// Name column
// ---------------------------------------------------------------------------

/// Why [`NameColumn::new`] rejected a buffer and offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnError {
    /// There are no offsets, but the buffer is not empty.
    EmptyOffsetsNonEmptyBuffer,
    /// The first offset is not zero.
    FirstOffsetNotZero,
    /// Offset `index` is not greater than the one before it.
    NotStrictlyIncreasing { index: usize },
    /// Offset `index` is not inside the buffer.
    OffsetOutOfBounds { index: usize },
    /// The byte before offset `index` is not NUL.
    MissingSeparator { index: usize },
    /// The buffer does not end with NUL.
    MissingTerminator,
}

impl fmt::Display for ColumnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyOffsetsNonEmptyBuffer => {
                write!(f, "no offsets were given for a non-empty name buffer")
            }
            Self::FirstOffsetNotZero => write!(f, "the first offset is not zero"),
            Self::NotStrictlyIncreasing { index } => {
                write!(f, "offset {index} is not greater than the previous offset")
            }
            Self::OffsetOutOfBounds { index } => {
                write!(f, "offset {index} is not inside the name buffer")
            }
            Self::MissingSeparator { index } => {
                write!(f, "the byte before offset {index} is not NUL")
            }
            Self::MissingTerminator => write!(f, "the name buffer does not end with NUL"),
        }
    }
}

impl std::error::Error for ColumnError {}

/// A validated view of a NUL-separated name buffer and its start offsets.
///
/// Entry `i` is `buf[offsets[i]..]` up to (not including) the NUL that ends
/// it. Entry IDs are `u32` indices into `offsets`.
#[derive(Debug, Clone, Copy)]
pub struct NameColumn<'a> {
    buf: &'a [u8],
    offsets: &'a [u32],
}

impl<'a> NameColumn<'a> {
    /// Checks the layout and wraps `buf` and `offsets`.
    ///
    /// Requires either both to be empty, or: `offsets[0] == 0`; offsets
    /// strictly increasing and each less than `buf.len()`; the byte before
    /// every later offset is NUL; and `buf` ends with NUL.
    pub fn new(buf: &'a [u8], offsets: &'a [u32]) -> Result<Self, ColumnError> {
        let Some(&first) = offsets.first() else {
            return if buf.is_empty() {
                Ok(Self { buf, offsets })
            } else {
                Err(ColumnError::EmptyOffsetsNonEmptyBuffer)
            };
        };
        if first != 0 {
            return Err(ColumnError::FirstOffsetNotZero);
        }
        if buf.is_empty() {
            return Err(ColumnError::OffsetOutOfBounds { index: 0 });
        }
        for index in 1..offsets.len() {
            let o = offsets[index] as usize;
            if offsets[index] <= offsets[index - 1] {
                return Err(ColumnError::NotStrictlyIncreasing { index });
            }
            if o >= buf.len() {
                return Err(ColumnError::OffsetOutOfBounds { index });
            }
            if buf[o - 1] != 0 {
                return Err(ColumnError::MissingSeparator { index });
            }
        }
        if buf.last() != Some(&0) {
            return Err(ColumnError::MissingTerminator);
        }
        Ok(Self { buf, offsets })
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.offsets.len()
    }

    /// Whether the column has no entries.
    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }

    /// The bytes of name `i`, without its NUL terminator.
    ///
    /// # Panics
    ///
    /// If `i >= self.len()`.
    pub fn name_bytes(&self, i: usize) -> &'a [u8] {
        &self.buf[self.start(i)..self.end(i)]
    }

    /// Start offset of entry `i`.
    fn start(&self, i: usize) -> usize {
        self.offsets[i] as usize
    }

    /// Exclusive end of entry `i` (the position of its NUL terminator).
    fn end(&self, i: usize) -> usize {
        match self.offsets.get(i + 1) {
            Some(&next) => next as usize - 1,
            None => self.buf.len() - 1,
        }
    }
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

/// How letters are compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseMode {
    /// Bytes must match exactly.
    Sensitive,
    /// ASCII letters match either case; search a column built on
    /// [`fold_ascii_lowercase`] output. Non-ASCII bytes match exactly.
    AsciiInsensitive,
}

/// What a query matches.
#[derive(Debug, Clone)]
enum Matcher {
    /// Empty needle: every entry.
    All,
    /// Needle containing NUL: no entry.
    Nothing,
    /// A non-empty needle without NUL. Boxed: on x86-64 the finder holds
    /// SIMD searchers of several hundred bytes, and the other variants are
    /// empty.
    Needle(Box<memmem::Finder<'static>>),
}

/// A prepared substring query: one [`memmem::Finder`] reused for every
/// chunk.
#[derive(Debug, Clone)]
pub struct SubstringQuery {
    matcher: Matcher,
}

impl SubstringQuery {
    /// Prepares a search for `needle`.
    ///
    /// With [`CaseMode::AsciiInsensitive`] the needle's ASCII letters are
    /// lowercased. An empty needle matches every entry; a needle containing
    /// NUL matches nothing.
    pub fn new(needle: &str, case: CaseMode) -> Self {
        let bytes = needle.as_bytes();
        let matcher = if bytes.is_empty() {
            Matcher::All
        } else if bytes.contains(&0) {
            Matcher::Nothing
        } else {
            let finder = match case {
                CaseMode::Sensitive => memmem::Finder::new(bytes).into_owned(),
                CaseMode::AsciiInsensitive => {
                    memmem::Finder::new(&bytes.to_ascii_lowercase()).into_owned()
                }
            };
            Matcher::Needle(Box::new(finder))
        };
        Self { matcher }
    }
}

/// Returns a copy of `buf` with ASCII letters lowercased.
///
/// The copy has the same length, so offsets into `buf` stay valid for it.
pub fn fold_ascii_lowercase(buf: &[u8]) -> Vec<u8> {
    buf.to_ascii_lowercase()
}

// ---------------------------------------------------------------------------
// Cancellation
// ---------------------------------------------------------------------------

/// A search was cancelled because its generation was superseded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

impl fmt::Display for Cancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "search cancelled by a newer query")
    }
}

impl std::error::Error for Cancelled {}

/// A query generation counter; bumping it cancels every token taken before.
#[derive(Debug, Default)]
pub struct Generation(AtomicU64);

impl Generation {
    /// A counter starting at zero.
    pub fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    /// The current generation.
    pub fn current(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }

    /// Advances the generation, cancelling outstanding tokens, and returns
    /// the new value.
    pub fn bump(&self) -> u64 {
        self.0.fetch_add(1, Ordering::AcqRel).wrapping_add(1)
    }

    /// A token for the current generation.
    pub fn token(&self) -> CancelToken<'_> {
        CancelToken {
            generation: Some(&self.0),
            captured: self.current(),
        }
    }
}

/// Captures a generation; cancelled once that generation is superseded.
#[derive(Debug, Clone, Copy)]
pub struct CancelToken<'a> {
    generation: Option<&'a AtomicU64>,
    captured: u64,
}

impl CancelToken<'static> {
    /// A token that is never cancelled.
    pub fn never() -> Self {
        Self {
            generation: None,
            captured: 0,
        }
    }
}

impl CancelToken<'_> {
    /// Whether the generation has moved on since the token was taken.
    pub fn is_cancelled(&self) -> bool {
        self.generation
            .is_some_and(|g| g.load(Ordering::Acquire) != self.captured)
    }

    fn check(&self) -> Result<(), Cancelled> {
        if self.is_cancelled() {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Scan
// ---------------------------------------------------------------------------

/// Returns the IDs of entries containing the query, in ascending order,
/// using [`DEFAULT_CHUNK_ENTRIES`] entries per parallel chunk.
///
/// Runs on the current rayon pool; use `ThreadPool::install` to choose the
/// thread count.
pub fn search(
    col: NameColumn<'_>,
    query: &SubstringQuery,
    token: &CancelToken<'_>,
) -> Result<Vec<u32>, Cancelled> {
    search_chunked(col, query, token, DEFAULT_CHUNK_ENTRIES)
}

/// Like [`search`], with `chunk_entries` entries per parallel chunk (0 is
/// treated as 1). The result does not depend on the chunk size.
pub fn search_chunked(
    col: NameColumn<'_>,
    query: &SubstringQuery,
    token: &CancelToken<'_>,
    chunk_entries: usize,
) -> Result<Vec<u32>, Cancelled> {
    let chunk = chunk_entries.max(1);
    let parts: Vec<Vec<u32>> = (0..chunk_count(col.len(), chunk))
        .into_par_iter()
        .map(|c| {
            let (a, b) = chunk_bounds(col.len(), chunk, c);
            let mut ids = Vec::new();
            scan_chunk(col, query, token, a, b, &mut |lo, hi| {
                // IDs fit in u32: offsets are u32 and strictly increasing, so
                // there are at most 2^32 entries.
                ids.extend((lo..hi).map(|i| i as u32));
            })?;
            Ok(ids)
        })
        .collect::<Result<_, Cancelled>>()?;
    let total = parts.iter().map(Vec::len).sum();
    let mut out = Vec::with_capacity(total);
    for p in parts {
        out.extend_from_slice(&p);
    }
    Ok(out)
}

/// Returns the number of entries containing the query.
pub fn count(
    col: NameColumn<'_>,
    query: &SubstringQuery,
    token: &CancelToken<'_>,
) -> Result<usize, Cancelled> {
    let chunk = DEFAULT_CHUNK_ENTRIES;
    (0..chunk_count(col.len(), chunk))
        .into_par_iter()
        .map(|c| {
            let (a, b) = chunk_bounds(col.len(), chunk, c);
            let mut n = 0;
            scan_chunk(col, query, token, a, b, &mut |lo, hi| n += hi - lo)?;
            Ok(n)
        })
        .try_reduce(|| 0, |x, y| Ok(x + y))
}

fn chunk_count(len: usize, chunk: usize) -> usize {
    len.div_ceil(chunk)
}

fn chunk_bounds(len: usize, chunk: usize, c: usize) -> (usize, usize) {
    let a = c * chunk;
    (a, (a + chunk).min(len))
}

/// Scans entries `[a, b)`, reporting matching entries to `hit` as ID ranges
/// `[lo, hi)` in ascending order. Checks `token` before the chunk and before
/// every [`CHECK_INTERVAL`] entries.
fn scan_chunk(
    col: NameColumn<'_>,
    query: &SubstringQuery,
    token: &CancelToken<'_>,
    a: usize,
    b: usize,
    hit: &mut impl FnMut(usize, usize),
) -> Result<(), Cancelled> {
    token.check()?;
    let mut lo = a;
    while lo < b {
        if lo != a {
            token.check()?;
        }
        let hi = (lo + CHECK_INTERVAL).min(b);
        match &query.matcher {
            Matcher::All => hit(lo, hi),
            Matcher::Nothing => {}
            Matcher::Needle(finder) => scan_range(col, finder, lo, hi, hit),
        }
        lo = hi;
    }
    Ok(())
}

/// Runs `finder` over the bytes of entries `[a, b)` (with `a < b`) and
/// reports each matching entry once.
fn scan_range(
    col: NameColumn<'_>,
    finder: &memmem::Finder<'_>,
    a: usize,
    b: usize,
    hit: &mut impl FnMut(usize, usize),
) {
    let end = col.end(b - 1);
    let mut pos = col.start(a);
    let mut entry = a;
    while pos < end {
        let Some(rel) = finder.find(&col.buf[pos..end]) else {
            break;
        };
        entry = locate(col.offsets, entry, b, pos + rel);
        hit(entry, entry + 1);
        entry += 1;
        if entry >= b {
            break;
        }
        pos = col.start(entry);
    }
}

/// Returns the entry in `[i, b)` containing byte `at`, given
/// `offsets[i] <= at`: the largest `j` with `offsets[j] <= at`.
///
/// Gallops forward from `i` and then binary-searches, so dense hits cost a
/// step or two and sparse hits a logarithmic search.
fn locate(offsets: &[u32], i: usize, b: usize, at: usize) -> usize {
    let mut lo = i;
    let mut step = 1;
    let hi = loop {
        let probe = lo + step;
        if probe >= b {
            break b;
        }
        if offsets[probe] as usize <= at {
            lo = probe;
            step *= 2;
        } else {
            break probe;
        }
    };
    lo + offsets[lo + 1..hi].partition_point(|&o| o as usize <= at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locate_finds_containing_entry() {
        let offsets = [0_u32, 3, 5, 9, 10, 20];
        for at in 0..25 {
            let want = offsets.iter().rposition(|&o| o as usize <= at).unwrap();
            assert_eq!(locate(&offsets, 0, offsets.len(), at), want, "at {at}");
        }
        assert_eq!(locate(&offsets, 2, 4, 15), 3);
    }
}
