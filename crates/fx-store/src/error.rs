//! Column identities and the errors of the checkpoint writer and loader.

use std::fmt;
use std::io;
use std::path::PathBuf;

use crate::layout::{
    KIND_CREATED, KIND_EXT_BYTES, KIND_EXT_IDS, KIND_EXT_OFFSETS, KIND_FILE_IDS, KIND_LIVE,
    KIND_META, KIND_MODIFIED, KIND_NAME_BYTES, KIND_NAME_OFFSETS, KIND_ORDER, KIND_PARENTS,
    KIND_VOLUMES,
};

/// One stored column: an `fx_index::IndexParts` array or a saved order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Column {
    /// Names joined by single NULs (`u8`).
    NameBytes,
    /// Start of each entry's name (`u32`).
    NameOffsets,
    /// Parent entry ID, `u32::MAX` for a root (`u32`).
    Parents,
    /// Volume ID per entry (`u32`).
    Volumes,
    /// File ID per entry (`u64`).
    FileIds,
    /// Packed size, kind and flags (`u64`).
    Meta,
    /// Modification time in seconds (`i64`).
    Modified,
    /// Creation time in seconds (`i64`).
    Created,
    /// Extension ID per entry (`u16`).
    ExtIds,
    /// Extension table bytes (`u8`).
    ExtBytes,
    /// Extension table offsets (`u32`).
    ExtOffsets,
    /// Live bitmap words (`u64`).
    Live,
    /// A saved order (`u32` permutation) with its sort key code.
    Order(u8),
}

impl Column {
    /// The 12 required columns, in column table order.
    pub const REQUIRED: [Column; 12] = [
        Column::NameBytes,
        Column::NameOffsets,
        Column::Parents,
        Column::Volumes,
        Column::FileIds,
        Column::Meta,
        Column::Modified,
        Column::Created,
        Column::ExtIds,
        Column::ExtBytes,
        Column::ExtOffsets,
        Column::Live,
    ];

    /// The `u16` kind stored in the column's record.
    pub fn kind(self) -> u16 {
        match self {
            Column::NameBytes => KIND_NAME_BYTES,
            Column::NameOffsets => KIND_NAME_OFFSETS,
            Column::Parents => KIND_PARENTS,
            Column::Volumes => KIND_VOLUMES,
            Column::FileIds => KIND_FILE_IDS,
            Column::Meta => KIND_META,
            Column::Modified => KIND_MODIFIED,
            Column::Created => KIND_CREATED,
            Column::ExtIds => KIND_EXT_IDS,
            Column::ExtBytes => KIND_EXT_BYTES,
            Column::ExtOffsets => KIND_EXT_OFFSETS,
            Column::Live => KIND_LIVE,
            Column::Order(_) => KIND_ORDER,
        }
    }

    /// The column for a stored kind and key code, `None` for an unknown kind.
    pub fn from_kind(kind: u16, key: u8) -> Option<Column> {
        if kind == KIND_ORDER {
            return Some(Column::Order(key));
        }
        Column::REQUIRED.into_iter().find(|c| c.kind() == kind)
    }

    /// Position among [`REQUIRED`](Self::REQUIRED), `None` for an order.
    pub fn required_index(self) -> Option<usize> {
        match self {
            Column::Order(_) => None,
            c => Some(c.kind() as usize - 1),
        }
    }

    /// The order key code stored in the record (zero for non-orders).
    pub fn key_code(self) -> u8 {
        match self {
            Column::Order(k) => k,
            _ => 0,
        }
    }

    /// Size of one element in bytes.
    pub fn element_size(self) -> usize {
        match self {
            Column::NameBytes | Column::ExtBytes => 1,
            Column::ExtIds => 2,
            Column::NameOffsets
            | Column::Parents
            | Column::Volumes
            | Column::ExtOffsets
            | Column::Order(_) => 4,
            Column::FileIds | Column::Meta | Column::Modified | Column::Created | Column::Live => 8,
        }
    }
}

impl fmt::Display for Column {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Column::NameBytes => "name_bytes",
            Column::NameOffsets => "name_offsets",
            Column::Parents => "parents",
            Column::Volumes => "volumes",
            Column::FileIds => "file_ids",
            Column::Meta => "meta",
            Column::Modified => "modified",
            Column::Created => "created",
            Column::ExtIds => "ext_ids",
            Column::ExtBytes => "ext_bytes",
            Column::ExtOffsets => "ext_offsets",
            Column::Live => "live",
            Column::Order(k) => return write!(f, "order {k}"),
        };
        f.write_str(name)
    }
}

/// How a saved order breaks the order contract in [`crate::layout`]: it
/// must be a permutation of the live entry IDs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderProblem {
    /// The order's length is not the live entry count.
    Length {
        /// The live entry count.
        expected: usize,
        /// Number of elements given.
        found: usize,
    },
    /// An element is not below `id_bound`.
    OutOfRange {
        /// The offending ID.
        id: u32,
        /// The index's `id_bound`.
        id_bound: usize,
    },
    /// An element names a removed (not live) entry.
    DeadId {
        /// The offending ID.
        id: u32,
    },
    /// An element appears more than once.
    DuplicateId {
        /// The offending ID.
        id: u32,
    },
}

impl fmt::Display for OrderProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OrderProblem::Length { expected, found } => {
                write!(f, "has {found} elements, expected {expected} live entries")
            }
            OrderProblem::OutOfRange { id, id_bound } => {
                write!(f, "holds entry ID {id}, not below id_bound {id_bound}")
            }
            OrderProblem::DeadId { id } => write!(f, "holds removed entry ID {id}"),
            OrderProblem::DuplicateId { id } => write!(f, "holds entry ID {id} twice"),
        }
    }
}

/// Why [`write_checkpoint`](crate::write_checkpoint) failed.
///
/// On every error the temp file is removed and the target is untouched,
/// except for [`DirSync`](Self::DirSync). [`Load`](Self::Load) and
/// [`Rebuild`](Self::Rebuild) come only from
/// [`open_or_rebuild`](crate::open_or_rebuild).
#[derive(Debug)]
pub enum StoreError {
    /// Creating, writing, syncing or renaming the temp file failed.
    Io(io::Error),
    /// The new file is in place, but syncing its directory failed, so the
    /// rename may not survive a power loss.
    DirSync(io::Error),
    /// The host is big-endian; the format is little-endian only.
    BigEndianHost,
    /// A per-entry column's length differs from `parents.len()`, or the
    /// live bitmap is too short.
    ColumnLength {
        /// The offending column.
        column: Column,
        /// Expected number of elements (minimum for the live bitmap).
        expected: usize,
        /// Number of elements given.
        found: usize,
    },
    /// More entries than `u32` entry IDs can address.
    TooManyEntries {
        /// Number of entries given.
        count: usize,
    },
    /// Two saved orders share a key code.
    DuplicateOrder {
        /// The key code.
        key: u8,
    },
    /// A saved order is not a permutation of the live entry IDs (see the
    /// order contract in [`crate::layout`]).
    InvalidOrder {
        /// The order's key code.
        key: u8,
        /// What is wrong with it.
        problem: OrderProblem,
    },
    /// Two volume records share a volume ID.
    DuplicateVolume {
        /// The volume ID.
        volume: u32,
    },
    /// Too many volumes or orders for the header.
    HeaderTooLarge,
    /// [`open_or_rebuild`](crate::open_or_rebuild) could not read the
    /// existing file for a reason that a rebuild would not fix (an I/O
    /// error other than "not found"), or the freshly written checkpoint
    /// failed to reopen.
    Load(LoadError),
    /// The caller's rebuild closure in
    /// [`open_or_rebuild`](crate::open_or_rebuild) failed. Nothing was
    /// written; any old file is untouched.
    Rebuild(Box<dyn std::error::Error + Send + Sync>),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Io(e) => write!(f, "checkpoint write failed: {e}"),
            StoreError::DirSync(e) => write!(f, "checkpoint directory sync failed: {e}"),
            StoreError::BigEndianHost => {
                f.write_str("index format is little-endian; big-endian hosts are unsupported")
            }
            StoreError::ColumnLength {
                column,
                expected,
                found,
            } => write!(
                f,
                "column {column} has {found} elements, expected {expected}"
            ),
            StoreError::TooManyEntries { count } => {
                write!(f, "{count} entries exceed the u32 entry ID range")
            }
            StoreError::DuplicateOrder { key } => write!(f, "two saved orders use key {key}"),
            StoreError::InvalidOrder { key, problem } => {
                write!(f, "saved order {key} {problem}")
            }
            StoreError::DuplicateVolume { volume } => {
                write!(f, "volume {volume} appears twice in the volume table")
            }
            StoreError::HeaderTooLarge => f.write_str("too many volumes or orders for the header"),
            StoreError::Load(e) => write!(f, "cannot open index checkpoint: {e}"),
            StoreError::Rebuild(e) => write!(f, "index rebuild failed: {e}"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StoreError::Io(e) | StoreError::DirSync(e) => Some(e),
            StoreError::Load(e) => Some(e),
            StoreError::Rebuild(e) => Some(e.as_ref()),
            _ => None,
        }
    }
}

impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        StoreError::Io(e)
    }
}

/// Why [`MappedIndex::open`](crate::MappedIndex::open) or
/// [`MappedIndex::verify_checksums`](crate::MappedIndex::verify_checksums)
/// rejected a file.
///
/// Open checks, in order: the file exists, magic, version, endianness,
/// header length, header checksum, then the tables and column records.
/// A file shorter than any of these needs is [`Truncated`](Self::Truncated).
#[derive(Debug)]
pub enum LoadError {
    /// There is no file at the path.
    NotFound {
        /// The path.
        path: PathBuf,
    },
    /// Opening, reading metadata of, or mapping the file failed.
    Io {
        /// The underlying error.
        source: io::Error,
    },
    /// The file does not start with `FXSTORE1`.
    BadMagic {
        /// The first 8 bytes.
        found: [u8; 8],
    },
    /// The format version is not [`FORMAT_VERSION`](crate::layout::FORMAT_VERSION)
    /// (version 1 prototype files land here).
    UnsupportedVersion {
        /// The version in the file.
        found: u32,
    },
    /// The endianness marker is byte-swapped (written by a big-endian
    /// host), or this host is big-endian.
    WrongEndianness {
        /// The marker as read little-endian.
        found: u32,
    },
    /// The file is shorter than its header or its columns need.
    Truncated {
        /// File length in bytes.
        len: u64,
        /// Bytes needed.
        needed: u64,
    },
    /// The header checksum does not match the header and tables.
    HeaderChecksum {
        /// Checksum stored in the header.
        stored: u64,
        /// Checksum computed over the header.
        computed: u64,
    },
    /// A column's bytes do not match its checksum.
    ColumnChecksum {
        /// The column.
        column: Column,
    },
    /// The header or tables are inconsistent (bad lengths, unknown or
    /// missing column, overlap, misalignment, wrong element size, ...).
    Malformed {
        /// What is wrong.
        reason: String,
    },
}

impl LoadError {
    pub(crate) fn malformed(reason: impl Into<String>) -> LoadError {
        LoadError::Malformed {
            reason: reason.into(),
        }
    }
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::NotFound { path } => write!(f, "no index file at {}", path.display()),
            LoadError::Io { source } => write!(f, "cannot read index file: {source}"),
            LoadError::BadMagic { found } => write!(
                f,
                "not an fx-store index file (magic {:?})",
                found.escape_ascii().to_string()
            ),
            LoadError::UnsupportedVersion { found } => write!(
                f,
                "unsupported index format version {found} (this build reads version {})",
                crate::layout::FORMAT_VERSION
            ),
            LoadError::WrongEndianness { found } => write!(
                f,
                "index file endianness marker {found:#010x} does not match this little-endian reader"
            ),
            LoadError::Truncated { len, needed } => {
                write!(f, "index file is truncated: {len} bytes, need {needed}")
            }
            LoadError::HeaderChecksum { stored, computed } => write!(
                f,
                "index header checksum mismatch (stored {stored:#018x}, computed {computed:#018x})"
            ),
            LoadError::ColumnChecksum { column } => {
                write!(f, "column {column} checksum mismatch (corrupt index file)")
            }
            LoadError::Malformed { reason } => write!(f, "malformed index file: {reason}"),
        }
    }
}

impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LoadError::Io { source } => Some(source),
            _ => None,
        }
    }
}

impl From<io::Error> for LoadError {
    fn from(source: io::Error) -> Self {
        LoadError::Io { source }
    }
}
