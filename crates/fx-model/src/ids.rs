//! Identifiers: dense entry indices, volumes and stable file keys.

/// A dense index into the index's column store.
///
/// Entry IDs are positions, not stable identities: compaction may renumber
/// them. Use [`FileKey`] for an identity that survives restarts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntryId(pub u32);

impl EntryId {
    /// Sentinel meaning "no entry"; used as the parent of a volume root.
    pub const NONE: EntryId = EntryId(u32::MAX);

    /// Returns `true` if this is the [`EntryId::NONE`] sentinel.
    pub const fn is_none(self) -> bool {
        self.0 == u32::MAX
    }

    /// Returns the ID as a `usize` for indexing into columns.
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Identifies one indexed volume (a mounted file system).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VolumeId(pub u32);

/// The stable identity of a file: its volume plus the file system's own
/// file ID (inode number on macOS and Linux, file reference number on
/// Windows).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileKey {
    /// The volume the file lives on.
    pub volume: VolumeId,
    /// The file system's ID for the file, unique within its volume.
    pub file_id: u64,
}

impl FileKey {
    /// Builds a key from a volume and a file ID.
    pub const fn new(volume: VolumeId, file_id: u64) -> Self {
        FileKey { volume, file_id }
    }
}
