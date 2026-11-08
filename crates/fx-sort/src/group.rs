//! Subtree groups: the entries below a few listed folders, read from the
//! Path order, and the hidden entries, read from the Name order.
//!
//! Clean-room: written from docs/PLAN.md ("Sorting") and the owner's
//! "Your files first" requirements only.

use std::cmp::Ordering;

use fx_model::EntryId;

use crate::key::{SortKey, cmp_folded, compare_paths};
use crate::order::{MatchSet, Order};
use crate::source::{SortSource, parent_of};

/// The live entries whose nearest listed entry at or above them (the entry
/// itself included) is listed `true`, leaving out every entry in
/// `left_out` and everything below one, as a [`MatchSet`] for
/// [`emit_tiered`](crate::emit_tiered).
///
/// `path` must be the [`SortKey::Path`] order of `src`: every folder's
/// subtree is then the contiguous range right after it. Each listed entry is
/// found by a binary search of that order and the end of its subtree by a
/// galloping search (each probe walks the probed entry's parents). The group
/// is then painted range by range, one bit per entry; a left-out entry met
/// on the way skips its subtree (its end found the same way, or at once for
/// an entry with no children). There are no per-entry parent or name
/// lookups. Nested ranges are resolved innermost first: an entry listed both
/// ways counts as `false`, and listed IDs that are not live are ignored. A
/// listed entry with a left-out entry at or above it (anywhere up to its
/// root) adds nothing.
///
/// For "Your files first", `left_out` is [`hidden_entries`].
pub fn group_subtrees<S: SortSource + ?Sized>(
    path: &Order,
    src: &S,
    listed: &[(EntryId, bool)],
    left_out: &MatchSet,
) -> MatchSet {
    debug_assert_eq!(
        path.key,
        SortKey::Path,
        "group_subtrees needs the Path order"
    );
    let ids = &path.ids;
    let bound = src.id_bound();
    let mut group = MatchSet::with_bound(bound);
    // The range of positions each listed entry's subtree covers.
    let mut ranges: Vec<(usize, usize, bool)> = listed
        .iter()
        .filter(|(id, _)| !id.is_none() && id.index() < bound && src.is_live(*id))
        .filter_map(|&(id, flag)| {
            let start = ids
                .binary_search_by(|&x| compare_paths(src, EntryId(x), id))
                .ok()?;
            let flag = flag && !left_out_at_or_above(src, id, left_out);
            Some((start, subtree_end(src, ids, start), flag))
        })
        .collect();
    // Subtrees nest or are disjoint. Outer ranges first; for the same range,
    // `true` first, so `false` is the inner one and wins.
    ranges.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)).then(b.2.cmp(&a.2)));

    // Positions before `skip_until` lie in a left-out subtree.
    let mut skip_until = 0;
    let mut paint = |from: usize, to: usize, flag: bool| {
        if !flag {
            return;
        }
        let mut i = from.max(skip_until);
        while i < to {
            let id = EntryId(ids[i]);
            if left_out.contains(id) {
                skip_until = subtree_end(src, ids, i);
                i = skip_until;
            } else {
                group.insert(id);
                i += 1;
            }
        }
    };
    // Sweep: `stack` holds the open ranges (end, flag), innermost last;
    // positions from `pos` on are not painted yet.
    let mut stack: Vec<(usize, bool)> = Vec::with_capacity(ranges.len());
    let mut pos = 0;
    for (start, end, flag) in ranges {
        while let Some(&(top_end, top_flag)) = stack.last() {
            if top_end > start {
                break;
            }
            paint(pos, top_end, top_flag);
            pos = top_end;
            stack.pop();
        }
        if let Some(&(_, top_flag)) = stack.last() {
            paint(pos, start, top_flag);
        }
        pos = start;
        // Clamp to the enclosing range (only a corrupt order could need it).
        let end = stack.last().map_or(end, |&(top_end, _)| end.min(top_end));
        stack.push((end, flag));
    }
    while let Some((top_end, top_flag)) = stack.pop() {
        paint(pos, top_end, top_flag);
        pos = top_end;
    }
    group
}

/// The live entries whose name starts with `.` (hidden on macOS and
/// Linux), read from the [`SortKey::Name`] order `name` of `src`.
///
/// The Name rule sorts by the case-folded name first, so these names are
/// the contiguous range of folded names from `"."` up to (not including)
/// `"/"`, found with two binary searches; the cost is O(log n) name reads
/// plus one bit per hidden entry.
pub fn hidden_entries<S: SortSource + ?Sized>(name: &Order, src: &S) -> MatchSet {
    debug_assert_eq!(
        name.key,
        SortKey::Name,
        "hidden_entries needs the Name order"
    );
    let ids = &name.ids;
    let before = |x: u32, bound: &str| cmp_folded(src.name(EntryId(x)), bound) == Ordering::Less;
    let lo = ids.partition_point(|&x| before(x, "."));
    let hi = lo + ids[lo..].partition_point(|&x| before(x, "/"));
    let mut hidden = MatchSet::with_bound(src.id_bound());
    for &id in &ids[lo..hi] {
        hidden.insert(EntryId(id));
    }
    hidden
}

/// One past the last position of the subtree of the entry at position `p`
/// of the Path order `ids`.
fn subtree_end<S: SortSource + ?Sized>(src: &S, ids: &[u32], p: usize) -> usize {
    let top = EntryId(ids[p]);
    // In the Path order a subtree's first descendant is a child, so an entry
    // whose next neighbour is not its child has no descendants (the common
    // case, which costs one parent read).
    let mut inside = p + 1;
    if inside >= ids.len() || parent_of(src, EntryId(ids[inside])) != Some(top) {
        return p + 1;
    }
    // Gallop: `inside` is below `top`; double the step until a position that
    // is not (or the end), then search between the two.
    let mut step = 1usize;
    let outside = loop {
        let probe = inside.saturating_add(step);
        if probe >= ids.len() {
            break ids.len();
        }
        if !is_below(src, EntryId(ids[probe]), top) {
            break probe;
        }
        inside = probe;
        step = step.saturating_mul(2);
    };
    inside + 1 + ids[inside + 1..outside].partition_point(|&x| is_below(src, EntryId(x), top))
}

/// Whether `id` or one of its ancestors is in `left_out`. The walk is
/// capped at `id_bound` steps, so a corrupt parent cycle cannot loop
/// forever.
fn left_out_at_or_above<S: SortSource + ?Sized>(src: &S, id: EntryId, left_out: &MatchSet) -> bool {
    if left_out.is_empty() {
        return false;
    }
    let mut cur = id;
    for _ in 0..=src.id_bound() {
        if left_out.contains(cur) {
            return true;
        }
        match parent_of(src, cur) {
            Some(p) => cur = p,
            None => return false,
        }
    }
    false
}

/// Whether `ancestor` is a proper ancestor of `id`. The walk is capped at
/// `id_bound` steps, so a corrupt parent cycle cannot loop forever.
fn is_below<S: SortSource + ?Sized>(src: &S, id: EntryId, ancestor: EntryId) -> bool {
    let mut cur = id;
    for _ in 0..=src.id_bound() {
        match parent_of(src, cur) {
            Some(p) if p == ancestor => return true,
            Some(p) => cur = p,
            None => return false,
        }
    }
    false
}
