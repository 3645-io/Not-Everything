//! On-disk persistence of the index: the checkpoint file format, its
//! checksums, the atomic checkpoint writer and the zero-copy mmap loader.
//!
//! The format is version 2, following `docs/PLAN.md` ("Saving and
//! loading", "Data layout", and the `ResumeCursor` in "Proposed
//! architecture"). [`layout`] documents every byte. A checkpoint holds:
//!
//! - every column of an `fx_index::IndexParts` (names, parents, keys,
//!   packed metadata, times, extension IDs and table, live bitmap), so
//!   entry IDs are preserved across a reload;
//! - one `fx_model::ResumeCursor` per volume;
//! - the clean-shutdown flag;
//! - zero or more saved orders: `u32` permutations of the live entry IDs,
//!   tagged with a `u8` key code the engine maps to a sort key.
//!
//! [`write_checkpoint`] writes a uniquely named temp file in the same
//! directory, flushes it with `sync_all` (`F_FULLFSYNC` on macOS), renames
//! it over the target and syncs the directory, so a crash leaves either the
//! old or the new file, never a torn one. On error the temp file is removed
//! and the old file is untouched. On Unix the file is private (mode 0600),
//! and [`create_private_dir`] creates a missing store folder private (0700)
//! but never changes a folder that already exists. [`make_dir_private`]
//! also tightens an existing folder; only the app calls it, on its own
//! folders.
//!
//! [`MappedIndex::open`] maps the file read-only with `memmap2` and checks
//! only the header, the volume table and the column table (O(1) in data
//! size); the accessors return zero-copy slices through `bytemuck`.
//! [`MappedIndex::verify_checksums`] checks every column, and
//! [`MappedIndex::to_index`] rebuilds an `fx_index::Index`.
//!
//! Old (such as the version 1 prototype's) and corrupt files are rejected
//! with a typed [`LoadError`]. [`open_or_rebuild`] is the startup policy
//! built on top: it loads a valid file (clean or not, reporting the flag),
//! and otherwise calls a caller-supplied rebuild (a scan), writes the
//! result as a new checkpoint and reports why the old file was rejected. It
//! also removes the target's own stale temp files of interrupted
//! checkpoints ([`cleanup_stale_temps`]).
//! Big-endian hosts are refused by both the writer and the loader.
//!
//! Clean-room: written from docs/PLAN.md and the in-repo code only.

mod checkpoint;
mod error;
pub mod layout;
mod mapped;
mod writer;

#[doc(hidden)]
pub use checkpoint::cleanup_stale_temps_older_than;
pub use checkpoint::{
    Opened, RebuiltIndex, RejectReason, STALE_TEMP_MIN_AGE, VerifyMode, cleanup_stale_temps,
    open_or_rebuild,
};
pub use error::{Column, LoadError, OrderProblem, StoreError};
pub use mapped::MappedIndex;
pub use writer::{
    PRIVATE_DIR_MODE, PRIVATE_FILE_MODE, SaveInput, create_private_dir, make_dir_private,
    temp_name_prefix, write_checkpoint,
};
