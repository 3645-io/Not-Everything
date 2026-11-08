//! FSEvents volume helpers in `fx_fs_macos::fsevents::volume` (macOS only).
//!
//! Contract pinned here:
//! - `current_event_id() -> u64` wraps `FSEventsGetCurrentEventId` and never
//!   goes backwards.
//! - `device_of(path) -> io::Result<libc::dev_t>` returns the `st_dev` of the
//!   path (no symlink games for directories) and fails with `NotFound` for a
//!   missing path.
//! - `volume_uuid(dev: libc::dev_t) -> Option<[u8; 16]>` wraps
//!   `FSEventsCopyUUIDForDevice` and is stable between calls.
//! - `last_event_id_before(dev, unix_secs) -> u64` wraps
//!   `FSEventsGetLastEventIdForDeviceBeforeTime`, taking seconds since the
//!   Unix epoch (any numeric type that `From<u32>` can build).
//!
//! The tests only read state; they create and remove nothing outside their
//! own temporary directory.

#![cfg(target_os = "macos")]

use std::io::ErrorKind;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fx_fs_macos::fsevents::cursor::{FsEventsCursor, decide_start};
use fx_fs_macos::fsevents::volume::{
    current_event_id, device_of, last_event_id_before, volume_uuid,
};
use fx_model::{ResumeCursor, StartOutcome};

/// A directory on the writable data volume (which keeps an FSEvents store).
fn data_dir() -> PathBuf {
    std::env::temp_dir()
}

fn now_secs() -> u32 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after 1970")
        .as_secs();
    u32::try_from(secs).expect("before 2106")
}

#[test]
fn current_event_id_is_monotonic() {
    let a = current_event_id();
    let b = current_event_id();
    assert!(b >= a, "{b} < {a}");
    assert_ne!(a, u64::MAX, "must not be the SinceNow sentinel");
}

#[test]
fn device_of_matches_stat() {
    for path in [Path::new("/"), data_dir().as_path()] {
        let dev = device_of(path).expect("device_of");
        let st = std::fs::metadata(path).expect("metadata");
        #[allow(clippy::cast_sign_loss, clippy::cast_lossless)]
        let dev_u64 = dev as u64;
        assert_eq!(dev_u64, st.dev(), "{}", path.display());
    }
}

#[test]
fn device_of_missing_path_is_not_found() {
    let missing = data_dir().join(format!(
        "fx-fsevents-volume-missing-{}-{}",
        std::process::id(),
        now_secs()
    ));
    let err = device_of(&missing).expect_err("missing path must fail");
    assert_eq!(err.kind(), ErrorKind::NotFound);
}

#[test]
fn data_volume_has_a_stable_uuid() {
    let dev = device_of(data_dir().as_path()).expect("device_of");
    let first = volume_uuid(dev).expect("data volume has an FSEvents store UUID");
    assert_ne!(first, [0u8; 16], "UUID must not be all zeroes");
    let second = volume_uuid(dev).expect("second lookup");
    assert_eq!(first, second, "UUID is stable between calls");
}

#[test]
fn last_event_id_before_is_bounded_and_monotonic_in_time() {
    let dev = device_of(data_dir().as_path()).expect("device_of");
    let now = now_secs();
    let hour_ago = last_event_id_before(dev, From::from(now - 3_600));
    let at_now = last_event_id_before(dev, From::from(now));
    let current = current_event_id();
    assert!(
        hour_ago <= at_now,
        "an hour ago ({hour_ago}) is after now ({at_now})"
    );
    assert!(
        at_now <= current,
        "last id before now ({at_now}) is past the current id ({current})"
    );
}

#[test]
fn live_cursor_resumes_and_detects_uuid_change() {
    let dev = device_of(data_dir().as_path()).expect("device_of");
    let uuid = volume_uuid(dev).expect("uuid");
    let saved = FsEventsCursor {
        event_id: current_event_id(),
        db_uuid: uuid,
    };
    let restored =
        ResumeCursor::from(FsEventsCursor::from_bytes(&saved.to_bytes()).expect("decode"));

    let now_id = current_event_id();
    assert_eq!(
        decide_start(Some(&restored), volume_uuid(dev), now_id),
        (StartOutcome::Resumed, saved.event_id)
    );

    let mut other = uuid;
    other[0] ^= 0xff;
    let foreign = ResumeCursor::FsEvents {
        event_id: saved.event_id,
        db_uuid: other,
    };
    assert_eq!(
        decide_start(Some(&foreign), volume_uuid(dev), now_id),
        (StartOutcome::GapDetected, u64::MAX)
    );
}
