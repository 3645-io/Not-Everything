//! The writer's navigation on the index (run 2.17): children by parent,
//! children by name, subtrees and what an upsert changed, checked against a
//! naive model through random inserts, renames, moves, attribute changes,
//! removals and compactions.

use std::collections::{HashMap, HashSet};

use fx_index::{Index, Upserted};
use fx_model::{Entry, EntryFlags, EntryId, EntryKind, FileKey, VolumeId};

/// A small deterministic generator.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn key(n: u64) -> FileKey {
    FileKey::new(VolumeId(1), n)
}

fn entry(n: u64, parent: Option<u64>, name: &str, dir: bool) -> Entry {
    Entry {
        key: key(n),
        parent: parent.map(key),
        name: name.to_string(),
        kind: if dir { EntryKind::Dir } else { EntryKind::File },
        flags: EntryFlags::empty(),
        size: if dir { 0 } else { n },
        modified: 100,
        created: 50,
    }
}

/// The model: every live entry by key.
#[derive(Default)]
struct Model {
    entries: HashMap<FileKey, Entry>,
}

impl Model {
    fn children(&self, parent: FileKey) -> HashSet<FileKey> {
        self.entries
            .values()
            .filter(|e| e.parent == Some(parent))
            .map(|e| e.key)
            .collect()
    }
    fn subtree(&self, top: FileKey) -> HashSet<FileKey> {
        let mut out = HashSet::from([top]);
        let mut grew = true;
        while grew {
            grew = false;
            for e in self.entries.values() {
                if let Some(p) = e.parent {
                    if out.contains(&p) && out.insert(e.key) {
                        grew = true;
                    }
                }
            }
        }
        out
    }
    fn folders(&self) -> Vec<FileKey> {
        let mut v: Vec<FileKey> = self
            .entries
            .values()
            .filter(|e| e.kind == EntryKind::Dir)
            .map(|e| e.key)
            .collect();
        v.sort_by_key(|k| k.file_id);
        v
    }
}

fn keys_of(index: &Index, ids: &[EntryId]) -> HashSet<FileKey> {
    ids.iter().map(|&id| index.key(id)).collect()
}

/// Checks every navigation answer against the model.
fn check(index: &Index, model: &Model, context: &str) {
    assert_eq!(
        index.live_count(),
        model.entries.len(),
        "{context}: live count"
    );
    for folder in model.folders() {
        let id = index.lookup(folder).expect("live folder");
        let kids = index.children(id);
        assert_eq!(
            kids.len(),
            keys_of(index, &kids).len(),
            "{context}: no duplicates"
        );
        assert_eq!(
            keys_of(index, &kids),
            model.children(folder),
            "{context}: children of {folder:?}"
        );
        for kid in model.children(folder) {
            let name = &model.entries[&kid].name;
            let found = index.child(id, name).map(|c| index.key(c));
            assert_eq!(found, Some(kid), "{context}: child {name:?} of {folder:?}");
        }
        assert!(
            index.child(id, "no-such-name").is_none(),
            "{context}: missing name"
        );
        let sub = index.subtree(id);
        assert_eq!(
            keys_of(index, &sub),
            model.subtree(folder),
            "{context}: subtree of {folder:?}"
        );
        let mut seen = HashSet::new();
        for (i, &s) in sub.iter().enumerate() {
            let e = index.get(index.key(s)).expect("live entry");
            if i > 0 {
                let parent = e.parent.expect("a descendant has a parent");
                assert!(seen.contains(&parent), "{context}: parent before child");
            }
            seen.insert(e.key);
        }
    }
    let roots: HashSet<FileKey> = index.roots().iter().map(|&r| index.key(r)).collect();
    let model_roots: HashSet<FileKey> = model
        .entries
        .values()
        .filter(|e| e.parent.is_none())
        .map(|e| e.key)
        .collect();
    assert_eq!(roots, model_roots, "{context}: roots");
    for e in model.entries.values() {
        assert_eq!(
            index.get(e.key).as_ref(),
            Some(e),
            "{context}: row of {:?}",
            e.key
        );
    }
}

fn random_run(seed: u64, auto_compact: bool) {
    let mut rng = Rng(seed);
    let mut index = Index::new();
    if !auto_compact {
        index.set_compact_fraction(f64::INFINITY);
    }
    let mut model = Model::default();
    let root = entry(1, None, "root", true);
    assert_eq!(
        index.upsert_reporting(root.clone()).unwrap().1,
        Upserted::Inserted
    );
    model.entries.insert(root.key, root);
    let mut next = 2u64;
    for step in 0..400 {
        let folders = model.folders();
        let op = rng.below(10);
        let context = format!("seed {seed} step {step} op {op}");
        match op {
            0..=3 => {
                // Insert, sometimes a folder; names repeat across folders.
                let parent = folders[rng.below(folders.len())];
                let dir = rng.below(3) == 0;
                let name = format!("n{}", rng.below(40));
                if model
                    .children(parent)
                    .iter()
                    .any(|k| model.entries[k].name == name)
                {
                    continue;
                }
                let e = entry(next, Some(parent.file_id), &name, dir);
                next += 1;
                assert_eq!(
                    index.upsert_reporting(e.clone()).unwrap().1,
                    Upserted::Inserted,
                    "{context}"
                );
                model.entries.insert(e.key, e);
            }
            4 => {
                // Rename within the folder.
                let all: Vec<FileKey> = model
                    .entries
                    .keys()
                    .copied()
                    .filter(|k| *k != key(1))
                    .collect();
                if all.is_empty() {
                    continue;
                }
                let k = all[rng.below(all.len())];
                let mut e = model.entries[&k].clone();
                let name = format!("r{}", rng.below(40));
                let parent = e.parent.unwrap();
                if model
                    .children(parent)
                    .iter()
                    .any(|c| model.entries[c].name == name)
                {
                    continue;
                }
                e.name = name;
                assert_eq!(
                    index.upsert_reporting(e.clone()).unwrap().1,
                    Upserted::Moved,
                    "{context}"
                );
                model.entries.insert(k, e);
            }
            5 => {
                // Move into another folder outside its own subtree.
                let all: Vec<FileKey> = model
                    .entries
                    .keys()
                    .copied()
                    .filter(|k| *k != key(1))
                    .collect();
                if all.is_empty() {
                    continue;
                }
                let k = all[rng.below(all.len())];
                let to = folders[rng.below(folders.len())];
                let mut e = model.entries[&k].clone();
                if model.subtree(k).contains(&to)
                    || e.parent == Some(to)
                    || model
                        .children(to)
                        .iter()
                        .any(|c| model.entries[c].name == e.name)
                {
                    continue;
                }
                e.parent = Some(to);
                assert_eq!(
                    index.upsert_reporting(e.clone()).unwrap().1,
                    Upserted::Moved,
                    "{context}"
                );
                model.entries.insert(k, e);
            }
            6 => {
                // Attribute change, and an unchanged upsert.
                let all: Vec<FileKey> = model.entries.keys().copied().collect();
                let k = all[rng.below(all.len())];
                let mut e = model.entries[&k].clone();
                assert_eq!(
                    index.upsert_reporting(e.clone()).unwrap().1,
                    Upserted::Unchanged,
                    "{context}"
                );
                e.modified += 1;
                assert_eq!(
                    index.upsert_reporting(e.clone()).unwrap().1,
                    Upserted::Updated,
                    "{context}"
                );
                model.entries.insert(k, e);
            }
            7 | 8 => {
                // Remove an entry and its subtree.
                let all: Vec<FileKey> = model
                    .entries
                    .keys()
                    .copied()
                    .filter(|k| *k != key(1))
                    .collect();
                if all.is_empty() {
                    continue;
                }
                let k = all[rng.below(all.len())];
                let doomed = model.subtree(k);
                assert_eq!(index.remove(k), doomed.len(), "{context}: removed count");
                for d in doomed {
                    model.entries.remove(&d);
                }
            }
            _ => {
                let _ = index.compact();
            }
        }
        if step % 10 == 0 || op == 9 {
            check(&index, &model, &context);
        }
    }
    check(&index, &model, &format!("seed {seed} end"));
}

#[test]
fn navigation_matches_a_naive_model() {
    for seed in 0..8 {
        random_run(0xC41D_0000 + seed, false);
    }
}

#[test]
fn navigation_matches_with_automatic_compaction() {
    for seed in 0..4 {
        random_run(0xA070_0000 + seed, true);
    }
}

#[test]
fn navigation_survives_a_restore_from_parts() {
    let mut index = Index::new();
    index.set_compact_fraction(f64::INFINITY);
    index.upsert(entry(1, None, "root", true)).unwrap();
    for n in 2..200u64 {
        let parent = if n < 20 { 1 } else { 2 + n % 18 };
        index
            .upsert(entry(n, Some(parent), &format!("e{n}"), n < 20))
            .unwrap();
    }
    index.upsert(entry(5, Some(7), "moved", true)).unwrap();
    let restored = Index::from_parts(index.snapshot().to_parts()).unwrap();
    for folder in 1..20u64 {
        let a = index.lookup(key(folder)).unwrap();
        let b = restored.lookup(key(folder)).unwrap();
        assert_eq!(
            keys_of(&index, &index.children(a)),
            keys_of(&restored, &restored.children(b))
        );
    }
    let seven = restored.lookup(key(7)).unwrap();
    assert_eq!(
        restored.child(seven, "moved").map(|c| restored.key(c)),
        Some(key(5))
    );
}

#[test]
fn a_large_folder_finds_every_child_by_name() {
    let mut index = Index::new();
    index.set_compact_fraction(f64::INFINITY);
    index.upsert(entry(1, None, "root", true)).unwrap();
    for n in 2..3000u64 {
        index
            .upsert(entry(n, Some(1), &format!("file-{}", 5000 - n), false))
            .unwrap();
    }
    let _ = index.compact();
    for n in 3000..3100u64 {
        index
            .upsert(entry(n, Some(1), &format!("new-{n}"), false))
            .unwrap();
    }
    let root = index.lookup(key(1)).unwrap();
    assert_eq!(index.children(root).len(), 3098);
    for n in (2..3100u64).step_by(7) {
        let name = if n < 3000 {
            format!("file-{}", 5000 - n)
        } else {
            format!("new-{n}")
        };
        assert_eq!(
            index.child(root, &name).map(|c| index.key(c)),
            Some(key(n)),
            "{name}"
        );
    }
}

#[test]
fn a_replaced_entry_waiting_for_removal_shares_its_name() {
    // A file replaced in place: the new key is inserted before the old one
    // is removed. Both are listed; the newer comes first.
    let mut index = Index::new();
    index.set_compact_fraction(f64::INFINITY);
    index.upsert(entry(1, None, "root", true)).unwrap();
    index.upsert(entry(2, Some(1), "a.txt", false)).unwrap();
    let _ = index.compact();
    index.upsert(entry(3, Some(1), "a.txt", false)).unwrap();
    let root = index.lookup(key(1)).unwrap();
    let named: Vec<FileKey> = index
        .children_named(root, "a.txt")
        .iter()
        .map(|&i| index.key(i))
        .collect();
    assert_eq!(named, vec![key(3), key(2)]);
    assert_eq!(
        index.child(root, "a.txt").map(|c| index.key(c)),
        Some(key(3))
    );
    index.remove(key(2));
    assert_eq!(
        index.child(root, "a.txt").map(|c| index.key(c)),
        Some(key(3))
    );
}

#[test]
fn removing_a_folder_removes_exactly_its_subtree() {
    let mut index = Index::new();
    index.set_compact_fraction(f64::INFINITY);
    index.upsert(entry(1, None, "root", true)).unwrap();
    index.upsert(entry(2, Some(1), "a", true)).unwrap();
    index.upsert(entry(3, Some(2), "b", true)).unwrap();
    index.upsert(entry(4, Some(3), "c.txt", false)).unwrap();
    index.upsert(entry(5, Some(1), "d.txt", false)).unwrap();
    assert_eq!(index.remove(key(2)), 3);
    assert_eq!(index.live_count(), 2);
    assert!(index.lookup(key(4)).is_none());
    assert!(index.lookup(key(5)).is_some());
}
