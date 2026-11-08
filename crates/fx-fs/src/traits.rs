//! Platform traits implemented by the per-OS crates (docs/PLAN.md,
//! "Platform traits").
//!
//! The traits exchange the `fx-model` types and never redefine them.
//!
//! # Error type
//!
//! Fallible trait methods return [`Result`], an alias for
//! [`std::io::Result`]. Every failure a platform walker or change source can
//! hit is ultimately a system-call failure (`getattrlistbulk`, FSEvents,
//! `DeviceIoControl`, fanotify, ...), and `io::Error` already carries the OS
//! error code (`raw_os_error`), a portable [`std::io::ErrorKind`] and an
//! optional message. Using it directly lets implementations propagate errors
//! with `?` without a conversion layer. Errors that are not OS errors can be
//! wrapped with `io::Error::other`.
//!
//! # Thread safety
//!
//! The traits have no `Send`/`Sync` supertraits so that simple or
//! single-threaded implementations stay possible; callers that move an
//! implementation to another thread ask for it explicitly, e.g.
//! `Box<dyn ChangeSource + Send>`.

use std::path::Path;
use std::time::Instant;

use fx_model::{Change, Entry, ResumeCursor, StartOutcome};

/// Result type of the platform traits: [`std::io::Result`]. See the module
/// documentation for why.
pub type Result<T> = std::io::Result<T>;

/// Enumerates every entry under a root, for the initial full scan.
pub trait VolumeEnumerator {
    /// Walks the tree at `root` and passes each entry to `sink`.
    ///
    /// Entries are emitted parent before child, so the receiver can resolve
    /// every `Entry::parent` key against an entry it has already seen.
    /// Implementations apply their exclusions before descending into a
    /// directory, so excluded subtrees are never read. An error aborts the
    /// walk; entries already passed to `sink` stay valid.
    fn enumerate(&self, root: &Path, sink: &mut dyn FnMut(Entry)) -> Result<()>;
}

/// Resolves one absolute path to the entry it names right now.
///
/// Used by the engine to check paths reported by a change source
/// ([`Change::CheckPath`]) against the disk before reconciling them with
/// [`reconcile_path`](crate::reconcile_path).
///
/// Implementations use lstat semantics: a symbolic link is reported as an
/// [`EntryKind::Symlink`](fx_model::EntryKind::Symlink) entry and is never
/// followed. The returned entry carries the same key, parent key, name and
/// metadata that the platform's [`VolumeEnumerator`] would report for it.
pub trait PathStat {
    /// Stats `path` (absolute) without following a final symlink.
    ///
    /// Returns `Ok(None)` when the path no longer exists (including when a
    /// parent component vanished), and an error for anything else, such as
    /// permission denied.
    fn stat(&self, path: &Path) -> Result<Option<Entry>>;
}

/// A live source of file-system changes that can resume from a saved cursor.
pub trait ChangeSource {
    /// Starts watching, catching up from `from` when given.
    ///
    /// The outcome tells the caller whether history was replayed
    /// ([`StartOutcome::Resumed`]), lost ([`StartOutcome::GapDetected`], so a
    /// re-scan is needed) or unsupported ([`StartOutcome::NoCursorSupport`],
    /// so reconcile by mtime).
    fn start(&mut self, from: Option<&ResumeCursor>) -> Result<StartOutcome>;

    /// Returns the next batch of changes, waiting no later than `deadline`.
    ///
    /// An empty batch means nothing arrived before the deadline.
    fn next_batch(&mut self, deadline: Instant) -> Result<Vec<Change>>;

    /// The position after the last batch returned, saved atomically with the
    /// index so the next [`ChangeSource::start`] can resume from it.
    fn cursor(&self) -> ResumeCursor;
}

/// Whether the app may read the whole file system (Full Disk Access on
/// macOS).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccessState {
    /// Full access is granted.
    Granted,
    /// Full access is denied; protected folders will be missing.
    Denied,
    /// The state could not be determined.
    Unknown,
}

/// Checks and requests the permission needed to index the whole disk.
pub trait PermissionProbe {
    /// Probes the current full-disk access state without prompting the user.
    fn full_access(&self) -> AccessState;

    /// Opens the system settings page where the user can grant access.
    fn open_settings(&self);
}
