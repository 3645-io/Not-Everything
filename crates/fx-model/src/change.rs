//! Change records and resume cursors exchanged with change sources.

use std::path::PathBuf;

use crate::entry::Entry;
use crate::ids::FileKey;

/// Where a change source left off, saved atomically with the index so the
/// next start can catch up instead of re-scanning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResumeCursor {
    /// macOS FSEvents: the last event ID plus the event database UUID.
    /// A different UUID means history was lost.
    FsEvents {
        /// Last FSEvents event ID applied.
        event_id: u64,
        /// UUID of the FSEvents database the ID belongs to.
        db_uuid: [u8; 16],
    },
    /// Windows NTFS USN change journal position.
    Usn {
        /// ID of the journal instance; changes when the journal is recreated.
        journal_id: u64,
        /// The next update sequence number to read.
        next_usn: i64,
    },
    /// No change history (Linux, FAT): fall back to mtime reconciliation.
    None {
        /// Epoch of the snapshot the reconciliation compares against.
        snapshot_epoch: u64,
    },
}

/// One change reported by a change source.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Change {
    /// Insert or update this entry.
    Upsert(Entry),
    /// Remove the entry with this key (and its descendants, if a directory).
    Remove {
        /// Key of the removed entry.
        key: FileKey,
    },
    /// Something at this path changed; stat it and reconcile.
    CheckPath(PathBuf),
    /// History under this directory is unreliable; re-scan the subtree.
    RescanSubtree(PathBuf),
    /// History for the whole volume is unreliable; re-scan it.
    RescanVolume,
}

/// Result of starting a change source from a saved cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StartOutcome {
    /// The source resumed from the cursor; no history was lost.
    Resumed,
    /// History between the cursor and now is missing; a re-scan is needed.
    GapDetected,
    /// The source has no change history; reconcile by mtime instead.
    NoCursorSupport,
}
