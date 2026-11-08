//! Contract tests for the shared reconcile-by-path logic in `fx-fs`.
//!
//! The API under test:
//!
//! - `fx_fs::PathStat`: a platform trait with
//!   `fn stat(&self, path: &Path) -> fx_fs::Result<Option<Entry>>`
//!   (lstat semantics, symlinks never followed; `Ok(None)` = path is gone).
//!   It must be object safe.
//! - `fx_fs::reconcile_path(on_disk: Option<Entry>, indexed: Option<&Entry>)
//!   -> fx_fs::PathReconcile`, where `PathReconcile` has public fields
//!   `changes: Vec<Change>` (only `Upsert` / `Remove`) and
//!   `rescan_subtree: bool` (set when the path now holds a directory whose
//!   `FileKey` differs from the indexed one).
//! - `fx_fs::diff_subtree(indexed: &HashMap<FileKey, Entry>, fresh: Vec<Entry>)
//!   -> fx_fs::SubtreeDiff`, where `SubtreeDiff` has public fields
//!   `changes: Vec<Change>` (only `Upsert` / `Remove`) and
//!   `moved: Vec<FileKey>` (upserted entries whose name or parent changed).
//!   `fresh` is a parent-before-child enumeration of the same folder.
//!   Applying `changes` in order to an index (upserts need a known parent,
//!   a `Remove` takes the entry's descendants with it) must leave exactly the
//!   fresh enumeration. A removed entry whose indexed ancestor is also
//!   removed is not listed again (minimal list).
//!
//! Everything here uses in-memory fakes, so it runs on every platform.

use std::collections::{BTreeSet, HashMap};
use std::io;
use std::path::{Path, PathBuf};

use fx_fs::{PathReconcile, PathStat, SubtreeDiff, diff_subtree, reconcile_path};
use fx_model::{Change, Entry, EntryFlags, EntryKind, FileKey, VolumeId};

const VOL: VolumeId = VolumeId(1);

fn key(id: u64) -> FileKey {
    FileKey::new(VOL, id)
}

fn entry(id: u64, parent: Option<u64>, name: &str, kind: EntryKind) -> Entry {
    Entry {
        key: key(id),
        parent: parent.map(key),
        name: name.to_string(),
        kind,
        flags: EntryFlags::empty(),
        size: 0,
        modified: 1_700_000_000,
        created: 1_600_000_000,
    }
}

fn file(id: u64, parent: u64, name: &str) -> Entry {
    entry(id, Some(parent), name, EntryKind::File)
}

fn dir(id: u64, parent: u64, name: &str) -> Entry {
    entry(id, Some(parent), name, EntryKind::Dir)
}

/// The folder being diffed: a volume root with key 1, present in both the
/// indexed map and the fresh enumeration.
fn root() -> Entry {
    entry(1, None, "", EntryKind::Dir)
}

// ---------------------------------------------------------------------------
// PathStat fakes
// ---------------------------------------------------------------------------

/// A file system as a map from absolute path to the lstat result.
struct FakeDisk {
    paths: HashMap<PathBuf, Entry>,
}

impl FakeDisk {
    fn new(items: &[(&str, Entry)]) -> Self {
        FakeDisk {
            paths: items
                .iter()
                .map(|(p, e)| (PathBuf::from(p), e.clone()))
                .collect(),
        }
    }
}

impl PathStat for FakeDisk {
    fn stat(&self, path: &Path) -> fx_fs::Result<Option<Entry>> {
        Ok(self.paths.get(path).cloned())
    }
}

/// A stat source whose every call fails, e.g. permission denied.
struct FailingDisk;

impl PathStat for FailingDisk {
    fn stat(&self, _path: &Path) -> fx_fs::Result<Option<Entry>> {
        Err(io::Error::from(io::ErrorKind::PermissionDenied))
    }
}

/// Stats `path` through a trait object and reconciles it against `indexed`.
fn check(disk: &dyn PathStat, path: &str, indexed: Option<&Entry>) -> PathReconcile {
    let on_disk = disk.stat(Path::new(path)).expect("stat succeeds");
    reconcile_path(on_disk, indexed)
}

// ---------------------------------------------------------------------------
// PathStat
// ---------------------------------------------------------------------------

#[test]
fn path_stat_is_object_safe_and_reports_gone_paths_as_none() {
    let disk = FakeDisk::new(&[("/v/a.txt", file(2, 1, "a.txt"))]);
    let stat: Box<dyn PathStat> = Box::new(disk);
    assert_eq!(
        stat.stat(Path::new("/v/a.txt")).unwrap(),
        Some(file(2, 1, "a.txt"))
    );
    assert_eq!(stat.stat(Path::new("/v/missing")).unwrap(), None);
}

#[test]
fn path_stat_errors_are_io_errors() {
    let stat: &dyn PathStat = &FailingDisk;
    let err = stat.stat(Path::new("/v/secret")).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
}

// ---------------------------------------------------------------------------
// reconcile_path
// ---------------------------------------------------------------------------

#[test]
fn reconcile_created_file_is_upserted() {
    let disk = FakeDisk::new(&[("/v/new.txt", file(2, 1, "new.txt"))]);
    let r = check(&disk, "/v/new.txt", None);
    assert_eq!(r.changes, vec![Change::Upsert(file(2, 1, "new.txt"))]);
    assert!(!r.rescan_subtree);
}

#[test]
fn reconcile_deleted_path_is_removed() {
    let disk = FakeDisk::new(&[]);
    let indexed = file(2, 1, "gone.txt");
    let r = check(&disk, "/v/gone.txt", Some(&indexed));
    assert_eq!(r.changes, vec![Change::Remove { key: key(2) }]);
    assert!(!r.rescan_subtree);
}

#[test]
fn reconcile_deleted_directory_is_removed_without_rescan() {
    let disk = FakeDisk::new(&[]);
    let indexed = dir(2, 1, "gone");
    let r = check(&disk, "/v/gone", Some(&indexed));
    assert_eq!(r.changes, vec![Change::Remove { key: key(2) }]);
    assert!(!r.rescan_subtree);
}

#[test]
fn reconcile_path_absent_everywhere_does_nothing() {
    let disk = FakeDisk::new(&[]);
    let r = check(&disk, "/v/never", None);
    assert!(r.changes.is_empty(), "got {:?}", r.changes);
    assert!(!r.rescan_subtree);
}

#[test]
fn reconcile_unchanged_entry_does_nothing() {
    let disk = FakeDisk::new(&[
        ("/v/a.txt", file(2, 1, "a.txt")),
        ("/v/docs", dir(3, 1, "docs")),
    ]);
    let r = check(&disk, "/v/a.txt", Some(&file(2, 1, "a.txt")));
    assert!(r.changes.is_empty(), "got {:?}", r.changes);
    assert!(!r.rescan_subtree);

    let r = check(&disk, "/v/docs", Some(&dir(3, 1, "docs")));
    assert!(r.changes.is_empty(), "got {:?}", r.changes);
    assert!(!r.rescan_subtree);
}

#[test]
fn reconcile_modified_file_with_same_key_is_upserted() {
    let mut now = file(2, 1, "a.txt");
    now.size = 4096;
    now.modified += 60;
    let disk = FakeDisk::new(&[("/v/a.txt", now.clone())]);
    let r = check(&disk, "/v/a.txt", Some(&file(2, 1, "a.txt")));
    assert_eq!(r.changes, vec![Change::Upsert(now)]);
    assert!(!r.rescan_subtree);
}

#[test]
fn reconcile_directory_with_same_key_and_new_metadata_needs_no_rescan() {
    let mut now = dir(3, 1, "docs");
    now.modified += 5;
    let disk = FakeDisk::new(&[("/v/docs", now.clone())]);
    let r = check(&disk, "/v/docs", Some(&dir(3, 1, "docs")));
    assert_eq!(r.changes, vec![Change::Upsert(now)]);
    assert!(!r.rescan_subtree);
}

#[test]
fn reconcile_folder_replaced_by_file_with_same_name() {
    // `docs` was a directory (key 3); now a regular file (key 9) has its name.
    let disk = FakeDisk::new(&[("/v/docs", file(9, 1, "docs"))]);
    let r = check(&disk, "/v/docs", Some(&dir(3, 1, "docs")));
    assert_eq!(
        r.changes,
        vec![
            Change::Remove { key: key(3) },
            Change::Upsert(file(9, 1, "docs")),
        ]
    );
    assert!(!r.rescan_subtree, "a file has no subtree to rescan");
}

#[test]
fn reconcile_folder_replaced_by_another_folder_rescans_subtree() {
    // Same name, new identity: the old folder's contents are stale and the
    // new folder's contents are unknown.
    let disk = FakeDisk::new(&[("/v/docs", dir(9, 1, "docs"))]);
    let r = check(&disk, "/v/docs", Some(&dir(3, 1, "docs")));
    assert_eq!(
        r.changes,
        vec![
            Change::Remove { key: key(3) },
            Change::Upsert(dir(9, 1, "docs")),
        ]
    );
    assert!(r.rescan_subtree);
}

#[test]
fn reconcile_file_replaced_by_folder_rescans_subtree() {
    let disk = FakeDisk::new(&[("/v/docs", dir(9, 1, "docs"))]);
    let r = check(&disk, "/v/docs", Some(&file(3, 1, "docs")));
    assert_eq!(
        r.changes,
        vec![
            Change::Remove { key: key(3) },
            Change::Upsert(dir(9, 1, "docs")),
        ]
    );
    assert!(r.rescan_subtree);
}

#[test]
fn reconcile_folder_replaced_by_symlink_is_not_followed() {
    let link = entry(9, Some(1), "docs", EntryKind::Symlink);
    let disk = FakeDisk::new(&[("/v/docs", link.clone())]);
    let r = check(&disk, "/v/docs", Some(&dir(3, 1, "docs")));
    assert_eq!(
        r.changes,
        vec![Change::Remove { key: key(3) }, Change::Upsert(link)]
    );
    assert!(!r.rescan_subtree, "symlinks are never followed");
}

// ---------------------------------------------------------------------------
// diff_subtree
// ---------------------------------------------------------------------------

fn index_of(entries: &[Entry]) -> HashMap<FileKey, Entry> {
    entries.iter().map(|e| (e.key, e.clone())).collect()
}

fn upserted_keys(d: &SubtreeDiff) -> BTreeSet<FileKey> {
    d.changes
        .iter()
        .filter_map(|c| match c {
            Change::Upsert(e) => Some(e.key),
            _ => None,
        })
        .collect()
}

fn removed_keys(d: &SubtreeDiff) -> BTreeSet<FileKey> {
    d.changes
        .iter()
        .filter_map(|c| match c {
            Change::Remove { key } => Some(*key),
            _ => None,
        })
        .collect()
}

fn moved_keys(d: &SubtreeDiff) -> BTreeSet<FileKey> {
    d.moved.iter().copied().collect()
}

fn keys(ids: &[u64]) -> BTreeSet<FileKey> {
    ids.iter().copied().map(key).collect()
}

/// Applies `changes` in order to a fake index with the real index's rules:
/// an upsert needs its parent present (or no parent), a remove takes the
/// entry's descendants along.
fn apply(start: &HashMap<FileKey, Entry>, changes: &[Change]) -> HashMap<FileKey, Entry> {
    let mut idx = start.clone();
    for c in changes {
        match c {
            Change::Upsert(e) => {
                if let Some(p) = e.parent {
                    assert!(
                        idx.contains_key(&p),
                        "upsert of {:?} before its parent {:?}",
                        e.key,
                        p
                    );
                }
                idx.insert(e.key, e.clone());
            }
            Change::Remove { key } => {
                let mut doomed = vec![*key];
                let mut i = 0;
                while i < doomed.len() {
                    let k = doomed[i];
                    doomed.extend(
                        idx.values()
                            .filter(|e| e.parent == Some(k) && e.key != k)
                            .map(|e| e.key),
                    );
                    i += 1;
                }
                for k in doomed {
                    idx.remove(&k);
                }
            }
            other => panic!("diff_subtree emitted {other:?}; only Upsert/Remove allowed"),
        }
    }
    idx
}

/// Runs the diff and checks that applying it reproduces the fresh state and
/// that no change touches an entry that did not change.
fn diff_and_apply(before: &[Entry], after: &[Entry]) -> SubtreeDiff {
    let indexed = index_of(before);
    let d = diff_subtree(&indexed, after.to_vec());
    let result = apply(&indexed, &d.changes);
    assert_eq!(
        result,
        index_of(after),
        "applying the diff must give the fresh state"
    );
    for c in &d.changes {
        if let Change::Upsert(e) = c {
            assert_ne!(
                indexed.get(&e.key),
                Some(e),
                "redundant upsert of an unchanged entry"
            );
        }
    }
    let moved: BTreeSet<FileKey> = moved_keys(&d);
    assert!(
        moved.is_subset(&upserted_keys(&d)),
        "every moved key is also upserted"
    );
    assert_eq!(moved.len(), d.moved.len(), "moved keys are listed once");
    d
}

fn base_tree() -> Vec<Entry> {
    // /            (1)
    // ├── a.txt    (2)
    // ├── docs/    (3)
    // │   ├── b.txt (4)
    // │   └── sub/  (5)
    // │       └── c.txt (6)
    // └── pics/    (7)
    vec![
        root(),
        file(2, 1, "a.txt"),
        dir(3, 1, "docs"),
        file(4, 3, "b.txt"),
        dir(5, 3, "sub"),
        file(6, 5, "c.txt"),
        dir(7, 1, "pics"),
    ]
}

#[test]
fn diff_unchanged_subtree_is_empty() {
    let d = diff_and_apply(&base_tree(), &base_tree());
    assert!(d.changes.is_empty(), "got {:?}", d.changes);
    assert!(d.moved.is_empty(), "got {:?}", d.moved);
}

#[test]
fn diff_unchanged_subtree_is_empty_regardless_of_enumeration_order() {
    let mut fresh = base_tree();
    // Still parent before child, but siblings in another order.
    fresh.swap(1, 6);
    let d = diff_subtree(&index_of(&base_tree()), fresh);
    assert!(d.changes.is_empty(), "got {:?}", d.changes);
    assert!(d.moved.is_empty(), "got {:?}", d.moved);
}

#[test]
fn diff_create_file_and_folder_with_contents() {
    let mut after = base_tree();
    after.push(file(10, 7, "new.png"));
    after.push(dir(11, 1, "music"));
    after.push(dir(12, 11, "album"));
    after.push(file(13, 12, "track.flac"));
    let d = diff_and_apply(&base_tree(), &after);
    assert_eq!(upserted_keys(&d), keys(&[10, 11, 12, 13]));
    assert!(removed_keys(&d).is_empty());
    assert!(
        d.moved.is_empty(),
        "new entries are not moves: {:?}",
        d.moved
    );
}

#[test]
fn diff_delete_file() {
    let after: Vec<Entry> = base_tree()
        .into_iter()
        .filter(|e| e.key != key(2))
        .collect();
    let d = diff_and_apply(&base_tree(), &after);
    assert_eq!(d.changes, vec![Change::Remove { key: key(2) }]);
    assert!(d.moved.is_empty());
}

#[test]
fn diff_delete_folder_removes_only_its_top() {
    // docs/ and everything under it is gone.
    let after: Vec<Entry> = base_tree()
        .into_iter()
        .filter(|e| ![3, 4, 5, 6].contains(&e.key.file_id))
        .collect();
    let d = diff_and_apply(&base_tree(), &after);
    assert_eq!(
        d.changes,
        vec![Change::Remove { key: key(3) }],
        "descendants of a removed folder go with it"
    );
    assert!(d.moved.is_empty());
}

#[test]
fn diff_rename_within_folder_is_a_move() {
    let after: Vec<Entry> = base_tree()
        .into_iter()
        .map(|e| {
            if e.key == key(4) {
                file(4, 3, "renamed.txt")
            } else {
                e
            }
        })
        .collect();
    let d = diff_and_apply(&base_tree(), &after);
    assert_eq!(d.changes, vec![Change::Upsert(file(4, 3, "renamed.txt"))]);
    assert_eq!(d.moved, vec![key(4)]);
}

#[test]
fn diff_reparent_file_is_a_move() {
    // b.txt moves from docs/ to pics/, keeping its name.
    let after: Vec<Entry> = base_tree()
        .into_iter()
        .map(|e| {
            if e.key == key(4) {
                file(4, 7, "b.txt")
            } else {
                e
            }
        })
        .collect();
    let d = diff_and_apply(&base_tree(), &after);
    assert_eq!(d.changes, vec![Change::Upsert(file(4, 7, "b.txt"))]);
    assert_eq!(d.moved, vec![key(4)]);
}

#[test]
fn diff_reparent_folder_moves_only_the_folder() {
    // sub/ (with c.txt inside) moves from docs/ to pics/ under a new name.
    let after: Vec<Entry> = base_tree()
        .into_iter()
        .map(|e| {
            if e.key == key(5) {
                dir(5, 7, "sub2")
            } else {
                e
            }
        })
        .collect();
    let d = diff_and_apply(&base_tree(), &after);
    assert_eq!(d.changes, vec![Change::Upsert(dir(5, 7, "sub2"))]);
    assert_eq!(d.moved, vec![key(5)], "children follow their folder");
}

#[test]
fn diff_metadata_change_is_upsert_but_not_move() {
    let mut grown = file(2, 1, "a.txt");
    grown.size = 1234;
    grown.modified += 10;
    let after: Vec<Entry> = base_tree()
        .into_iter()
        .map(|e| if e.key == key(2) { grown.clone() } else { e })
        .collect();
    let d = diff_and_apply(&base_tree(), &after);
    assert_eq!(d.changes, vec![Change::Upsert(grown)]);
    assert!(d.moved.is_empty(), "same name and parent: not a move");
}

#[test]
fn diff_folder_replaced_by_file_with_same_name() {
    // docs/ (3, with contents) is deleted and a file named "docs" (20) appears.
    let mut after: Vec<Entry> = base_tree()
        .into_iter()
        .filter(|e| ![3, 4, 5, 6].contains(&e.key.file_id))
        .collect();
    after.push(file(20, 1, "docs"));
    let d = diff_and_apply(&base_tree(), &after);
    assert_eq!(upserted_keys(&d), keys(&[20]));
    assert_eq!(removed_keys(&d), keys(&[3]));
    assert!(d.moved.is_empty());
}

#[test]
fn diff_folder_moved_out_of_deleted_folder_keeps_its_children() {
    // sub/ (5, holding c.txt) is moved to the root, then docs/ is deleted.
    // The move must be applied so that removing docs/ does not take sub/
    // or c.txt with it.
    let after = vec![
        root(),
        file(2, 1, "a.txt"),
        dir(5, 1, "sub"),
        file(6, 5, "c.txt"),
        dir(7, 1, "pics"),
    ];
    let d = diff_and_apply(&base_tree(), &after);
    assert_eq!(upserted_keys(&d), keys(&[5]));
    assert_eq!(removed_keys(&d), keys(&[3]), "b.txt goes with docs/");
    assert_eq!(d.moved, vec![key(5)]);
}

#[test]
fn diff_mixed_changes_in_one_pass() {
    // Create, delete, rename and reparent at once; b.txt (4) is deleted.
    let after = vec![
        root(),
        file(2, 1, "a-renamed.txt"), // rename
        dir(3, 1, "docs"),
        dir(5, 3, "sub"),
        dir(7, 1, "pics"),
        file(6, 7, "c.txt"),      // reparent
        file(30, 5, "fresh.txt"), // create
    ];
    let d = diff_and_apply(&base_tree(), &after);
    assert_eq!(upserted_keys(&d), keys(&[2, 6, 30]));
    assert_eq!(removed_keys(&d), keys(&[4]));
    assert_eq!(moved_keys(&d), keys(&[2, 6]));
}
