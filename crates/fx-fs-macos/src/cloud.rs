//! Cloud-file policy: a scan must never trigger a cloud download.
//!
//! Files and folders managed by a file provider (iCloud Drive, Dropbox,
//! OneDrive, ...) can be *dataless*: only their metadata is on disk, and
//! the `SF_DATALESS` flag is set. Reading such an item, or listing such a
//! folder, would normally make the system download it ("materialize" it).
//! The scanner prevents that in three ways:
//!
//! * [`disable_dataless_materialization`] (macOS) switches materialization
//!   off for the whole process with `setiopolicy_np`;
//!   [`materialization_disabled`] reads the policy back with
//!   `getiopolicy_np`.
//! * [`is_dataless`] spots the `SF_DATALESS` flag so the walker never
//!   descends into a dataless folder.
//! * [`classify_open_error`] (macOS) turns the errors from opening or
//!   listing a directory into an [`OpenSkip`] reason. With materialization
//!   off, touching a dataless item fails with `EDEADLK`; that is a skip,
//!   never a retry.

use crate::attrbuf::SF_DATALESS;

/// True iff `SF_DATALESS` is set in a `st_flags` / `ATTR_CMN_FLAGS` word.
#[must_use]
pub fn is_dataless(flags: u32) -> bool {
    flags & SF_DATALESS != 0
}

/// Why a directory that could not be opened or listed is skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenSkip {
    /// `EDEADLK`: the item is dataless and would need a download.
    Dataless,
    /// `ENOENT`: the item disappeared between listing and opening.
    Vanished,
    /// `EACCES` or `EPERM`: no permission (for example, no Full Disk
    /// Access).
    PermissionDenied,
}

#[cfg(target_os = "macos")]
pub use macos::{classify_open_error, disable_dataless_materialization, materialization_disabled};

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::c_int;
    use std::io;

    use super::OpenSkip;

    /// `IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES` (`sys/resource.h`).
    const IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES: c_int = 3;
    /// `IOPOL_SCOPE_PROCESS` (`sys/resource.h`).
    const IOPOL_SCOPE_PROCESS: c_int = 0;
    /// `IOPOL_MATERIALIZE_DATALESS_FILES_OFF` (`sys/resource.h`).
    const IOPOL_MATERIALIZE_DATALESS_FILES_OFF: c_int = 1;

    unsafe extern "C" {
        fn setiopolicy_np(iotype: c_int, scope: c_int, policy: c_int) -> c_int;
        fn getiopolicy_np(iotype: c_int, scope: c_int) -> c_int;
    }

    /// Switches off dataless-file materialization for the whole process.
    /// Idempotent.
    ///
    /// # Errors
    ///
    /// The `setiopolicy_np` error. The walker's descend policy still never
    /// enters dataless folders if this fails.
    pub fn disable_dataless_materialization() -> io::Result<()> {
        // SAFETY: plain syscall wrapper with integer arguments.
        let rc = unsafe {
            setiopolicy_np(
                IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
                IOPOL_SCOPE_PROCESS,
                IOPOL_MATERIALIZE_DATALESS_FILES_OFF,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// Reads the process's materialization policy back with
    /// `getiopolicy_np`; true if it is
    /// `IOPOL_MATERIALIZE_DATALESS_FILES_OFF`.
    ///
    /// # Errors
    ///
    /// The `getiopolicy_np` error.
    pub fn materialization_disabled() -> io::Result<bool> {
        // SAFETY: plain syscall wrapper with integer arguments.
        let policy = unsafe {
            getiopolicy_np(
                IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
                IOPOL_SCOPE_PROCESS,
            )
        };
        if policy < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(policy == IOPOL_MATERIALIZE_DATALESS_FILES_OFF)
    }

    /// Maps an errno from opening or listing a directory to a skip reason:
    /// `EDEADLK` -> [`OpenSkip::Dataless`], `ENOENT` ->
    /// [`OpenSkip::Vanished`], `EACCES` / `EPERM` ->
    /// [`OpenSkip::PermissionDenied`]. Any other errno is a real error and
    /// returns `None`. None of these is ever retried.
    #[must_use]
    pub fn classify_open_error(errno: i32) -> Option<OpenSkip> {
        match errno {
            libc::EDEADLK => Some(OpenSkip::Dataless),
            libc::ENOENT => Some(OpenSkip::Vanished),
            libc::EACCES | libc::EPERM => Some(OpenSkip::PermissionDenied),
            _ => None,
        }
    }
}
