//! Cross-platform tests for `fx_fs_macos::fsevents::flags`.
//!
//! Contract pinned here:
//! - `pub struct EventFlags(pub u32)` wraps the raw `FSEventStreamEventFlags`
//!   word and is `Clone + Copy`. Every documented `kFSEventStreamEventFlag*` bit is an associated
//!   `EventFlags` constant in SCREAMING_SNAKE_CASE (`MUST_SCAN_SUB_DIRS`,
//!   `ITEM_IS_LAST_HARDLINK`, ...), holding the value from Apple's
//!   FSEvents.h.
//! - `classify(path: &Path, flags: EventFlags, volume_root: &Path)
//!   -> EventAction` is pure. `volume_root` is the watched volume's mount
//!   path as `VolumeStream::mount_point()` hands it out (e.g. `/Volumes/X`
//!   or `/`). `EventAction` is `HistoryDone | RescanVolume |
//!   RescanSubtree(PathBuf) | CheckPath(PathBuf)` and derives `Debug` and
//!   `PartialEq`.
//! - Priority: HistoryDone, then volume-level rescans (EventIdsWrapped;
//!   RootChanged or Unmount on a path equal to `volume_root`, or on `""`,
//!   `"/"` or `"."`), then subtree rescans (MustScanSubDirs, UserDropped,
//!   KernelDropped), then CheckPath for every item-level event (flags are
//!   only hints) and for any other Mount or Unmount. Renames are not paired.

use std::path::{Path, PathBuf};

use fx_fs_macos::fsevents::flags::{EventAction, EventFlags, classify as classify_on};

/// `classify` on the root volume (`volume_root` = `/`), for the tests whose
/// paths are never the volume root.
fn classify(path: &Path, flags: EventFlags) -> EventAction {
    classify_on(path, flags, Path::new("/"))
}

fn or(parts: &[EventFlags]) -> EventFlags {
    EventFlags(parts.iter().fold(0, |acc, f| acc | f.0))
}

fn check(p: &str) -> EventAction {
    EventAction::CheckPath(PathBuf::from(p))
}

fn subtree(p: &str) -> EventAction {
    EventAction::RescanSubtree(PathBuf::from(p))
}

// ------------------------------------------------------------ bit values

#[test]
fn flag_constants_match_fsevents_header() {
    let expected: [(&str, EventFlags, u32); 23] = [
        (
            "MustScanSubDirs",
            EventFlags::MUST_SCAN_SUB_DIRS,
            0x0000_0001,
        ),
        ("UserDropped", EventFlags::USER_DROPPED, 0x0000_0002),
        ("KernelDropped", EventFlags::KERNEL_DROPPED, 0x0000_0004),
        (
            "EventIdsWrapped",
            EventFlags::EVENT_IDS_WRAPPED,
            0x0000_0008,
        ),
        ("HistoryDone", EventFlags::HISTORY_DONE, 0x0000_0010),
        ("RootChanged", EventFlags::ROOT_CHANGED, 0x0000_0020),
        ("Mount", EventFlags::MOUNT, 0x0000_0040),
        ("Unmount", EventFlags::UNMOUNT, 0x0000_0080),
        ("ItemCreated", EventFlags::ITEM_CREATED, 0x0000_0100),
        ("ItemRemoved", EventFlags::ITEM_REMOVED, 0x0000_0200),
        (
            "ItemInodeMetaMod",
            EventFlags::ITEM_INODE_META_MOD,
            0x0000_0400,
        ),
        ("ItemRenamed", EventFlags::ITEM_RENAMED, 0x0000_0800),
        ("ItemModified", EventFlags::ITEM_MODIFIED, 0x0000_1000),
        (
            "ItemFinderInfoMod",
            EventFlags::ITEM_FINDER_INFO_MOD,
            0x0000_2000,
        ),
        (
            "ItemChangeOwner",
            EventFlags::ITEM_CHANGE_OWNER,
            0x0000_4000,
        ),
        ("ItemXattrMod", EventFlags::ITEM_XATTR_MOD, 0x0000_8000),
        ("ItemIsFile", EventFlags::ITEM_IS_FILE, 0x0001_0000),
        ("ItemIsDir", EventFlags::ITEM_IS_DIR, 0x0002_0000),
        ("ItemIsSymlink", EventFlags::ITEM_IS_SYMLINK, 0x0004_0000),
        ("OwnEvent", EventFlags::OWN_EVENT, 0x0008_0000),
        ("ItemIsHardlink", EventFlags::ITEM_IS_HARDLINK, 0x0010_0000),
        (
            "ItemIsLastHardlink",
            EventFlags::ITEM_IS_LAST_HARDLINK,
            0x0020_0000,
        ),
        ("ItemCloned", EventFlags::ITEM_CLONED, 0x0040_0000),
    ];
    let mut seen = 0u32;
    for (name, flag, value) in expected {
        assert_eq!(flag.0, value, "kFSEventStreamEventFlag{name}");
        assert_eq!(seen & value, 0, "{name} overlaps another flag");
        seen |= value;
    }
}

// ------------------------------------------------------------ HistoryDone

#[test]
fn history_done_marker_is_recognised() {
    assert_eq!(
        classify(Path::new(""), EventFlags::HISTORY_DONE),
        EventAction::HistoryDone
    );
    assert_eq!(
        classify(Path::new(""), EventFlags(0x0000_0010)),
        EventAction::HistoryDone
    );
}

// ------------------------------------------------------------ volume rescans

#[test]
fn event_ids_wrapped_rescans_volume() {
    assert_eq!(
        classify(Path::new(""), EventFlags::EVENT_IDS_WRAPPED),
        EventAction::RescanVolume
    );
}

#[test]
fn root_changed_rescans_volume() {
    assert_eq!(
        classify(Path::new(""), EventFlags::ROOT_CHANGED),
        EventAction::RescanVolume
    );
    assert_eq!(
        classify(Path::new("/"), EventFlags::ROOT_CHANGED),
        EventAction::RescanVolume
    );
}

#[test]
fn unmount_of_watched_volume_rescans_volume() {
    // On a device-relative stream the watched volume's root is "" (or "/").
    assert_eq!(
        classify(Path::new(""), EventFlags::UNMOUNT),
        EventAction::RescanVolume
    );
    assert_eq!(
        classify(Path::new("/"), EventFlags::UNMOUNT),
        EventAction::RescanVolume
    );
}

#[test]
fn unmount_of_watched_volume_by_absolute_mount_path_rescans_volume() {
    let x = Path::new("/Volumes/X");
    assert_eq!(
        classify_on(Path::new("/Volumes/X"), EventFlags::UNMOUNT, x),
        EventAction::RescanVolume
    );
    let flags = or(&[EventFlags::UNMOUNT, EventFlags::ITEM_IS_DIR]);
    assert_eq!(
        classify_on(Path::new("/Volumes/X"), flags, x),
        EventAction::RescanVolume
    );
    // A trailing slash names the same directory.
    assert_eq!(
        classify_on(Path::new("/Volumes/X/"), EventFlags::UNMOUNT, x),
        EventAction::RescanVolume
    );
}

#[test]
fn root_changed_on_absolute_mount_path_rescans_volume() {
    let x = Path::new("/Volumes/X");
    assert_eq!(
        classify_on(Path::new("/Volumes/X"), EventFlags::ROOT_CHANGED, x),
        EventAction::RescanVolume
    );
}

#[test]
fn unmount_of_another_volume_checks_its_path() {
    let x = Path::new("/Volumes/X");
    assert_eq!(
        classify_on(Path::new("/Volumes/Y"), EventFlags::UNMOUNT, x),
        check("/Volumes/Y")
    );
    // A volume mounted inside the watched one is not the watched volume.
    assert_eq!(
        classify_on(Path::new("/Volumes/X/inner"), EventFlags::UNMOUNT, x),
        check("/Volumes/X/inner")
    );
    // On the root volume, unmounting /Volumes/USB is just a path change.
    assert_eq!(
        classify_on(
            Path::new("/Volumes/USB"),
            EventFlags::UNMOUNT,
            Path::new("/")
        ),
        check("/Volumes/USB")
    );
    assert_eq!(
        classify_on(
            Path::new("Volumes/USB"),
            EventFlags::UNMOUNT,
            Path::new("/")
        ),
        check("Volumes/USB")
    );
}

#[test]
fn mount_never_rescans_the_volume() {
    let x = Path::new("/Volumes/X");
    assert_eq!(
        classify_on(Path::new("/Volumes/X/sub"), EventFlags::MOUNT, x),
        check("/Volumes/X/sub")
    );
    assert_eq!(
        classify_on(Path::new("/Volumes/Y"), EventFlags::MOUNT, x),
        check("/Volumes/Y")
    );
}

#[test]
fn root_volume_cases_hold_for_any_volume_root() {
    // "", "/" and "." name the root of a device-relative stream whatever
    // the mount path is.
    for root in ["/", "/Volumes/X", "/System/Volumes/Data"] {
        let root = Path::new(root);
        for p in ["", "/", "."] {
            for f in [EventFlags::UNMOUNT, EventFlags::ROOT_CHANGED] {
                assert_eq!(
                    classify_on(Path::new(p), f, root),
                    EventAction::RescanVolume,
                    "path {p:?}, flags {:#x}, root {}",
                    f.0,
                    root.display()
                );
            }
        }
        assert_eq!(
            classify_on(Path::new("a/b"), EventFlags::EVENT_IDS_WRAPPED, root),
            EventAction::RescanVolume
        );
        assert_eq!(
            classify_on(Path::new(""), EventFlags::HISTORY_DONE, root),
            EventAction::HistoryDone
        );
    }
}

#[test]
fn item_events_on_the_mount_path_check_it() {
    let x = Path::new("/Volumes/X");
    assert_eq!(
        classify_on(Path::new("/Volumes/X"), EventFlags::ITEM_INODE_META_MOD, x),
        check("/Volumes/X")
    );
    assert_eq!(
        classify_on(Path::new("/Volumes/X/a"), EventFlags::MUST_SCAN_SUB_DIRS, x),
        subtree("/Volumes/X/a")
    );
}

#[test]
fn volume_rescan_wins_over_subtree_rescan_and_item_flags() {
    let flags = or(&[
        EventFlags::EVENT_IDS_WRAPPED,
        EventFlags::MUST_SCAN_SUB_DIRS,
        EventFlags::ITEM_CREATED,
    ]);
    assert_eq!(classify(Path::new("a/b"), flags), EventAction::RescanVolume);
}

// ------------------------------------------------------------ subtree rescans

#[test]
fn must_scan_sub_dirs_rescans_subtree() {
    assert_eq!(
        classify(
            Path::new("Users/alice/docs"),
            EventFlags::MUST_SCAN_SUB_DIRS
        ),
        subtree("Users/alice/docs")
    );
}

#[test]
fn dropped_events_rescan_subtree() {
    assert_eq!(
        classify(Path::new("Users/alice"), EventFlags::USER_DROPPED),
        subtree("Users/alice")
    );
    assert_eq!(
        classify(Path::new("Users/alice"), EventFlags::KERNEL_DROPPED),
        subtree("Users/alice")
    );
    let both = or(&[EventFlags::MUST_SCAN_SUB_DIRS, EventFlags::KERNEL_DROPPED]);
    assert_eq!(classify(Path::new("tmp"), both), subtree("tmp"));
    let both = or(&[EventFlags::MUST_SCAN_SUB_DIRS, EventFlags::USER_DROPPED]);
    assert_eq!(classify(Path::new("tmp"), both), subtree("tmp"));
}

#[test]
fn subtree_rescan_wins_over_item_flags() {
    let flags = or(&[
        EventFlags::MUST_SCAN_SUB_DIRS,
        EventFlags::ITEM_CREATED,
        EventFlags::ITEM_IS_DIR,
    ]);
    assert_eq!(classify(Path::new("x/y"), flags), subtree("x/y"));
}

// ------------------------------------------------------------ item events

#[test]
fn every_item_level_event_checks_the_path() {
    let item_flags = [
        EventFlags::ITEM_CREATED,
        EventFlags::ITEM_REMOVED,
        EventFlags::ITEM_RENAMED,
        EventFlags::ITEM_MODIFIED,
        EventFlags::ITEM_INODE_META_MOD,
        EventFlags::ITEM_FINDER_INFO_MOD,
        EventFlags::ITEM_CHANGE_OWNER,
        EventFlags::ITEM_XATTR_MOD,
        EventFlags::ITEM_CLONED,
    ];
    let kinds = [
        EventFlags::ITEM_IS_FILE,
        EventFlags::ITEM_IS_DIR,
        EventFlags::ITEM_IS_SYMLINK,
        EventFlags::ITEM_IS_HARDLINK,
        EventFlags::ITEM_IS_LAST_HARDLINK,
    ];
    for item in item_flags {
        assert_eq!(
            classify(Path::new("dir/file.txt"), item),
            check("dir/file.txt"),
            "flags {:#x}",
            item.0
        );
        for kind in kinds {
            let flags = or(&[item, kind]);
            assert_eq!(
                classify(Path::new("dir/file.txt"), flags),
                check("dir/file.txt"),
                "flags {:#x}",
                flags.0
            );
        }
    }
}

#[test]
fn contradictory_hints_still_check_the_path() {
    // Coalesced events can carry created+removed+renamed at once; the flags
    // are hints, so the only safe answer is to stat the path.
    let flags = or(&[
        EventFlags::ITEM_CREATED,
        EventFlags::ITEM_REMOVED,
        EventFlags::ITEM_RENAMED,
        EventFlags::ITEM_MODIFIED,
        EventFlags::ITEM_IS_FILE,
        EventFlags::OWN_EVENT,
    ]);
    assert_eq!(classify(Path::new("a/b/c"), flags), check("a/b/c"));
}

#[test]
fn raw_kernel_words_decode() {
    // ItemIsFile | ItemCreated, as delivered by the kernel.
    assert_eq!(
        classify(Path::new("Users/bob/new.txt"), EventFlags(0x0001_0100)),
        check("Users/bob/new.txt")
    );
    // ItemIsDir | ItemRemoved.
    assert_eq!(
        classify(Path::new("Users/bob/old"), EventFlags(0x0002_0200)),
        check("Users/bob/old")
    );
    // MustScanSubDirs | KernelDropped.
    assert_eq!(
        classify(Path::new("Users/bob"), EventFlags(0x0000_0005)),
        subtree("Users/bob")
    );
}

#[test]
fn unknown_high_bits_do_not_hide_item_events() {
    let flags = EventFlags(EventFlags::ITEM_MODIFIED.0 | 0x8000_0000);
    assert_eq!(classify(Path::new("f"), flags), check("f"));
}

#[test]
fn renames_are_unpaired_check_on_each_path() {
    let flags = or(&[EventFlags::ITEM_RENAMED, EventFlags::ITEM_IS_FILE]);
    assert_eq!(
        classify(Path::new("docs/old.txt"), flags),
        check("docs/old.txt")
    );
    assert_eq!(
        classify(Path::new("docs/new.txt"), flags),
        check("docs/new.txt")
    );
}

#[test]
fn mount_checks_the_mount_point() {
    assert_eq!(
        classify(Path::new("Volumes/USB"), EventFlags::MOUNT),
        check("Volumes/USB")
    );
    let flags = or(&[EventFlags::MOUNT, EventFlags::ITEM_IS_DIR]);
    assert_eq!(
        classify(Path::new("Volumes/USB"), flags),
        check("Volumes/USB")
    );
}

#[test]
fn classify_preserves_non_utf8_and_absolute_paths() {
    assert_eq!(
        classify(Path::new("/private/tmp/x"), EventFlags::ITEM_CREATED),
        check("/private/tmp/x")
    );
    #[cfg(unix)]
    {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let raw = Path::new(OsStr::from_bytes(b"dir/\xff\xfe.bin"));
        assert_eq!(
            classify(raw, EventFlags::ITEM_MODIFIED),
            EventAction::CheckPath(raw.to_path_buf())
        );
    }
}
