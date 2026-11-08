//! Snapshot tests: a snapshot is an immutable, `Send + Sync` view behind an
//! `Arc` that later inserts, deletes, renames, moves and compactions of the
//! `Index` never change.
//!
//! Contract pinned here: `Index::snapshot() -> Arc<Snapshot>`, and
//! a snapshot holds no borrow of the index, so it may outlive it.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::thread;

use fx_index::{Index, Snapshot};
use fx_model::{Entry, EntryFlags, EntryId, EntryKind, FileKey, VolumeId};

const VOL: VolumeId = VolumeId(3);

fn key(n: u64) -> FileKey {
    FileKey::new(VOL, n)
}

fn mk(k: u64, parent: Option<u64>, name: &str, kind: EntryKind) -> Entry {
    Entry {
        key: key(k),
        parent: parent.map(key),
        name: name.to_string(),
        kind,
        flags: EntryFlags::empty(),
        size: if kind == EntryKind::File { k * 7 } else { 0 },
        modified: 1_700_000_000 + k as i64,
        created: 1_600_000_000 + k as i64,
    }
}

fn dir(k: u64, parent: Option<u64>, name: &str) -> Entry {
    mk(k, parent, name, EntryKind::Dir)
}

fn file(k: u64, parent: u64, name: &str) -> Entry {
    mk(k, Some(parent), name, EntryKind::File)
}

/// vol/{docs/{a.txt, b.txt, sub/{c.rs}}, pics/{p.jpg}, Makefile}
fn sample() -> Index {
    let mut index = Index::new();
    for e in [
        dir(1, None, "vol"),
        dir(2, Some(1), "docs"),
        file(3, 2, "a.txt"),
        file(4, 2, "b.txt"),
        dir(5, Some(2), "sub"),
        file(6, 5, "c.rs"),
        dir(7, Some(1), "pics"),
        file(8, 7, "p.jpg"),
        file(9, 1, "Makefile"),
    ] {
        index.upsert(e).expect("insert sample entry");
    }
    index
}

type Row = (
    FileKey,
    String,
    String,
    EntryKind,
    EntryFlags,
    u64,
    i64,
    i64,
    bool,
);

/// Everything a snapshot says about `keys`, with no EntryIds, plus a check
/// that its live IDs are exactly the IDs of the keys it knows.
fn rows(snap: &Snapshot, keys: impl IntoIterator<Item = u64>) -> BTreeSet<Row> {
    let mut ids = BTreeSet::new();
    let mut out = BTreeSet::new();
    for k in keys {
        let Some(id) = snap.lookup(key(k)) else {
            continue;
        };
        ids.insert(id);
        let p = snap.parent(id);
        out.insert((
            key(k),
            snap.path(id),
            snap.name(id).to_string(),
            snap.kind(id),
            snap.flags(id),
            snap.size(id),
            snap.modified(id),
            snap.created(id),
            p == EntryId::NONE,
        ));
    }
    assert_eq!(snap.live_ids().collect::<BTreeSet<_>>(), ids);
    assert_eq!(snap.live_count(), ids.len());
    out
}

fn path(snap: &Snapshot, k: u64) -> Option<String> {
    snap.lookup(key(k)).map(|id| snap.path(id))
}

#[test]
fn snapshot_types_are_send_sync_and_static() {
    fn assert_send_sync<T: Send + Sync + 'static>() {}
    assert_send_sync::<Snapshot>();
    assert_send_sync::<Arc<Snapshot>>();
    fn assert_send<T: Send>() {}
    assert_send::<Index>();
}

#[test]
fn snapshot_is_unaffected_by_later_insert_delete_rename_and_move() {
    let mut index = sample();
    let s1: Arc<Snapshot> = index.snapshot();
    let all = 1..100u64;
    let before = rows(&s1, all.clone());
    assert_eq!(before.len(), 9);

    index.upsert(file(20, 7, "new.png")).unwrap();
    index.upsert(file(21, 2, "doomed.txt")).unwrap();
    assert_eq!(index.remove(key(5)), 2);
    index.upsert(dir(2, Some(1), "papers")).unwrap();
    index.upsert(dir(7, Some(2), "pics")).unwrap();
    let mut big = file(9, 1, "Makefile");
    big.size = 1 << 40;
    big.flags = EntryFlags::HIDDEN;
    index.upsert(big).unwrap();

    assert_eq!(rows(&s1, all.clone()), before, "s1 changed after mutations");
    assert_eq!(path(&s1, 20), None, "s1 must not see a later insert");
    assert_eq!(path(&s1, 6).as_deref(), Some("vol/docs/sub/c.rs"));
    assert_eq!(path(&s1, 8).as_deref(), Some("vol/pics/p.jpg"));
    assert_eq!(s1.size(s1.lookup(key(9)).unwrap()), 63);

    let s2 = index.snapshot();
    assert_eq!(path(&s2, 20).as_deref(), Some("vol/papers/pics/new.png"));
    assert_eq!(path(&s2, 8).as_deref(), Some("vol/papers/pics/p.jpg"));
    assert_eq!(path(&s2, 6), None);
    assert_eq!(s2.size(s2.lookup(key(9)).unwrap()), 1 << 40);
    let s2_rows = rows(&s2, all.clone());

    // Compaction renumbers the index but leaves both snapshots alone.
    index.compact();
    index.remove(key(1));
    assert_eq!(index.live_count(), 0);
    assert_eq!(
        rows(&s1, all.clone()),
        before,
        "s1 changed after compaction"
    );
    assert_eq!(
        rows(&s2, all.clone()),
        s2_rows,
        "s2 changed after compaction"
    );
    assert_eq!(index.snapshot().live_count(), 0);
}

#[test]
fn snapshot_outlives_the_index() {
    let index = sample();
    let snap = index.snapshot();
    drop(index);
    assert_eq!(snap.live_count(), 9);
    assert_eq!(path(&snap, 6).as_deref(), Some("vol/docs/sub/c.rs"));
}

#[test]
fn every_snapshot_freezes_its_own_moment() {
    let mut index = sample();
    index.set_compact_fraction(0.2);
    let mut history: Vec<(Arc<Snapshot>, BTreeSet<Row>)> = Vec::new();
    for step in 0..60u64 {
        match step % 4 {
            0 => {
                index
                    .upsert(file(100 + step, 7, &format!("f{step}.jpg")))
                    .unwrap();
            }
            1 => {
                index
                    .upsert(dir(2, Some(1), &format!("docs{step}")))
                    .unwrap();
            }
            2 => {
                let parent = if step % 8 == 2 { 7 } else { 2 };
                index.upsert(dir(5, Some(parent), "sub")).unwrap();
            }
            _ => {
                index.remove(key(100 + step - 3));
            }
        }
        if step % 20 == 19 {
            index.compact();
        }
        let snap = index.snapshot();
        let r = rows(&snap, 1..200);
        history.push((snap, r));
    }
    for (i, (snap, r)) in history.iter().enumerate() {
        assert_eq!(&rows(snap, 1..200), r, "snapshot {i} changed");
    }
}

#[test]
fn one_snapshot_read_from_many_threads_while_the_index_changes() {
    let mut index = Index::new();
    index.upsert(dir(1, None, "vol")).unwrap();
    for d in 0..40u64 {
        index
            .upsert(dir(10 + d, Some(1), &format!("dir{d}")))
            .unwrap();
        for f in 0..50u64 {
            let k = 1_000 + d * 100 + f;
            index
                .upsert(file(k, 10 + d, &format!("file{f}.dat")))
                .unwrap();
        }
    }
    let snap = index.snapshot();
    let all_keys = || (1..6_000u64).collect::<Vec<_>>();
    let expected = Arc::new(rows(&snap, all_keys()));
    assert_eq!(expected.len(), 1 + 40 + 40 * 50);

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let snap = Arc::clone(&snap);
            let expected = Arc::clone(&expected);
            thread::spawn(move || {
                for _ in 0..5 {
                    assert_eq!(rows(&snap, all_keys()), *expected);
                    let paths: BTreeSet<String> = snap.live_ids().map(|id| snap.path(id)).collect();
                    assert_eq!(paths.len(), expected.len());
                    assert!(paths.contains("vol/dir7/file3.dat"));
                }
            })
        })
        .collect();

    // Keep mutating the index while readers run.
    for d in 0..40u64 {
        index
            .upsert(dir(10 + d, Some(1), &format!("renamed{d}")))
            .unwrap();
        if d % 3 == 0 {
            index.remove(key(10 + d));
        }
        index.upsert(file(9_000 + d, 1, "late.txt")).unwrap();
    }
    index.compact();

    for h in handles {
        h.join().expect("reader thread panicked");
    }
    assert_eq!(rows(&snap, all_keys()), *expected);
    assert_eq!(path(&snap, 1_703).as_deref(), Some("vol/dir7/file3.dat"));
    assert_eq!(
        path(&index.snapshot(), 1_703).as_deref(),
        Some("vol/renamed7/file3.dat")
    );
}

// --- Heap accounting --------------------------------------------------------
//
// Contract: `Snapshot::heap_bytes()` is the heap memory the snapshot's
// shared state holds (base columns, key table, delta overlay, deletion
// bitset, extension table), measured by the same accounting as
// `Index::heap_bytes()`. It leaves out only the writer-only parts of the
// index (the live-children counts and any pending compaction remap).

/// One root, 40 folders of 50 files each.
fn wide_index() -> (Index, usize) {
    let mut index = Index::new();
    let mut name_bytes = 0;
    let mut add = |index: &mut Index, e: Entry| {
        name_bytes += e.name.len();
        index.upsert(e).expect("insert entry");
    };
    add(&mut index, dir(1, None, "vol"));
    for d in 0..40u64 {
        add(&mut index, dir(10 + d, Some(1), &format!("dir{d}")));
        for f in 0..50u64 {
            add(
                &mut index,
                file(1_000 + d * 100 + f, 10 + d, &format!("file{f}.dat")),
            );
        }
    }
    (index, name_bytes)
}

/// Minimum dense bytes per entry ID: parent (4), packed meta (8), two
/// times (16), extension ID (2), volume (4) and file ID (8).
const MIN_SLOT_BYTES: usize = 4 + 8 + 16 + 2 + 4 + 8;

fn assert_matches_index_minus_writer_only(index: &mut Index, snap: &Snapshot, what: &str) {
    // Drop a pending remap so only the children counts remain writer-only.
    index.take_auto_remap();
    let index_heap = index.heap_bytes();
    let snap_heap = snap.heap_bytes();
    assert!(
        snap_heap <= index_heap,
        "{what}: snapshot ({snap_heap}) holds no more than the index ({index_heap})"
    );
    // The live-children counts are one u32 per entry ID; allow for a
    // doubled capacity.
    let writer_only = 2 * std::mem::size_of::<u32>() * snap.id_bound() + 64;
    assert!(
        index_heap - snap_heap <= writer_only,
        "{what}: index {index_heap} - snapshot {snap_heap} exceeds the writer-only \
         children counts (<= {writer_only})"
    );
}

#[test]
fn snapshot_heap_bytes_is_the_index_accounting_without_writer_only_parts() {
    let (mut index, name_bytes) = wide_index();
    let snap = index.snapshot();
    assert!(
        snap.heap_bytes() >= name_bytes + snap.id_bound() * MIN_SLOT_BYTES,
        "snapshot heap {} counts the names ({name_bytes} B) and the columns of {} IDs",
        snap.heap_bytes(),
        snap.id_bound()
    );
    assert_matches_index_minus_writer_only(&mut index, &snap, "after inserts");

    index.compact();
    let snap = index.snapshot();
    assert_matches_index_minus_writer_only(&mut index, &snap, "after compaction");
    assert!(snap.heap_bytes() >= name_bytes + snap.id_bound() * MIN_SLOT_BYTES);

    // An uncompacted delta (updates and inserts) is shared state too.
    index.set_compact_fraction(f64::INFINITY);
    for f in 0..50u64 {
        let mut e = file(1_000 + f, 10, &format!("file{f}.dat"));
        e.size = 999_999 + f;
        index.upsert(e).unwrap();
        index
            .upsert(file(50_000 + f, 11, &format!("extra{f}.txt")))
            .unwrap();
    }
    assert!(index.delta_len() > 0, "the changes stay in the delta");
    let snap = index.snapshot();
    assert_matches_index_minus_writer_only(&mut index, &snap, "with a delta");
}

#[test]
fn snapshot_heap_bytes_counts_the_delta_and_stays_frozen() {
    let (mut index, _) = wide_index();
    index.compact();
    index.set_compact_fraction(f64::INFINITY);
    let compacted = index.snapshot();
    let base_heap = compacted.heap_bytes();
    let bound = compacted.id_bound();

    // Updates of base entries: same IDs, same names, only delta overrides.
    for f in 0..50u64 {
        let mut e = file(1_000 + f, 10, &format!("file{f}.dat"));
        e.size = 123_456 + f;
        index.upsert(e).unwrap();
    }
    assert!(index.delta_len() > 0, "updates stay in the delta");
    let updated = index.snapshot();
    assert_eq!(updated.id_bound(), bound, "updates keep the ID bound");
    assert!(
        updated.heap_bytes() > base_heap,
        "delta overrides are counted: {} > {base_heap}",
        updated.heap_bytes()
    );

    // New entries in the delta add more.
    for f in 0..50u64 {
        index
            .upsert(file(60_000 + f, 12, &format!("added{f}.txt")))
            .unwrap();
    }
    let inserted = index.snapshot();
    assert!(
        inserted.heap_bytes() > updated.heap_bytes(),
        "delta inserts are counted: {} > {}",
        inserted.heap_bytes(),
        updated.heap_bytes()
    );

    // Earlier snapshots keep their own accounting after later writes and a
    // compaction.
    let updated_heap = updated.heap_bytes();
    index.remove(key(12));
    index.compact();
    assert_eq!(compacted.heap_bytes(), base_heap, "snapshot is frozen");
    assert_eq!(updated.heap_bytes(), updated_heap, "snapshot is frozen");
}
