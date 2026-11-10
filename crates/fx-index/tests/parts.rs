//! Bulk export and restore of an index as dense columns.
//!
//! Contract pinned here, so `fx-store` can persist an index without
//! per-entry upserts:
//!
//! - `fx_index::IndexParts` has public, owned columns of plain POD integers
//!   or bytes, each indexed by entry ID `0..id_bound` (dead slots included):
//!   - `name_bytes: Vec<u8>` and `name_offsets: Vec<u32>` (the name buffer
//!     layout; one offset per entry),
//!   - `parents: Vec<u32>` (`u32::MAX` = root),
//!   - `volumes: Vec<u32>` and `file_ids: Vec<u64>` (the `FileKey` split),
//!   - `meta: Vec<u64>` (the packed size/kind/flags),
//!   - `modified: Vec<i64>`, `created: Vec<i64>`,
//!   - `ext_ids: Vec<u16>` (the `ExtId` per entry),
//!   - `ext_bytes: Vec<u8>` and `ext_offsets: Vec<u32>` (the extension
//!     table),
//!   - `live: Vec<u64>` (bitmap words; bit `i % 64` of word `i / 64` set =
//!     entry `i` is live).
//! - `Snapshot::to_parts(&self) -> IndexParts` preserves entry IDs exactly.
//! - `Index::from_parts(IndexParts) -> Result<Index, IndexError>` validates
//!   and never panics; the restored index behaves exactly like the original.
//!
//! Clean-room: written from docs/PLAN.md and the in-repo fx-index code only.

use std::panic::{AssertUnwindSafe, catch_unwind};

use fx_index::{Index, IndexError, IndexParts, Snapshot};
use fx_model::{Entry, EntryFlags, EntryId, EntryKind, FileKey, VolumeId};

const VOL_A: VolumeId = VolumeId(1);
const VOL_B: VolumeId = VolumeId(7);

fn ka(n: u64) -> FileKey {
    FileKey::new(VOL_A, n)
}

fn kb(n: u64) -> FileKey {
    FileKey::new(VOL_B, n)
}

fn mk(key: FileKey, parent: Option<FileKey>, name: &str, kind: EntryKind) -> Entry {
    let n = key.file_id;
    let mut flags = EntryFlags::empty();
    if n % 5 == 0 {
        flags.insert(EntryFlags::HIDDEN);
    }
    if n % 7 == 0 {
        flags.insert(EntryFlags::DATALESS);
    }
    Entry {
        key,
        parent,
        name: name.to_string(),
        kind,
        flags,
        size: if kind == EntryKind::File {
            n * 1013 + 1
        } else {
            0
        },
        modified: 1_700_000_000 + n as i64,
        created: -(n as i64) * 31,
    }
}

fn dir(key: FileKey, parent: Option<FileKey>, name: &str) -> Entry {
    mk(key, parent, name, EntryKind::Dir)
}

fn file(key: FileKey, parent: FileKey, name: &str) -> Entry {
    mk(key, Some(parent), name, EntryKind::File)
}

/// A two-volume index with a compacted base, a delta of new entries,
/// renames and moves of base entries, removals in both base and delta, a
/// removed subtree, and a key that was removed and inserted again.
fn rich_index() -> Index {
    let mut index = Index::new();
    index.set_compact_fraction(f64::INFINITY);
    let base = [
        dir(ka(1), None, "vol"),
        dir(ka(2), Some(ka(1)), "docs"),
        file(ka(3), ka(2), "a.txt"),
        file(ka(4), ka(2), "B.TXT"),
        dir(ka(5), Some(ka(2)), "sub"),
        file(ka(6), ka(5), "c.rs"),
        file(ka(7), ka(5), "d.tar.gz"),
        dir(ka(8), Some(ka(1)), "pics"),
        file(ka(9), ka(8), "p.jpg"),
        file(ka(10), ka(1), "Makefile"),
        file(ka(11), ka(1), ".hidden"),
        file(ka(12), ka(8), "résumé.pdf"),
        dir(kb(1), None, "other"),
        file(kb(2), kb(1), "x.txt"),
        dir(kb(3), Some(kb(1)), "deep"),
        file(kb(4), kb(3), "y.md"),
    ];
    for e in base {
        index.upsert(e).expect("insert base entry");
    }
    for i in 0..40u64 {
        index
            .upsert(file(ka(100 + i), ka(2), &format!("bulk{i}.log")))
            .expect("insert bulk entry");
    }
    index.compact();

    // Delta: new entries, including a new extension.
    index
        .upsert(dir(ka(20), Some(ka(1)), "new dir"))
        .expect("insert");
    index
        .upsert(file(ka(21), ka(20), "fresh.swift"))
        .expect("insert");
    index
        .upsert(file(ka(22), ka(20), "gone.tmp"))
        .expect("insert");
    index.upsert(file(kb(5), kb(3), "z.json")).expect("insert");
    // Rename and move base entries.
    index
        .upsert(file(ka(3), ka(2), "a-renamed.txt"))
        .expect("rename");
    index
        .upsert(file(ka(9), ka(20), "p-moved.jpg"))
        .expect("move");
    let mut grown = file(ka(10), ka(1), "Makefile");
    grown.size = 999_999;
    index.upsert(grown).expect("update");
    // Removals: base leaf, delta leaf, base subtree, bulk entries.
    assert_eq!(index.remove(ka(4)), 1);
    assert_eq!(index.remove(ka(22)), 1);
    assert_eq!(index.remove(ka(5)), 3);
    for i in (0..40u64).step_by(3) {
        assert_eq!(index.remove(ka(100 + i)), 1);
    }
    // Remove a key and insert it again: the dead slot keeps the key, the
    // live one gets a new ID.
    assert_eq!(index.remove(kb(2)), 1);
    index
        .upsert(file(kb(2), kb(1), "x-again.txt"))
        .expect("re-insert");
    assert!(index.delta_len() > 0, "fixture must keep a delta");
    assert!(index.deleted_len() > 0, "fixture must keep dead slots");
    index
}

/// Every key the fixture ever used, live or not.
fn all_keys() -> Vec<FileKey> {
    let mut keys: Vec<FileKey> = (1..=12).map(ka).collect();
    keys.extend((20..=22).map(ka));
    keys.extend((100..140).map(ka));
    keys.extend((1..=5).map(kb));
    keys.push(ka(9999));
    keys
}

fn assert_same(a: &Snapshot, b: &Snapshot) {
    assert_eq!(a.id_bound(), b.id_bound(), "id_bound");
    assert_eq!(a.live_count(), b.live_count(), "live_count");
    assert_eq!(
        a.live_ids().collect::<Vec<_>>(),
        b.live_ids().collect::<Vec<_>>(),
        "live_ids"
    );
    for i in 0..a.id_bound() {
        let id = EntryId(i as u32);
        assert_eq!(a.is_live(id), b.is_live(id), "is_live {i}");
        assert_eq!(a.key(id), b.key(id), "key {i}");
        assert_eq!(a.name(id), b.name(id), "name {i}");
        assert_eq!(a.parent(id), b.parent(id), "parent {i}");
        assert_eq!(a.kind(id), b.kind(id), "kind {i}");
        assert_eq!(a.flags(id), b.flags(id), "flags {i}");
        assert_eq!(a.size(id), b.size(id), "size {i}");
        assert_eq!(a.modified(id), b.modified(id), "modified {i}");
        assert_eq!(a.created(id), b.created(id), "created {i}");
        assert_eq!(a.ext(id), b.ext(id), "ext {i}");
    }
    for id in a.live_ids() {
        assert_eq!(a.path(id), b.path(id), "path {id:?}");
        assert_eq!(b.lookup(a.key(id)), Some(id), "lookup of live {id:?}");
    }
    let (ea, eb) = (a.extensions(), b.extensions());
    assert_eq!(ea.len(), eb.len(), "extension count");
    for i in 0..=ea.len() + 1 {
        let ext = fx_index::ExtId(i as u16);
        assert_eq!(ea.as_str(ext), eb.as_str(ext), "extension {i}");
        if let Some(s) = ea.as_str(ext) {
            assert_eq!(eb.lookup(s), Some(ext), "extension lookup {s}");
        }
    }
}

fn assert_parts_equal(a: &IndexParts, b: &IndexParts) {
    // The annotations pin the column types as plain POD vectors, so another
    // crate can cast them with bytemuck.
    fn same<T: PartialEq>(what: &str, a: &[T], b: &[T]) {
        assert!(a == b, "column {what} differs");
    }
    same::<u8>("name_bytes", &a.name_bytes, &b.name_bytes);
    same::<u32>("name_offsets", &a.name_offsets, &b.name_offsets);
    same::<u32>("parents", &a.parents, &b.parents);
    same::<u32>("volumes", &a.volumes, &b.volumes);
    same::<u64>("file_ids", &a.file_ids, &b.file_ids);
    same::<u64>("meta", &a.meta, &b.meta);
    same::<i64>("modified", &a.modified, &b.modified);
    same::<i64>("created", &a.created, &b.created);
    same::<u16>("ext_ids", &a.ext_ids, &b.ext_ids);
    same::<u8>("ext_bytes", &a.ext_bytes, &b.ext_bytes);
    same::<u32>("ext_offsets", &a.ext_offsets, &b.ext_offsets);
    same::<u64>("live", &a.live, &b.live);
}

fn bit(words: &[u64], i: usize) -> bool {
    (words[i / 64] >> (i % 64)) & 1 == 1
}

#[test]
fn to_parts_columns_are_dense_and_keep_entry_ids() {
    let index = rich_index();
    let snap = index.snapshot();
    let parts = snap.to_parts();
    let n = snap.id_bound();
    assert_eq!(parts.name_offsets.len(), n);
    assert_eq!(parts.parents.len(), n);
    assert_eq!(parts.volumes.len(), n);
    assert_eq!(parts.file_ids.len(), n);
    assert_eq!(parts.meta.len(), n);
    assert_eq!(parts.modified.len(), n);
    assert_eq!(parts.created.len(), n);
    assert_eq!(parts.ext_ids.len(), n);
    assert!(parts.live.len() * 64 >= n, "live bitmap too short");
    for i in 0..n {
        let id = EntryId(i as u32);
        assert_eq!(bit(&parts.live, i), snap.is_live(id), "live bit {i}");
        assert_eq!(parts.parents[i], snap.parent(id).0, "parent {i}");
        assert_eq!(
            VolumeId(parts.volumes[i]),
            snap.key(id).volume,
            "volume {i}"
        );
        assert_eq!(parts.file_ids[i], snap.key(id).file_id, "file id {i}");
        assert_eq!(parts.modified[i], snap.modified(id), "modified {i}");
        assert_eq!(parts.created[i], snap.created(id), "created {i}");
        assert_eq!(parts.ext_ids[i], snap.ext(id).0, "ext {i}");
    }
}

#[test]
fn round_trip_preserves_every_accessor() {
    let index = rich_index();
    let snap = index.snapshot();
    let restored = Index::from_parts(snap.to_parts()).expect("valid parts restore");
    let rsnap = restored.snapshot();
    assert_same(&snap, &rsnap);
    assert_eq!(restored.live_count(), index.live_count());
    assert_eq!(restored.deleted_len(), index.deleted_len());
    for k in all_keys() {
        assert_eq!(restored.lookup(k), index.lookup(k), "Index::lookup {k:?}");
        assert_eq!(rsnap.lookup(k), snap.lookup(k), "Snapshot::lookup {k:?}");
    }
    // Exporting the restored index gives the same columns again.
    assert_parts_equal(&snap.to_parts(), &rsnap.to_parts());
}

#[test]
fn round_trip_of_an_old_snapshot_ignores_later_changes() {
    let mut index = rich_index();
    let snap = index.snapshot();
    index
        .upsert(file(ka(30), ka(1), "later.txt"))
        .expect("insert");
    index.remove(ka(20));
    index.compact();
    let restored = Index::from_parts(snap.to_parts()).expect("restore");
    assert_same(&snap, &restored.snapshot());
}

#[test]
fn empty_index_round_trips() {
    let index = Index::new();
    let parts = index.snapshot().to_parts();
    assert!(parts.parents.is_empty());
    let mut restored = Index::from_parts(parts).expect("empty parts restore");
    assert_eq!(restored.live_count(), 0);
    assert_eq!(restored.snapshot().id_bound(), 0);
    let id = restored.upsert(dir(ka(1), None, "root")).expect("insert");
    assert_eq!(id, EntryId(0));
}

#[test]
fn restored_index_accepts_the_same_later_changes() {
    let mut original = rich_index();
    let mut restored = Index::from_parts(original.snapshot().to_parts()).expect("restore");
    restored.set_compact_fraction(f64::INFINITY);

    fn op(n: usize, i: &mut Index) -> String {
        match n {
            0 => format!("{:?}", i.upsert(file(ka(50), ka(20), "added.txt"))),
            1 => format!("{:?}", i.upsert(file(ka(51), ka(1), "NEW.EXT"))),
            2 => format!("{:?}", i.upsert(file(ka(3), ka(20), "moved-again.txt"))),
            3 => format!("{:?}", i.upsert(dir(ka(2), Some(ka(1)), "documents"))),
            // A cycle must still be refused (parent pointers restored).
            4 => format!("{:?}", i.upsert(dir(ka(1), Some(ka(20)), "vol"))),
            // An unknown parent must still be refused (key map restored).
            5 => format!("{:?}", i.upsert(file(ka(52), ka(4), "orphan"))),
            // Leaf removal and subtree removal (child counts restored).
            6 => format!("{}", i.remove(ka(21))),
            7 => format!("{}", i.remove(ka(20))),
            8 => format!("{}", i.remove(kb(1))),
            9 => format!("{}", i.remove(ka(4))),
            _ => format!("{:?}", i.upsert(dir(kb(1), None, "other again"))),
        }
    }
    for n in 0..=10 {
        assert_eq!(op(n, &mut restored), op(n, &mut original), "op {n}");
        assert_eq!(restored.live_count(), original.live_count(), "op {n}");
        assert_same(&original.snapshot(), &restored.snapshot());
    }

    let remap_a = original.compact();
    let remap_b = restored.compact();
    assert_eq!(remap_a, remap_b, "compaction remap");
    assert_eq!(restored.deleted_len(), 0);
    assert_same(&original.snapshot(), &restored.snapshot());
    for k in all_keys() {
        assert_eq!(restored.lookup(k), original.lookup(k), "lookup {k:?}");
    }
}

#[test]
fn restored_index_auto_compacts_like_a_fresh_one() {
    let original = rich_index();
    let mut restored = Index::from_parts(original.snapshot().to_parts()).expect("restore");
    restored.set_compact_fraction(0.0);
    let id = restored
        .upsert(file(ka(60), ka(1), "trigger.txt"))
        .expect("insert");
    assert_eq!(
        restored.deleted_len(),
        0,
        "auto compaction dropped dead slots"
    );
    assert_eq!(restored.delta_len(), 0);
    let snap = restored.snapshot();
    assert_eq!(snap.lookup(ka(60)), Some(id));
    assert_eq!(snap.name(id), "trigger.txt");
    assert_eq!(snap.path(id), "vol/trigger.txt");
    assert!(restored.take_auto_remap().is_some());
}

// ---------------------------------------------------------------------------
// Malformed parts: rejected with an error, never a panic.
// ---------------------------------------------------------------------------

/// A small valid export: four live entries with extensions, one dead.
fn small_parts() -> IndexParts {
    let mut index = Index::new();
    index.set_compact_fraction(f64::INFINITY);
    index.upsert(dir(ka(1), None, "root")).expect("insert");
    index.upsert(file(ka(2), ka(1), "one.txt")).expect("insert");
    index.upsert(file(ka(3), ka(1), "two.rs")).expect("insert");
    index
        .upsert(file(ka(4), ka(1), "three.md"))
        .expect("insert");
    index.upsert(file(ka(5), ka(1), "dead.md")).expect("insert");
    index.remove(ka(5));
    let parts = index.snapshot().to_parts();
    assert!(Index::from_parts(index.snapshot().to_parts()).is_ok());
    parts
}

fn assert_rejected(what: &str, mutate: impl FnOnce(&mut IndexParts)) {
    let mut parts = small_parts();
    mutate(&mut parts);
    let result = catch_unwind(AssertUnwindSafe(|| Index::from_parts(parts)));
    let result = result.unwrap_or_else(|_| panic!("from_parts panicked on {what}"));
    match result {
        Ok(_) => panic!("from_parts accepted {what}"),
        Err(err) => {
            // Bad data gets its own error variant, not one meant for
            // refused changes.
            match err {
                IndexError::UnknownParent { .. }
                | IndexError::Cycle { .. }
                | IndexError::Full
                | IndexError::NeedsFileSystem => {
                    panic!("{what}: expected a bad-parts error variant, got {err:?}")
                }
                _ => {}
            }
            assert!(!err.to_string().is_empty(), "{what}: empty message");
        }
    }
}

#[test]
fn rejects_columns_of_different_lengths() {
    assert_rejected("short parents", |p| {
        p.parents.pop();
    });
    assert_rejected("long meta", |p| p.meta.push(0));
    assert_rejected("short volumes", |p| {
        p.volumes.pop();
    });
    assert_rejected("short file ids", |p| {
        p.file_ids.pop();
    });
    assert_rejected("short modified", |p| {
        p.modified.pop();
    });
    assert_rejected("long created", |p| p.created.push(0));
    assert_rejected("short ext ids", |p| {
        p.ext_ids.pop();
    });
    assert_rejected("short name offsets", |p| {
        p.name_offsets.pop();
    });
}

#[test]
fn rejects_a_short_live_bitmap() {
    assert_rejected("empty live bitmap", |p| p.live.clear());
}

#[test]
fn rejects_name_offsets_out_of_bounds() {
    assert_rejected("name offset past the end", |p| {
        let last = p.name_offsets.len() - 1;
        p.name_offsets[last] = p.name_bytes.len() as u32 + 10;
    });
    assert_rejected("name offset u32::MAX", |p| p.name_offsets[1] = u32::MAX);
    assert_rejected("truncated name bytes", |p| p.name_bytes.clear());
}

#[test]
fn rejects_invalid_utf8_names() {
    assert_rejected("invalid UTF-8 in a name", |p| {
        let start = p.name_offsets[2] as usize;
        p.name_bytes[start] = 0xFF;
    });
}

#[test]
fn rejects_parents_out_of_range() {
    assert_rejected("parent past id_bound", |p| {
        p.parents[2] = p.parents.len() as u32 + 5;
    });
    assert_rejected("parent equal to id_bound", |p| {
        p.parents[3] = p.parents.len() as u32;
    });
}

#[test]
fn rejects_ext_ids_not_in_the_table() {
    assert_rejected("ext id beyond the table", |p| {
        p.ext_ids[1] = 1000;
    });
}

#[test]
fn rejects_a_malformed_extension_table() {
    assert_rejected("ext offsets out of bounds", |p| {
        assert!(!p.ext_offsets.is_empty());
        p.ext_offsets.fill(u32::MAX);
    });
    assert_rejected("invalid UTF-8 extension", |p| {
        assert!(!p.ext_bytes.is_empty());
        p.ext_bytes.fill(0xFF);
    });
}

#[test]
fn rejects_duplicate_live_keys() {
    assert_rejected("two live entries with one key", |p| {
        p.volumes[3] = p.volumes[2];
        p.file_ids[3] = p.file_ids[2];
    });
}

#[test]
fn accepts_a_dead_slot_sharing_a_live_key() {
    // Removing a key and inserting it again leaves the old key in the dead
    // slot; that is valid and the lookup goes to the live entry.
    let mut index = Index::new();
    index.set_compact_fraction(f64::INFINITY);
    index.upsert(dir(ka(1), None, "root")).expect("insert");
    index.upsert(file(ka(2), ka(1), "a.txt")).expect("insert");
    index.remove(ka(2));
    let again = index.upsert(file(ka(2), ka(1), "a.txt")).expect("insert");
    let snap = index.snapshot();
    assert_eq!(snap.key(EntryId(1)), ka(2));
    assert!(!snap.is_live(EntryId(1)));
    let restored = Index::from_parts(snap.to_parts()).expect("restore");
    assert_eq!(restored.lookup(ka(2)), Some(again));
    assert_same(&snap, &restored.snapshot());
}

#[test]
fn garbage_parts_never_panic() {
    // Arbitrary byte-flips across every column: from_parts may accept or
    // reject, but must never panic, and anything it accepts must be usable.
    let base = small_parts();
    let n = base.parents.len();
    for seed in 0..64u32 {
        let mut p = small_parts();
        let i = seed as usize % n;
        match seed % 8 {
            0 => p.parents[i] = p.parents[i].wrapping_add(seed),
            1 => p.name_offsets[i] = p.name_offsets[i].wrapping_add(seed),
            2 => p.ext_ids[i] = p.ext_ids[i].wrapping_add(seed as u16),
            3 => p.live[0] ^= 1u64 << (seed % 64),
            4 => {
                let j = seed as usize % p.name_bytes.len();
                p.name_bytes[j] ^= 0x80;
            }
            5 => {
                if !p.ext_offsets.is_empty() {
                    let j = seed as usize % p.ext_offsets.len();
                    p.ext_offsets[j] = p.ext_offsets[j].wrapping_add(seed);
                }
            }
            6 => {
                if !p.ext_bytes.is_empty() {
                    let j = seed as usize % p.ext_bytes.len();
                    p.ext_bytes[j] = 0;
                }
            }
            _ => p.file_ids[i] = p.file_ids[(i + 1) % n],
        }
        let result = catch_unwind(AssertUnwindSafe(|| Index::from_parts(p)));
        let result = result.unwrap_or_else(|_| panic!("from_parts panicked (seed {seed})"));
        if let Ok(index) = result {
            let snap = index.snapshot();
            let used = catch_unwind(AssertUnwindSafe(|| {
                for id in snap.live_ids() {
                    let _ = snap.name(id);
                    let _ = snap.ext(id);
                    let _ = snap.lookup(snap.key(id));
                }
            }));
            assert!(used.is_ok(), "accepted parts unusable (seed {seed})");
        }
    }
}

// ---------------------------------------------------------------------------
// Shared columns (run 2.17 step 2): the same index served from a buffer such
// as a saved index's memory map, without copying its base columns.
// ---------------------------------------------------------------------------

use std::ops::Range;
use std::sync::Arc;

use fx_index::{Backing, PartsError, SharedParts};

/// A test buffer: 8-byte aligned, text checked on every call.
#[derive(Debug)]
struct Words(Vec<u64>);

impl Backing for Words {
    fn bytes(&self) -> &[u8] {
        bytemuck::cast_slice(&self.0)
    }

    fn text(&self, range: Range<usize>) -> Option<&str> {
        std::str::from_utf8(self.bytes().get(range)?).ok()
    }
}

/// `bytes` (a whole number of words) in an 8-byte aligned buffer.
fn words(bytes: &[u8]) -> Words {
    Words(
        bytes
            .chunks_exact(8)
            .map(|c| u64::from_le_bytes(c.try_into().expect("8 bytes")))
            .collect(),
    )
}

/// Appends `data` at the next 8-byte boundary plus `skew` bytes.
fn put(bytes: &mut Vec<u8>, skew: usize, data: &[u8]) -> Range<usize> {
    while bytes.len() % 8 != 0 {
        bytes.push(0);
    }
    bytes.extend(std::iter::repeat_n(0xEE, skew));
    let start = bytes.len();
    bytes.extend_from_slice(data);
    start..bytes.len()
}

/// `parts`' per-entry columns laid out in one buffer, as a saved index's
/// map holds them; `skew` bytes before each column misalign it.
fn share(parts: &IndexParts, skew: usize) -> SharedParts {
    let mut b = Vec::new();
    let name_bytes = put(&mut b, skew, &parts.name_bytes);
    let name_offsets = put(&mut b, skew, bytemuck::cast_slice(&parts.name_offsets));
    let parents = put(&mut b, skew, bytemuck::cast_slice(&parts.parents));
    let volumes = put(&mut b, skew, bytemuck::cast_slice(&parts.volumes));
    let file_ids = put(&mut b, skew, bytemuck::cast_slice(&parts.file_ids));
    let meta = put(&mut b, skew, bytemuck::cast_slice(&parts.meta));
    let modified = put(&mut b, skew, bytemuck::cast_slice(&parts.modified));
    let created = put(&mut b, skew, bytemuck::cast_slice(&parts.created));
    let ext_ids = put(&mut b, skew, bytemuck::cast_slice(&parts.ext_ids));
    while b.len() % 8 != 0 {
        b.push(0);
    }
    SharedParts {
        backing: Arc::new(words(&b)),
        name_bytes,
        name_offsets,
        parents,
        volumes,
        file_ids,
        meta,
        modified,
        created,
        ext_ids,
        ext_bytes: parts.ext_bytes.clone(),
        ext_offsets: parts.ext_offsets.clone(),
        live: parts.live.clone(),
    }
}

/// Bytes of the per-entry columns of `parts`.
fn column_bytes(parts: &IndexParts) -> usize {
    let n = parts.id_bound();
    parts.name_bytes.len() + n * (4 + 4 + 4 + 8 + 8 + 8 + 8 + 2)
}

#[test]
fn a_shared_index_matches_the_owned_one_and_maps_its_columns() {
    let parts = rich_index().snapshot().to_parts();
    let owned = Index::from_parts(parts.clone()).expect("owned");
    let shared = Index::from_shared(share(&parts, 0)).expect("shared");
    assert_same(&owned.snapshot(), &shared.snapshot());
    assert_parts_equal(&owned.snapshot().to_parts(), &shared.snapshot().to_parts());
    assert_eq!(owned.mapped_bytes(), 0);
    assert_eq!(shared.mapped_bytes(), column_bytes(&parts));
    assert_eq!(shared.snapshot().mapped_bytes(), column_bytes(&parts));
    // The memory report keeps counting every column the index holds.
    assert!(shared.heap_bytes() >= column_bytes(&parts));
}

#[test]
fn misaligned_columns_are_copied_instead() {
    let parts = rich_index().snapshot().to_parts();
    let owned = Index::from_parts(parts.clone()).expect("owned");
    let shared = Index::from_shared(share(&parts, 1)).expect("shared");
    assert_same(&owned.snapshot(), &shared.snapshot());
    // Bytes need no alignment, so only the names stay shared.
    assert_eq!(shared.mapped_bytes(), parts.name_bytes.len());
}

#[test]
fn a_shared_index_takes_the_same_later_changes() {
    let mut owned = rich_index();
    let mut shared = Index::from_shared(share(&owned.snapshot().to_parts(), 0)).expect("shared");
    shared.set_compact_fraction(f64::INFINITY);
    fn op(n: usize, i: &mut Index) -> String {
        match n {
            0 => format!("{:?}", i.upsert(file(ka(50), ka(20), "añadido.txt"))),
            1 => format!("{:?}", i.upsert(file(ka(3), ka(20), "moved-again.txt"))),
            2 => format!("{:?}", i.upsert(dir(ka(2), Some(ka(1)), "documents"))),
            3 => format!("{:?}", i.upsert(dir(ka(1), Some(ka(20)), "vol"))),
            4 => format!("{}", i.remove(ka(20))),
            _ => format!("{}", i.remove(kb(1))),
        }
    }
    for n in 0..=5 {
        assert_eq!(op(n, &mut shared), op(n, &mut owned), "op {n}");
        assert_same(&owned.snapshot(), &shared.snapshot());
    }
    assert_eq!(shared.compact(), owned.compact(), "compaction remap");
    assert_same(&owned.snapshot(), &shared.snapshot());
    assert_eq!(
        shared.mapped_bytes(),
        0,
        "a compaction builds the base on the heap"
    );
}

fn assert_shared_rejected(what: &str, mutate: impl FnOnce(&mut SharedParts)) -> PartsError {
    let mut parts = share(&small_parts(), 0);
    mutate(&mut parts);
    let result = catch_unwind(AssertUnwindSafe(|| Index::from_shared(parts)));
    match result.unwrap_or_else(|_| panic!("from_shared panicked on {what}")) {
        Ok(_) => panic!("from_shared accepted {what}"),
        Err(IndexError::InvalidParts(e)) => e,
        Err(e) => panic!("{what}: expected a bad-parts error, got {e:?}"),
    }
}

#[test]
fn bad_shared_parts_are_rejected_without_a_panic() {
    let len = |p: &SharedParts| p.backing.bytes().len();
    let e = assert_shared_rejected("a column past the end", |p| {
        p.meta = p.meta.start..len(p) + 8;
    });
    assert_eq!(e, PartsError::SharedColumn { column: "meta" });
    let e = assert_shared_rejected("a column of part values", |p| {
        p.parents = p.parents.start..p.parents.end - 1;
    });
    assert_eq!(e, PartsError::SharedColumn { column: "parents" });
    let e = assert_shared_rejected("a short column", |p| {
        p.created = p.created.start..p.created.end - 8;
    });
    assert!(
        matches!(
            e,
            PartsError::ColumnLength {
                column: "created",
                ..
            }
        ),
        "{e:?}"
    );
    let e = assert_shared_rejected("a name that isn't UTF-8", |p| {
        let mut bytes = p.backing.bytes().to_vec();
        bytes[p.name_bytes.start] = 0xFF;
        p.backing = Arc::new(words(&bytes));
    });
    assert!(matches!(e, PartsError::NameBuffer { .. }), "{e:?}");
    assert_shared_rejected("a parent out of range", |p| {
        let mut bytes = p.backing.bytes().to_vec();
        bytes[p.parents.start..p.parents.start + 4].copy_from_slice(&77u32.to_le_bytes());
        p.backing = Arc::new(words(&bytes));
    });
}

/// An index whose delta is empty and nothing removed: as after a first
/// scan or a compaction.
fn compacted_index() -> Index {
    let mut index = rich_index();
    index.compact();
    assert_eq!((index.delta_len(), index.deleted_len()), (0, 0));
    index
}

#[test]
fn adopt_base_serves_an_exact_base_from_its_save() {
    let mut index = compacted_index();
    let mut reference = compacted_index();
    let saved = index.snapshot();
    // Changes made while the save is written stay in the delta.
    for i in [&mut index, &mut reference] {
        i.upsert(file(ka(70), ka(1), "while saving.txt"))
            .expect("insert");
        i.upsert(file(ka(9), ka(1), "p-back.jpg")).expect("move");
        assert_eq!(i.remove(ka(10)), 1);
    }
    let restored = Index::from_shared(share(&saved.to_parts(), 0)).expect("restore");
    assert!(
        index.adopt_base(&saved, restored),
        "an exact base is adopted"
    );
    assert_eq!(index.mapped_bytes(), column_bytes(&saved.to_parts()));
    assert_same(&reference.snapshot(), &index.snapshot());
    // The index goes on as before: more changes, then a compaction.
    for i in [&mut index, &mut reference] {
        i.upsert(file(ka(71), ka(70), "later.md")).ok();
        i.remove(kb(3));
    }
    assert_same(&reference.snapshot(), &index.snapshot());
    assert_eq!(index.compact(), reference.compact());
    assert_same(&reference.snapshot(), &index.snapshot());
}

#[test]
fn adopt_base_refuses_anything_but_an_exact_save_of_the_current_base() {
    // The saved snapshot had a delta: its file isn't the base.
    let mut index = rich_index();
    let saved = index.snapshot();
    let restored = Index::from_shared(share(&saved.to_parts(), 0)).expect("restore");
    assert!(!index.adopt_base(&saved, restored));
    assert_eq!(index.mapped_bytes(), 0);

    // The base was compacted again after the save.
    let mut index = compacted_index();
    let saved = index.snapshot();
    index.upsert(file(ka(80), ka(1), "x.bin")).expect("insert");
    index.compact();
    let restored = Index::from_shared(share(&saved.to_parts(), 0)).expect("restore");
    assert!(!index.adopt_base(&saved, restored));
    assert_eq!(index.mapped_bytes(), 0);

    // A snapshot of another index.
    let mut index = compacted_index();
    let other = compacted_index().snapshot();
    let restored = Index::from_shared(share(&other.to_parts(), 0)).expect("restore");
    assert!(!index.adopt_base(&other, restored));
}
