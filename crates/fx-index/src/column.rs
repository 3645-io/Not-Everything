//! Base columns that are either owned or a read-only view of a shared
//! buffer, such as the memory map of a saved index.
//!
//! A saved index's columns are served from its map instead of being copied
//! to the heap at launch: the map's pages belong to the file, so the kernel
//! can drop and re-read them, and they don't count in the app's footprint.
//! Casts go through `bytemuck`, so this crate keeps `forbid(unsafe_code)`;
//! the one place that turns mapped bytes into `&str` without validating
//! them again is the [`Backing`] implementation, outside this crate.

use std::fmt;
use std::mem::size_of;
use std::ops::{Deref, Range};
use std::sync::Arc;

use bytemuck::Pod;

/// A read-only byte buffer that base columns can borrow from, such as the
/// memory map of a saved index. Its bytes must not change while a column
/// holds it.
pub trait Backing: Send + Sync + fmt::Debug {
    /// The whole buffer.
    fn bytes(&self) -> &[u8];

    /// `bytes()[range]` as UTF-8 text, or `None` when the range is out of
    /// bounds, doesn't start and end on character boundaries, or isn't valid
    /// UTF-8. Every name read from a shared name column comes through here,
    /// so an implementation should validate its text once and answer later
    /// calls without scanning the bytes again.
    fn text(&self, range: Range<usize>) -> Option<&str>;
}

/// A column of plain values: an owned vector, or a slice of a [`Backing`]
/// checked once for bounds and alignment.
pub(crate) enum Column<T> {
    Owned(Vec<T>),
    Shared {
        backing: Arc<dyn Backing>,
        /// Byte offset of the first value in the backing.
        start: usize,
        /// Number of values.
        len: usize,
    },
}

impl<T: Pod> Column<T> {
    /// A column over `backing.bytes()[range]`, without copying when the
    /// bytes are aligned for `T`, as a saved index's columns are. Misaligned
    /// bytes are copied to an owned column instead. `None` when the range is
    /// out of bounds or isn't a whole number of values.
    pub(crate) fn shared(backing: &Arc<dyn Backing>, range: Range<usize>) -> Option<Column<T>> {
        let bytes = backing.bytes().get(range.clone())?;
        if bytes.len() % size_of::<T>() != 0 {
            return None;
        }
        Some(match bytemuck::try_cast_slice::<u8, T>(bytes) {
            Ok(values) => Column::Shared {
                backing: Arc::clone(backing),
                start: range.start,
                len: values.len(),
            },
            Err(_) => Column::Owned(
                bytes
                    .chunks_exact(size_of::<T>())
                    .map(bytemuck::pod_read_unaligned)
                    .collect(),
            ),
        })
    }

    /// The owned vector, copying a shared column to the heap first.
    pub(crate) fn to_mut(&mut self) -> &mut Vec<T> {
        if let Column::Shared { .. } = self {
            *self = Column::Owned(self.to_vec());
        }
        match self {
            Column::Owned(values) => values,
            Column::Shared { .. } => unreachable!("converted to owned above"),
        }
    }

    /// Number of values. Unlike the slice's `len` through `Deref`, it
    /// doesn't look at a shared column's backing: [`Base::len`] asks for it
    /// on every row read.
    ///
    /// [`Base::len`]: crate::store::Base::len
    pub(crate) fn len(&self) -> usize {
        match self {
            Column::Owned(values) => values.len(),
            Column::Shared { len, .. } => *len,
        }
    }

    pub(crate) fn push(&mut self, value: T) {
        self.to_mut().push(value);
    }

    pub(crate) fn shrink_to_fit(&mut self) {
        if let Column::Owned(values) = self {
            values.shrink_to_fit();
        }
    }

    /// Bytes the column holds: the heap capacity of an owned column, the
    /// mapped length of a shared one.
    pub(crate) fn held_bytes(&self) -> usize {
        match self {
            Column::Owned(values) => values.capacity() * size_of::<T>(),
            Column::Shared { len, .. } => len * size_of::<T>(),
        }
    }

    /// Bytes served from a backing instead of the heap.
    pub(crate) fn mapped_bytes(&self) -> usize {
        match self {
            Column::Owned(_) => 0,
            Column::Shared { len, .. } => len * size_of::<T>(),
        }
    }
}

impl<T: Pod> Deref for Column<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        match self {
            Column::Owned(values) => values,
            Column::Shared {
                backing,
                start,
                len,
            } => bytemuck::cast_slice(&backing.bytes()[*start..*start + len * size_of::<T>()]),
        }
    }
}

impl<T> Default for Column<T> {
    fn default() -> Self {
        Column::Owned(Vec::new())
    }
}

impl<T> From<Vec<T>> for Column<T> {
    fn from(values: Vec<T>) -> Self {
        Column::Owned(values)
    }
}

impl<T: Clone> Clone for Column<T> {
    fn clone(&self) -> Self {
        match self {
            Column::Owned(values) => Column::Owned(values.clone()),
            Column::Shared {
                backing,
                start,
                len,
            } => Column::Shared {
                backing: Arc::clone(backing),
                start: *start,
                len: *len,
            },
        }
    }
}

impl<T: Pod + fmt::Debug> fmt::Debug for Column<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Column::Owned(values) => f.debug_tuple("Owned").field(&values.len()).finish(),
            Column::Shared { start, len, .. } => f
                .debug_struct("Shared")
                .field("start", start)
                .field("len", len)
                .finish(),
        }
    }
}
