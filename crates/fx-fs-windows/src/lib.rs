//! File-system source for Windows.
//!
//! Placeholder crate. It will hold the NTFS USN enumerator
//! (`FSCTL_ENUM_USN_DATA`) and change-journal reader, plus a
//! `ReadDirectoryChangesW` fallback for volumes without a journal. It
//! implements the platform traits from `fx-fs`.
