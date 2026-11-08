//! Batched merges of index changes into a pre-sorted order.
//!
//! Clean-room: written from docs/PLAN.md ("Sorting", "Updates and
//! concurrency") only.
//!
//! Changes are never inserted one entry at a time. A [`Batch`] collects the
//! removed and the new-or-changed IDs; [`Order::apply`] drops every listed ID
//! from the old order in one pass, sorts the listed IDs that are still live
//! with the order's comparator, and merges the two sorted runs.
//!
//! The batch separates structural changes from attribute changes. The Path
//! order depends only on names and parents, so only a folder whose name or
//! parent changed ([`Batch::moved`]) changes the path of every entry below
//! it. Those descendants are not listed in the batch: they are found as the
//! contiguous range right after the folder in the old order (checked against
//! the parent chain in the new state) and re-sorted with the batch. An
//! attribute-only change ([`Batch::upsert`]) re-sorts just the entry itself
//! in every order, so touching a big folder's mtime stays cheap.

use std::cmp::Ordering;

use fx_model::EntryId;
use rayon::prelude::*;

use crate::key::SortKey;
use crate::order::{MatchSet, Order};
use crate::source::{SortSource, parent_of};

/// Below this many IDs the batch is sorted on the calling thread.
const PAR_SORT_MIN: usize = 4096;

/// One batch of changes to merge into an [`Order`].
///
/// The changes must already be applied to the source passed to
/// [`Order::apply`]. Duplicate IDs are harmless, and an ID may appear in both
/// lists: every listed ID is taken out of the order and put back exactly
/// when it is live in the source. So an entry upserted and removed in the
/// same batch counts as removed if it is not live, and as upserted if it is.
///
/// Changes come in three kinds:
/// - [`Batch::remove`]: the entry is gone.
/// - [`Batch::upsert`]: a new entry, or an attribute-only change (size,
///   dates, kind). Only the entry itself is re-sorted, in every order.
/// - [`Batch::moved`]: the entry's name or parent changed (a rename or a
///   move). It is re-sorted in every order like an upsert, and for
///   [`SortKey::Path`] its whole subtree is re-sorted with it.
///
/// A caller that renames or reparents a folder must record it with
/// [`Batch::moved`]; recording it with [`Batch::upsert`] leaves the folder's
/// descendants at their old place in the Path order, which is then wrong.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Batch {
    removed: Vec<EntryId>,
    upserted: Vec<EntryId>,
    moved: Vec<EntryId>,
}

impl Batch {
    /// An empty batch.
    pub fn new() -> Batch {
        Batch::default()
    }

    /// Records a removed entry. Removing an ID that is not in the order is a
    /// no-op.
    pub fn remove(&mut self, id: EntryId) {
        self.removed.push(id);
    }

    /// Records a new entry or an attribute-only change (size, dates, kind).
    ///
    /// Only the entry itself is re-sorted, in every order; its descendants
    /// are left in place. That is correct only when the entry's name and
    /// parent are unchanged (or the entry is new): a renamed or reparented
    /// entry must be recorded with [`Batch::moved`] instead, otherwise the
    /// Path order is wrong. Upserting an ID that is not live is ignored.
    pub fn upsert(&mut self, id: EntryId) {
        self.upserted.push(id);
    }

    /// Records an entry whose name or parent changed (a rename or a move),
    /// possibly together with attribute changes.
    ///
    /// The entry counts as upserted for every order. For [`SortKey::Path`]
    /// its whole subtree moves with it: the descendants need not be listed.
    /// For the other orders only the entry itself is re-sorted. Recording an
    /// ID that is not live is ignored.
    pub fn moved(&mut self, id: EntryId) {
        self.moved.push(id);
    }

    /// The removed IDs, as recorded (possibly with duplicates).
    pub fn removed(&self) -> &[EntryId] {
        &self.removed
    }

    /// The IDs recorded with [`Batch::upsert`] (possibly with duplicates).
    /// IDs recorded with [`Batch::moved`] are listed by
    /// [`Batch::moved_ids`] only.
    pub fn upserted(&self) -> &[EntryId] {
        &self.upserted
    }

    /// The IDs recorded with [`Batch::moved`] (possibly with duplicates).
    pub fn moved_ids(&self) -> &[EntryId] {
        &self.moved
    }

    /// Whether the batch records no change.
    pub fn is_empty(&self) -> bool {
        self.removed.is_empty() && self.upserted.is_empty() && self.moved.is_empty()
    }

    /// Number of recorded IDs, counting duplicates.
    pub fn len(&self) -> usize {
        self.removed.len() + self.upserted.len() + self.moved.len()
    }

    /// Forgets every recorded change, keeping the allocations.
    pub fn clear(&mut self) {
        self.removed.clear();
        self.upserted.clear();
        self.moved.clear();
    }
}

/// Whether some ancestor of `id` (in the new state) is structurally changed
/// or already extracted, so `id`'s path may have changed.
fn has_moved_ancestor<S: SortSource + ?Sized>(
    src: &S,
    id: EntryId,
    structural: &MatchSet,
    extracted: &MatchSet,
) -> bool {
    let cap = src.id_bound();
    let mut cur = id;
    for _ in 0..=cap {
        match parent_of(src, cur) {
            Some(p) => {
                if structural.contains(p) || extracted.contains(p) {
                    return true;
                }
                cur = p;
            }
            None => return false,
        }
    }
    // A corrupt parent cycle: re-sort the entry to stay safe.
    true
}

/// Finds the clean entries of the old Path order whose path changed because
/// a structurally changed ancestor (moved, or removed and possibly
/// re-inserted) is in the batch, marks them in `extracted` and returns them.
///
/// Every such entry sits in the contiguous range right after a structural
/// entry in the old order. Each scan walks forward from a structural entry,
/// skipping dirty entries (re-sorted anyway) and taking clean ones that have
/// a structural or extracted ancestor in `src`; it stops at the first clean
/// entry whose whole parent chain is unchanged, which cannot be inside the
/// range. A structural entry inside an earlier scan's range is not scanned
/// again, so the scans visit each position at most once. Attribute-only
/// upserts never start a scan, so they never touch their subtree.
fn moved_descendants<S: SortSource + ?Sized>(
    src: &S,
    old: &[u32],
    dirty: &MatchSet,
    structural: &MatchSet,
    extracted: &mut MatchSet,
) -> Vec<u32> {
    let mut moved = Vec::new();
    let mut scan_end = 0usize;
    for (i, &u) in old.iter().enumerate() {
        if i < scan_end || !structural.contains(EntryId(u)) {
            continue;
        }
        let mut j = i + 1;
        while let Some(&y) = old.get(j) {
            let id = EntryId(y);
            if dirty.contains(id) || extracted.contains(id) {
                j += 1;
                continue;
            }
            if !has_moved_ancestor(src, id, structural, extracted) {
                break;
            }
            extracted.insert(id);
            moved.push(y);
            j += 1;
        }
        scan_end = j;
    }
    moved
}

/// First index at or after `start` whose ID is not `less`; `kept` must be
/// partitioned by `less` (all `less` IDs first). Gallops from `start`, so a
/// short step costs few comparisons.
fn gallop(kept: &[u32], start: usize, less: impl Fn(u32) -> bool) -> usize {
    let n = kept.len();
    let mut lo = start;
    let mut step = 1usize;
    let mut hi = start;
    while hi < n && less(kept[hi]) {
        lo = hi + 1;
        hi = lo.saturating_add(step);
        step = step.saturating_mul(2);
    }
    let hi = hi.min(n);
    lo + kept[lo..hi].partition_point(|&k| less(k))
}

impl Order {
    /// Merges `batch` into the order, so that it equals
    /// [`Order::build`]`(src, self.key)` afterwards.
    ///
    /// `src` must hold the new state: every change in the batch already
    /// applied. Every ID listed in the batch is dropped from the order in
    /// one pass; those still live in `src` are deduplicated, sorted with the
    /// order's comparator and merged back in one linear (galloping) merge.
    /// An entry upserted and removed in the same batch therefore counts as
    /// removed if it is not live in `src`, and is kept if it is. Moved IDs
    /// count as upserted for every order.
    ///
    /// For [`SortKey::Path`], a folder recorded with [`Batch::moved`]
    /// carries its whole subtree: the descendants are found as the
    /// contiguous range after the folder in the old order, with membership
    /// checked through the parent chain in `src`, and are re-sorted with the
    /// batch. They need not be listed in the batch. A folder recorded only
    /// with [`Batch::upsert`] is re-sorted alone and its subtree is not
    /// scanned, so a caller that renames or reparents a folder must use
    /// [`Batch::moved`], otherwise the Path order is wrong afterwards.
    pub fn apply<S: SortSource + Sync + ?Sized>(&mut self, src: &S, batch: &Batch) {
        if batch.is_empty() {
            return;
        }
        let key = self.key;
        let mut dirty = MatchSet::with_bound(src.id_bound());
        let mut fresh: Vec<u32> = Vec::new();
        for &id in batch
            .removed
            .iter()
            .chain(&batch.upserted)
            .chain(&batch.moved)
        {
            if dirty.insert(id) && src.is_live(id) {
                fresh.push(id.0);
            }
        }

        let mut kept = std::mem::take(&mut self.ids);
        // Removed IDs count as structural too: one that is live again was
        // re-inserted, possibly under another name or parent.
        let structural =
            if key == SortKey::Path && !(batch.moved.is_empty() && batch.removed.is_empty()) {
                let mut set = MatchSet::with_bound(src.id_bound());
                for &id in batch.removed.iter().chain(&batch.moved) {
                    set.insert(id);
                }
                Some(set)
            } else {
                None
            };
        if let Some(structural) = structural {
            let mut extracted = MatchSet::with_bound(src.id_bound());
            let moved = moved_descendants(src, &kept, &dirty, &structural, &mut extracted);
            if !moved.is_empty() {
                fresh.extend(moved.into_iter().filter(|&y| src.is_live(EntryId(y))));
                kept.retain(|&id| {
                    let id = EntryId(id);
                    !dirty.contains(id) && !extracted.contains(id)
                });
            } else {
                kept.retain(|&id| !dirty.contains(EntryId(id)));
            }
        } else {
            kept.retain(|&id| !dirty.contains(EntryId(id)));
        }

        if fresh.is_empty() {
            self.ids = kept;
            return;
        }
        let cmp = |a: &u32, b: &u32| key.compare(src, EntryId(*a), EntryId(*b));
        if fresh.len() >= PAR_SORT_MIN {
            fresh.par_sort_unstable_by(cmp);
        } else {
            fresh.sort_unstable_by(cmp);
        }

        let mut out = Vec::with_capacity(kept.len() + fresh.len());
        let mut start = 0usize;
        for &id in &fresh {
            let pos = gallop(&kept, start, |k| {
                key.compare(src, EntryId(k), EntryId(id)) == Ordering::Less
            });
            out.extend_from_slice(&kept[start..pos]);
            out.push(id);
            start = pos;
        }
        out.extend_from_slice(&kept[start..]);
        self.ids = out;
    }
}
