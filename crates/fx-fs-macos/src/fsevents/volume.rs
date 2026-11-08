//! Safe wrappers for per-volume FSEvents queries (macOS only).

use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use super::sys;

/// The system-wide current FSEvents event ID (`FSEventsGetCurrentEventId`).
/// Never goes backwards.
#[must_use]
pub fn current_event_id() -> u64 {
    // SAFETY: takes no arguments and only reads the current event counter.
    unsafe { sys::FSEventsGetCurrentEventId() }
}

/// The UUID of the FSEvents database of device `dev`
/// (`FSEventsCopyUUIDForDevice`), or `None` if the device has no FSEvents
/// store (read-only volumes, for example).
///
/// The UUID changes whenever the volume's event history is reset, so a
/// saved event ID is only valid while the UUID stays the same.
#[must_use]
pub fn volume_uuid(dev: libc::dev_t) -> Option<[u8; 16]> {
    // SAFETY: any dev_t is accepted; the result is a +1 CFUUIDRef or null.
    let uuid = unsafe { sys::FSEventsCopyUUIDForDevice(dev) };
    if uuid.is_null() {
        return None;
    }
    // SAFETY: `uuid` is a valid, non-null CFUUIDRef we own.
    let bytes = unsafe { sys::CFUUIDGetUUIDBytes(uuid) };
    // SAFETY: balances the +1 reference from the Copy function; `uuid` is
    // not used afterwards.
    unsafe { sys::CFRelease(uuid.cast()) };
    Some(bytes.to_array())
}

/// The device (`st_dev`) holding `path`. Symlinks are followed, as `stat`
/// does. A missing path fails with [`io::ErrorKind::NotFound`].
pub fn device_of(path: &Path) -> io::Result<libc::dev_t> {
    let meta = std::fs::metadata(path)?;
    // std widens the native `dev_t` (i32 on macOS) to u64; narrowing it back
    // restores the original value exactly.
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let dev = meta.dev() as libc::dev_t;
    Ok(dev)
}

/// The last event ID on device `dev` before `unix_secs` seconds since the
/// Unix epoch (`FSEventsGetLastEventIdForDeviceBeforeTime`). Useful to
/// replay history from a point in time when no cursor was saved.
#[must_use]
pub fn last_event_id_before(dev: libc::dev_t, unix_secs: f64) -> u64 {
    let cf_time = unix_secs - sys::kCFAbsoluteTimeIntervalSince1970;
    // SAFETY: plain values in, plain value out; any dev_t and time are
    // accepted.
    unsafe { sys::FSEventsGetLastEventIdForDeviceBeforeTime(dev, cf_time) }
}
