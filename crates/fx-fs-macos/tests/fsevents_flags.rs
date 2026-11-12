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
//! - Priority: HistoryDone, then volume-level rescans (EventIdsWrapped,
//!   RootChanged, UserDropped or KernelDropped on any path; Unmount on a
//!   path equal to `volume_root`, or on `""`, `"/"` or `"."`), then events
//!   above the scope root (run 2.1, below), then subtree rescans
//!   (MustScanSubDirs, a Mount over the scope root, a flag word with no
//!   item or special bit), then CheckPath for every item-level event (flags
//!   are only hints) and for any other Mount or Unmount. Renames are not
//!   paired.
//! - Run 2.1: `volume_root` is the scope root, the root of what the stream
//!   delivers (its path filter, or its volume's mount point without one).
//!   An event on a folder above it that asks for a subtree rescan, mounts
//!   or unmounts over it, creates, removes or renames it, or has a flag
//!   word with no item bit, rescans the scope root
//!   (`RescanSubtree(volume_root)`); any other event above it (metadata)
//!   is `EventAction::Ignore`. `admit(path, flags, filter)` is the stream's
//!   filter: everything without a filter; with one, events at or below
//!   it, the HistoryDone marker, the volume-level flags on any path, and
//!   the events above it that `classify` does not ignore.

use std::path::{Path, PathBuf};

use fx_fs_macos::fsevents::flags::{EventAction, EventFlags, admit, classify as classify_on};

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
fn dropped_events_rescan_the_volume() {
    // FSEvents.h: with UserDropped or KernelDropped "the client must do a
    // full scan of any directories (and their subdirectories, recursively)
    // being monitored by this stream", whatever the event's path.
    assert_eq!(
        classify(Path::new("Users/alice"), EventFlags::USER_DROPPED),
        EventAction::RescanVolume
    );
    assert_eq!(
        classify(Path::new("Users/alice"), EventFlags::KERNEL_DROPPED),
        EventAction::RescanVolume
    );
    let both = or(&[EventFlags::MUST_SCAN_SUB_DIRS, EventFlags::KERNEL_DROPPED]);
    assert_eq!(classify(Path::new("tmp"), both), EventAction::RescanVolume);
    let both = or(&[EventFlags::MUST_SCAN_SUB_DIRS, EventFlags::USER_DROPPED]);
    assert_eq!(classify(Path::new("tmp"), both), EventAction::RescanVolume);
    let both = or(&[EventFlags::MUST_SCAN_SUB_DIRS, EventFlags::USER_DROPPED]);
    assert_eq!(classify(Path::new("/"), both), EventAction::RescanVolume);
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
    // MustScanSubDirs | KernelDropped: a full scan of the watched scope.
    assert_eq!(
        classify(Path::new("Users/bob"), EventFlags(0x0000_0005)),
        EventAction::RescanVolume
    );
    // MustScanSubDirs alone.
    assert_eq!(
        classify(Path::new("Users/bob"), EventFlags(0x0000_0001)),
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

// ------------------------------------------------------------ run 2.1

#[test]
fn flag_word_without_item_bits_rescans_the_folder() {
    // FSEvents.h, kFSEventStreamEventFlagNone: "There was some change in
    // the directory at the specific path supplied in this event." A check
    // of the path alone would never list the folder.
    assert_eq!(
        classify(Path::new("/u/dir"), EventFlags::NONE),
        subtree("/u/dir")
    );
    assert_eq!(
        classify(Path::new("/u/dir"), EventFlags::OWN_EVENT),
        subtree("/u/dir")
    );
    // Only bits this SDK does not define: no item bit either.
    assert_eq!(
        classify(Path::new("/u/dir"), EventFlags(0x8000_0000)),
        subtree("/u/dir")
    );
    // A type bit alone names an item: check it.
    assert_eq!(
        classify(Path::new("/u/f"), EventFlags::ITEM_IS_FILE),
        check("/u/f")
    );
}

const SCOPE: &str = "/Users/me/proj";

fn scoped(path: &str, flags: EventFlags) -> EventAction {
    classify_on(Path::new(path), flags, Path::new(SCOPE))
}

#[test]
fn rescans_reported_above_the_scope_root_rescan_the_scope_root() {
    // FSEvents.h's own example coalesces events in a user's Music and
    // Pictures folders into MustScanSubDirs on the user's home folder.
    for above in ["/Users/me", "/Users", "/"] {
        assert_eq!(
            scoped(above, EventFlags::MUST_SCAN_SUB_DIRS),
            subtree(SCOPE),
            "MustScanSubDirs on {above}"
        );
    }
    assert_eq!(scoped("", EventFlags::MUST_SCAN_SUB_DIRS), subtree(SCOPE));
}

#[test]
fn moving_creating_or_removing_a_folder_above_the_scope_root_rescans_it() {
    // What kFSEventStreamCreateFlagWatchRoot's RootChanged reports for a
    // stream rooted at the folder: the indexed root moved, went away or
    // came back.
    for flags in [
        or(&[EventFlags::ITEM_RENAMED, EventFlags::ITEM_IS_DIR]),
        or(&[EventFlags::ITEM_REMOVED, EventFlags::ITEM_IS_DIR]),
        or(&[EventFlags::ITEM_CREATED, EventFlags::ITEM_IS_DIR]),
        or(&[
            EventFlags::ITEM_CREATED,
            EventFlags::ITEM_REMOVED,
            EventFlags::ITEM_IS_DIR,
        ]),
        EventFlags::NONE,
    ] {
        for above in ["/Users/me", "/Users"] {
            assert_eq!(
                scoped(above, flags),
                subtree(SCOPE),
                "flags {:#x} on {above}",
                flags.0
            );
        }
    }
}

#[test]
fn mounting_or_unmounting_over_the_scope_root_rescans_it() {
    let mount = or(&[EventFlags::MOUNT, EventFlags::ITEM_IS_DIR]);
    let unmount = or(&[EventFlags::UNMOUNT, EventFlags::ITEM_IS_DIR]);
    assert_eq!(scoped("/Users/me", mount), subtree(SCOPE));
    assert_eq!(scoped("/Users/me", unmount), subtree(SCOPE));
    // A volume mounted on the scope root itself hides what was indexed.
    assert_eq!(scoped(SCOPE, mount), subtree(SCOPE));
    // Unmounting the scope root stays a volume rescan.
    assert_eq!(scoped(SCOPE, unmount), EventAction::RescanVolume);
    // A mount below the scope root is only a path check.
    assert_eq!(
        scoped("/Users/me/proj/disk", mount),
        check("/Users/me/proj/disk")
    );
}

#[test]
fn metadata_changes_above_the_scope_root_are_ignored() {
    for flags in [
        or(&[EventFlags::ITEM_INODE_META_MOD, EventFlags::ITEM_IS_DIR]),
        or(&[EventFlags::ITEM_XATTR_MOD, EventFlags::ITEM_IS_DIR]),
        or(&[EventFlags::ITEM_FINDER_INFO_MOD, EventFlags::ITEM_IS_DIR]),
        or(&[EventFlags::ITEM_CHANGE_OWNER, EventFlags::ITEM_IS_DIR]),
        or(&[EventFlags::ITEM_MODIFIED, EventFlags::ITEM_IS_DIR]),
    ] {
        assert_eq!(
            scoped("/Users/me", flags),
            EventAction::Ignore,
            "flags {:#x}",
            flags.0
        );
    }
}

#[test]
fn volume_level_flags_above_or_beside_the_scope_root_still_rescan_the_volume() {
    for path in ["/Users/me", "/", "/Library/x", SCOPE] {
        for flags in [
            EventFlags::EVENT_IDS_WRAPPED,
            EventFlags::ROOT_CHANGED,
            or(&[EventFlags::MUST_SCAN_SUB_DIRS, EventFlags::KERNEL_DROPPED]),
            or(&[EventFlags::MUST_SCAN_SUB_DIRS, EventFlags::USER_DROPPED]),
        ] {
            assert_eq!(
                scoped(path, flags),
                EventAction::RescanVolume,
                "flags {:#x} on {path}",
                flags.0
            );
        }
    }
}

#[test]
fn events_at_or_below_the_scope_root_keep_their_routes() {
    let file = or(&[EventFlags::ITEM_CREATED, EventFlags::ITEM_IS_FILE]);
    assert_eq!(
        scoped("/Users/me/proj/a.txt", file),
        check("/Users/me/proj/a.txt")
    );
    let meta = or(&[EventFlags::ITEM_INODE_META_MOD, EventFlags::ITEM_IS_DIR]);
    assert_eq!(scoped(SCOPE, meta), check(SCOPE));
    let renamed = or(&[EventFlags::ITEM_RENAMED, EventFlags::ITEM_IS_DIR]);
    assert_eq!(scoped(SCOPE, renamed), check(SCOPE));
    assert_eq!(
        scoped("/Users/me/proj/sub", EventFlags::MUST_SCAN_SUB_DIRS),
        subtree("/Users/me/proj/sub")
    );
    // Component-wise: /Users/me/pro is not above /Users/me/proj.
    assert_eq!(scoped("/Users/me/pro", renamed), check("/Users/me/pro"));
}

// ------------------------------------------------------------ admit (the stream's filter)

fn admitted(path: &str, flags: EventFlags) -> bool {
    admit(Path::new(path), flags, Some(Path::new(SCOPE)))
}

#[test]
fn without_a_filter_everything_is_admitted() {
    for flags in [
        EventFlags::NONE,
        EventFlags::ITEM_CREATED,
        EventFlags::UNMOUNT,
        EventFlags::ITEM_INODE_META_MOD,
    ] {
        assert!(admit(Path::new("/anything"), flags, None));
        assert!(admit(Path::new("/"), flags, Some(Path::new("/"))));
    }
}

#[test]
fn the_filter_admits_its_own_subtree_and_the_marker() {
    let file = or(&[EventFlags::ITEM_CREATED, EventFlags::ITEM_IS_FILE]);
    assert!(admitted(SCOPE, file));
    assert!(admitted("/Users/me/proj/a/b.txt", file));
    assert!(!admitted("/Users/me/other.txt", file));
    assert!(!admitted("/Users/me/projX/a.txt", file), "component-wise");
    assert!(admitted("/", EventFlags::HISTORY_DONE));
    assert!(admitted("/elsewhere", EventFlags::HISTORY_DONE));
}

#[test]
fn the_filter_admits_volume_level_flags_on_any_path() {
    for flags in [
        EventFlags::EVENT_IDS_WRAPPED,
        EventFlags::ROOT_CHANGED,
        or(&[EventFlags::MUST_SCAN_SUB_DIRS, EventFlags::USER_DROPPED]),
        or(&[EventFlags::MUST_SCAN_SUB_DIRS, EventFlags::KERNEL_DROPPED]),
    ] {
        for path in ["/", "/System/Volumes/Data", "/Users/me/other"] {
            assert!(admitted(path, flags), "flags {:#x} on {path}", flags.0);
        }
    }
}

#[test]
fn the_filter_admits_events_above_it_that_rescan_it() {
    let renamed = or(&[EventFlags::ITEM_RENAMED, EventFlags::ITEM_IS_DIR]);
    let removed = or(&[EventFlags::ITEM_REMOVED, EventFlags::ITEM_IS_DIR]);
    let created = or(&[EventFlags::ITEM_CREATED, EventFlags::ITEM_IS_DIR]);
    let meta = or(&[EventFlags::ITEM_INODE_META_MOD, EventFlags::ITEM_IS_DIR]);
    for above in ["/Users/me", "/Users", "/"] {
        assert!(admitted(above, EventFlags::MUST_SCAN_SUB_DIRS), "{above}");
        assert!(admitted(above, renamed), "{above}");
        assert!(admitted(above, removed), "{above}");
        assert!(admitted(above, created), "{above}");
        assert!(admitted(above, EventFlags::MOUNT), "{above}");
        assert!(admitted(above, EventFlags::UNMOUNT), "{above}");
        assert!(!admitted(above, meta), "metadata above the filter: {above}");
    }
    // Beside the filter, not above it: not admitted.
    assert!(!admitted("/Users/me/other", EventFlags::MUST_SCAN_SUB_DIRS));
    assert!(!admitted("/Users/me/other", renamed));
    assert!(!admitted("/Volumes/USB", EventFlags::UNMOUNT));
    assert!(!admitted("/Users/me/pro", renamed), "component-wise");
}
