//! Platform-independent file-system layer.
//!
//! This crate defines the platform traits that the per-OS scanners and
//! change sources implement ([`VolumeEnumerator`], [`ChangeSource`],
//! [`PermissionProbe`] with its [`AccessState`]), built on the shared
//! `fx-model` types (`Entry`, `ResumeCursor`, `Change`, `StartOutcome`).
//!
//! It also holds the platform-agnostic exclusion engine ([`Exclusions`],
//! compiled from an [`ExclusionConfig`]): hidden and system entries,
//! excluded folders, wildcard and regex patterns, and include-only folders.
//! Its [`Verdict`] tells a walker whether to emit an entry, skip just that
//! entry, skip a whole subtree, or traverse a directory without emitting it.
//!
//! Finally it holds the shared reconcile logic the engine applies to change
//! batches: [`PathStat`] resolves one path on disk (lstat semantics),
//! [`reconcile_path`] turns that result plus the indexed entry into a
//! [`PathReconcile`] (upsert, remove, and whether to re-scan a replaced
//! directory), and [`diff_subtree`] compares an indexed folder with a fresh
//! enumeration into a minimal [`SubtreeDiff`] that also lists moved entries.
//!
//! Clean-room: written from docs/PLAN.md ("Proposed architecture",
//! "Platform traits", the "Exclusions ... and include-only folders" feature
//! row) and voidtools' public Indexes support page only.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod exclude;
mod reconcile;
mod traits;

pub use exclude::{ExclusionConfig, ExclusionError, Exclusions, Verdict};
pub use reconcile::{PathReconcile, SubtreeDiff, diff_subtree, reconcile_path};
pub use traits::{AccessState, ChangeSource, PathStat, PermissionProbe, Result, VolumeEnumerator};
