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

    /// Flags that ask for a rescan of the event's directory subtree.
    const SUBTREE_RESCAN: u32 =
        Self::MUST_SCAN_SUB_DIRS.0 | Self::USER_DROPPED.0 | Self::KERNEL_DROPPED.0;

    /// Flags that invalidate the whole volume, whatever the path.
    const VOLUME_RESCAN: u32 = Self::EVENT_IDS_WRAPPED.0 | Self::ROOT_CHANGED.0;

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

    /// True if the event asks for a rescan, of its subtree
    /// (`MustScanSubDirs`, `UserDropped`, `KernelDropped`) or of the
    /// volume (`EventIdsWrapped`, `RootChanged`): what [`classify`] turns
    /// into [`EventAction::RescanSubtree`] or [`EventAction::RescanVolume`]
    /// whatever the path.
    #[must_use]
    pub const fn asks_rescan(self) -> bool {
        self.0 & (Self::SUBTREE_RESCAN | Self::VOLUME_RESCAN) != 0
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
    /// The stream can no longer be trusted for this volume (event IDs
    /// wrapped, the root changed, or the watched volume was unmounted):
    /// rescan the whole volume.
    RescanVolume,
    /// Events below this directory were coalesced or dropped: rescan it and
    /// everything below it.
    RescanSubtree(PathBuf),
    /// Something may have changed at this path: stat it and update the
    /// index (insert, update or remove).
    CheckPath(PathBuf),
}

/// Classifies one event.
///
/// Priority, highest first:
///
/// 1. `HistoryDone` gives [`EventAction::HistoryDone`].
/// 2. `EventIdsWrapped`, `RootChanged` (which only a `WatchRoot` stream
///    reports; treated as volume-level wherever it appears), or `Unmount`
///    of the watched volume itself give [`EventAction::RescanVolume`]. The
///    watched volume is named by a path equal to `volume_root` (component
///    wise, so a trailing slash does not matter) or by `""`, `"/"` or `"."`
///    (the root of a device-relative stream).
/// 3. `MustScanSubDirs`, `UserDropped` or `KernelDropped` give
///    [`EventAction::RescanSubtree`] on the path.
/// 4. Anything else (every item-level event, any `Mount`, an `Unmount` of
///    another volume or of a volume mounted inside the watched one, unknown
///    bits) gives [`EventAction::CheckPath`]. The item flags are only hints,
///    and renames are not paired: each side of a rename is checked on its
///    own.
///
/// `volume_root` is the watched volume's mount path in the form the stream
/// hands out paths: pass `VolumeStream::mount_point()` (macOS), e.g. `/`,
/// `/Volumes/X` or `/System/Volumes/Data`.
#[must_use]
pub fn classify(path: &Path, flags: EventFlags, volume_root: &Path) -> EventAction {
    if flags.contains(EventFlags::HISTORY_DONE) {
        return EventAction::HistoryDone;
    }
    if flags.0 & EventFlags::VOLUME_RESCAN != 0
        || (flags.contains(EventFlags::UNMOUNT) && (path == volume_root || is_volume_root(path)))
    {
        return EventAction::RescanVolume;
    }
    if flags.0 & EventFlags::SUBTREE_RESCAN != 0 {
        return EventAction::RescanSubtree(path.to_path_buf());
    }
    EventAction::CheckPath(path.to_path_buf())
}

/// True for the root of a device-relative stream: `""`, `"/"` or `"."`.
fn is_volume_root(path: &Path) -> bool {
    path.components()
        .all(|c| matches!(c, Component::RootDir | Component::CurDir))
}
