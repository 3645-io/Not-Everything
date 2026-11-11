//! FSEvents change source.
//!
//! A hand-written binding to Apple's FSEvents API (CoreServices) and the
//! few CoreFoundation helpers it needs. It is written only from Apple's
//! FSEvents documentation and the SDK header `FSEvents.h`; no third-party
//! binding crate is used.
//!
//! * [`flags`] (every OS): the `kFSEventStreamEventFlag*` bit values as
//!   [`flags::EventFlags`] and the pure [`flags::classify`] that turns one
//!   event into an [`flags::EventAction`].
//! * [`change`] (every OS): the pure [`change::map_event`] from one event
//!   to an [`fx_model::Change`] and the per-batch [`change::coalesce`].
//! * [`cursor`] (every OS): the start decision over
//!   [`fx_model::ResumeCursor::FsEvents`] ([`cursor::decide_start`], which
//!   detects history gaps), the thin [`cursor::FsEventsCursor`] view with
//!   its 24-byte encoding, and the pure [`cursor::CursorTracker`] that
//!   keeps a stream's cursor safe during replay and counts the events
//!   dropped before the cursor.
//! * `source` (macOS only): `FsEventsSource`, the per-volume
//!   `fx_fs::ChangeSource`.
//! * `volumes_source` (macOS only): `VolumesSource`, the root volume's
//!   source plus a live stream per mounted external volume, which asks
//!   the engine to index a volume when it is mounted and to check its
//!   mount point when it is unmounted (run 2.4).
//! * `sys` (macOS only): raw `extern "C"` declarations.
//! * `volume` (macOS only): safe wrappers for the per-volume queries
//!   (current event ID, database UUID, device of a path, last event ID
//!   before a time).
//! * `stream` (macOS only): the per-volume `VolumeStream` with replay from
//!   a saved cursor, the HistoryDone marker and live events.
//!
pub mod change;
pub mod cursor;
pub mod flags;

#[cfg(target_os = "macos")]
pub mod source;
#[cfg(target_os = "macos")]
pub mod stream;
#[cfg(target_os = "macos")]
pub mod sys;
#[cfg(target_os = "macos")]
pub mod volumes_source;

#[cfg(target_os = "macos")]
pub mod volume;
