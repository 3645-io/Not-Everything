//! In-crate checks of the column store: a seeded random op sequence against
//! a naive model, compaction equivalence, snapshot isolation and the memory
//! budget. The integration tests in `tests/` cover the same contract in
//! more detail.

use std::collections::{BTreeSet, HashMap};

use fx_model::{Entry, EntryFlags, EntryId, EntryKind, FileKey, VolumeId};

use crate::{Index, IndexError, Snapshot};

fn key(n: u64) -> FileKey {
    FileKey::new(VolumeId(9), n)
}

fn mk(k: u64, parent: Option<u64>, name: &str, kind: EntryKind) -> Entry {
    Entry {
        key: key(k),
        parent: parent.map(key),
        name: name.to_string(),
        kind,
        flags: EntryFlags::empty(),
        size: k * 3,
        modified: k as i64,
        created: -(k as i64),
    }
}

/// Everything a snapshot says about the live entries, without EntryIds.
fn rows(snap: &Snapshot) -> BTreeSet<(FileKey, String, String, u64, i64, i64)> {
    let out: BTreeSet<_> = snap
        .live_ids()
        .map(|id| {
            (
                snap.key(id),
                snap.path(id),
                format!(
                    "{:?}/{:?}/{:?}",
                    snap.kind(id),
                    snap.flags(id),
                    snap.ext(id)
                ),
                snap.size(id),
                snap.modified(id),
                snap.created(id),
            )
        })
        .collect();
    assert_eq!(out.len(), snap.live_count());
    for id in snap.live_ids() {
        assert_eq!(snap.lookup(snap.key(id)), Some(id));
    }
    out
}

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

fn model_path(m: &HashMap<FileKey, Entry>, k: FileKey) -> String {
    let e = &m[&k];
    match e.parent {
        None => e.name.clone(),
        Some(p) => format!("{}/{}", model_path(m, p), e.name),
    }
}

fn is_ancestor(m: &HashMap<FileKey, Entry>, a: FileKey, mut k: FileKey) -> bool {
    loop {
        if k == a {
            return true;
        }
        match m[&k].parent {
            Some(p) => k = p,
            None => return false,
        }
    }
}

fn check(index: &Index, m: &HashMap<FileKey, Entry>) {
    let snap = index.snapshot();
    assert_eq!(snap.live_count(), m.len());
    for (k, e) in m {
        let id = snap.lookup(*k).expect("model key is indexed");
        assert_eq!(snap.path(id), model_path(m, *k));
        assert_eq!(snap.kind(id), e.kind);
        assert_eq!(snap.flags(id), e.flags);
        assert_eq!(snap.size(id), e.size);
        assert_eq!(snap.modified(id), e.modified);
        let parent = e.parent.map_or(EntryId::NONE, |p| snap.lookup(p).unwrap());
        assert_eq!(snap.parent(id), parent);
    }
    assert_eq!(snap.live_ids().count(), m.len());
}

#[test]
fn random_ops_match_a_naive_model_with_snapshots() {
    let mut rng = Rng(42);
    for fraction in [0.0, 0.05, 0.3, f64::INFINITY] {
        let mut index = Index::new();
        index.set_compact_fraction(fraction);
        let mut m: HashMap<FileKey, Entry> = HashMap::new();
        let root = mk(1, None, "root", EntryKind::Dir);
        index.upsert(root.clone()).unwrap();
        m.insert(root.key, root);
        let mut next = 2u64;
        let mut frozen: Vec<(std::sync::Arc<Snapshot>, BTreeSet<_>)> = Vec::new();
        for step in 0..1_500 {
            let mut keys: Vec<FileKey> = m.keys().copied().collect();
            keys.sort();
            let mut dirs: Vec<FileKey> = m
                .values()
                .filter(|e| e.kind == EntryKind::Dir)
                .map(|e| e.key)
                .collect();
            dirs.sort();
            let roll = rng.below(100);
            if roll < 45 || m.len() < 3 {
                let kind = if rng.below(3) == 0 {
                    EntryKind::Dir
                } else {
                    EntryKind::File
                };
                let p = dirs[rng.below(dirs.len())];
                let mut e = mk(next, None, &format!("n{next}.e{}", rng.below(4)), kind);
                e.parent = Some(p);
                e.flags = EntryFlags::from_bits_truncate(rng.below(16) as u8);
                next += 1;
                index.upsert(e.clone()).unwrap();
                m.insert(e.key, e);
            } else if roll < 60 {
                let mut e = m[&keys[rng.below(keys.len())]].clone();
                e.size = rng.next() >> 9;
                index.upsert(e.clone()).unwrap();
                m.insert(e.key, e);
            } else if roll < 80 {
                let k = keys[rng.below(keys.len())];
                if m[&k].parent.is_none() {
                    continue;
                }
                let mut e = m[&k].clone();
                let target = dirs[rng.below(dirs.len())];
                e.parent = Some(target);
                e.name = format!("m{}", rng.below(1000));
                if is_ancestor(&m, k, target) {
                    let err = index.upsert(e).unwrap_err();
                    assert!(matches!(err, IndexError::Cycle { .. }));
                } else {
                    index.upsert(e.clone()).unwrap();
                    m.insert(k, e);
                }
            } else if roll < 95 {
                let k = keys[rng.below(keys.len())];
                if k == key(1) {
                    continue;
                }
                let doomed: Vec<FileKey> = m
                    .keys()
                    .copied()
                    .filter(|&c| is_ancestor(&m, k, c))
                    .collect();
                for d in &doomed {
                    m.remove(d);
                }
                assert_eq!(index.remove(k), doomed.len());
            } else {
                let before = rows(&index.snapshot());
                index.compact();
                assert_eq!(index.delta_len(), 0);
                assert_eq!(rows(&index.snapshot()), before);
            }
            if step % 50 == 0 {
                check(&index, &m);
                let s = index.snapshot();
                let r = rows(&s);
                frozen.push((s, r));
            }
        }
        check(&index, &m);
        for (s, r) in &frozen {
            assert_eq!(&rows(s), r, "a snapshot changed");
        }
        let before = rows(&index.snapshot());
        let remap = index.compact();
        assert_eq!(remap.iter().filter(|id| !id.is_none()).count(), m.len());
        assert_eq!(rows(&index.snapshot()), before);
        check(&index, &m);
    }
}

fn dir(k: u64, parent: Option<u64>, name: &str) -> Entry {
    mk(k, parent, name, EntryKind::Dir)
}

fn file(k: u64, parent: u64, name: &str) -> Entry {
    mk(k, Some(parent), name, EntryKind::File)
}

#[test]
fn compaction_remap_and_delta_accounting() {
    let mut index = Index::new();
    index.upsert(dir(1, None, "vol")).unwrap();
    index.upsert(dir(2, Some(1), "docs")).unwrap();
    index.upsert(dir(5, Some(2), "sub")).unwrap();
    index.upsert(file(6, 5, "c.rs")).unwrap();
    index.upsert(dir(7, Some(1), "pics")).unwrap();
    for k in 400..500 {
        index.upsert(file(k, 1, &format!("b{k}.bin"))).unwrap();
    }
    index.compact();
    index.set_compact_fraction(1.0);
    for k in 100..150 {
        index.upsert(file(k, 7, &format!("i{k}.jpg"))).unwrap();
    }
    index.upsert(dir(2, Some(7), "docs-moved")).unwrap();
    assert_eq!(index.delta_len(), 51);
    assert_eq!(index.remove(key(5)), 2);
    for k in (100..150).step_by(3) {
        assert_eq!(index.remove(key(k)), 1);
    }
    assert_eq!(index.deleted_len(), 19);
    let old: Vec<(u64, EntryId)> = (1..600)
        .filter_map(|k| index.lookup(key(k)).map(|id| (k, id)))
        .collect();
    let before = rows(&index.snapshot());
    let remap = index.compact();
    assert_eq!(rows(&index.snapshot()), before);
    assert_eq!(index.delta_len(), 0);
    for (k, id) in &old {
        assert_eq!(Some(remap[id.index()]), index.lookup(key(*k)));
    }
    assert_eq!(remap.iter().filter(|id| id.is_none()).count(), 19);
    let ids: Vec<u32> = index.snapshot().live_ids().map(|id| id.0).collect();
    assert_eq!(ids, (0..old.len() as u32).collect::<Vec<_>>());
    index.upsert(file(300, 2, "late.txt")).unwrap();
    let snap = index.snapshot();
    let id = snap.lookup(key(300)).unwrap();
    assert_eq!(snap.path(id), "vol/pics/docs-moved/late.txt");
    assert_eq!(index.compact().len(), old.len() + 1);
}

#[test]
fn auto_compaction_bounds_delta_and_reports_remap() {
    let mut index = Index::new();
    index.upsert(dir(1, None, "vol")).unwrap();
    for k in 2..2_000u64 {
        index.upsert(file(k, 1, &format!("f{k}.txt"))).unwrap();
        assert!(index.delta_len() as f64 <= 0.1 * index.live_count() as f64);
    }
    index.compact();
    index.take_auto_remap();
    index.set_compact_fraction(1.0);
    for k in 2_000..2_100u64 {
        index.upsert(file(k, 1, &format!("g{k}"))).unwrap();
    }
    assert_eq!(index.delta_len(), 100);
    for k in 2..12u64 {
        index.upsert(file(k, 1, &format!("renamed{k}"))).unwrap();
    }
    assert_eq!(index.delta_len(), 110);
    let before = index.lookup(key(40)).unwrap();
    assert_eq!(index.remove(key(3)), 1);
    index.set_compact_fraction(0.05);
    let id = index.upsert(file(5_000, 1, "trigger")).unwrap();
    assert_eq!(index.delta_len(), 0, "auto-compacted");
    assert_eq!(index.lookup(key(5_000)), Some(id));
    let remap = index.take_auto_remap().expect("auto compaction ran");
    assert_eq!(Some(remap[before.index()]), index.lookup(key(40)));
    assert!(index.take_auto_remap().is_none());
    assert_eq!(index.live_count(), 2_099);
}

#[test]
fn snapshot_survives_mutations_and_compaction() {
    let mut index = Index::new();
    index.upsert(dir(1, None, "vol")).unwrap();
    index.upsert(dir(2, Some(1), "docs")).unwrap();
    index.upsert(file(3, 2, "a.txt")).unwrap();
    index.upsert(dir(7, Some(1), "pics")).unwrap();
    let s1 = index.snapshot();
    let r1 = rows(&s1);
    index.upsert(dir(2, Some(7), "papers")).unwrap();
    index.upsert(file(20, 2, "new.md")).unwrap();
    index.remove(key(3));
    index.compact();
    index.remove(key(1));
    assert_eq!(index.live_count(), 0);
    assert_eq!(rows(&s1), r1);
    assert_eq!(s1.path(s1.lookup(key(3)).unwrap()), "vol/docs/a.txt");
    assert_eq!(index.snapshot().live_ids().count(), 0);
}

#[test]
fn memory_budget_at_100k() {
    let tree = fx_bench::generate_tree(100_000, 7, fx_bench::TreeShape::default());
    let mut index = Index::new();
    for i in 0..tree.len() {
        let kind = if tree.is_dir[i] {
            EntryKind::Dir
        } else {
            EntryKind::File
        };
        let mut e = mk(i as u64 + 1, None, tree.names.get(i), kind);
        let p = tree.parent[i];
        e.parent = (p != u32::MAX).then(|| key(u64::from(p) + 1));
        index.upsert(e).unwrap();
    }
    index.compact();
    assert_eq!(index.live_count(), 100_000);
    let snap = index.snapshot();
    let id = snap.lookup(key(100_000)).unwrap();
    assert_eq!(snap.path(id), tree.path(99_999));
    let per = index.heap_bytes_per_entry();
    assert!((34..=100).contains(&per), "{per} B/entry");
}
