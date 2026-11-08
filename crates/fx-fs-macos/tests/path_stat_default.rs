//! `MacPathStat::default()` keeps the cloud-file guard (macOS only).
//!
//! `MacPathStat::new()` switches off dataless materialization for the
//! process, so stat'ing a path never starts a cloud download. A
//! `MacPathStat` made with `Default` must do the same.
//!
//! The policy is process-wide, so this test lives in its own test binary
//! (its own process) with nothing else in it: no other test can switch the
//! policy off first and hide a `Default` that skips it.

#![cfg(target_os = "macos")]

use std::ffi::c_int;

use fx_fs_macos::cloud::materialization_disabled;
use fx_fs_macos::path_stat::MacPathStat;

/// `IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES` (`sys/resource.h`).
const IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES: c_int = 3;
/// `IOPOL_SCOPE_PROCESS` (`sys/resource.h`).
const IOPOL_SCOPE_PROCESS: c_int = 0;
/// `IOPOL_MATERIALIZE_DATALESS_FILES_ON` (`sys/resource.h`).
const IOPOL_MATERIALIZE_DATALESS_FILES_ON: c_int = 2;

unsafe extern "C" {
    fn setiopolicy_np(iotype: c_int, scope: c_int, policy: c_int) -> c_int;
}

#[test]
fn default_switches_off_dataless_materialization() {
    // Start with materialization on, whatever the parent process left
    // (the system default can read back as off). This process touches no
    // files, so nothing is downloaded meanwhile.
    // SAFETY: plain syscall wrapper with integer arguments.
    let rc = unsafe {
        setiopolicy_np(
            IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
            IOPOL_SCOPE_PROCESS,
            IOPOL_MATERIALIZE_DATALESS_FILES_ON,
        )
    };
    assert_eq!(rc, 0, "setiopolicy_np: {}", std::io::Error::last_os_error());
    assert!(
        !materialization_disabled().expect("getiopolicy_np"),
        "the policy starts with materialization on"
    );

    let _stat = MacPathStat::default();

    assert!(
        materialization_disabled().expect("getiopolicy_np"),
        "MacPathStat::default() must switch off dataless materialization, as new() does"
    );
}
