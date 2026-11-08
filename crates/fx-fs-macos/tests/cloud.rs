//! Tests for `fx_fs_macos::cloud` (cloud-file policy).
//!
//! Gate: "0 cloud downloads". The scanner switches off dataless
//! materialization for the process, never descends into `SF_DATALESS`
//! folders, and treats `EDEADLK` as "skip, don't retry".
//!
//! Contract pinned here:
//! - `is_dataless(flags: u32) -> bool` is pure and compiles on every host
//!   (CI also runs on Ubuntu): true iff `SF_DATALESS` (`0x4000_0000`, the
//!   same value as `attrbuf::SF_DATALESS`) is set in a `st_flags` /
//!   `ATTR_CMN_FLAGS` word.
//! - macOS only:
//!   - `disable_dataless_materialization() -> std::io::Result<()>` calls
//!     `setiopolicy_np(IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
//!     IOPOL_SCOPE_PROCESS, IOPOL_MATERIALIZE_DATALESS_FILES_OFF)`. It is
//!     idempotent.
//!   - `materialization_disabled() -> std::io::Result<bool>` reads the
//!     process policy back with `getiopolicy_np` and returns whether it is
//!     `IOPOL_MATERIALIZE_DATALESS_FILES_OFF`.
//!   - `classify_open_error(errno: i32) -> Option<OpenSkip>` maps errors
//!     from opening or listing a directory to a skip reason, never a retry:
//!     `EDEADLK` -> `OpenSkip::Dataless` (the item would need a download),
//!     `ENOENT` -> `OpenSkip::Vanished`, `EACCES` and `EPERM` ->
//!     `OpenSkip::PermissionDenied`. Any other errno returns `None` (a real
//!     error the caller reports). `OpenSkip` derives `Debug + Clone + Copy +
//!     PartialEq + Eq`.

use fx_fs_macos::cloud::is_dataless;

const SF_DATALESS: u32 = 0x4000_0000;
const UF_HIDDEN: u32 = 0x0000_8000;
const UF_COMPRESSED: u32 = 0x0000_0020;
const SF_FIRMLINK: u32 = 0x0080_0000;
const SF_RESTRICTED: u32 = 0x0008_0000;

#[test]
fn dataless_bit_is_detected() {
    assert!(is_dataless(SF_DATALESS));
    assert!(is_dataless(SF_DATALESS | UF_HIDDEN | UF_COMPRESSED));
    assert!(is_dataless(u32::MAX));
}

#[test]
fn other_flags_are_not_dataless() {
    assert!(!is_dataless(0));
    assert!(!is_dataless(UF_HIDDEN));
    assert!(!is_dataless(UF_COMPRESSED));
    assert!(!is_dataless(SF_FIRMLINK));
    assert!(!is_dataless(SF_RESTRICTED));
    assert!(!is_dataless(!SF_DATALESS));
}

#[test]
fn agrees_with_the_walker_constant() {
    assert_eq!(fx_fs_macos::attrbuf::SF_DATALESS, SF_DATALESS);
    assert!(is_dataless(fx_fs_macos::attrbuf::SF_DATALESS));
}

#[cfg(target_os = "macos")]
mod macos {
    use fx_fs_macos::cloud::{
        OpenSkip, classify_open_error, disable_dataless_materialization, materialization_disabled,
    };

    #[test]
    fn materialization_reads_back_off_after_disable() {
        disable_dataless_materialization().expect("setiopolicy_np");
        assert!(
            materialization_disabled().expect("getiopolicy_np"),
            "getiopolicy_np must report materialization off after disable"
        );
    }

    #[test]
    fn disable_is_idempotent() {
        disable_dataless_materialization().expect("first call");
        disable_dataless_materialization().expect("second call");
        assert!(materialization_disabled().expect("getiopolicy_np"));
    }

    #[test]
    fn edeadlk_is_skipped_as_dataless() {
        assert_eq!(classify_open_error(libc::EDEADLK), Some(OpenSkip::Dataless));
    }

    #[test]
    fn vanished_and_denied_entries_are_skipped() {
        assert_eq!(classify_open_error(libc::ENOENT), Some(OpenSkip::Vanished));
        assert_eq!(
            classify_open_error(libc::EACCES),
            Some(OpenSkip::PermissionDenied)
        );
        assert_eq!(
            classify_open_error(libc::EPERM),
            Some(OpenSkip::PermissionDenied)
        );
    }

    #[test]
    fn other_errors_are_not_skip_reasons() {
        for errno in [libc::EIO, libc::ENOMEM, libc::EMFILE, libc::EBADF, 0] {
            assert_eq!(classify_open_error(errno), None, "errno {errno}");
        }
    }

    #[test]
    fn open_error_classification_matches_io_error_kinds() {
        // Errors as `std::io::Error` produces them, e.g. from a failed open.
        let deadlk = std::io::Error::from_raw_os_error(libc::EDEADLK);
        assert_eq!(
            classify_open_error(deadlk.raw_os_error().unwrap()),
            Some(OpenSkip::Dataless)
        );
        let missing = std::fs::read_dir("/definitely/not/a/real/path/fx-cloud").unwrap_err();
        assert_eq!(
            classify_open_error(missing.raw_os_error().unwrap()),
            Some(OpenSkip::Vanished)
        );
    }
}
