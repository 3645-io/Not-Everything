//! Pre-sorted orders: building, validating and filtered emit.
//!
//! Clean-room: written from docs/PLAN.md ("Sorting") only.

use std::fmt;

use fx_model::EntryId;
use rayon::prelude::*;

use crate::key::{SortKey, compare_names};
use crate::source::{SortSource, parent_of};

/// A permutation of every live entry ID in one key's total order.
///
/// Costs 4 bytes per live entry. Descending output is the exact reverse of
/// `ids`, so one order serves both directions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Order {
    /// The key the IDs are sorted by.
    pub key: SortKey,
    /// Every live entry ID exactly once, ascending by `key`.
    pub ids: Vec<u32>,
}

/// Why [`Order::validate`] rejected an order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderError {
    /// The order holds a different number of IDs than there are live entries.
    WrongLength {
        /// Live entries in the source.
        expected: usize,
        /// IDs in the order.
        found: usize,
    },
    /// The ID at `position` is out of range or removed.
    NotLive {
        /// Position in the order.
        position: usize,
        /// The offending ID.
        id: u32,
    },
    /// The ID at `position` already appeared earlier.
    Duplicate {
        /// Position in the order.
        position: usize,
        /// The repeated ID.
        id: u32,
    },
    /// The IDs at `position` and `position + 1` are not in key order.
    OutOfOrder {
        /// Position of the first of the two IDs.
        position: usize,
    },
}

impl fmt::Display for OrderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OrderError::WrongLength { expected, found } => {
                write!(f, "order has {found} ids but {expected} entries are live")
            }
            OrderError::NotLive { position, id } => {
                write!(f, "id {id} at position {position} is not live")
            }
            OrderError::Duplicate { position, id } => {
                write!(f, "id {id} at position {position} appears twice")
            }
            OrderError::OutOfOrder { position } => {
                write!(
                    f,
                    "ids at positions {position} and {} are out of order",
                    position + 1
                )
            }
        }
    }
}

impl std::error::Error for OrderError {}

/// Every live ID in ascending ID order.
fn live_ids<S: SortSource + Sync + ?Sized>(src: &S) -> Vec<u32> {
    let bound = u32::try_from(src.id_bound()).unwrap_or(u32::MAX);
    (0..bound)
        .into_par_iter()
        .filter(|&i| src.is_live(EntryId(i)))
        .collect()
}

/// Live IDs sorted by the Name rule.
fn name_order<S: SortSource + Sync + ?Sized>(src: &S) -> Vec<u32> {
    let mut ids = live_ids(src);
    ids.par_sort_unstable_by(|&a, &b| compare_names(src, EntryId(a), EntryId(b)));
    ids
}

/// Position of every live ID in the name order; `u32::MAX` for others.
fn name_ranks(bound: usize, by_name: &[u32]) -> Vec<u32> {
    let mut rank = vec![u32::MAX; bound];
    for (pos, &id) in by_name.iter().enumerate() {
        rank[id as usize] = pos as u32;
    }
    rank
}

/// Sorts the name order by a numeric key, keeping name order within ties.
fn numeric_order<S, K, F>(src: &S, by_name: &[u32], key: F) -> Vec<u32>
where
    S: SortSource + Sync + ?Sized,
    K: Ord + Send,
    F: Fn(&S, EntryId) -> K + Sync,
{
    let rank = name_ranks(src.id_bound(), by_name);
    let mut ids = by_name.to_vec();
    ids.par_sort_unstable_by_key(|&id| (key(src, EntryId(id)), rank[id as usize]));
    ids
}

/// Path order as a depth-first walk: roots and each folder's children in
/// name order. Falls back to a comparator sort if some live entry is not
/// reachable from a root through live folders.
fn path_order<S: SortSource + Sync + ?Sized>(src: &S, by_name: &[u32]) -> Vec<u32> {
    let bound = src.id_bound();
    // Children grouped by parent (counting sort), each group in name order.
    let mut start = vec![0u32; bound + 1];
    let mut roots = Vec::new();
    let parents: Vec<Option<EntryId>> = by_name
        .par_iter()
        .map(|&id| parent_of(src, EntryId(id)))
        .collect();
    for (&id, parent) in by_name.iter().zip(&parents) {
        match parent {
            Some(p) if p.index() < bound => start[p.index() + 1] += 1,
            Some(_) => {}
            None => roots.push(id),
        }
    }
    for i in 0..bound {
        start[i + 1] += start[i];
    }
    let mut cursor = start.clone();
    let mut children = vec![0u32; start[bound] as usize];
    for (&id, parent) in by_name.iter().zip(&parents) {
        match parent {
            Some(p) if p.index() < bound => {
                let slot = &mut cursor[p.index()];
                children[*slot as usize] = id;
                *slot += 1;
            }
            _ => {}
        }
    }
    drop(cursor);

    let mut out = Vec::with_capacity(by_name.len());
    let mut stack: Vec<u32> = roots.iter().rev().copied().collect();
    while let Some(id) = stack.pop() {
        out.push(id);
        if out.len() > by_name.len() {
            break;
        }
        let (lo, hi) = (start[id as usize] as usize, start[id as usize + 1] as usize);
        stack.extend(children[lo..hi].iter().rev().copied());
    }
    if out.len() != by_name.len() {
        // Orphans or a parent cycle: sort by the comparator directly.
        out = by_name.to_vec();
        out.par_sort_unstable_by(|&a, &b| SortKey::Path.compare(src, EntryId(a), EntryId(b)));
    }
    out
}

impl Order {
    /// Builds the order for `key` from every live entry in `src`.
    ///
    /// The heavy sorts run in parallel on the rayon pool. Every key first
    /// sorts by the Name rule; Path then walks the tree in that order and
    /// the numeric keys sort by `(value, name rank)`.
    pub fn build<S: SortSource + Sync + ?Sized>(src: &S, key: SortKey) -> Order {
        let by_name = name_order(src);
        Self::from_name_order(src, key, by_name)
    }

    /// Builds the orders for every key in [`SortKey::ALL`], sharing one Name
    /// sort between them.
    pub fn build_all<S: SortSource + Sync + ?Sized>(src: &S) -> Vec<Order> {
        let by_name = name_order(src);
        SortKey::ALL
            .iter()
            .map(|&key| Self::from_name_order(src, key, by_name.clone()))
            .collect()
    }

    /// Builds the orders for `keys`, in that order, sharing one Name sort
    /// between them: the engine keeps four orders, and a first scan or a
    /// large batch rebuilds them all at once.
    pub fn build_keys<S: SortSource + Sync + ?Sized>(src: &S, keys: &[SortKey]) -> Vec<Order> {
        let by_name = name_order(src);
        keys.iter()
            .map(|&key| Self::from_name_order(src, key, by_name.clone()))
            .collect()
    }

    fn from_name_order<S: SortSource + Sync + ?Sized>(
        src: &S,
        key: SortKey,
        by_name: Vec<u32>,
    ) -> Order {
        let ids = match key {
            SortKey::Name => by_name,
            SortKey::Path => path_order(src, &by_name),
            SortKey::Size => numeric_order(src, &by_name, |s, id| s.size(id)),
            SortKey::Modified => numeric_order(src, &by_name, |s, id| s.modified(id)),
            SortKey::Created => numeric_order(src, &by_name, |s, id| s.created(id)),
        };
        Order { key, ids }
    }

    /// Number of IDs in the order.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Whether the order holds no IDs.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Checks that the order is a permutation of `src`'s live IDs, strictly
    /// ascending by its key.
    pub fn validate<S: SortSource + ?Sized>(&self, src: &S) -> Result<(), OrderError> {
        let bound = src.id_bound();
        let mut seen = vec![false; bound];
        for (position, &id) in self.ids.iter().enumerate() {
            let i = id as usize;
            if i >= bound || !src.is_live(EntryId(id)) {
                return Err(OrderError::NotLive { position, id });
            }
            if seen[i] {
                return Err(OrderError::Duplicate { position, id });
            }
            seen[i] = true;
        }
        let expected = (0..bound)
            .filter(|&i| src.is_live(EntryId(i as u32)))
            .count();
        if expected != self.ids.len() {
            return Err(OrderError::WrongLength {
                expected,
                found: self.ids.len(),
            });
        }
        for (position, pair) in self.ids.windows(2).enumerate() {
            let ord = self.key.compare(src, EntryId(pair[0]), EntryId(pair[1]));
            if ord != std::cmp::Ordering::Less {
                return Err(OrderError::OutOfOrder { position });
            }
        }
        Ok(())
    }
}

/// A set of entry IDs, stored as a bitset over `EntryId`.
///
/// Used to filter an order down to a query's matches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MatchSet {
    words: Vec<u64>,
    count: usize,
}

impl MatchSet {
    /// An empty set.
    pub fn new() -> MatchSet {
        MatchSet::default()
    }

    /// An empty set with room for IDs below `id_bound` without growing.
    pub fn with_bound(id_bound: usize) -> MatchSet {
        MatchSet {
            words: vec![0; id_bound.div_ceil(64)],
            count: 0,
        }
    }

    /// Adds `id`; returns whether it was new. [`EntryId::NONE`] is ignored.
    pub fn insert(&mut self, id: EntryId) -> bool {
        if id.is_none() {
            return false;
        }
        let (w, bit) = (id.index() / 64, 1u64 << (id.index() % 64));
        if w >= self.words.len() {
            self.words.resize(w + 1, 0);
        }
        let new = self.words[w] & bit == 0;
        self.words[w] |= bit;
        self.count += usize::from(new);
        new
    }

    /// Whether `id` is in the set.
    pub fn contains(&self, id: EntryId) -> bool {
        let w = id.index() / 64;
        self.words
            .get(w)
            .is_some_and(|word| word & (1u64 << (id.index() % 64)) != 0)
    }

    /// Number of IDs in the set.
    pub fn len(&self) -> usize {
        self.count
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Heap bytes held by the bitset (its allocated capacity).
    pub fn heap_bytes(&self) -> usize {
        self.words.capacity() * std::mem::size_of::<u64>()
    }
}

impl FromIterator<EntryId> for MatchSet {
    fn from_iter<I: IntoIterator<Item = EntryId>>(iter: I) -> Self {
        let mut set = MatchSet::new();
        set.extend(iter);
        set
    }
}

impl Extend<EntryId> for MatchSet {
    fn extend<I: IntoIterator<Item = EntryId>>(&mut self, iter: I) {
        for id in iter {
            self.insert(id);
        }
    }
}

/// Which way to read an order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Direction {
    /// The order as stored.
    #[default]
    Ascending,
    /// The exact reverse of the stored order.
    Descending,
}

/// One window of sorted results.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Page {
    /// How many entries of the order matched in total.
    pub total: usize,
    /// The matching IDs in the requested window, in display order.
    pub ids: Vec<u32>,
}

/// Emits the matches of `order` in one pass: the total match count plus the
/// IDs at match positions `offset .. offset + limit` (in `direction`).
///
/// Offsets past the end give an empty window; huge limits are fine.
pub fn emit(
    order: &Order,
    matches: &MatchSet,
    direction: Direction,
    offset: usize,
    limit: usize,
) -> Page {
    if matches.is_empty() {
        return Page::default();
    }
    let end = offset.saturating_add(limit);
    let mut total = 0usize;
    let mut ids = Vec::with_capacity(limit.min(order.ids.len()).min(4096));
    let mut visit = |id: u32| {
        if matches.contains(EntryId(id)) {
            if total >= offset && total < end {
                ids.push(id);
            }
            total += 1;
        }
    };
    match direction {
        Direction::Ascending => order.ids.iter().for_each(|&id| visit(id)),
        Direction::Descending => order.ids.iter().rev().for_each(|&id| visit(id)),
    }
    Page { total, ids }
}

/// Calls `f` on every ID of `order` in `direction` until it returns `false`.
fn walk(order: &Order, direction: Direction, mut f: impl FnMut(u32) -> bool) {
    match direction {
        Direction::Ascending => {
            for &id in &order.ids {
                if !f(id) {
                    break;
                }
            }
        }
        Direction::Descending => {
            for &id in order.ids.iter().rev() {
                if !f(id) {
                    break;
                }
            }
        }
    }
}

/// Like [`emit`], but lists the matches in `first` before the others: the
/// matches that are also in `first`, in `order` read in `direction`, then
/// the remaining matches in the same order. `total` counts every match, as
/// [`emit`] does, and the window is taken from that grouped list.
///
/// One pass counts both groups and collects the window's part of the first
/// group; a second pass, only when the window reaches past the first group,
/// collects the rest and stops once the window is full. An empty `first`
/// is plain [`emit`].
pub fn emit_tiered(
    order: &Order,
    matches: &MatchSet,
    first: &MatchSet,
    direction: Direction,
    offset: usize,
    limit: usize,
) -> Page {
    if first.is_empty() {
        return emit(order, matches, direction, offset, limit);
    }
    if matches.is_empty() {
        return Page::default();
    }
    let end = offset.saturating_add(limit);
    let mut ids = Vec::with_capacity(limit.min(order.ids.len()).min(4096));
    // Pass 1: both counts, and the window's part of the first group.
    let (mut in_first, mut rest) = (0usize, 0usize);
    walk(order, direction, |id| {
        let id = EntryId(id);
        if matches.contains(id) {
            if first.contains(id) {
                if in_first >= offset && in_first < end {
                    ids.push(id.0);
                }
                in_first += 1;
            } else {
                rest += 1;
            }
        }
        true
    });
    // Pass 2: the window's part of the other matches, at positions
    // `in_first + k`.
    if end > in_first && rest > 0 {
        let (from, to) = (offset.saturating_sub(in_first), end - in_first);
        if from < rest {
            let mut k = 0usize;
            walk(order, direction, |id| {
                let id = EntryId(id);
                if matches.contains(id) && !first.contains(id) {
                    if k >= from {
                        ids.push(id.0);
                    }
                    k += 1;
                }
                k < to
            });
        }
    }
    Page {
        total: in_first + rest,
        ids,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fx_model::EntryKind;

    /// Minimal table: `(name, parent, live)` per ID.
    struct Table(Vec<(&'static str, u32, bool)>);

    impl SortSource for Table {
        fn id_bound(&self) -> usize {
            self.0.len()
        }
        fn is_live(&self, id: EntryId) -> bool {
            self.0.get(id.index()).is_some_and(|e| e.2)
        }
        fn name(&self, id: EntryId) -> &str {
            self.0[id.index()].0
        }
        fn parent(&self, id: EntryId) -> EntryId {
            EntryId(self.0[id.index()].1)
        }
        fn size(&self, _: EntryId) -> u64 {
            0
        }
        fn modified(&self, _: EntryId) -> i64 {
            0
        }
        fn created(&self, _: EntryId) -> i64 {
            0
        }
        fn kind(&self, _: EntryId) -> EntryKind {
            EntryKind::File
        }
    }

    const NONE: u32 = u32::MAX;

    #[test]
    fn path_order_falls_back_for_orphans_and_self_parents() {
        // 1 is live under a removed folder 0; 3 names itself as its parent.
        let src = Table(vec![
            ("dead", NONE, false),
            ("orphan", 0, true),
            ("b", NONE, true),
            ("a", 3, true),
            ("c", 2, true),
        ]);
        let order = Order::build(&src, SortKey::Path);
        assert_eq!(order.ids, vec![3, 2, 4, 1]);
        assert!(order.validate(&src).is_ok());
    }

    #[test]
    fn folded_compare_matches_folded_strings() {
        let words = [
            "", "a", "A", "ab", "aB", "é", "É", "ß", "SS", "\u{212A}", "k", "İ", "i",
        ];
        let fold = |s: &str| s.chars().flat_map(char::to_lowercase).collect::<String>();
        for a in words {
            for b in words {
                assert_eq!(
                    crate::cmp_folded(a, b),
                    fold(a).cmp(&fold(b)),
                    "{a:?} {b:?}"
                );
            }
        }
    }
}
