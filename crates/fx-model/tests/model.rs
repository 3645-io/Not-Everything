//! Contract tests for the shared types in `fx-model`.
//!
//! Clean-room: written from docs/PLAN.md ("Proposed architecture") only.

use std::collections::HashSet;
use std::mem::size_of;
use std::path::PathBuf;

use fx_model::{
    Change, Entry, EntryFlags, EntryId, EntryKind, FileKey, ResumeCursor, StartOutcome, VolumeId,
};

fn key(volume: u32, file_id: u64) -> FileKey {
    FileKey {
        volume: VolumeId(volume),
        file_id,
    }
}

fn sample_entry() -> Entry {
    Entry {
        key: key(1, 42),
        parent: Some(key(1, 2)),
        name: "report.pdf".to_string(),
        kind: EntryKind::File,
        flags: EntryFlags::HIDDEN,
        size: 1234,
        modified: 1_700_000_000,
        created: 1_600_000_000,
    }
}

fn empty_flags() -> EntryFlags {
    let mut f = EntryFlags::HIDDEN;
    f.remove(EntryFlags::HIDDEN);
    f
}

#[test]
fn entry_id_is_four_bytes() {
    assert_eq!(size_of::<EntryId>(), 4);
}

#[test]
fn entry_id_none_is_u32_max_sentinel() {
    assert_eq!(EntryId::NONE, EntryId(u32::MAX));
    assert_eq!(EntryId::NONE.0, u32::MAX);
    assert_ne!(EntryId(0), EntryId::NONE);
    let copy = EntryId::NONE;
    assert_eq!(copy, EntryId::NONE);
}

#[test]
fn entry_id_and_volume_id_are_copy_and_hash() {
    let a = EntryId(7);
    let b = a; // Copy
    assert_eq!(a, b);
    let mut ids = HashSet::new();
    ids.insert(a);
    ids.insert(b);
    ids.insert(EntryId(8));
    assert_eq!(ids.len(), 2);

    let v = VolumeId(3);
    let w = v;
    assert_eq!(v, w);
    assert_ne!(VolumeId(3), VolumeId(4));
    let vols: HashSet<VolumeId> = [VolumeId(1), VolumeId(1), VolumeId(2)]
        .into_iter()
        .collect();
    assert_eq!(vols.len(), 2);
}

#[test]
fn flags_distinct_and_nonoverlapping() {
    let all = [
        EntryFlags::HIDDEN,
        EntryFlags::SYSTEM,
        EntryFlags::LOCKED,
        EntryFlags::DATALESS,
    ];
    for (i, a) in all.iter().enumerate() {
        assert!(a.contains(*a));
        for (j, b) in all.iter().enumerate() {
            if i != j {
                assert_ne!(a, b);
                assert!(!a.contains(*b), "flag {i} must not contain flag {j}");
            }
        }
    }
}

#[test]
fn flags_set_clear_contains_round_trip() {
    let all = [
        EntryFlags::HIDDEN,
        EntryFlags::SYSTEM,
        EntryFlags::LOCKED,
        EntryFlags::DATALESS,
    ];
    let mut f = empty_flags();
    for flag in all {
        assert!(!f.contains(flag));
    }

    for (n, flag) in all.iter().enumerate() {
        f.insert(*flag);
        for (m, other) in all.iter().enumerate() {
            assert_eq!(f.contains(*other), m <= n);
        }
    }

    // Inserting twice is idempotent.
    let before = f;
    f.insert(EntryFlags::SYSTEM);
    assert_eq!(f, before);

    // Remove one in the middle; the others stay.
    f.remove(EntryFlags::LOCKED);
    assert!(f.contains(EntryFlags::HIDDEN));
    assert!(f.contains(EntryFlags::SYSTEM));
    assert!(!f.contains(EntryFlags::LOCKED));
    assert!(f.contains(EntryFlags::DATALESS));

    // Removing an absent flag is a no-op.
    let before = f;
    f.remove(EntryFlags::LOCKED);
    assert_eq!(f, before);

    for flag in all {
        f.remove(flag);
    }
    assert_eq!(f, empty_flags());
    for flag in all {
        assert!(!f.contains(flag));
    }
}

#[test]
fn flags_are_one_byte_copy_and_hash() {
    assert_eq!(size_of::<EntryFlags>(), 1);
    let a = EntryFlags::DATALESS;
    let b = a;
    assert_eq!(a, b);
    let set: HashSet<EntryFlags> = [EntryFlags::HIDDEN, EntryFlags::HIDDEN, EntryFlags::SYSTEM]
        .into_iter()
        .collect();
    assert_eq!(set.len(), 2);
}

#[test]
fn file_key_equality_and_hashing() {
    assert_eq!(key(1, 10), key(1, 10));
    assert_ne!(key(1, 10), key(2, 10), "volume must be part of identity");
    assert_ne!(key(1, 10), key(1, 11));

    let k = key(5, u64::MAX);
    let copy = k; // Copy
    assert_eq!(k.volume, VolumeId(5));
    assert_eq!(copy.file_id, u64::MAX);

    let mut set = HashSet::new();
    assert!(set.insert(key(1, 10)));
    assert!(!set.insert(key(1, 10)));
    assert!(set.insert(key(2, 10)));
    assert!(set.insert(key(1, 11)));
    assert_eq!(set.len(), 3);
    assert!(set.contains(&key(2, 10)));
    assert!(!set.contains(&key(3, 10)));
}

#[test]
fn entry_kind_variants() {
    let kinds = [EntryKind::File, EntryKind::Dir, EntryKind::Symlink];
    for (i, a) in kinds.iter().enumerate() {
        for (j, b) in kinds.iter().enumerate() {
            assert_eq!(i == j, a == b);
        }
    }
    let k = EntryKind::Dir;
    let copy = k;
    assert_eq!(k, copy);
    let set: HashSet<EntryKind> = kinds.into_iter().collect();
    assert_eq!(set.len(), 3);
}

#[test]
fn entry_construction_clone_and_equality() {
    let e = sample_entry();
    assert_eq!(e.key, key(1, 42));
    assert_eq!(e.parent, Some(key(1, 2)));
    assert_eq!(e.name, "report.pdf");
    assert_eq!(e.kind, EntryKind::File);
    assert!(e.flags.contains(EntryFlags::HIDDEN));
    assert_eq!(e.size, 1234);
    assert_eq!(e.modified, 1_700_000_000);
    assert_eq!(e.created, 1_600_000_000);

    let c = e.clone();
    assert_eq!(c, e);

    let mut renamed = e.clone();
    renamed.name = "report-final.pdf".to_string();
    assert_ne!(renamed, e);

    let mut root = e.clone();
    root.parent = None;
    root.kind = EntryKind::Dir;
    assert_ne!(root, e);

    // Times are signed seconds since the Unix epoch: pre-1970 is representable.
    let mut old = e.clone();
    old.modified = -86_400;
    old.created = -1;
    assert_eq!(old.modified, -86_400);
    assert_ne!(old, e);

    // Debug is implemented.
    assert!(format!("{e:?}").contains("report.pdf"));
}

#[test]
fn resume_cursor_variants() {
    let fse = ResumeCursor::FsEvents {
        event_id: 123_456,
        db_uuid: [7u8; 16],
    };
    let usn = ResumeCursor::Usn {
        journal_id: 99,
        next_usn: -5,
    };
    let none = ResumeCursor::None { snapshot_epoch: 3 };

    assert_eq!(fse.clone(), fse);
    assert_eq!(usn.clone(), usn);
    assert_eq!(none.clone(), none);
    assert_ne!(fse, usn);
    assert_ne!(usn, none);
    assert_ne!(fse, none);

    assert_ne!(
        fse,
        ResumeCursor::FsEvents {
            event_id: 123_456,
            db_uuid: [8u8; 16],
        },
        "a different FSEvents database UUID is a different cursor"
    );
    assert_ne!(
        fse,
        ResumeCursor::FsEvents {
            event_id: 123_457,
            db_uuid: [7u8; 16],
        }
    );
    assert_ne!(
        usn,
        ResumeCursor::Usn {
            journal_id: 100,
            next_usn: -5,
        }
    );
    assert_ne!(none, ResumeCursor::None { snapshot_epoch: 4 });

    match fse {
        ResumeCursor::FsEvents { event_id, db_uuid } => {
            assert_eq!(event_id, 123_456);
            assert_eq!(db_uuid, [7u8; 16]);
        }
        other => panic!("unexpected {other:?}"),
    }
    match usn {
        ResumeCursor::Usn {
            journal_id,
            next_usn,
        } => {
            assert_eq!(journal_id, 99);
            assert_eq!(next_usn, -5);
        }
        other => panic!("unexpected {other:?}"),
    }
    match none {
        ResumeCursor::None { snapshot_epoch } => assert_eq!(snapshot_epoch, 3),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn change_variants() {
    let upsert = Change::Upsert(sample_entry());
    let remove = Change::Remove { key: key(1, 42) };
    let check = Change::CheckPath(PathBuf::from("/Users/a/doc.txt"));
    let subtree = Change::RescanSubtree(PathBuf::from("/Users/a"));
    let volume = Change::RescanVolume;

    let all = [
        upsert.clone(),
        remove.clone(),
        check.clone(),
        subtree.clone(),
        volume.clone(),
    ];
    for (i, a) in all.iter().enumerate() {
        for (j, b) in all.iter().enumerate() {
            assert_eq!(i == j, a == b, "variants {i} and {j}");
        }
    }

    let mut other_entry = sample_entry();
    other_entry.size += 1;
    assert_ne!(upsert, Change::Upsert(other_entry));
    assert_eq!(upsert, Change::Upsert(sample_entry()));
    assert_ne!(remove, Change::Remove { key: key(1, 43) });
    assert_ne!(
        check,
        Change::CheckPath(PathBuf::from("/Users/a/other.txt"))
    );
    assert_ne!(subtree, Change::RescanSubtree(PathBuf::from("/Users/b")));
    // Same path, different intent.
    assert_ne!(
        Change::CheckPath(PathBuf::from("/x")),
        Change::RescanSubtree(PathBuf::from("/x"))
    );

    match upsert {
        Change::Upsert(e) => assert_eq!(e, sample_entry()),
        other => panic!("unexpected {other:?}"),
    }
    match remove {
        Change::Remove { key: k } => assert_eq!(k, key(1, 42)),
        other => panic!("unexpected {other:?}"),
    }
    match check {
        Change::CheckPath(p) => assert_eq!(p, PathBuf::from("/Users/a/doc.txt")),
        other => panic!("unexpected {other:?}"),
    }
    match subtree {
        Change::RescanSubtree(p) => assert_eq!(p, PathBuf::from("/Users/a")),
        other => panic!("unexpected {other:?}"),
    }
    assert!(matches!(volume, Change::RescanVolume));
}

#[test]
fn start_outcome_variants() {
    let all = [
        StartOutcome::Resumed,
        StartOutcome::GapDetected,
        StartOutcome::NoCursorSupport,
    ];
    for (i, a) in all.iter().enumerate() {
        for (j, b) in all.iter().enumerate() {
            assert_eq!(i == j, a == b);
        }
    }
    let o = StartOutcome::GapDetected;
    let copy = o;
    assert_eq!(o, copy);
    assert!(format!("{o:?}").contains("GapDetected"));
}
