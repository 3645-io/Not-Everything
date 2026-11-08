//! FSEvents as an `fx_fs::ChangeSource`.
//!
//! Contract pinned here:
//! - `fx_fs_macos::fsevents::change` (every OS) holds the pure mapping:
//!   - `map_event(path: &Path, flags: EventFlags, rescan_subtree: bool,
//!     volume_root: &Path) -> Option<fx_model::Change>`: `None` only for the
//!     HistoryDone marker; item create/remove/rename/modify give
//!     `CheckPath`; `MustScanSubDirs`, `UserDropped`, `KernelDropped`, a
//!     directory rename (`ITEM_RENAMED | ITEM_IS_DIR`) or `rescan_subtree`
//!     give `RescanSubtree(path)`; `RootChanged`, `EventIdsWrapped` or an
//!     unmount of the watched volume give `RescanVolume`.
//!   - `coalesce(changes: Vec<Change>) -> Vec<Change>`: keeps one copy of
//!     each change, drops a `CheckPath` strictly below a `RescanSubtree` in
//!     the same batch (component-wise), and never drops a rescan.
//! - `CursorTracker::dropped_before_cursor(&self) -> u64` counts replayed
//!   events at or before the replay start that must not be delivered.
//! - `fx_fs_macos::fsevents::source::FsEventsSource` (macOS only):
//!   `FsEventsSource::new(StreamConfig)` implements `fx_fs::ChangeSource`
//!   with `fx_model::ResumeCursor::FsEvents` cursors.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use fx_fs_macos::fsevents::change::{coalesce, map_event};
use fx_fs_macos::fsevents::cursor::CursorTracker;
use fx_fs_macos::fsevents::flags::EventFlags as F;
use fx_model::Change;

const ROOT: &str = "/";

fn map(path: &str, flags: F) -> Option<Change> {
    map_event(Path::new(path), flags, false, Path::new(ROOT))
}

fn check(p: &str) -> Change {
    Change::CheckPath(PathBuf::from(p))
}

fn subtree(p: &str) -> Change {
    Change::RescanSubtree(PathBuf::from(p))
}

// ------------------------------------------------------------ flag routes

#[test]
fn item_events_check_the_path() {
    for flags in [
        F::ITEM_CREATED | F::ITEM_IS_FILE,
        F::ITEM_REMOVED | F::ITEM_IS_FILE,
        F::ITEM_RENAMED | F::ITEM_IS_FILE,
        F::ITEM_MODIFIED | F::ITEM_IS_FILE,
        F::ITEM_INODE_META_MOD | F::ITEM_IS_SYMLINK,
        F::ITEM_CREATED | F::ITEM_IS_DIR,
        F::ITEM_REMOVED | F::ITEM_IS_DIR,
        F::ITEM_XATTR_MOD | F::ITEM_IS_FILE,
        F::NONE,
    ] {
        assert_eq!(
            map("/u/a.txt", flags),
            Some(check("/u/a.txt")),
            "{:#x}",
            flags.bits()
        );
    }
}

#[test]
fn coalesced_or_dropped_events_rescan_the_subtree() {
    for flags in [
        F::MUST_SCAN_SUB_DIRS,
        F::MUST_SCAN_SUB_DIRS | F::USER_DROPPED,
        F::MUST_SCAN_SUB_DIRS | F::KERNEL_DROPPED,
        F::USER_DROPPED,
        F::KERNEL_DROPPED,
    ] {
        assert_eq!(
            map("/u/dir", flags),
            Some(subtree("/u/dir")),
            "{:#x}",
            flags.bits()
        );
    }
}

#[test]
fn directory_rename_rescans_the_subtree() {
    assert_eq!(
        map("/u/dir", F::ITEM_RENAMED | F::ITEM_IS_DIR),
        Some(subtree("/u/dir"))
    );
}

#[test]
fn rescan_subtree_field_rescans_without_flag_bits() {
    let flags = F::ITEM_RENAMED | F::ITEM_IS_DIR;
    assert_eq!(
        map_event(Path::new("/u/dir"), flags, true, Path::new(ROOT)),
        Some(subtree("/u/dir"))
    );
    assert_eq!(
        map_event(
            Path::new("/u/f"),
            F::ITEM_CREATED | F::ITEM_IS_FILE,
            true,
            Path::new(ROOT)
        ),
        Some(subtree("/u/f"))
    );
}

#[test]
fn volume_level_flags_rescan_the_volume() {
    assert_eq!(map("/", F::ROOT_CHANGED), Some(Change::RescanVolume));
    assert_eq!(map("/u", F::EVENT_IDS_WRAPPED), Some(Change::RescanVolume));
    assert_eq!(map("/", F::UNMOUNT), Some(Change::RescanVolume));
    // Volume-level wins over subtree-level and item-level bits.
    assert_eq!(
        map("/u", F::MUST_SCAN_SUB_DIRS | F::EVENT_IDS_WRAPPED),
        Some(Change::RescanVolume)
    );
}

#[test]
fn mount_or_unmount_of_another_volume_checks_the_path() {
    assert_eq!(map("/Volumes/X", F::MOUNT), Some(check("/Volumes/X")));
    assert_eq!(map("/Volumes/X", F::UNMOUNT), Some(check("/Volumes/X")));
}

#[test]
fn history_done_marker_maps_to_nothing() {
    assert_eq!(map("/", F::HISTORY_DONE), None);
}

// ------------------------------------------------------------ coalescing

fn as_set(v: &[Change]) -> HashSet<Change> {
    v.iter().cloned().collect()
}

#[test]
fn duplicate_paths_are_coalesced() {
    let out = coalesce(vec![
        check("/u/a"),
        check("/u/b"),
        check("/u/a"),
        check("/u/a"),
    ]);
    assert_eq!(out.len(), 2, "{out:?}");
    assert_eq!(as_set(&out), as_set(&[check("/u/a"), check("/u/b")]));
}

#[test]
fn check_paths_under_a_rescanned_ancestor_are_dropped() {
    let out = coalesce(vec![
        check("/u/d/x"),
        check("/u/d/sub/y"),
        subtree("/u/d"),
        check("/u/dz"),
        check("/u/other"),
        check("/u/d/late"),
    ]);
    let set = as_set(&out);
    assert!(set.contains(&subtree("/u/d")), "{out:?}");
    assert!(!set.contains(&check("/u/d/x")), "{out:?}");
    assert!(!set.contains(&check("/u/d/sub/y")), "{out:?}");
    assert!(!set.contains(&check("/u/d/late")), "{out:?}");
    // Component-wise: /u/dz is not below /u/d.
    assert!(set.contains(&check("/u/dz")), "{out:?}");
    assert!(set.contains(&check("/u/other")), "{out:?}");
}

#[test]
fn rescans_are_never_dropped_and_the_batch_is_never_emptied() {
    let input = vec![
        subtree("/u/a"),
        check("/u/b"),
        subtree("/u/a"),
        subtree("/v/c"),
    ];
    let out = coalesce(input);
    let set = as_set(&out);
    assert!(set.contains(&subtree("/u/a")));
    assert!(set.contains(&subtree("/v/c")));
    assert!(set.contains(&check("/u/b")), "unrelated paths survive");
    assert_eq!(
        out.iter().filter(|c| **c == subtree("/u/a")).count(),
        1,
        "{out:?}"
    );

    let out = coalesce(vec![check("/u/a"), Change::RescanVolume, check("/u/b")]);
    assert!(out.contains(&Change::RescanVolume), "{out:?}");
    assert!(!out.is_empty());
}

#[test]
fn coalesce_of_nothing_is_nothing() {
    assert!(coalesce(Vec::new()).is_empty());
}

// ------------------------------------------------------------ dropped_before_cursor

#[test]
fn replayed_events_at_or_before_the_cursor_are_counted() {
    let mut t = CursorTracker::replay(100);
    assert_eq!(t.dropped_before_cursor(), 0);
    t.on_event(50);
    t.on_event(100);
    t.on_event(150);
    t.on_event(101);
    assert_eq!(t.dropped_before_cursor(), 2);
}

#[test]
fn since_now_streams_drop_nothing() {
    let mut t = CursorTracker::since_now(500);
    t.on_event(501);
    t.on_event(502);
    assert_eq!(t.dropped_before_cursor(), 0);
}

// ------------------------------------------------------------ live stream

#[cfg(target_os = "macos")]
mod live {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use fx_fs::ChangeSource;
    use fx_fs_macos::fsevents::source::FsEventsSource;
    use fx_fs_macos::fsevents::stream::StreamConfig;
    use fx_model::{Change, ResumeCursor, StartOutcome};

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(tag: &str) -> TempRoot {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let p = std::env::temp_dir().join(format!("fx-fsev-src-{tag}-{nanos}"));
            fs::create_dir_all(&p).expect("create temp root");
            TempRoot(fs::canonicalize(&p).expect("canonicalize"))
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn source(root: &Path) -> FsEventsSource {
        let mut cfg = StreamConfig::new(PathBuf::from("/"));
        cfg.path_filter = Some(root.to_path_buf());
        cfg.latency = Duration::from_millis(10);
        let mut src = FsEventsSource::new(cfg);
        let outcome = src.start(None).expect("start");
        assert_eq!(outcome, StartOutcome::NoCursorSupport);
        src
    }

    /// How long a change may take to arrive in these tests: far above the
    /// 1 s freshness gate, which the engine's `freshness_bench` example
    /// measures, so a loaded machine does not fail them.
    const CHANGE_LIMIT: Duration = Duration::from_secs(10);

    /// Waits up to [`CHANGE_LIMIT`] for a change that covers `path`.
    fn expect_change(src: &mut FsEventsSource, path: &Path, what: &str) {
        let started = Instant::now();
        let deadline = started + CHANGE_LIMIT;
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            let batch = src
                .next_batch(deadline.min(Instant::now() + Duration::from_millis(200)))
                .expect("next_batch");
            for c in batch {
                let hit = match &c {
                    Change::CheckPath(p) => p == path,
                    Change::RescanSubtree(p) => path.starts_with(p),
                    Change::RescanVolume => true,
                    _ => false,
                };
                if hit {
                    return;
                }
                seen.push(c);
            }
        }
        panic!(
            "{what}: no change for {} within {CHANGE_LIMIT:?}; saw {seen:?}",
            path.display()
        );
    }

    #[test]
    fn create_is_reported() {
        let root = TempRoot::new("create");
        let mut src = source(&root.0);
        let f = root.0.join("created.txt");
        fs::write(&f, b"x").expect("write");
        expect_change(&mut src, &f, "create");
    }

    #[test]
    fn rename_reports_the_new_side() {
        let root = TempRoot::new("rename");
        let old = root.0.join("old.txt");
        fs::write(&old, b"x").expect("write");
        let mut src = source(&root.0);
        let new = root.0.join("new.txt");
        fs::rename(&old, &new).expect("rename");
        expect_change(&mut src, &new, "rename (new side)");
    }

    #[test]
    fn delete_is_reported() {
        let root = TempRoot::new("delete");
        let f = root.0.join("doomed.txt");
        fs::write(&f, b"x").expect("write");
        let mut src = source(&root.0);
        fs::remove_file(&f).expect("remove");
        expect_change(&mut src, &f, "delete");
    }

    #[test]
    fn cursor_round_trips_through_resume_cursor() {
        let root = TempRoot::new("cursor");
        let src = source(&root.0);
        let saved = src.cursor();
        let ResumeCursor::FsEvents { event_id, .. } = saved else {
            panic!("expected an FsEvents cursor, got {saved:?}");
        };
        assert_ne!(event_id, u64::MAX, "cursor must not be SinceNow");
        drop(src);

        let mut cfg = StreamConfig::new(PathBuf::from("/"));
        cfg.path_filter = Some(root.0.clone());
        let mut again = FsEventsSource::new(cfg);
        assert_eq!(
            again.start(Some(&saved)).expect("resume"),
            StartOutcome::Resumed
        );
        let after = again.cursor();
        let ResumeCursor::FsEvents {
            event_id: id2,
            db_uuid: uuid2,
        } = after
        else {
            panic!("expected an FsEvents cursor, got {after:?}");
        };
        let ResumeCursor::FsEvents { db_uuid, .. } = saved else {
            unreachable!()
        };
        assert_eq!(uuid2, db_uuid);
        assert!(id2 >= event_id);

        let tampered = ResumeCursor::FsEvents {
            event_id,
            db_uuid: [0xEE; 16],
        };
        let mut cfg = StreamConfig::new(PathBuf::from("/"));
        cfg.path_filter = Some(root.0.clone());
        let mut gap = FsEventsSource::new(cfg);
        assert_eq!(
            gap.start(Some(&tampered)).expect("start"),
            StartOutcome::GapDetected
        );
    }
}
