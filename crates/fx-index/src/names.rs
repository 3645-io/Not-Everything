//! The name buffer: every file name in one contiguous allocation.
//!
//! Names are stored as UTF-8, joined by a single NUL byte, with one `u32`
//! offset per name pointing at its first byte (docs/PLAN.md, data-layout
//! table: "Name buffer (NUL-separated)" at roughly 17-18 B per entry plus a
//! 4 B "Name offset"). There is no trailing NUL after the last name, so
//! [`NameBuffer::bytes`] is exactly the names joined by `"\0"`.
//!
//! Names are kept byte-exact: the buffer never normalizes Unicode, so an NFC
//! and an NFD spelling of the same name are two distinct entries.
//!
//! Clean-room: written from docs/PLAN.md only.

use std::fmt;
use std::ops::Range;
use std::sync::Arc;

use crate::column::{Backing, Column};
use crate::parts::PartsError;

/// Whether `offsets` are exactly the start offsets of the pieces of `bytes`
/// split on NUL, with every piece non-empty. An empty offset list matches
/// only empty bytes.
pub(crate) fn nul_separated_layout_matches(bytes: &[u8], offsets: &[u32]) -> bool {
    if offsets.is_empty() {
        return bytes.is_empty();
    }
    let mut expected = 0usize;
    let mut k = 0usize;
    for piece in bytes.split(|&b| b == 0) {
        match offsets.get(k) {
            Some(&o) if o as usize == expected && !piece.is_empty() => {}
            _ => return false,
        }
        expected += piece.len() + 1;
        k += 1;
    }
    k == offsets.len()
}

/// Why [`NameBuffer::push`] refused a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameError {
    /// The name was the empty string.
    Empty,
    /// The name contained a NUL byte, which is the buffer's separator.
    ContainsNul,
    /// Adding the name would push a byte offset or the name count past
    /// `u32::MAX`.
    Overflow,
}

impl fmt::Display for NameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NameError::Empty => f.write_str("file name is empty"),
            NameError::ContainsNul => f.write_str("file name contains a NUL byte"),
            NameError::Overflow => f.write_str("name buffer exceeds the u32 offset space"),
        }
    }
}

impl std::error::Error for NameError {}

/// One contiguous buffer of NUL-separated UTF-8 names plus a `u32` offset
/// per name.
///
/// Indices returned by [`push`](NameBuffer::push) are dense, starting at 0.
/// [`bytes`](NameBuffer::bytes) and [`offsets`](NameBuffer::offsets) expose
/// the raw layout so a matcher can scan names without copying: name `i`
/// starts at `offsets()[i]` and runs to the next NUL or the end of `bytes()`.
///
/// A buffer is either owned or a read-only view of a saved index's memory
/// map ([`from_shared`](NameBuffer::from_shared)); a shared buffer becomes
/// owned on its first [`push`](NameBuffer::push).
#[derive(Clone, Default)]
pub struct NameBuffer {
    /// Names joined by `'\0'`; no trailing separator.
    data: Text,
    /// Byte offset of the first byte of each name, strictly ascending.
    offsets: Column<u32>,
}

/// The joined names: an owned `String`, so slicing a name back out needs
/// neither validation nor `unsafe`, or text in a [`Backing`], which answers
/// for a range of it.
#[derive(Clone)]
enum Text {
    Owned(String),
    Shared {
        backing: Arc<dyn Backing>,
        start: usize,
        len: usize,
    },
}

impl Default for Text {
    fn default() -> Self {
        Text::Owned(String::new())
    }
}

impl Text {
    fn bytes(&self) -> &[u8] {
        match self {
            Text::Owned(s) => s.as_bytes(),
            Text::Shared {
                backing,
                start,
                len,
            } => &backing.bytes()[*start..*start + *len],
        }
    }

    fn len(&self) -> usize {
        match self {
            Text::Owned(s) => s.len(),
            Text::Shared { len, .. } => *len,
        }
    }

    fn get(&self, range: Range<usize>) -> Option<&str> {
        match self {
            Text::Owned(s) => s.get(range),
            Text::Shared {
                backing,
                start,
                len,
            } => {
                if range.start > range.end || range.end > *len {
                    return None;
                }
                backing.text(start + range.start..start + range.end)
            }
        }
    }

    fn as_str(&self) -> &str {
        self.get(0..self.len())
            .expect("a shared name buffer is validated as UTF-8 when it is made")
    }

    fn to_mut(&mut self) -> &mut String {
        if let Text::Shared { .. } = self {
            *self = Text::Owned(self.as_str().to_owned());
        }
        match self {
            Text::Owned(s) => s,
            Text::Shared { .. } => unreachable!("converted to owned above"),
        }
    }
}

impl PartialEq for NameBuffer {
    fn eq(&self, other: &Self) -> bool {
        self.data.bytes() == other.data.bytes() && self.offsets[..] == other.offsets[..]
    }
}

impl Eq for NameBuffer {}

impl fmt::Debug for NameBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NameBuffer")
            .field("names", &self.offsets.len())
            .field("bytes", &self.data.len())
            .field("shared", &matches!(self.data, Text::Shared { .. }))
            .finish()
    }
}

impl NameBuffer {
    /// Creates an empty buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an empty buffer with room for `names` names totalling about
    /// `bytes` bytes (separators included).
    pub fn with_capacity(names: usize, bytes: usize) -> Self {
        Self {
            data: Text::Owned(String::with_capacity(bytes)),
            offsets: Column::Owned(Vec::with_capacity(names)),
        }
    }

    /// Appends a name and returns its index.
    ///
    /// Rejects empty names and names containing NUL. Returns
    /// [`NameError::Overflow`] instead of panicking if the buffer would grow
    /// past what a `u32` offset can address. A rejected push leaves the
    /// buffer unchanged.
    pub fn push(&mut self, name: &str) -> Result<u32, NameError> {
        if name.is_empty() {
            return Err(NameError::Empty);
        }
        if name.as_bytes().contains(&0) {
            return Err(NameError::ContainsNul);
        }
        let index = u32::try_from(self.offsets.len()).map_err(|_| NameError::Overflow)?;
        let separator = usize::from(self.data.len() != 0);
        let start = self
            .data
            .len()
            .checked_add(separator)
            .ok_or(NameError::Overflow)?;
        let end = start.checked_add(name.len()).ok_or(NameError::Overflow)?;
        // Keep the end of every name addressable too, so the whole buffer
        // stays within the u32 offset space.
        if u32::try_from(end).is_err() {
            return Err(NameError::Overflow);
        }
        let start = u32::try_from(start).map_err(|_| NameError::Overflow)?;

        let data = self.data.to_mut();
        if separator == 1 {
            data.push('\0');
        }
        data.push_str(name);
        self.offsets.push(start);
        Ok(index)
    }

    /// Rebuilds a buffer from its raw layout, as returned by
    /// [`bytes`](NameBuffer::bytes) and [`offsets`](NameBuffer::offsets).
    ///
    /// Validates instead of trusting the input: the bytes must be valid
    /// UTF-8 that fits the `u32` offset space, and splitting them on NUL
    /// must give exactly one non-empty name per offset, each starting at its
    /// offset. Never panics.
    pub fn from_raw_parts(bytes: Vec<u8>, offsets: Vec<u32>) -> Result<Self, PartsError> {
        if u32::try_from(bytes.len()).is_err() {
            return Err(PartsError::NameBuffer {
                reason: "name bytes exceed the u32 offset space",
            });
        }
        if !nul_separated_layout_matches(&bytes, &offsets) {
            return Err(PartsError::NameBuffer {
                reason: "name offsets do not match the NUL-separated name bytes",
            });
        }
        let data = String::from_utf8(bytes).map_err(|_| PartsError::NameBuffer {
            reason: "name bytes are not valid UTF-8",
        })?;
        Ok(Self {
            data: Text::Owned(data),
            offsets: Column::Owned(offsets),
        })
    }

    /// A buffer over the names at `bytes` and the `u32` offsets at
    /// `offsets` in `backing`, checked like
    /// [`from_raw_parts`](NameBuffer::from_raw_parts) but not copied: the
    /// layout must match and the backing must vouch for the text as UTF-8.
    pub fn from_shared(
        backing: &Arc<dyn Backing>,
        bytes: Range<usize>,
        offsets: Range<usize>,
    ) -> Result<Self, PartsError> {
        let offsets: Column<u32> =
            Column::shared(backing, offsets).ok_or(PartsError::NameBuffer {
                reason: "name offsets are out of bounds or not whole u32 values",
            })?;
        let Some(raw) = backing.bytes().get(bytes.clone()) else {
            return Err(PartsError::NameBuffer {
                reason: "name bytes are out of bounds",
            });
        };
        if u32::try_from(raw.len()).is_err() {
            return Err(PartsError::NameBuffer {
                reason: "name bytes exceed the u32 offset space",
            });
        }
        if !nul_separated_layout_matches(raw, &offsets) {
            return Err(PartsError::NameBuffer {
                reason: "name offsets do not match the NUL-separated name bytes",
            });
        }
        if backing.text(bytes.clone()).is_none() {
            return Err(PartsError::NameBuffer {
                reason: "name bytes are not valid UTF-8",
            });
        }
        Ok(Self {
            data: Text::Shared {
                backing: Arc::clone(backing),
                start: bytes.start,
                len: bytes.len(),
            },
            offsets,
        })
    }

    /// Returns name `index`.
    ///
    /// # Panics
    ///
    /// Panics if `index >= self.len()`, like slice indexing. Use
    /// [`try_get`](NameBuffer::try_get) for a checked lookup.
    pub fn get(&self, index: u32) -> &str {
        match self.try_get(index) {
            Some(name) => name,
            None => panic!(
                "name index {index} out of range for NameBuffer of length {}",
                self.len()
            ),
        }
    }

    /// Returns name `index`, or `None` if it is out of range.
    pub fn try_get(&self, index: u32) -> Option<&str> {
        let i = usize::try_from(index).ok()?;
        // One look at a shared column, not two.
        let offsets: &[u32] = &self.offsets;
        let start = *offsets.get(i)? as usize;
        let end = match offsets.get(i + 1) {
            // The next name starts right after this name's NUL separator.
            Some(&next) => next as usize - 1,
            None => self.data.len(),
        };
        self.data.get(start..end)
    }

    /// Number of names in the buffer.
    pub fn len(&self) -> usize {
        self.offsets.len()
    }

    /// Whether the buffer holds no names.
    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }

    /// The raw buffer: all names joined by a single NUL byte, with no
    /// trailing NUL.
    pub fn bytes(&self) -> &[u8] {
        self.data.bytes()
    }

    /// Start offset of each name in [`bytes`](NameBuffer::bytes), one per
    /// name, strictly ascending.
    pub fn offsets(&self) -> &[u32] {
        &self.offsets
    }

    /// Iterates over all names in index order.
    pub fn iter(&self) -> impl Iterator<Item = &str> + '_ {
        // Names never contain NUL, so splitting on it recovers them exactly.
        // An empty buffer would yield one empty piece, so skip that case.
        let n = self.len();
        self.data.as_str().split('\0').take(n)
    }

    /// Heap memory owned by the buffer, in bytes (allocated capacity of the
    /// name bytes plus the offsets).
    pub fn heap_bytes(&self) -> usize {
        self.data_heap_bytes() + self.offsets_heap_bytes()
    }

    /// Allocated capacity of the name bytes alone.
    pub(crate) fn data_heap_bytes(&self) -> usize {
        match &self.data {
            Text::Owned(s) => s.capacity(),
            Text::Shared { len, .. } => *len,
        }
    }

    /// Allocated capacity of the offsets alone.
    pub(crate) fn offsets_heap_bytes(&self) -> usize {
        self.offsets.held_bytes()
    }

    /// Bytes of names and offsets served from a backing instead of the
    /// heap (both are still counted by [`heap_bytes`](NameBuffer::heap_bytes),
    /// which keeps its meaning for a shared buffer).
    pub fn mapped_bytes(&self) -> usize {
        let data = match &self.data {
            Text::Owned(_) => 0,
            Text::Shared { len, .. } => *len,
        };
        data + self.offsets.mapped_bytes()
    }

    /// Releases spare capacity.
    pub fn shrink_to_fit(&mut self) {
        if let Text::Owned(s) = &mut self.data {
            s.shrink_to_fit();
        }
        self.offsets.shrink_to_fit();
    }
}
