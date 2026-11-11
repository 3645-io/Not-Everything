//! Per-volume FSEvents stream (macOS only).
//!
//! [`VolumeStream`] watches one whole volume with file-level events. It is
//! created with `FSEventStreamCreateRelativeToDevice` on the device holding
//! [`StreamConfig::watch_path`], so event IDs and the resume cursor belong to
//! that volume's FSEvents database. The stream runs on its own serial
//! dispatch queue (`FSEventStreamSetDispatchQueue`); the C callback turns
//! every event into a [`RawEvent`] and sends it through an
//! [`std::sync::mpsc`] channel.
//!
//! # Paths
//!
//! A device-relative stream reports paths relative to the volume root. They
//! are joined to the volume's mount point (`statfs` `f_mntonname`) to get an
//! absolute path. On macOS 10.15 and later the user data lives on the Data
//! volume, mounted at `/System/Volumes/Data`, and is reached from `/` through
//! firmlinks (`/Users`, `/private`, `/Applications`, ...). Observed on macOS
//! (Apple silicon): a file under `std::env::temp_dir()` (`/private/var/...`)
//! is on the Data volume, `statfs` reports `/System/Volumes/Data` as its
//! mount point, and the stream reports it relative to the Data volume root
//! (`private/var/...`). Joining gives `/System/Volumes/Data/private/var/...`,
//! which [`normalise_firmlinks`] maps back to the firmlinked `/private/var/...`
//! form using the firmlink table in `/usr/share/firmlinks` (with a built-in
//! copy of the standard entries as a fallback). Paths on the Data volume that
//! are not below a firmlink keep their `/System/Volumes/Data/...` form.
//!
//! File-level events (`kFSEventStreamCreateFlagFileEvents`) work with
//! device-relative streams. If creating the device-relative stream ever
//! fails, the stream falls back to `FSEventStreamCreate` rooted at the mount
//! point; that stream reports absolute paths, which go through the same
//! normalisation, but its event IDs are still the system-wide ones.
//!
//! # Filtering and the cursor
//!
//! With a [`StreamConfig::path_filter`], only events at or below the prefix
//! are delivered. The HistoryDone marker is always delivered. The cursor
//! ([`VolumeStream::cursor`], bookkeeping in [`CursorTracker`]) is always
//! safe to resume from: replaying from an older ID only repeats events.
//! While a resumed stream is still replaying it stays at the replay start,
//! because replay is not in ID order; the HistoryDone marker moves it to
//! the highest delivered ID or the marker's ID, whichever is larger, and
//! live events advance it from there. Events that are filtered out do not
//! move the cursor. A rescan event reported on the volume's mount point or
//! on an ancestor of the filter is delivered as a rescan of the filter's
//! folder: that is how FSEvents reports the events it dropped
//! (`MustScanSubDirs | UserDropped` on the volume root), and filtering it
//! out lost those changes (found by the soak).
//!
//! [`VolumeStream::mount_point`] is the `volume_root` to pass to
//! [`classify`](super::flags::classify).
//!
//! # Replay
//!
//! When the start decision is [`StartOutcome::Resumed`], FSEvents first
//! replays the volume's history after the saved event ID and then sends the
//! HistoryDone marker (reported with the mount point as its path); live
//! events follow. Events with an ID at or below the saved one are dropped
//! and counted in [`VolumeStream::dropped_before_cursor`].
//! Observed during replay: the history store is coalesced per path and
//! delivered roughly in path order (not ID order), and events that were
//! recorded below a directory which was renamed later are not replayed at
//! all; only the directory's own rename event (old and new name) is. So
//! during replay a directory rename event is delivered with
//! [`RawEvent::rescan_subtree`] set, telling the consumer to rescan the
//! subtree below both names. [`RawEvent::flags`] is always the exact kernel
//! word.
//!
//! # Shutdown
//!
//! [`VolumeStream::stop`] (also run by `Drop`) stops and invalidates the
//! stream, then waits on the serial queue so that a callback that was
//! already running has finished, and only then releases the stream and the
//! queue. The callback state is reference counted and outlives the stream,
//! so a late callback can never touch freed memory.

use std::ffi::{CStr, CString, OsStr, c_char, c_void};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::time::{Duration, Instant};

use fx_model::{ResumeCursor, StartOutcome};

use super::cursor::{CursorTracker, EVENT_ID_SINCE_NOW, decide_start};
use super::flags::EventFlags;
use super::sys;
use super::volume::{current_event_id, device_of, volume_uuid};
use crate::firmlinks::FirmlinkMap;

/// Settings for [`VolumeStream::start`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamConfig {
    /// Any path on the volume to watch; it selects the device.
    pub watch_path: PathBuf,
    /// Absolute path prefix: when set, only events at or below it are
    /// delivered (the HistoryDone marker is always delivered).
    pub path_filter: Option<PathBuf>,
    /// FSEvents coalescing latency (default 10 ms).
    pub latency: Duration,
    /// `kFSEventStreamCreateFlagNoDefer`: deliver the first event after a
    /// quiet period at once instead of after `latency`.
    pub no_defer: bool,
}

impl StreamConfig {
    /// Defaults: no filter, 10 ms latency, `no_defer` off.
    #[must_use]
    pub fn new(watch_path: PathBuf) -> StreamConfig {
        StreamConfig {
            watch_path,
            path_filter: None,
            latency: Duration::from_millis(10),
            no_defer: false,
        }
    }
}

/// One event as delivered by FSEvents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawEvent {
    /// Absolute path in its firmlinked form (`/private/var/...`, not
    /// `/System/Volumes/Data/private/var/...`).
    pub path: PathBuf,
    /// The exact kernel flag word; the stream never adds or clears bits.
    pub flags: EventFlags,
    /// Set by the stream during replay (before the HistoryDone marker) on
    /// directory renames (`ITEM_RENAMED | ITEM_IS_DIR`), because the
    /// history store drops the events recorded below a directory that was
    /// renamed later: the consumer must rescan the subtree. Always false
    /// for live events.
    pub rescan_subtree: bool,
    /// The FSEvents event ID.
    pub id: u64,
    /// When the callback received the event.
    pub received: Instant,
}

/// Mutable state shared with the callback.
struct Inner {
    /// `None` once the stream is stopped; dropping it disconnects the
    /// receiver.
    tx: Option<mpsc::Sender<RawEvent>>,
    /// The resume cursor ID, the replay state and the events dropped
    /// before the cursor; all change together under the lock.
    tracker: CursorTracker,
}

/// State the C callback reads through its `info` pointer.
struct Shared {
    inner: Mutex<Inner>,
    mount: PathBuf,
    filter: Option<PathBuf>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// A running FSEvents stream over one volume. See the module docs.
pub struct VolumeStream {
    stream: sys::FSEventStreamRef,
    queue: sys::dispatch_queue_t,
    shared: Arc<Shared>,
    rx: mpsc::Receiver<RawEvent>,
    uuid: [u8; 16],
}

// SAFETY: the stream and queue handles are only used through FSEvents and
// libdispatch calls that may be made from any thread (the callback runs on
// the stream's own queue); `stop` takes `&mut self`, so no two threads tear
// the stream down at once. `Shared` is `Send + Sync`.
unsafe impl Send for VolumeStream {}

impl VolumeStream {
    /// Starts a stream on the volume holding `config.watch_path`.
    ///
    /// The start point comes from [`decide_start`] with the volume's
    /// FSEvents database UUID and the current event ID: replay from `from`
    /// when it is still valid ([`StartOutcome::Resumed`]), otherwise stream
    /// from now.
    ///
    /// # Errors
    ///
    /// Fails if `watch_path` cannot be inspected or the stream cannot be
    /// created or started.
    pub fn start(
        config: StreamConfig,
        from: Option<&ResumeCursor>,
    ) -> io::Result<(VolumeStream, StartOutcome)> {
        let dev = device_of(&config.watch_path)?;
        let mount = mount_point(&config.watch_path)?;
        let uuid = volume_uuid(dev);
        let now = current_event_id();
        let (outcome, since_when) = decide_start(from, uuid, now);
        let replay = since_when != EVENT_ID_SINCE_NOW;

        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner {
                tx: Some(tx),
                tracker: if replay {
                    CursorTracker::replay(since_when)
                } else {
                    CursorTracker::since_now(now)
                },
            }),
            mount: normalise_firmlinks(&mount),
            filter: config.path_filter.as_deref().map(normalise_firmlinks),
        });

        let mut context = sys::FSEventStreamContext {
            version: 0,
            info: Arc::as_ptr(&shared).cast_mut().cast(),
            retain: None,
            release: None,
            copyDescription: None,
        };
        let mut flags = sys::kFSEventStreamCreateFlagFileEvents;
        if config.no_defer {
            flags |= sys::kFSEventStreamCreateFlagNoDefer;
        }
        let latency = config.latency.as_secs_f64();

        // The whole device: the empty path is the volume root.
        let stream = {
            let paths = CfArray::of_paths(&[OsStr::new("")])?;
            // SAFETY: `context` is valid for the call (FSEvents copies it);
            // `paths` is a live CFArray of CFStrings; the callback matches
            // `FSEventStreamCallback`, and `info` points into `shared`,
            // which outlives the stream (see `stop`).
            unsafe {
                sys::FSEventStreamCreateRelativeToDevice(
                    std::ptr::null(),
                    callback,
                    &raw mut context,
                    dev,
                    paths.0,
                    since_when,
                    latency,
                    flags,
                )
            }
        };
        let stream = if stream.is_null() {
            let paths = CfArray::of_paths(&[mount.as_os_str()])?;
            // SAFETY: as above, with an absolute path.
            unsafe {
                sys::FSEventStreamCreate(
                    std::ptr::null(),
                    callback,
                    &raw mut context,
                    paths.0,
                    since_when,
                    latency,
                    flags,
                )
            }
        } else {
            stream
        };
        if stream.is_null() {
            return Err(io::Error::other("FSEventStreamCreate failed"));
        }

        let label = CString::new("fx-fs-macos.fsevents").expect("no NUL in label");
        // SAFETY: a valid C string label and a null attribute (serial queue).
        let queue = unsafe { sys::dispatch_queue_create(label.as_ptr(), std::ptr::null_mut()) };
        if queue.is_null() {
            // SAFETY: the stream was created and never scheduled or started.
            unsafe { sys::FSEventStreamRelease(stream) };
            return Err(io::Error::other("dispatch_queue_create failed"));
        }
        // SAFETY: both handles are valid; the stream is not yet scheduled.
        unsafe { sys::FSEventStreamSetDispatchQueue(stream, queue) };

        let mut this = VolumeStream {
            stream,
            queue,
            shared,
            rx,
            uuid: uuid.unwrap_or([0; 16]),
        };
        // SAFETY: the stream is valid and scheduled on a queue.
        if unsafe { sys::FSEventStreamStart(this.stream) } == 0 {
            this.stop();
            return Err(io::Error::other("FSEventStreamStart failed"));
        }
        Ok((this, outcome))
    }

    /// Waits up to `timeout` for the next event.
    ///
    /// # Errors
    ///
    /// [`mpsc::RecvTimeoutError::Timeout`] if nothing arrived in time,
    /// [`mpsc::RecvTimeoutError::Disconnected`] once the stream is stopped
    /// and drained.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<RawEvent, mpsc::RecvTimeoutError> {
        self.rx.recv_timeout(timeout)
    }

    /// The events that are already waiting, without blocking.
    pub fn try_iter(&self) -> mpsc::TryIter<'_, RawEvent> {
        self.rx.try_iter()
    }

    /// The resume point: an event ID and the volume's FSEvents database
    /// UUID. Stays valid after [`VolumeStream::stop`].
    ///
    /// Before the HistoryDone marker of a resumed stream this returns the
    /// replay start (the saved cursor's event ID): replay arrives in path
    /// order, not ID order, so a higher replayed ID does not mean the lower
    /// ones were delivered. When the marker arrives the ID moves to the
    /// highest delivered ID or the marker's ID, whichever is larger; after
    /// that every delivered live event advances it. For a stream that
    /// starts from now it starts at the volume's current event ID. The ID
    /// and [`VolumeStream::history_done`] change together, so a cursor read
    /// before `history_done()` returned false was taken before HistoryDone.
    /// See [`CursorTracker`].
    ///
    /// Always the [`ResumeCursor::FsEvents`] variant.
    #[must_use]
    pub fn cursor(&self) -> ResumeCursor {
        ResumeCursor::FsEvents {
            event_id: self.shared.lock().tracker.cursor_id(),
            db_uuid: self.uuid,
        }
    }

    /// How many replayed events at or before the resume cursor the stream
    /// dropped instead of delivering (they were already applied). Always 0
    /// for a stream that starts from now. Events outside the
    /// [`StreamConfig::path_filter`] are not counted.
    #[must_use]
    pub fn dropped_before_cursor(&self) -> u64 {
        self.shared.lock().tracker.dropped_before_cursor()
    }

    /// True once every historical event was delivered (at once for a stream
    /// that starts from now).
    #[must_use]
    pub fn history_done(&self) -> bool {
        self.shared.lock().tracker.history_done()
    }

    /// The watched volume's mount path (`statfs` `f_mntonname`) in the same
    /// normalised form as the event paths, e.g. `/`, `/Volumes/X` or
    /// `/System/Volumes/Data`. Pass it as `volume_root` to
    /// [`classify`](super::flags::classify) so that an `Unmount` of the
    /// watched volume itself becomes a volume rescan.
    #[must_use]
    pub fn mount_point(&self) -> &Path {
        &self.shared.mount
    }

    /// Delivers every event that already happened, ignoring the latency.
    /// Returns after the callback has run for them. No-op once stopped.
    pub fn flush_sync(&self) {
        if !self.stream.is_null() {
            // SAFETY: the stream is valid and started; this is not called
            // from the stream's own queue.
            unsafe { sys::FSEventStreamFlushSync(self.stream) };
        }
    }

    /// Stops the stream and releases it. Events already in the channel can
    /// still be received; afterwards the channel reports `Disconnected`.
    /// Idempotent.
    pub fn stop(&mut self) {
        if self.stream.is_null() {
            return;
        }
        // From here on the callback delivers nothing and the cursor is
        // frozen.
        self.shared.lock().tx = None;
        // SAFETY: `stream` is valid; stopping a stream that never started
        // is allowed. After `Invalidate` no new callback is queued.
        unsafe {
            sys::FSEventStreamStop(self.stream);
            sys::FSEventStreamInvalidate(self.stream);
        }
        // SAFETY: `queue` is a valid serial queue and this is not running on
        // it, so this waits for any callback already queued or running.
        unsafe { sys::dispatch_sync_f(self.queue, std::ptr::null_mut(), noop) };
        // SAFETY: balances the create / queue-create references; neither
        // handle is used afterwards.
        unsafe {
            sys::FSEventStreamRelease(self.stream);
            sys::dispatch_release(self.queue.cast());
        }
        self.stream = std::ptr::null_mut();
        self.queue = std::ptr::null_mut();
    }
}

impl Drop for VolumeStream {
    fn drop(&mut self) {
        self.stop();
    }
}

unsafe extern "C" fn noop(_: *mut c_void) {}

/// The `FSEventStreamCallback`. Never unwinds into C.
unsafe extern "C" fn callback(
    _stream: sys::ConstFSEventStreamRef,
    info: *mut c_void,
    num_events: usize,
    event_paths: *mut c_void,
    event_flags: *const sys::FSEventStreamEventFlags,
    event_ids: *const sys::FSEventStreamEventId,
) {
    let received = Instant::now();
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if info.is_null() || num_events == 0 {
            return;
        }
        // SAFETY: `info` is the `Shared` the stream was created with; it is
        // kept alive by `VolumeStream` until the stream is released. Without
        // `UseCFTypes`, FSEvents passes `num_events` C strings, flags and
        // IDs, valid for the duration of the call.
        let (shared, paths, flags, ids) = unsafe {
            (
                &*info.cast::<Shared>(),
                std::slice::from_raw_parts(event_paths.cast::<*const c_char>(), num_events),
                std::slice::from_raw_parts(event_flags, num_events),
                std::slice::from_raw_parts(event_ids, num_events),
            )
        };
        deliver(shared, paths, flags, ids, received);
    }));
}

fn deliver(
    shared: &Shared,
    paths: &[*const c_char],
    flags: &[u32],
    ids: &[u64],
    received: Instant,
) {
    let mut inner = shared.lock();
    if inner.tx.is_none() {
        return;
    }
    for ((&raw_path, &bits), &id) in paths.iter().zip(flags).zip(ids) {
        let flags = EventFlags(bits);
        let marker = flags.contains(EventFlags::HISTORY_DONE);
        // The history store drops events recorded below a directory that
        // was renamed later, so during replay the subtree must be rescanned.
        let rescan_subtree = !marker
            && !inner.tracker.history_done()
            && flags.contains(EventFlags::ITEM_RENAMED | EventFlags::ITEM_IS_DIR);
        let path = if raw_path.is_null() {
            shared.mount.clone()
        } else {
            // SAFETY: FSEvents passes NUL-terminated strings.
            let bytes = unsafe { CStr::from_ptr(raw_path) }.to_bytes();
            absolute_path(&shared.mount, Path::new(OsStr::from_bytes(bytes)))
        };
        let path = if marker {
            path
        } else {
            match filter_event(path, flags, shared.filter.as_deref(), &shared.mount) {
                Some(path) => path,
                None => continue,
            }
        };
        if marker {
            inner.tracker.on_history_done(id);
        } else if !inner.tracker.on_event(id) {
            // At or before the replay start: already applied, counted in
            // `dropped_before_cursor`.
            continue;
        }
        if let Some(tx) = &inner.tx {
            let _ = tx.send(RawEvent {
                path,
                flags,
                rescan_subtree,
                id,
                received,
            });
        }
    }
}

/// Applies [`StreamConfig::path_filter`] to one event (not the HistoryDone
/// marker): the path to deliver it with, or `None` to drop it.
///
/// Events at or below `filter` pass unchanged, and everything passes
/// without a filter. Outside the filter only a rescan that covers it
/// passes, as a rescan of the filtered folder itself: an event asking for
/// a rescan (`MustScanSubDirs`, `UserDropped`, `KernelDropped`,
/// `EventIdsWrapped`, `RootChanged`) on the volume's mount point `mount`
/// or on an ancestor of the filter. FSEvents reports the events it dropped
/// that way, as a rescan of the volume root (observed by the soak on the
/// Data volume: `MustScanSubDirs | UserDropped` on `/System/Volumes/Data`,
/// in bursts while the Mac was busy and at the start of a replay). The
/// mount point is not an ancestor of a filter in its firmlinked form
/// (`/private/var/...`), hence the separate test. Dropping such an event
/// lost every change it stood for.
fn filter_event(
    path: PathBuf,
    flags: EventFlags,
    filter: Option<&Path>,
    mount: &Path,
) -> Option<PathBuf> {
    let Some(filter) = filter else {
        return Some(path);
    };
    if path.starts_with(filter) {
        return Some(path);
    }
    if flags.asks_rescan() && (path == mount || filter.starts_with(&path)) {
        return Some(filter.to_path_buf());
    }
    None
}

/// Turns a path reported by the stream into an absolute, firmlinked path.
/// Device-relative paths are joined to `mount`; paths that already start
/// with `mount` (from the `FSEventStreamCreate` fallback) are kept.
fn absolute_path(mount: &Path, reported: &Path) -> PathBuf {
    let joined = if reported.is_absolute() && reported.starts_with(mount) {
        reported.to_path_buf()
    } else {
        let mut out = mount.to_path_buf();
        out.extend(
            reported
                .components()
                .filter(|c| matches!(c, Component::Normal(_))),
        );
        out
    };
    normalise_firmlinks(&joined)
}

/// Maps `/System/Volumes/Data/<firmlinked dir>/...` to `/<firmlinked
/// dir>/...`. Other paths are returned unchanged.
///
/// This is [`FirmlinkMap::to_root`] with the system table
/// ([`FirmlinkMap::system`]).
#[must_use]
pub fn normalise_firmlinks(path: &Path) -> PathBuf {
    FirmlinkMap::system().to_root(path)
}

/// Mount point of the volume holding `path` (`statfs` `f_mntonname`).
fn mount_point(path: &Path) -> io::Result<PathBuf> {
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    // SAFETY: `statfs` is plain old data; all-zero is a valid value.
    let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: valid C string and a valid out pointer.
    if unsafe { libc::statfs(c_path.as_ptr(), &raw mut buf) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the kernel NUL-terminates `f_mntonname`, which lies in `buf`.
    let name = unsafe { CStr::from_ptr(buf.f_mntonname.as_ptr()) };
    Ok(PathBuf::from(OsStr::from_bytes(name.to_bytes())))
}

/// An owned `CFArrayRef` of `CFStringRef`s.
struct CfArray(sys::CFArrayRef);

impl CfArray {
    fn of_paths(paths: &[&OsStr]) -> io::Result<CfArray> {
        let mut strings = Vec::with_capacity(paths.len());
        for p in paths {
            let c = CString::new(p.as_bytes())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
            // SAFETY: valid C string; returns a +1 CFString or null.
            let s = unsafe {
                sys::CFStringCreateWithFileSystemRepresentation(std::ptr::null(), c.as_ptr())
            };
            if s.is_null() {
                for s in &strings {
                    // SAFETY: each is a +1 CFString we own.
                    unsafe { sys::CFRelease(*s) };
                }
                return Err(io::Error::other("CFStringCreate failed"));
            }
            strings.push(s.cast::<c_void>());
        }
        #[allow(clippy::cast_possible_wrap)]
        let count = strings.len() as sys::CFIndex;
        // SAFETY: `strings` holds `count` CF objects; the type callbacks
        // retain them, so our references can be released right after.
        let array = unsafe {
            sys::CFArrayCreate(
                std::ptr::null(),
                strings.as_ptr(),
                count,
                &raw const sys::kCFTypeArrayCallBacks,
            )
        };
        for s in &strings {
            // SAFETY: each is a +1 CFString we own; the array retains them.
            unsafe { sys::CFRelease(*s) };
        }
        if array.is_null() {
            return Err(io::Error::other("CFArrayCreate failed"));
        }
        Ok(CfArray(array))
    }
}

impl Drop for CfArray {
    fn drop(&mut self) {
        // SAFETY: a +1 CFArray we own.
        unsafe { sys::CFRelease(self.0.cast()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::firmlinks::DATA_VOLUME;

    /// The table in file order.
    fn parse_firmlinks(text: &[u8]) -> Vec<(PathBuf, PathBuf)> {
        FirmlinkMap::parse(&String::from_utf8_lossy(text))
            .entries()
            .to_vec()
    }

    const DROPPED: EventFlags =
        EventFlags(EventFlags::MUST_SCAN_SUB_DIRS.0 | EventFlags::USER_DROPPED.0);
    const CREATED: EventFlags = EventFlags(EventFlags::ITEM_CREATED.0 | EventFlags::ITEM_IS_FILE.0);

    fn routed(path: &str, flags: EventFlags, filter: Option<&str>) -> Option<PathBuf> {
        filter_event(
            PathBuf::from(path),
            flags,
            filter.map(Path::new),
            Path::new("/System/Volumes/Data"),
        )
    }

    #[test]
    fn events_at_or_below_the_filter_pass_unchanged() {
        let filter = Some("/private/var/t/root");
        for flags in [CREATED, DROPPED] {
            assert_eq!(
                routed("/private/var/t/root/a/b", flags, filter),
                Some(PathBuf::from("/private/var/t/root/a/b"))
            );
            assert_eq!(
                routed("/private/var/t/root", flags, filter),
                Some(PathBuf::from("/private/var/t/root"))
            );
        }
        assert_eq!(
            routed("/elsewhere/x", CREATED, None),
            Some(PathBuf::from("/elsewhere/x"))
        );
    }

    #[test]
    fn other_events_outside_the_filter_are_dropped() {
        let filter = Some("/private/var/t/root");
        assert_eq!(routed("/private/var/t/other", CREATED, filter), None);
        assert_eq!(routed("/private/var/t", CREATED, filter), None);
        assert_eq!(routed("/System/Volumes/Data", CREATED, filter), None);
        // A rescan of a folder beside the filter doesn't cover it.
        assert_eq!(routed("/private/var/t/other", DROPPED, filter), None);
        assert_eq!(routed("/private/var/t/root2", DROPPED, filter), None);
    }

    /// Found by the soak: FSEvents reports events it dropped as a rescan of
    /// the volume root (`MustScanSubDirs | UserDropped` on
    /// `/System/Volumes/Data`), which the filter used to hide, so the
    /// dropped changes were never picked up.
    #[test]
    fn a_rescan_of_the_volume_root_or_an_ancestor_rescans_the_filtered_folder() {
        let filter = Some("/private/var/t/root");
        let want = Some(PathBuf::from("/private/var/t/root"));
        assert_eq!(routed("/System/Volumes/Data", DROPPED, filter), want);
        assert_eq!(routed("/private/var/t", DROPPED, filter), want);
        assert_eq!(routed("/", DROPPED, filter), want);
        let kernel = EventFlags(EventFlags::MUST_SCAN_SUB_DIRS.0 | EventFlags::KERNEL_DROPPED.0);
        assert_eq!(routed("/System/Volumes/Data", kernel, filter), want);
        assert_eq!(
            routed(
                "/System/Volumes/Data",
                EventFlags::MUST_SCAN_SUB_DIRS,
                filter
            ),
            want
        );
        // A volume rescan reaches the source too, whatever its path.
        assert_eq!(
            routed(
                "/System/Volumes/Data",
                EventFlags::EVENT_IDS_WRAPPED,
                filter
            ),
            want
        );
    }

    #[test]
    fn firmlinked_data_paths_are_normalised() {
        assert_eq!(
            normalise_firmlinks(Path::new("/System/Volumes/Data/private/var/x")),
            Path::new("/private/var/x")
        );
        assert_eq!(
            normalise_firmlinks(Path::new("/System/Volumes/Data/Users")),
            Path::new("/Users")
        );
        assert_eq!(
            normalise_firmlinks(Path::new("/System/Volumes/Data/.fseventsd")),
            Path::new("/System/Volumes/Data/.fseventsd")
        );
        assert_eq!(
            normalise_firmlinks(Path::new("/private/var/x")),
            Path::new("/private/var/x")
        );
    }

    #[test]
    fn device_relative_paths_are_joined_to_the_mount() {
        let data = Path::new(DATA_VOLUME);
        assert_eq!(
            absolute_path(data, Path::new("private/tmp/a")),
            Path::new("/private/tmp/a")
        );
        assert_eq!(
            absolute_path(data, Path::new("/System/Volumes/Data/private/tmp/a")),
            Path::new("/private/tmp/a")
        );
        assert_eq!(
            absolute_path(Path::new("/"), Path::new("usr/bin")),
            Path::new("/usr/bin")
        );
        assert_eq!(
            absolute_path(Path::new("/Volumes/X"), Path::new("")),
            Path::new("/Volumes/X")
        );
    }

    #[test]
    fn firmlinks_file_format_parses() {
        let t = parse_firmlinks(b"/Users\tUsers\n/usr/local\tusr/local\n\nbogus\n");
        assert_eq!(
            t,
            vec![
                (PathBuf::from("/Users"), PathBuf::from("Users")),
                (PathBuf::from("/usr/local"), PathBuf::from("usr/local")),
            ]
        );
    }
}
