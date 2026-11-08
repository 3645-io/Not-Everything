//! Sort keys and their comparators.
//!
//! Clean-room: written from docs/PLAN.md ("Sorting") only.
//!
//! Every comparator here is a total order over entry IDs: two different IDs
//! never compare equal, so sorted output is deterministic.

use std::cmp::Ordering;

use fx_model::EntryId;

use crate::source::{SortSource, parent_of};

/// The column an [`Order`](crate::Order) sorts by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SortKey {
    /// The Name rule: case-folded name, then raw name bytes, then EntryId.
    Name,
    /// Full path, compared component-wise from the root, each component by
    /// the Name rule, a shorter prefix first. A folder's subtree is the
    /// contiguous range right after it.
    Path,
    /// Size in bytes, then the Name rule.
    Size,
    /// Last modification time, then the Name rule.
    Modified,
    /// Creation time, then the Name rule.
    Created,
}

impl SortKey {
    /// Every key, in declaration order.
    pub const ALL: [SortKey; 5] = [
        SortKey::Name,
        SortKey::Path,
        SortKey::Size,
        SortKey::Modified,
        SortKey::Created,
    ];

    /// Compares two entries by this key's total order.
    pub fn compare<S: SortSource + ?Sized>(self, src: &S, a: EntryId, b: EntryId) -> Ordering {
        match self {
            SortKey::Name => compare_names(src, a, b),
            SortKey::Path => compare_paths(src, a, b),
            SortKey::Size => src
                .size(a)
                .cmp(&src.size(b))
                .then_with(|| compare_names(src, a, b)),
            SortKey::Modified => src
                .modified(a)
                .cmp(&src.modified(b))
                .then_with(|| compare_names(src, a, b)),
            SortKey::Created => src
                .created(a)
                .cmp(&src.created(b))
                .then_with(|| compare_names(src, a, b)),
        }
    }
}

/// Compares two strings by their case-folded forms: each `char` mapped
/// through [`char::to_lowercase`], the results compared code point by code
/// point (the same as comparing the folded strings).
///
/// Runs without allocating; pure-ASCII stretches are compared byte by byte.
pub fn cmp_folded(a: &str, b: &str) -> Ordering {
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    let n = ab.len().min(bb.len());
    let mut i = 0;
    while i < n {
        let (x, y) = (ab[i], bb[i]);
        if !x.is_ascii() || !y.is_ascii() {
            break;
        }
        if x != y {
            let (lx, ly) = (x.to_ascii_lowercase(), y.to_ascii_lowercase());
            if lx != ly {
                return lx.cmp(&ly);
            }
        }
        i += 1;
    }
    // Every byte before `i` was ASCII in both, so `i` is a char boundary in
    // both strings.
    let (ra, rb) = (&a[i..], &b[i..]);
    ra.chars()
        .flat_map(char::to_lowercase)
        .cmp(rb.chars().flat_map(char::to_lowercase))
}

/// The Name rule for two names alone: case-folded, then raw bytes.
pub fn cmp_name_strs(a: &str, b: &str) -> Ordering {
    cmp_folded(a, b).then_with(|| a.as_bytes().cmp(b.as_bytes()))
}

/// The Name rule: case-folded name, then raw name bytes, then EntryId.
pub fn compare_names<S: SortSource + ?Sized>(src: &S, a: EntryId, b: EntryId) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    cmp_name_strs(src.name(a), src.name(b)).then(a.0.cmp(&b.0))
}

/// Number of ancestors of `id`, capped at `id_bound` so a corrupt parent
/// cycle cannot loop forever.
pub(crate) fn depth<S: SortSource + ?Sized>(src: &S, id: EntryId) -> usize {
    let cap = src.id_bound();
    let mut d = 0;
    let mut cur = id;
    while let Some(p) = parent_of(src, cur) {
        d += 1;
        if d > cap {
            break;
        }
        cur = p;
    }
    d
}

fn lift<S: SortSource + ?Sized>(src: &S, mut id: EntryId, steps: usize) -> EntryId {
    for _ in 0..steps {
        match parent_of(src, id) {
            Some(p) => id = p,
            None => break,
        }
    }
    id
}

/// The Path rule: component-wise from the root, each component by the Name
/// rule (including its EntryId), a shorter prefix (an ancestor) first.
///
/// Runs without allocating: both entries are lifted to the same depth, then
/// walked up together until they share a parent.
pub fn compare_paths<S: SortSource + ?Sized>(src: &S, a: EntryId, b: EntryId) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let (da, db) = (depth(src, a), depth(src, b));
    let x = lift(src, a, da.saturating_sub(db));
    let y = lift(src, b, db.saturating_sub(da));
    if x == y {
        // One is an ancestor of the other: the shorter path comes first.
        return da.cmp(&db);
    }
    let (mut x, mut y) = (x, y);
    for _ in 0..=src.id_bound() {
        let (px, py) = (parent_of(src, x), parent_of(src, y));
        if px == py {
            return compare_names(src, x, y);
        }
        match (px, py) {
            (Some(px), Some(py)) => {
                x = px;
                y = py;
            }
            _ => break,
        }
    }
    // Only reachable with a corrupt parent chain; stay total and deterministic.
    compare_names(src, x, y).then(a.0.cmp(&b.0))
}
