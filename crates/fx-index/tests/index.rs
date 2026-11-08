//! Index mutation tests: insert, update, subtree delete, folder rename and
//! move, unknown parents, re-insert, compaction and a seeded random op
//! sequence checked against a naive model.
//!
//! Contract pinned here:
//! - `Index::new()`, `upsert(Entry) -> Result<EntryId, IndexError>`,
//!   `remove(FileKey) -> usize` (entries removed, descendants included; 0 for
//!   an unknown key), `apply(Change) -> Result<_, IndexError>`.
//! - An entry whose parent key is unknown is rejected with
//!   `IndexError::UnknownParent { .. }` and leaves the index unchanged.
//! - `EntryId`s are stable between compactions; `compact()` returns the remap
//!   as a `Vec<EntryId>` indexed by old ID, `EntryId::NONE` for dropped IDs.
//! - Auto-compaction runs when the delta passes `compact_fraction()` of the
//!   live entries (default 0.10), configurable with `set_compact_fraction`.

use std::collections::{BTreeSet, HashMap};

use fx_index::{ExtId, Index, IndexError};
use fx_model::{Change, Entry, EntryFlags, EntryId, EntryKind, FileKey, VolumeId};

const VOL: VolumeId = VolumeId(1);

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
        size: if kind == EntryKind::File { k * 10 } else { 0 },
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

fn path_of(index: &Index, k: u64) -> Option<String> {
    let snap = index.snapshot();
    snap.lookup(key(k)).map(|id| snap.path(id))
}

#[test]
fn insert_makes_entries_visible_with_all_columns() {
    let index = sample();
    let snap = index.snapshot();
    assert_eq!(snap.live_count(), 9);
    assert_eq!(index.live_count(), 9);

    let id = snap.lookup(key(3)).expect("a.txt is indexed");
    assert_eq!(index.lookup(key(3)), Some(id));
    assert_eq!(snap.name(id), "a.txt");
    assert_eq!(snap.path(id), "vol/docs/a.txt");
    assert_eq!(snap.kind(id), EntryKind::File);
    assert_eq!(snap.flags(id), EntryFlags::empty());
    assert_eq!(snap.size(id), 30);
    assert_eq!(snap.modified(id), 1_700_000_003);
    assert_eq!(snap.created(id), 1_600_000_003);
    assert_eq!(snap.parent(id), snap.lookup(key(2)).unwrap());

    let root = snap.lookup(key(1)).unwrap();
    assert_eq!(snap.parent(root), EntryId::NONE);
    assert_eq!(snap.path(root), "vol");
    assert_eq!(snap.kind(root), EntryKind::Dir);
    assert_eq!(path_of(&index, 6).as_deref(), Some("vol/docs/sub/c.rs"));
    assert_eq!(snap.lookup(key(999)), None);

    let ids: BTreeSet<EntryId> = snap.live_ids().collect();
    let expected: BTreeSet<EntryId> = (1..=9).map(|k| snap.lookup(key(k)).unwrap()).collect();
    assert_eq!(ids, expected);
}

#[test]
fn extension_ids_follow_the_name() {
    let index = sample();
    let snap = index.snapshot();
    let ext = |k| snap.ext(snap.lookup(key(k)).unwrap());
    assert_eq!(ext(3), ext(4), "a.txt and b.txt share an extension");
    assert!(ext(3).is_real());
    assert_ne!(ext(3), ext(6), "txt and rs differ");
    assert_ne!(ext(6), ext(8));
    assert_eq!(ext(9), ExtId::NONE, "Makefile has no extension");
}

#[test]
fn packed_size_kind_and_flags_round_trip() {
    let mut index = Index::new();
    index.upsert(dir(1, None, "vol")).unwrap();
    let max56 = (1u64 << 56) - 1;
    let all = EntryFlags::HIDDEN | EntryFlags::SYSTEM | EntryFlags::LOCKED | EntryFlags::DATALESS;
    let cases = [
        (10, EntryKind::File, EntryFlags::empty(), 0u64, 0u64),
        (11, EntryKind::File, EntryFlags::HIDDEN, 12_345, 12_345),
        (
            12,
            EntryKind::Symlink,
            EntryFlags::DATALESS | EntryFlags::LOCKED,
            max56,
            max56,
        ),
        (13, EntryKind::File, all, max56 + 1, max56),
        (14, EntryKind::File, EntryFlags::SYSTEM, u64::MAX, max56),
        (15, EntryKind::Dir, all, 4096, 4096),
    ];
    for (k, kind, flags, size, _) in cases {
        let mut e = mk(k, Some(1), &format!("e{k}"), kind);
        e.flags = flags;
        e.size = size;
        e.modified = -86_400 * k as i64; // before 1970
        e.created = i64::MIN + k as i64;
        index.upsert(e).unwrap();
    }
    let snap = index.snapshot();
    for (k, kind, flags, _, stored) in cases {
        let id = snap.lookup(key(k)).unwrap();
        assert_eq!(snap.kind(id), kind, "kind of {k}");
        assert_eq!(snap.flags(id), flags, "flags of {k}");
        assert_eq!(snap.size(id), stored, "size of {k} saturates at 56 bits");
        assert_eq!(snap.modified(id), -86_400 * k as i64);
        assert_eq!(snap.created(id), i64::MIN + k as i64);
    }
}

#[test]
fn upsert_of_an_existing_key_updates_in_place() {
    let mut index = sample();
    let before = index.lookup(key(3)).unwrap();
    let mut e = file(3, 2, "a.txt");
    e.size = 777;
    e.modified = 42;
    e.flags = EntryFlags::HIDDEN;
    let id = index.upsert(e).unwrap();
    assert_eq!(id, before, "an update keeps the EntryId");
    assert_eq!(index.live_count(), 9, "an update does not add an entry");
    let snap = index.snapshot();
    assert_eq!(snap.size(id), 777);
    assert_eq!(snap.modified(id), 42);
    assert_eq!(snap.flags(id), EntryFlags::HIDDEN);
    assert_eq!(snap.path(id), "vol/docs/a.txt");
}

#[test]
fn remove_deletes_the_whole_subtree() {
    let mut index = sample();
    let removed = index.remove(key(2));
    assert_eq!(removed, 5, "docs, a.txt, b.txt, sub, c.rs");
    assert_eq!(index.live_count(), 4);
    let snap = index.snapshot();
    assert_eq!(snap.live_count(), 4);
    for k in 2..=6 {
        assert_eq!(snap.lookup(key(k)), None, "key {k} must be gone");
    }
    for k in [1, 7, 8, 9] {
        assert!(snap.lookup(key(k)).is_some(), "key {k} must survive");
    }
    assert_eq!(snap.live_ids().count(), 4);
    assert_eq!(index.remove(key(2)), 0, "removing again is a no-op");
    assert_eq!(index.remove(key(12_345)), 0, "unknown key");
    assert_eq!(index.remove(key(8)), 1, "a file removes only itself");
    assert_eq!(index.live_count(), 3);
}

#[test]
fn renaming_a_folder_renames_every_descendant_path() {
    let mut index = sample();
    let child_before = index.lookup(key(6)).unwrap();
    let id_before = index.lookup(key(2)).unwrap();
    let id = index.upsert(dir(2, Some(1), "papers")).unwrap();
    assert_eq!(id, id_before, "a rename keeps the EntryId");
    assert_eq!(
        index.lookup(key(6)),
        Some(child_before),
        "children are untouched"
    );
    assert_eq!(index.live_count(), 9);
    assert_eq!(path_of(&index, 2).as_deref(), Some("vol/papers"));
    assert_eq!(path_of(&index, 3).as_deref(), Some("vol/papers/a.txt"));
    assert_eq!(path_of(&index, 6).as_deref(), Some("vol/papers/sub/c.rs"));
    assert_eq!(index.snapshot().name(id), "papers");
}

#[test]
fn moving_a_folder_moves_every_descendant_path() {
    let mut index = sample();
    let sub = index.upsert(dir(5, Some(7), "sub")).unwrap();
    let snap = index.snapshot();
    assert_eq!(snap.parent(sub), snap.lookup(key(7)).unwrap());
    assert_eq!(path_of(&index, 5).as_deref(), Some("vol/pics/sub"));
    assert_eq!(path_of(&index, 6).as_deref(), Some("vol/pics/sub/c.rs"));
    // Move and rename at once.
    index.upsert(dir(5, Some(1), "moved")).unwrap();
    assert_eq!(path_of(&index, 6).as_deref(), Some("vol/moved/c.rs"));
    assert_eq!(index.live_count(), 9);
    // Removing the old parent no longer touches the moved folder.
    assert_eq!(index.remove(key(2)), 3, "docs, a.txt, b.txt");
    assert_eq!(path_of(&index, 6).as_deref(), Some("vol/moved/c.rs"));
}

#[test]
fn unknown_parent_is_rejected_and_changes_nothing() {
    let mut index = sample();
    let err = index.upsert(file(50, 4040, "orphan.txt")).unwrap_err();
    assert!(matches!(err, IndexError::UnknownParent { .. }), "{err:?}");
    fn is_error<E: std::error::Error>(_: &E) {}
    is_error(&err);
    assert!(!err.to_string().is_empty());
    assert_eq!(index.live_count(), 9);
    assert_eq!(index.lookup(key(50)), None);

    // Moving an existing entry under an unknown parent is rejected too.
    assert!(index.upsert(file(3, 4040, "a.txt")).is_err());
    assert_eq!(path_of(&index, 3).as_deref(), Some("vol/docs/a.txt"));

    // A deleted parent counts as unknown.
    index.remove(key(5));
    let err = index.upsert(file(51, 5, "late.rs")).unwrap_err();
    assert!(matches!(err, IndexError::UnknownParent { .. }), "{err:?}");
}

#[test]
fn reinsert_after_delete_does_not_resurrect_descendants() {
    let mut index = sample();
    index.remove(key(2));
    let id = index.upsert(dir(2, Some(1), "docs")).unwrap();
    let snap = index.snapshot();
    assert_eq!(snap.lookup(key(2)), Some(id));
    assert_eq!(snap.path(id), "vol/docs");
    assert_eq!(snap.live_count(), 5);
    for k in 3..=6 {
        assert_eq!(snap.lookup(key(k)), None, "child {k} stays deleted");
    }
    index.upsert(file(3, 2, "again.txt")).unwrap();
    assert_eq!(path_of(&index, 3).as_deref(), Some("vol/docs/again.txt"));
    assert_eq!(index.live_count(), 6);
}

#[test]
fn apply_handles_upsert_and_remove_changes() {
    let mut index = sample();
    index
        .apply(Change::Upsert(file(20, 7, "q.png")))
        .expect("upsert");
    assert_eq!(path_of(&index, 20).as_deref(), Some("vol/pics/q.png"));
    index.apply(Change::Remove { key: key(7) }).expect("remove");
    assert_eq!(index.lookup(key(20)), None);
    assert_eq!(index.live_count(), 7);
    assert!(index.apply(Change::Upsert(file(21, 7, "x"))).is_err());
}

type Row = (
    FileKey,
    String,
    String,
    Option<FileKey>,
    EntryKind,
    EntryFlags,
    u64,
    i64,
    i64,
    bool,
);

/// Every live entry described by stable data only (no EntryIds).
fn rows(index: &Index, keys: impl IntoIterator<Item = u64>) -> BTreeSet<Row> {
    let snap = index.snapshot();
    let mut by_id: HashMap<EntryId, FileKey> = HashMap::new();
    let mut found = Vec::new();
    for k in keys {
        if let Some(id) = snap.lookup(key(k)) {
            by_id.insert(id, key(k));
            found.push((key(k), id));
        }
    }
    let live: BTreeSet<EntryId> = snap.live_ids().collect();
    assert_eq!(
        live,
        by_id.keys().copied().collect(),
        "live ids == looked-up ids"
    );
    assert_eq!(snap.live_count(), live.len());
    found
        .into_iter()
        .map(|(k, id)| {
            let p = snap.parent(id);
            let parent = if p == EntryId::NONE {
                None
            } else {
                Some(by_id[&p])
            };
            (
                k,
                snap.path(id),
                snap.name(id).to_string(),
                parent,
                snap.kind(id),
                snap.flags(id),
                snap.size(id),
                snap.modified(id),
                snap.created(id),
                snap.ext(id) == ExtId::NONE,
            )
        })
        .collect()
}

#[test]
fn compaction_preserves_content_and_returns_the_remap() {
    let mut index = sample();
    for k in 400..500 {
        index.upsert(file(k, 1, &format!("base{k}.bin"))).unwrap();
    }
    index.compact();
    // Keep every later change in the delta: it never passes the live count.
    index.set_compact_fraction(1.0);
    for k in 100..150 {
        index.upsert(file(k, 7, &format!("img{k}.jpg"))).unwrap();
    }
    index.upsert(dir(2, Some(7), "docs-moved")).unwrap();
    index.upsert(file(4, 2, "b2.md")).unwrap();
    index.remove(key(5));
    for k in (100..150).step_by(3) {
        index.remove(key(k));
    }
    assert!(
        index.delta_len() > 0,
        "new and renamed entries are in the delta"
    );
    let all = || (1..600u64).collect::<Vec<_>>();
    let before = rows(&index, all());
    let old_ids: HashMap<u64, EntryId> = all()
        .into_iter()
        .filter_map(|k| index.lookup(key(k)).map(|id| (k, id)))
        .collect();

    let remap = index.compact();
    assert_eq!(index.delta_len(), 0, "compaction empties the delta");
    let after = rows(&index, all());
    assert_eq!(before, after, "compaction must not change content");

    for (k, old) in &old_ids {
        assert_eq!(
            remap[old.index()],
            index.lookup(key(*k)).unwrap(),
            "remap of key {k}"
        );
    }
    let live = old_ids.len();
    let dropped = remap.iter().filter(|id| **id == EntryId::NONE).count();
    assert_eq!(
        remap.len() - dropped,
        live,
        "every live entry is remapped exactly once"
    );
    assert!(dropped >= 2 + 17, "deleted entries map to NONE");
    // The new base is dense.
    let ids: BTreeSet<u32> = index.snapshot().live_ids().map(|id| id.0).collect();
    assert_eq!(ids, (0..live as u32).collect());
    // Mutations keep working after compaction.
    index.upsert(file(300, 2, "late.txt")).unwrap();
    assert_eq!(
        path_of(&index, 300).as_deref(),
        Some("vol/pics/docs-moved/late.txt")
    );
    assert_eq!(index.compact().len(), live + 1);
}

#[test]
fn auto_compaction_bounds_the_delta() {
    let mut index = Index::new();
    assert!(
        (index.compact_fraction() - 0.10).abs() < 1e-9,
        "default is 10%"
    );
    index.upsert(dir(1, None, "vol")).unwrap();
    for k in 2..2_000u64 {
        index.upsert(file(k, 1, &format!("f{k}.txt"))).unwrap();
        let limit = (0.10_f64 * index.live_count() as f64).ceil() as usize + 1;
        assert!(
            index.delta_len() <= limit,
            "delta {} at {k}",
            index.delta_len()
        );
    }
    index.compact();
    index.set_compact_fraction(1.0);
    for k in 2_000..2_100u64 {
        index.upsert(file(k, 1, &format!("g{k}"))).unwrap();
    }
    assert_eq!(index.delta_len(), 100, "new entries go to the delta");
    for k in 2..12u64 {
        index.upsert(file(k, 1, &format!("renamed{k}"))).unwrap();
    }
    assert!(index.delta_len() >= 110, "renamed entries go to the delta");
    index.set_compact_fraction(0.05);
    index.upsert(file(5_000, 1, "trigger")).unwrap();
    assert!(index.delta_len() as f64 <= 0.05 * index.live_count() as f64 + 1.0);
    assert_eq!(path_of(&index, 5).as_deref(), Some("vol/renamed5"));
    assert_eq!(index.live_count(), 2_100);
}

#[test]
fn infinite_compact_fraction_never_auto_compacts_even_when_empty() {
    // `set_compact_fraction(f64::INFINITY)` turns automatic compaction off.
    // That must hold when every entry is gone too (live == 0), where
    // INFINITY * 0 would be NaN.
    let mut index = Index::new();
    index.set_compact_fraction(f64::INFINITY);
    index.upsert(dir(1, None, "vol")).unwrap();
    for k in 2..6u64 {
        index.upsert(file(k, 1, &format!("f{k}.txt"))).unwrap();
    }
    assert_eq!(index.live_count(), 5);
    for k in 2..6u64 {
        assert_eq!(index.remove(key(k)), 1, "remove leaf {k}");
    }
    assert_eq!(index.remove(key(1)), 1, "remove the now-empty root");
    assert_eq!(index.live_count(), 0);
    assert!(
        index.deleted_len() > 0,
        "no automatic compaction ran, so removed entries still hold IDs"
    );
    assert_eq!(index.deleted_len(), 5);
    assert!(
        index.take_auto_remap().is_none(),
        "no automatic compaction ran"
    );
    assert_eq!(index.lookup(key(1)), None);

    // Removing a whole subtree in one call down to live == 0 also must not
    // trigger a compaction.
    let mut index = Index::new();
    index.set_compact_fraction(f64::INFINITY);
    index.upsert(dir(1, None, "vol")).unwrap();
    index.upsert(dir(2, Some(1), "docs")).unwrap();
    index.upsert(file(3, 2, "a.txt")).unwrap();
    index.upsert(file(4, 1, "b.txt")).unwrap();
    assert_eq!(index.remove(key(1)), 4);
    assert_eq!(index.live_count(), 0);
    assert_eq!(index.deleted_len(), 4);
    assert!(index.take_auto_remap().is_none());

    // An explicit compaction still works and empties everything.
    index.compact();
    assert_eq!(index.deleted_len(), 0);
    assert_eq!(index.delta_len(), 0);
}

#[test]
fn memory_estimate_is_at_most_100_bytes_per_entry_at_100k() {
    let tree = fx_bench::generate_tree(100_000, 7, fx_bench::TreeShape::default());
    let mut index = Index::new();
    for i in 0..tree.len() {
        let parent = tree.parent[i];
        let kind = if tree.is_dir[i] {
            EntryKind::Dir
        } else {
            EntryKind::File
        };
        let mut e = mk(i as u64 + 1, None, tree.names.get(i), kind);
        e.parent = (parent != u32::MAX).then(|| key(parent as u64 + 1));
        index.upsert(e).unwrap();
    }
    index.compact();
    assert_eq!(index.live_count(), 100_000);
    assert_eq!(path_of(&index, 99_999 + 1), Some(tree.path(99_999)));
    let per_entry = index.heap_bytes_per_entry() as f64;
    assert!(per_entry >= 34.0, "estimate {per_entry} misses the columns");
    assert!(
        per_entry <= 100.0,
        "{per_entry} B/entry exceeds the 100 B budget"
    );
}

/// SplitMix64: a tiny fixed-seed generator so the test needs no crate.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Naive model: key -> entry.
#[derive(Default)]
struct Model(HashMap<FileKey, Entry>);

impl Model {
    fn path(&self, k: FileKey) -> String {
        let e = &self.0[&k];
        match e.parent {
            None => e.name.clone(),
            Some(p) => format!("{}/{}", self.path(p), e.name),
        }
    }
    fn is_ancestor(&self, a: FileKey, mut k: FileKey) -> bool {
        loop {
            if k == a {
                return true;
            }
            match self.0[&k].parent {
                Some(p) => k = p,
                None => return false,
            }
        }
    }
    fn remove(&mut self, k: FileKey) -> usize {
        let doomed: Vec<FileKey> = self
            .0
            .keys()
            .copied()
            .filter(|&c| self.is_ancestor(k, c))
            .collect();
        for c in &doomed {
            self.0.remove(c);
        }
        doomed.len()
    }
    fn dirs(&self) -> Vec<FileKey> {
        let mut v: Vec<FileKey> = self
            .0
            .values()
            .filter(|e| e.kind == EntryKind::Dir)
            .map(|e| e.key)
            .collect();
        v.sort();
        v
    }
    fn keys(&self) -> Vec<FileKey> {
        let mut v: Vec<FileKey> = self.0.keys().copied().collect();
        v.sort();
        v
    }
}

fn check(index: &Index, model: &Model, step: usize) {
    let snap = index.snapshot();
    assert_eq!(
        snap.live_count(),
        model.0.len(),
        "live count at step {step}"
    );
    let mut ids = BTreeSet::new();
    for (k, e) in &model.0 {
        let id = snap
            .lookup(*k)
            .unwrap_or_else(|| panic!("{k:?} missing at step {step}"));
        assert!(ids.insert(id), "duplicate id at step {step}");
        assert_eq!(snap.path(id), model.path(*k), "path at step {step}");
        assert_eq!(snap.name(id), e.name);
        assert_eq!(snap.kind(id), e.kind);
        assert_eq!(snap.flags(id), e.flags);
        assert_eq!(snap.size(id), e.size);
        assert_eq!(snap.modified(id), e.modified);
        assert_eq!(snap.created(id), e.created);
        let parent = e.parent.map_or(EntryId::NONE, |p| snap.lookup(p).unwrap());
        assert_eq!(snap.parent(id), parent, "parent at step {step}");
    }
    assert_eq!(
        snap.live_ids().collect::<BTreeSet<_>>(),
        ids,
        "live ids at step {step}"
    );
}

#[test]
fn random_ops_match_a_naive_model() {
    let mut rng = Rng(0x00F1_1E5E_ED00_0011);
    let mut index = Index::new();
    index.set_compact_fraction(0.05);
    let mut model = Model::default();
    let mut next_key = 2u64;
    let root = dir(1, None, "root");
    index.upsert(root.clone()).unwrap();
    model.0.insert(root.key, root);

    for step in 0..4_000 {
        let roll = rng.below(100);
        if roll < 45 || model.0.len() < 3 {
            // Insert a new file or folder under a random folder.
            let dirs = model.dirs();
            let parent = dirs[rng.below(dirs.len())];
            let kind = if rng.below(3) == 0 {
                EntryKind::Dir
            } else {
                EntryKind::File
            };
            let mut e = mk(
                next_key,
                None,
                &format!("n{next_key}.e{}", rng.below(5)),
                kind,
            );
            e.parent = Some(parent);
            // Stay within the 56 size bits; saturation is tested separately.
            e.size = (rng.next() >> rng.below(64)) & ((1 << 56) - 1);
            e.flags = EntryFlags::from_bits_truncate(rng.below(16) as u8);
            next_key += 1;
            index.upsert(e.clone()).unwrap();
            model.0.insert(e.key, e);
        } else if roll < 60 {
            // Update metadata in place.
            let keys = model.keys();
            let mut e = model.0[&keys[rng.below(keys.len())]].clone();
            e.size = rng.next() >> 20;
            e.modified = rng.next() as i64 >> 8;
            index.upsert(e.clone()).unwrap();
            model.0.insert(e.key, e);
        } else if roll < 72 {
            // Rename.
            let keys = model.keys();
            let mut e = model.0[&keys[rng.below(keys.len())]].clone();
            e.name = format!("r{}", rng.below(1_000_000));
            index.upsert(e.clone()).unwrap();
            model.0.insert(e.key, e);
        } else if roll < 82 {
            // Move (and maybe rename) to a folder that is not a descendant.
            let keys = model.keys();
            let k = keys[rng.below(keys.len())];
            if model.0[&k].parent.is_none() {
                continue;
            }
            let dirs: Vec<FileKey> = model
                .dirs()
                .into_iter()
                .filter(|&d| !model.is_ancestor(k, d))
                .collect();
            let mut e = model.0[&k].clone();
            e.parent = Some(dirs[rng.below(dirs.len())]);
            if rng.below(2) == 0 {
                e.name = format!("m{}", rng.below(1_000_000));
            }
            index.upsert(e.clone()).unwrap();
            model.0.insert(k, e);
        } else if roll < 95 {
            // Remove a non-root entry and its subtree.
            let keys = model.keys();
            let k = keys[rng.below(keys.len())];
            if k == key(1) {
                continue;
            }
            let expected = model.remove(k);
            assert_eq!(index.remove(k), expected, "removed count at step {step}");
        } else if roll < 98 {
            // Insert under a key that does not exist (or no longer exists).
            let e = file(next_key, next_key + 1_000_000, "orphan");
            assert!(index.upsert(e).is_err(), "orphan accepted at step {step}");
        } else {
            index.compact();
            assert_eq!(index.delta_len(), 0);
        }
        if step % 97 == 0 {
            check(&index, &model, step);
        }
    }
    check(&index, &model, usize::MAX);
    index.compact();
    check(&index, &model, usize::MAX);
}
