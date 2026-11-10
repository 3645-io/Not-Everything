//! `IndexBuilder` (run 2.17): a first scan built straight into the base
//! columns equals the same entries upserted and compacted.

use fx_index::{Index, IndexBuilder, IndexError};
use fx_model::{Entry, EntryFlags, EntryKind, FileKey, VolumeId};

fn key(n: u64) -> FileKey {
    FileKey::new(VolumeId(7), n)
}

fn entry(n: u64, parent: Option<u64>, name: &str, dir: bool) -> Entry {
    Entry {
        key: key(n),
        parent: parent.map(key),
        name: name.to_string(),
        kind: if dir { EntryKind::Dir } else { EntryKind::File },
        flags: if n % 5 == 0 {
            EntryFlags::HIDDEN
        } else {
            EntryFlags::empty()
        },
        size: if dir { 0 } else { n * 13 },
        modified: 1_000 + n as i64,
        created: 500 + n as i64,
    }
}

/// A root, 40 folders and 2,000 files, parent before child.
fn scan() -> Vec<Entry> {
    let mut out = vec![entry(1, None, "root", true)];
    for d in 0..40u64 {
        out.push(entry(2 + d, Some(1), &format!("dir{d}"), true));
    }
    for f in 0..2000u64 {
        let ext = ["txt", "rs", "png", "md"][f as usize % 4];
        out.push(entry(
            100 + f,
            Some(2 + f % 40),
            &format!("file{f}.{ext}"),
            false,
        ));
    }
    out
}

#[test]
fn the_builder_equals_upserting_and_compacting() {
    let mut builder = IndexBuilder::new();
    let mut upserted = Index::new();
    upserted.set_compact_fraction(f64::INFINITY);
    for e in scan() {
        let a = builder.push(e.clone()).unwrap();
        let b = upserted.upsert(e).unwrap();
        assert_eq!(a, b, "the same IDs in the same order");
    }
    let _ = upserted.compact();
    let built = builder.finish();
    assert_eq!(built.live_count(), upserted.live_count());
    assert_eq!(built.snapshot().to_parts(), upserted.snapshot().to_parts());
    assert!(
        built.snapshot().heap_bytes() <= upserted.snapshot().heap_bytes(),
        "every column trimmed to its length"
    );
    for e in scan() {
        assert_eq!(built.get(e.key), Some(e));
    }
    let root = built.lookup(key(1)).unwrap();
    assert_eq!(built.children(root).len(), 40);
    let dir3 = built.child(root, "dir3").unwrap();
    assert_eq!(
        built.child(dir3, "file3.md").map(|c| built.key(c)),
        Some(key(103))
    );
}

#[test]
fn an_unknown_parent_is_rejected_and_changes_nothing() {
    let mut builder = IndexBuilder::new();
    builder.push(entry(1, None, "root", true)).unwrap();
    let err = builder
        .push(entry(5, Some(99), "orphan", false))
        .unwrap_err();
    assert!(matches!(err, IndexError::UnknownParent { .. }), "{err:?}");
    assert!(matches!(
        builder.push(entry(6, Some(1), "", false)).unwrap_err(),
        IndexError::InvalidName { .. }
    ));
    assert_eq!(builder.len(), 1);
    let index = builder.finish();
    assert_eq!(index.live_count(), 1);
    assert!(index.lookup(key(5)).is_none());
}

#[test]
fn a_repeated_key_keeps_the_first_entry() {
    let mut builder = IndexBuilder::new();
    builder.push(entry(1, None, "root", true)).unwrap();
    builder.push(entry(2, Some(1), "a", true)).unwrap();
    let first = builder
        .push(entry(3, Some(1), "link-1.txt", false))
        .unwrap();
    // The same file again under another folder (a second hard link).
    let again = builder
        .push(entry(3, Some(2), "link-2.txt", false))
        .unwrap();
    assert_eq!(first, again);
    assert_eq!(builder.duplicates(), 1);
    assert_eq!(builder.len(), 3);
    let index = builder.finish();
    assert_eq!(index.get(key(3)).unwrap().name, "link-1.txt");
}

#[test]
fn an_empty_builder_makes_an_empty_index() {
    let index = IndexBuilder::new().finish();
    assert_eq!(index.live_count(), 0);
    assert!(index.roots().is_empty());
}
