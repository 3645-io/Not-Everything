//! FSEvents event flags and the pure per-event classifier (every OS).
//!
//! The bit values are the `kFSEventStreamEventFlag*` constants from Apple's
//! `FSEvents.h`. The kernel coalesces events, so a flag word is only a hint
//! about what happened to a path: the safe reaction to any item-level event
//! is to look at the path again ([`EventAction::CheckPath`]).

use std::path::{Component, Path, PathBuf};

/// The raw `FSEventStreamEventFlags` word delivered with each event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct EventFlags(pub u32);

impl EventFlags {
    /// `kFSEventStreamEventFlagNone`.
    pub const NONE: EventFlags = EventFlags(0x0000_0000);
    /// `kFSEventStreamEventFlagMustScanSubDirs`: coalescing lost detail,
    /// rescan the directory and everything below it.
    pub const MUST_SCAN_SUB_DIRS: EventFlags = EventFlags(0x0000_0001);
    /// `kFSEventStreamEventFlagUserDropped`: events were dropped in user
    /// space (sent together with `MUST_SCAN_SUB_DIRS`).
    pub const USER_DROPPED: EventFlags = EventFlags(0x0000_0002);
    /// `kFSEventStreamEventFlagKernelDropped`: events were dropped in the
    /// kernel (sent together with `MUST_SCAN_SUB_DIRS`).
    pub const KERNEL_DROPPED: EventFlags = EventFlags(0x0000_0004);
    /// `kFSEventStreamEventFlagEventIdsWrapped`: the 64-bit event ID wrapped.
    pub const EVENT_IDS_WRAPPED: EventFlags = EventFlags(0x0000_0008);
    /// `kFSEventStreamEventFlagHistoryDone`: marker sent once all historical
    /// events (from `sinceWhen`) have been delivered.
    pub const HISTORY_DONE: EventFlags = EventFlags(0x0000_0010);
    /// `kFSEventStreamEventFlagRootChanged`: a watched root changed (only with
    /// `kFSEventStreamCreateFlagWatchRoot`).
    pub const ROOT_CHANGED: EventFlags = EventFlags(0x0000_0020);
    /// `kFSEventStreamEventFlagMount`: a volume was mounted under the path.
    pub const MOUNT: EventFlags = EventFlags(0x0000_0040);
    /// `kFSEventStreamEventFlagUnmount`: a volume was unmounted at the path.
    pub const UNMOUNT: EventFlags = EventFlags(0x0000_0080);
    /// `kFSEventStreamEventFlagItemCreated`.
    pub const ITEM_CREATED: EventFlags = EventFlags(0x0000_0100);
    /// `kFSEventStreamEventFlagItemRemoved`.
    pub const ITEM_REMOVED: EventFlags = EventFlags(0x0000_0200);
    /// `kFSEventStreamEventFlagItemInodeMetaMod`.
    pub const ITEM_INODE_META_MOD: EventFlags = EventFlags(0x0000_0400);
    /// `kFSEventStreamEventFlagItemRenamed` (sent for both the old and the
    /// new name; the two are not paired).
    pub const ITEM_RENAMED: EventFlags = EventFlags(0x0000_0800);
    /// `kFSEventStreamEventFlagItemModified`.
    pub const ITEM_MODIFIED: EventFlags = EventFlags(0x0000_1000);
    /// `kFSEventStreamEventFlagItemFinderInfoMod`.
    pub const ITEM_FINDER_INFO_MOD: EventFlags = EventFlags(0x0000_2000);
    /// `kFSEventStreamEventFlagItemChangeOwner`.
    pub const ITEM_CHANGE_OWNER: EventFlags = EventFlags(0x0000_4000);
    /// `kFSEventStreamEventFlagItemXattrMod`.
    pub const ITEM_XATTR_MOD: EventFlags = EventFlags(0x0000_8000);
    /// `kFSEventStreamEventFlagItemIsFile`.
    pub const ITEM_IS_FILE: EventFlags = EventFlags(0x0001_0000);
    /// `kFSEventStreamEventFlagItemIsDir`.
    pub const ITEM_IS_DIR: EventFlags = EventFlags(0x0002_0000);
    /// `kFSEventStreamEventFlagItemIsSymlink`.
    pub const ITEM_IS_SYMLINK: EventFlags = EventFlags(0x0004_0000);
    /// `kFSEventStreamEventFlagOwnEvent` (only with
    /// `kFSEventStreamCreateFlagMarkSelf`).
    pub const OWN_EVENT: EventFlags = EventFlags(0x0008_0000);
    /// `kFSEventStreamEventFlagItemIsHardlink`.
    pub const ITEM_IS_HARDLINK: EventFlags = EventFlags(0x0010_0000);
    /// `kFSEventStreamEventFlagItemIsLastHardlink`.
    pub const ITEM_IS_LAST_HARDLINK: EventFlags = EventFlags(0x0020_0000);
    /// `kFSEventStreamEventFlagItemCloned`.
    pub const ITEM_CLONED: EventFlags = EventFlags(0x0040_0000);

    /// Flags that invalidate everything the stream watches, whatever the
    /// path. `FSEvents.h`: after UserDropped or KernelDropped "the client
    /// must do a full scan of any directories (and their subdirectories,
    /// recursively) being monitored by this stream"; after EventIdsWrapped
    /// no earlier ID is a valid start; RootChanged reports a change on the
    /// path to a watched directory.
    pub(crate) const VOLUME_RESCAN: u32 = Self::EVENT_IDS_WRAPPED.0
        | Self::ROOT_CHANGED.0
        | Self::USER_DROPPED.0
        | Self::KERNEL_DROPPED.0;

    /// The special (non-item) flags: `MustScanSubDirs` through `Unmount`.
    /// Events carrying one of these are not historical item records, so
    /// the replay's "at or before the saved ID" rule never drops them
    /// (`FSEvents.h` gives RootChanged an event ID of zero).
    pub(crate) const SPECIAL: u32 = 0x0000_00FF;

    /// The item action bits (`ItemCreated` through `ItemXattrMod`, and
    /// `ItemCloned`).
    const ITEM_ACTION: u32 = 0x0000_FF00 | Self::ITEM_CLONED.0;

    /// The item type bits (`ItemIsFile`, `ItemIsDir`, `ItemIsSymlink`,
    /// `ItemIsHardlink`, `ItemIsLastHardlink`).
    const ITEM_TYPE: u32 = Self::ITEM_IS_FILE.0
        | Self::ITEM_IS_DIR.0
        | Self::ITEM_IS_SYMLINK.0
        | Self::ITEM_IS_HARDLINK.0
        | Self::ITEM_IS_LAST_HARDLINK.0;

    /// Item actions that change which names exist at a path.
    const NAMESPACE: u32 = Self::ITEM_CREATED.0 | Self::ITEM_REMOVED.0 | Self::ITEM_RENAMED.0;

    /// The raw flag word.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// True if every bit of `other` is set in `self`.
    #[must_use]
    pub const fn contains(self, other: EventFlags) -> bool {
        self.0 & other.0 == other.0
    }

    /// True if any bit of `other` is set in `self`.
    #[must_use]
    pub const fn intersects(self, other: EventFlags) -> bool {
        self.0 & other.0 != 0
    }

    /// True for a directory-level event: no item bit (action or type) and
    /// no special bit, only possibly `OwnEvent` or bits this SDK does not
    /// define. `FSEvents.h` (`kFSEventStreamEventFlagNone`): "There was
    /// some change in the directory at the specific path supplied in this
    /// event", so the folder must be listed, not only checked.
    #[must_use]
    pub const fn is_directory_level(self) -> bool {
        self.0 & (Self::ITEM_ACTION | Self::ITEM_TYPE | Self::SPECIAL) == 0
    }

    /// True if the event asks for a rescan, of its subtree
    /// (`MustScanSubDirs`) or of the volume (`VOLUME_RESCAN`:
    /// `EventIdsWrapped`, `RootChanged`, `UserDropped`, `KernelDropped`).
    #[must_use]
    pub const fn asks_rescan(self) -> bool {
        self.0 & (Self::MUST_SCAN_SUB_DIRS.0 | Self::VOLUME_RESCAN) != 0
    }
}

impl std::ops::BitOr for EventFlags {
    type Output = EventFlags;

    fn bitor(self, rhs: EventFlags) -> EventFlags {
        EventFlags(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for EventFlags {
    fn bitor_assign(&mut self, rhs: EventFlags) {
        self.0 |= rhs.0;
    }
}

/// What the index must do in response to one FSEvents event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventAction {
    /// All historical events have been delivered; live events follow.
    HistoryDone,
    /// The stream can no longer be trusted for anything it watches (event
    /// IDs wrapped, events dropped, the root changed, or the watched volume
    /// was unmounted): rescan the whole volume.
    RescanVolume,
    /// Events below this directory were coalesced, or the directory's
    /// contents changed in a way only a listing shows: rescan it and
    /// everything below it.
    RescanSubtree(PathBuf),
    /// Something may have changed at this path: stat it and update the
    /// index (insert, update or remove).
    CheckPath(PathBuf),
    /// A metadata change on a folder above the scope root: nothing the
    /// index holds changed.
    Ignore,
}

/// Classifies one event.
///
/// `scope_root` is the root of what the stream delivers, in the form the
/// stream hands out paths: pass `VolumeStream::scope_root()` (macOS), which
/// is the stream's path filter, or its volume's mount point (`/`,
/// `/Volumes/X`, `/System/Volumes/Data`) when it has none.
///
/// Priority, highest first:
///
/// 1. `HistoryDone` gives [`EventAction::HistoryDone`] (`FSEvents.h`: the
///    path is to be ignored).
/// 2. `EventIdsWrapped`, `RootChanged` (which only a `WatchRoot` stream
///    reports; treated as volume-level wherever it appears), `UserDropped`
///    or `KernelDropped` on any path, or `Unmount` of the scope root or the
///    volume root, give [`EventAction::RescanVolume`]. The volume root is
///    named by `""`, `"/"` or `"."` (the root of a device-relative
///    stream); paths are compared component-wise, so a trailing slash does
///    not matter.
/// 3. An event on a folder strictly above the scope root (an ancestor of
///    it, or the volume root) gives
///    [`EventAction::RescanSubtree`]`(scope_root)` when it asks for a
///    subtree rescan, mounts or unmounts a volume there, creates, removes
///    or renames the folder (the scope root moved, went away or came back:
///    what a `WatchRoot` stream would report as RootChanged), or is a
///    directory-level event; any other event there (a metadata change) is
///    [`EventAction::Ignore`].
/// 4. `Mount` on the scope root itself (a volume now hides what was
///    indexed) gives [`EventAction::RescanSubtree`]`(scope_root)`.
/// 5. `MustScanSubDirs`, or a directory-level event
///    ([`EventFlags::is_directory_level`]), gives
///    [`EventAction::RescanSubtree`] on the path.
/// 6. Anything else (every item-level event, any other `Mount`, an
///    `Unmount` of another volume or of a volume mounted inside the scope)
///    gives [`EventAction::CheckPath`]. The item flags are only hints, and
///    renames are not paired: each side of a rename is checked on its own.
#[must_use]
pub fn classify(path: &Path, flags: EventFlags, scope_root: &Path) -> EventAction {
    if flags.contains(EventFlags::HISTORY_DONE) {
        return EventAction::HistoryDone;
    }
    if flags.0 & EventFlags::VOLUME_RESCAN != 0
        || (flags.contains(EventFlags::UNMOUNT) && (path == scope_root || is_volume_root(path)))
    {
        return EventAction::RescanVolume;
    }
    if is_above(path, scope_root) {
        let rescans = flags.0
            & (EventFlags::MUST_SCAN_SUB_DIRS.0
                | EventFlags::MOUNT.0
                | EventFlags::UNMOUNT.0
                | EventFlags::NAMESPACE)
            != 0;
        return if rescans || flags.is_directory_level() {
            EventAction::RescanSubtree(scope_root.to_path_buf())
        } else {
            EventAction::Ignore
        };
    }
    if flags.contains(EventFlags::MOUNT) && path == scope_root {
        return EventAction::RescanSubtree(scope_root.to_path_buf());
    }
    if flags.contains(EventFlags::MUST_SCAN_SUB_DIRS) || flags.is_directory_level() {
        return EventAction::RescanSubtree(path.to_path_buf());
    }
    EventAction::CheckPath(path.to_path_buf())
}

/// Whether a stream filtered to `filter` delivers this event
/// (`StreamConfig::path_filter`, macOS).
///
/// Without a filter, everything. With one: events at or below it
/// (component-wise), the HistoryDone marker, the volume-level flags
/// (`EventIdsWrapped`, `RootChanged`, `UserDropped`, `KernelDropped`) on any
/// path, and the events on a folder above it that [`classify`] does not
/// ignore (they rescan the filtered root). Everything else concerns paths
/// outside the filter. Dropping the events above the filter used to lose
/// changes: a MustScanSubDirs coalesced onto a parent folder, dropped
/// events reported on the volume root, and the filtered root moving away
/// all went unseen.
#[must_use]
pub fn admit(path: &Path, flags: EventFlags, filter: Option<&Path>) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    if path.starts_with(filter)
        || flags.0 & (EventFlags::HISTORY_DONE.0 | EventFlags::VOLUME_RESCAN) != 0
    {
        return true;
    }
    is_above(path, filter) && classify(path, flags, filter) != EventAction::Ignore
}

/// True for a folder strictly above `scope_root`: a proper ancestor of it
/// (component-wise), or the root of a device-relative stream (`""`, `"/"`
/// or `"."`) when the scope root is not itself the volume root.
fn is_above(path: &Path, scope_root: &Path) -> bool {
    if is_volume_root(path) {
        return !is_volume_root(scope_root);
    }
    path != scope_root && scope_root.starts_with(path)
}

/// True for the root of a device-relative stream: `""`, `"/"` or `"."`.
fn is_volume_root(path: &Path) -> bool {
    path.components()
        .all(|c| matches!(c, Component::RootDir | Component::CurDir))
}
