//! Pure mapping from FSEvents events to [`fx_model::Change`] (every OS).
//!
//! [`map_event`] turns one event into at most one change, and [`coalesce`]
//! tidies one batch. Both are pure so they can be tested without a live
//! stream; `FsEventsSource` (macOS only) applies them to every batch it
//! returns.
//!
//! Routes, highest priority first:
//!
//! | event | change |
//! |---|---|
//! | `HistoryDone` marker | none |
//! | `EventIdsWrapped`, `RootChanged`, `Unmount` of the watched volume | [`Change::RescanVolume`] |
//! | `MustScanSubDirs`, `UserDropped`, `KernelDropped`, a directory rename (`ItemRenamed \| ItemIsDir`), or the stream's `rescan_subtree` field | [`Change::RescanSubtree`] |
//! | anything else (item create/remove/rename/modify, `Mount`, `Unmount` of another volume, unknown bits) | [`Change::CheckPath`] |
//!
//! A directory rename is a subtree rescan even live, because FSEvents does
//! not pair the two sides of a rename and reports nothing for the entries
//! below the moved directory.
//!
//! Start-time history gaps (a UUID mismatch, a saved ID above the current
//! ID) are not events: they surface as
//! [`fx_model::StartOutcome::GapDetected`] from
//! [`decide_start`](super::cursor::decide_start).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use fx_model::Change;

use super::flags::{EventAction, EventFlags, classify};

/// Maps one event to a change. `None` only for the HistoryDone marker.
///
/// `rescan_subtree` is the stream's separate rescan field
/// (`RawEvent::rescan_subtree`, macOS only); `volume_root` is
/// the watched volume's mount point in the stream's normalised form, so an
/// `Unmount` of the watched volume itself becomes a volume rescan.
#[must_use]
pub fn map_event(
    path: &Path,
    flags: EventFlags,
    rescan_subtree: bool,
    volume_root: &Path,
) -> Option<Change> {
    match classify(path, flags, volume_root) {
        EventAction::HistoryDone => None,
        EventAction::RescanVolume => Some(Change::RescanVolume),
        EventAction::RescanSubtree(p) => Some(Change::RescanSubtree(p)),
        EventAction::CheckPath(p) => {
            if rescan_subtree || flags.contains(EventFlags::ITEM_RENAMED | EventFlags::ITEM_IS_DIR)
            {
                Some(Change::RescanSubtree(p))
            } else {
                Some(Change::CheckPath(p))
            }
        }
    }
}

/// Tidies one batch of changes:
///
/// * keeps one copy of each change (first occurrence order);
/// * drops a [`Change::CheckPath`] strictly below (component-wise) a
///   [`Change::RescanSubtree`] anywhere in the same batch, since the rescan
///   covers it;
/// * never drops a rescan, and never drops anything else, so a batch with
///   changes never comes out empty.
#[must_use]
pub fn coalesce(changes: Vec<Change>) -> Vec<Change> {
    let subtrees: HashSet<PathBuf> = changes
        .iter()
        .filter_map(|c| match c {
            Change::RescanSubtree(p) => Some(p.clone()),
            _ => None,
        })
        .collect();
    let mut seen: HashSet<Change> = HashSet::with_capacity(changes.len());
    let mut out = Vec::with_capacity(changes.len());
    for change in changes {
        if let Change::CheckPath(p) = &change {
            if !subtrees.is_empty() && p.ancestors().skip(1).any(|a| subtrees.contains(a)) {
                continue;
            }
        }
        if seen.contains(&change) {
            continue;
        }
        seen.insert(change.clone());
        out.push(change);
    }
    out
}
