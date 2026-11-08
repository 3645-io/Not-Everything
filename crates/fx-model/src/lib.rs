//! Shared data types used across the workspace.
//!
//! This crate holds the platform-agnostic types every other crate speaks:
//! the file key, the volume ID, an index entry, a file-system change record
//! and the resume cursor used to catch up on changes after a restart.
//!
//! The platform traits (`VolumeEnumerator`, `ChangeSource`) live in `fx-fs`,
//! not here; this crate only defines the data they exchange.
//!
//! Clean-room: written from docs/PLAN.md ("Proposed architecture") only.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod change;
mod entry;
mod ids;

pub use change::{Change, ResumeCursor, StartOutcome};
pub use entry::{Entry, EntryFlags, EntryKind};
pub use ids::{EntryId, FileKey, VolumeId};
