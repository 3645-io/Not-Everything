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
///
/// The IDs from [`VolumeId::OFFLINE_FIRST`] to [`VolumeId::OFFLINE_LAST`]
/// are reserved for volumes that were unmounted and are kept in the index
/// as offline (run 2.4): the engine moves an unmounted volume's entries
/// onto one of them, so the ID the system gives the next mounted volume
/// (a macOS device number is reused as soon as its disk is gone) never
/// meets them. Platform crates key live volumes outside the range: macOS
/// device numbers of disks have a major number below 255.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VolumeId(pub u32);

impl VolumeId {
    /// The first ID reserved for offline volumes.
    pub const OFFLINE_FIRST: u32 = 0xFF00_0000;
    /// The last ID reserved for offline volumes (`u32::MAX`, which is
    /// `NODEV` on macOS, is not one).
    pub const OFFLINE_LAST: u32 = 0xFFFF_FFFE;

    /// The `n`th offline volume ID (from 0), or `None` past the range.
    pub const fn offline(n: u32) -> Option<VolumeId> {
        if n <= Self::OFFLINE_LAST - Self::OFFLINE_FIRST {
            Some(VolumeId(Self::OFFLINE_FIRST + n))
        } else {
            None
        }
    }

    /// Whether this ID is one of the offline volume IDs.
    pub const fn is_offline(self) -> bool {
        self.0 >= Self::OFFLINE_FIRST && self.0 <= Self::OFFLINE_LAST
    }
}

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

#[cfg(test)]
mod tests {
    use super::VolumeId;

    #[test]
    fn offline_ids_are_a_reserved_range() {
        assert_eq!(VolumeId::offline(0), Some(VolumeId(0xFF00_0000)));
        assert_eq!(VolumeId::offline(0x00FF_FFFE), Some(VolumeId(0xFFFF_FFFE)));
        assert_eq!(VolumeId::offline(0x00FF_FFFF), None);
        assert!(VolumeId(0xFF00_0000).is_offline());
        assert!(VolumeId(0xFFFF_FFFE).is_offline());
        assert!(!VolumeId(u32::MAX).is_offline(), "NODEV");
        // Disk device numbers (major 1) and a high-major one like devfs.
        for dev in [0x0100_000D, 0x0100_0019, 0xBF00_0025, 1, 0] {
            assert!(!VolumeId(dev).is_offline(), "{dev}");
        }
    }
}
