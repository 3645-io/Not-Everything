//! A file-system entry as reported by an enumerator or change source.

use crate::ids::FileKey;

/// What kind of file-system object an entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntryKind {
    /// A regular file.
    File,
    /// A directory (folder).
    Dir,
    /// A symbolic link. Links are indexed but never followed.
    Symlink,
}

/// Attribute flags of an entry, packed into one byte.
///
/// A small hand-rolled bitflag set: combine flags with [`insert`](Self::insert)
/// or `|`, test them with [`contains`](Self::contains).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct EntryFlags(u8);

impl EntryFlags {
    /// The entry is hidden (dot-file or hidden attribute).
    pub const HIDDEN: EntryFlags = EntryFlags(1 << 0);
    /// The entry is a system file.
    pub const SYSTEM: EntryFlags = EntryFlags(1 << 1);
    /// The entry is locked (immutable) and cannot be modified.
    pub const LOCKED: EntryFlags = EntryFlags(1 << 2);
    /// The entry is dataless: cloud-only, its contents not downloaded.
    /// Such entries must never be materialized or recursed into.
    pub const DATALESS: EntryFlags = EntryFlags(1 << 3);

    /// All flags this type defines.
    const ALL_BITS: u8 = 0b1111;

    /// The empty set: no flags.
    pub const fn empty() -> Self {
        EntryFlags(0)
    }

    /// Returns `true` if no flag is set.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The raw bit pattern.
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Builds a set from raw bits, dropping any bit that is not a known flag.
    pub const fn from_bits_truncate(bits: u8) -> Self {
        EntryFlags(bits & Self::ALL_BITS)
    }

    /// Returns `true` if every flag in `other` is set in `self`.
    pub const fn contains(self, other: EntryFlags) -> bool {
        self.0 & other.0 == other.0
    }

    /// Sets every flag in `other`.
    pub fn insert(&mut self, other: EntryFlags) {
        self.0 |= other.0;
    }

    /// Clears every flag in `other`.
    pub fn remove(&mut self, other: EntryFlags) {
        self.0 &= !other.0;
    }

    /// Sets or clears the flags in `other` depending on `value`.
    pub fn set(&mut self, other: EntryFlags, value: bool) {
        if value {
            self.insert(other);
        } else {
            self.remove(other);
        }
    }
}

impl std::ops::BitOr for EntryFlags {
    type Output = EntryFlags;

    fn bitor(self, rhs: EntryFlags) -> EntryFlags {
        EntryFlags(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for EntryFlags {
    fn bitor_assign(&mut self, rhs: EntryFlags) {
        self.0 |= rhs.0;
    }
}

/// One file-system object with its metadata, as handed to the index.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Entry {
    /// Stable identity of this entry.
    pub key: FileKey,
    /// Identity of the containing directory; `None` for a volume root.
    pub parent: Option<FileKey>,
    /// The entry's own name (one path component, not a full path).
    pub name: String,
    /// File, directory or symlink.
    pub kind: EntryKind,
    /// Attribute flags.
    pub flags: EntryFlags,
    /// Size in bytes (0 for directories unless computed).
    pub size: u64,
    /// Last modification time, seconds since the Unix epoch.
    pub modified: i64,
    /// Creation time, seconds since the Unix epoch.
    pub created: i64,
}
