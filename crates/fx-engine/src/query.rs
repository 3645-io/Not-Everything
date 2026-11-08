//! The query API: [`fx_query::SearchView`] over a published snapshot, one
//! window of sorted results, and the positions of known entries in them.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use fx_model::{EntryFlags, EntryId, EntryKind};
use fx_query::SearchView;
use fx_query::eval::EvalContext;
use fx_sort::{Direction, MatchSet, Order, SortKey, cmp_name_strs};

use crate::snapshot::{EngineSnapshot, Generation};

/// One window of search results, from one published snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Page {
    /// The generation of the snapshot searched; the IDs are entry IDs of
    /// that snapshot's index.
    pub generation: Generation,
    /// How many entries matched in total.
    pub total: usize,
    /// The matching entry IDs at positions `offset .. offset + limit`, in
    /// the requested sort order and direction.
    pub ids: Vec<u32>,
}

impl EngineSnapshot {
    /// Whether `id` is in range and names a live entry.
    pub(crate) fn live(&self, id: u32) -> bool {
        (id as usize) < self.index.id_bound() && self.index.is_live(EntryId(id))
    }

    /// The merged order for `sort`, or one built on the spot for a key the
    /// engine does not keep.
    pub(crate) fn order_or_build(&self, sort: SortKey) -> Cow<'_, Order> {
        match self.order(sort) {
            Some(order) => Cow::Borrowed(order),
            None => Cow::Owned(Order::build(self.index(), sort)),
        }
    }

    /// The search hits (entry IDs of this snapshot) as a match set for
    /// [`fx_sort::emit`].
    pub(crate) fn match_set(&self, hits: &[u32]) -> MatchSet {
        let mut matches = MatchSet::with_bound(self.index.id_bound());
        for &id in hits {
            matches.insert(EntryId(id));
        }
        matches
    }

    /// The live entry whose path is `path`, in the form
    /// [`fx_index::Snapshot::path`] gives (names from the root entry down,
    /// joined with `/`; a root named `/` makes it start with `//`). Names
    /// must match exactly, byte for byte.
    ///
    /// A binary search of the Path order: about `log2(entries)` path
    /// comparisons, each walking the entry's parents, and no allocation per
    /// entry. If two siblings had the same name (which no file system
    /// allows), only one of them could be found.
    pub fn find_path(&self, path: &str) -> Option<u32> {
        let want = path_components(path);
        let order = self.order_or_build(SortKey::Path);
        let mut chain = Vec::new();
        let at = order
            .ids
            .partition_point(|&id| self.cmp_path(EntryId(id), &want, &mut chain) == Ordering::Less);
        let &id = order.ids.get(at)?;
        (self.cmp_path(EntryId(id), &want, &mut chain) == Ordering::Equal).then_some(id)
    }

    /// Compares entry `id`'s path with the components `want` by the Path
    /// rule (component-wise from the root, each by the Name rule, an
    /// ancestor first), ignoring the entry-ID tie-break. `chain` is scratch
    /// space for the entry's ancestors.
    fn cmp_path(&self, id: EntryId, want: &[&str], chain: &mut Vec<EntryId>) -> Ordering {
        chain.clear();
        let mut cur = id;
        // Capped like fx-sort's depth, so a corrupt parent cycle ends.
        let cap = self.index.id_bound();
        loop {
            chain.push(cur);
            let parent = self.index.parent(cur);
            if parent.is_none() || parent == cur || chain.len() > cap {
                break;
            }
            cur = parent;
        }
        for (k, &entry) in chain.iter().rev().enumerate() {
            let Some(name) = want.get(k) else {
                // `want` is an ancestor of the entry: it comes first.
                return Ordering::Greater;
            };
            match cmp_name_strs(self.index.name(entry), name) {
                Ordering::Equal => {}
                other => return other,
            }
        }
        chain.len().cmp(&want.len())
    }
}

/// The names in an index path, root first. Names never contain `/`, except
/// a root named `/` (a volume root), whose index paths are `/` and
/// `//child/...`.
fn path_components(path: &str) -> Vec<&str> {
    match path.strip_prefix('/') {
        Some(rest) => {
            let mut parts = vec!["/"];
            let rest = rest.strip_prefix('/').unwrap_or(rest);
            if !rest.is_empty() {
                parts.extend(rest.split('/'));
            }
            parts
        }
        None => path.split('/').collect(),
    }
}

/// Where some entries sit in one search's results, from
/// [`Engine::positions`](crate::Engine::positions).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Positions {
    /// The generation of the snapshot searched; the rows are positions in
    /// its results.
    pub generation: Generation,
    /// For each path asked about, in the same order: the offset at which
    /// [`Engine::search`](crate::Engine::search) returns that entry (in the
    /// same query, sort and direction), or `None` when no live entry has the
    /// path or it does not match the query.
    pub rows: Vec<Option<usize>>,
}

/// For each of `targets` (entry IDs that are in `matches`, or `None`), its
/// position among the matches of `order` read in `direction`, or, with
/// `first` (the priority group of "Your files first"), in the grouped list
/// [`fx_sort::emit_tiered`] reads: the matches in `first`, then the others.
///
/// One pass over the order: a bit test per entry, plus a second one for the
/// matches (and a third with `first`). It stops as soon as every target is
/// found, unless one of them is outside `first`: its position comes after
/// the whole group, so the pass then counts the group to the end.
pub(crate) fn positions_in(
    order: &Order,
    matches: &MatchSet,
    first: Option<&MatchSet>,
    direction: Direction,
    targets: &[Option<u32>],
) -> Vec<Option<usize>> {
    let first = first.filter(|f| !f.is_empty());
    let mut wanted = MatchSet::new();
    for &id in targets.iter().flatten() {
        wanted.insert(EntryId(id));
    }
    // Each target found: whether it is in the group, and its position in
    // its part of the list.
    let mut found: HashMap<u32, (bool, usize)> = HashMap::with_capacity(wanted.len());
    let (mut in_first, mut rest) = (0usize, 0usize);
    let mut rest_found = false;
    if !wanted.is_empty() {
        let mut visit = |id: u32| {
            let entry = EntryId(id);
            if matches.contains(entry) {
                let grouped = first.is_some_and(|f| f.contains(entry));
                let position = if grouped { &mut in_first } else { &mut rest };
                if wanted.contains(entry) {
                    found.insert(id, (grouped, *position));
                    rest_found |= first.is_some() && !grouped;
                }
                *position += 1;
            }
            found.len() < wanted.len() || rest_found
        };
        match direction {
            Direction::Ascending => {
                for &id in &order.ids {
                    if !visit(id) {
                        break;
                    }
                }
            }
            Direction::Descending => {
                for &id in order.ids.iter().rev() {
                    if !visit(id) {
                        break;
                    }
                }
            }
        }
    }
    targets
        .iter()
        .map(|t| {
            let &(grouped, position) = found.get(&(*t)?)?;
            Some(if grouped || first.is_none() {
                position
            } else {
                in_first + position
            })
        })
        .collect()
}

/// IDs are the raw `u32` entry IDs of the snapshot's index; deleted slots
/// are not live.
impl SearchView for EngineSnapshot {
    fn len(&self) -> usize {
        self.index.id_bound()
    }

    fn name(&self, id: u32) -> &str {
        self.index.name(EntryId(id))
    }

    fn parent(&self, id: u32) -> Option<u32> {
        let p = self.index.parent(EntryId(id));
        if p.is_none() || p.0 == id {
            None
        } else {
            Some(p.0)
        }
    }

    fn is_folder(&self, id: u32) -> bool {
        self.index.kind(EntryId(id)) == EntryKind::Dir
    }

    fn is_live(&self, id: u32) -> bool {
        self.live(id)
    }

    fn size(&self, id: u32) -> Option<u64> {
        // Folder sizes are not tracked.
        (!self.is_folder(id)).then(|| self.index.size(EntryId(id)))
    }

    fn modified(&self, id: u32) -> Option<i64> {
        Some(self.index.modified(EntryId(id)))
    }

    fn created(&self, id: u32) -> Option<i64> {
        Some(self.index.created(EntryId(id)))
    }

    fn hidden(&self, id: u32) -> bool {
        self.index.flags(EntryId(id)).contains(EntryFlags::HIDDEN)
    }

    fn symlink(&self, id: u32) -> bool {
        self.index.kind(EntryId(id)) == EntryKind::Symlink
    }

    fn read_only(&self, id: u32) -> bool {
        self.index.flags(EntryId(id)).contains(EntryFlags::LOCKED)
    }
}

/// The clock and time zone for date functions: now, with `fixed` as the
/// UTC offset or else the operating system's local offset.
pub(crate) fn eval_context(fixed: Option<i32>) -> EvalContext {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    EvalContext {
        now,
        utc_offset_secs: fixed.unwrap_or_else(|| local_utc_offset(now)),
    }
}

/// The local time zone's offset east of UTC at Unix time `now`, in seconds,
/// from `localtime_r` (POSIX; `tm_gmtoff` is a BSD/glibc field present on
/// macOS and Linux).
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn local_utc_offset(now: i64) -> i32 {
    // `time_t` can be 32 bits on Linux (`.ok()`: on 64-bit targets the
    // conversion cannot fail, and a plain `let Ok` would not be refutable).
    let Some(time) = libc::time_t::try_from(now).ok() else {
        return 0;
    };
    // SAFETY: `libc::tm` is plain old data; all-zero is a valid value (its
    // `tm_zone` pointer is null).
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are valid for the call; `localtime_r` is the
    // thread-safe variant and writes only into `tm`.
    let ok = unsafe { !libc::localtime_r(&raw const time, &raw mut tm).is_null() };
    if !ok {
        return 0;
    }
    // Real offsets are within ±26 hours.
    i32::try_from(tm.tm_gmtoff)
        .ok()
        .filter(|o| o.abs() <= 26 * 3600)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_offset_wins_and_the_clock_is_now() {
        let ctx = eval_context(Some(-5 * 3600));
        assert_eq!(ctx.utc_offset_secs, -5 * 3600);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert!((ctx.now - now).abs() <= 5);
    }

    #[test]
    fn local_offset_is_a_plausible_time_zone() {
        for t in [0, 1_700_000_000, 1_793_700_000] {
            let off = local_utc_offset(t);
            assert!(off.abs() <= 26 * 3600, "{off}");
            assert_eq!(off % 60, 0, "offsets are whole minutes: {off}");
        }
    }
}

/// UTC where the local offset is not available yet.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn local_utc_offset(now: i64) -> i32 {
    let _ = now;
    0
}
