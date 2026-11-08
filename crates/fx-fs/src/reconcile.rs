//! Shared reconcile-by-path and subtree-diff logic (docs/PLAN.md,
//! "Proposed architecture": the engine checks every reported path on disk).
//!
//! These helpers are pure functions over `fx-model` types, so every platform
//! and every test fake shares one implementation:
//!
//! - [`reconcile_path`] turns the result of a [`PathStat`](crate::PathStat)
//!   call plus the index's current entry at that path into index changes.
//! - [`diff_subtree`] compares the index's entries under a folder with a
//!   fresh enumeration of that folder and returns the minimal change list.
//!
//! Both only emit [`Change::Upsert`] and [`Change::Remove`].

use std::collections::{HashMap, HashSet};

use fx_model::{Change, Entry, EntryKind, FileKey};

/// What to do about one checked path; returned by [`reconcile_path`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PathReconcile {
    /// Changes to apply in order (only `Upsert` and `Remove`). Empty when the
    /// index already matches the disk.
    pub changes: Vec<Change>,
    /// The path now holds a directory whose [`FileKey`] differs from the
    /// indexed entry (or that was not indexed at all), so its contents are
    /// unknown and the subtree under the path must be re-scanned.
    pub rescan_subtree: bool,
}

/// Reconciles one path: `on_disk` is the [`PathStat`](crate::PathStat)
/// result for the path and `indexed` the index's current entry at the same
/// path, if any.
///
/// - Gone from disk and indexed: `Remove` (a directory's descendants go with
///   it, so no rescan is needed).
/// - On disk and not indexed: `Upsert`.
/// - Same key on both sides: `Upsert` if anything differs, otherwise nothing.
/// - Different keys (the path was replaced, e.g. a folder by a file with the
///   same name): `Remove` of the old key, then `Upsert` of the new entry.
///
/// [`PathReconcile::rescan_subtree`] is set whenever the path now holds a
/// directory with a new identity (replaced, or newly appeared), because a
/// change source reports such a directory without its contents. Symlinks
/// are never followed, so they never ask for a rescan.
pub fn reconcile_path(on_disk: Option<Entry>, indexed: Option<&Entry>) -> PathReconcile {
    let mut out = PathReconcile::default();
    match (on_disk, indexed) {
        (None, None) => {}
        (None, Some(old)) => out.changes.push(Change::Remove { key: old.key }),
        (Some(now), None) => {
            out.rescan_subtree = now.kind == EntryKind::Dir;
            out.changes.push(Change::Upsert(now));
        }
        (Some(now), Some(old)) if now.key == old.key => {
            if &now != old {
                out.changes.push(Change::Upsert(now));
            }
        }
        (Some(now), Some(old)) => {
            out.rescan_subtree = now.kind == EntryKind::Dir;
            out.changes.push(Change::Remove { key: old.key });
            out.changes.push(Change::Upsert(now));
        }
    }
    out
}

/// The minimal change list that turns an indexed folder into its fresh
/// enumeration; returned by [`diff_subtree`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SubtreeDiff {
    /// Changes to apply in order (only `Upsert` and `Remove`).
    pub changes: Vec<Change>,
    /// Keys of upserted entries that were already indexed under another name
    /// or parent: renames and reparents, for `Batch::moved`. Each key is
    /// listed once.
    pub moved: Vec<FileKey>,
}

/// Diffs the index's entries under a folder against a fresh enumeration of
/// the same folder.
///
/// `indexed` maps every indexed entry under the folder (including the folder
/// itself) by key. `fresh` is the new enumeration, parent before child.
///
/// The result lists, in this order:
///
/// 1. An `Upsert` for every fresh entry that is new or differs from its
///    indexed version, in `fresh` order, so each upsert's parent is already
///    known. Entries whose name or parent changed are also listed in
///    [`SubtreeDiff::moved`].
/// 2. A `Remove` for every indexed entry missing from `fresh`, sorted by key,
///    except entries whose indexed parent is removed too: a remove takes the
///    entry's descendants with it.
///
/// Upserts come first so that an entry moved out of a removed folder is
/// re-parented before the folder is removed. Applying the changes leaves
/// exactly the fresh enumeration; an unchanged folder gives an empty diff.
pub fn diff_subtree(indexed: &HashMap<FileKey, Entry>, fresh: Vec<Entry>) -> SubtreeDiff {
    let mut out = SubtreeDiff::default();
    let fresh_keys: HashSet<FileKey> = fresh.iter().map(|e| e.key).collect();
    let mut moved_seen: HashSet<FileKey> = HashSet::new();

    for now in fresh {
        match indexed.get(&now.key) {
            Some(old) if *old == now => {}
            Some(old) => {
                if (old.name != now.name || old.parent != now.parent) && moved_seen.insert(now.key)
                {
                    out.moved.push(now.key);
                }
                out.changes.push(Change::Upsert(now));
            }
            None => out.changes.push(Change::Upsert(now)),
        }
    }

    let mut removed: Vec<FileKey> = indexed
        .keys()
        .filter(|k| !fresh_keys.contains(k))
        .copied()
        .collect();
    removed.sort_unstable();
    let removed_set: HashSet<FileKey> = removed.iter().copied().collect();
    out.changes.extend(
        removed
            .into_iter()
            .filter(|k| {
                indexed[k]
                    .parent
                    .is_none_or(|p| p == *k || !removed_set.contains(&p))
            })
            .map(|key| Change::Remove { key }),
    );
    out
}
