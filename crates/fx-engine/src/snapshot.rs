//! Immutable snapshots published by the writer thread.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use fx_model::ResumeCursor;
use fx_sort::{MatchSet, Order, SortKey};

/// The sort keys the engine keeps merged, in the order they are stored.
pub const ENGINE_SORT_KEYS: [SortKey; 4] = [
    SortKey::Name,
    SortKey::Path,
    SortKey::Size,
    SortKey::Modified,
];

/// The generation of a published snapshot, as sent to subscribers (see
/// [`Engine::subscribe`](crate::Engine::subscribe)). Converts to the `u64`
/// returned by [`EngineSnapshot::generation`] with `u64::from` and compares
/// with it directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Generation(pub u64);

impl Generation {
    /// The generation number.
    pub fn get(self) -> u64 {
        self.0
    }
}

impl From<Generation> for u64 {
    fn from(g: Generation) -> u64 {
        g.0
    }
}

impl From<u64> for Generation {
    fn from(g: u64) -> Generation {
        Generation(g)
    }
}

impl PartialEq<u64> for Generation {
    fn eq(&self, other: &u64) -> bool {
        self.0 == *other
    }
}

impl PartialEq<Generation> for u64 {
    fn eq(&self, other: &Generation) -> bool {
        *self == other.0
    }
}

impl PartialOrd<u64> for Generation {
    fn partial_cmp(&self, other: &u64) -> Option<std::cmp::Ordering> {
        Some(self.0.cmp(other))
    }
}

impl PartialOrd<Generation> for u64 {
    fn partial_cmp(&self, other: &Generation) -> Option<std::cmp::Ordering> {
        Some(self.cmp(&other.0))
    }
}

impl std::fmt::Display for Generation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// One published state of the engine: the index, its merged sort orders,
/// the change-source cursor it covers and its generation.
///
/// Snapshots never change after publishing; readers hold them as long as
/// they like while the writer prepares the next one.
#[derive(Debug, Clone)]
pub struct EngineSnapshot {
    pub(crate) index: Arc<fx_index::Snapshot>,
    pub(crate) orders: Arc<Vec<Order>>,
    pub(crate) cursor: Option<ResumeCursor>,
    pub(crate) generation: u64,
    /// The priority group of this index; `None` without priority folders.
    pub(crate) priority: Option<Arc<MatchSet>>,
    /// Published only while the writer merges a batch into the orders in
    /// place: the previous snapshot without its orders, which the writer
    /// took over. [`settled`] waits for the snapshot that follows.
    pub(crate) orders_pending: bool,
}

/// How long [`settled`] waits for the writer to finish merging the orders
/// in place. The merge takes a few milliseconds per million entries; the
/// limit only guards against a writer that never publishes again, and the
/// snapshot it then returns is still correct (an order it lacks is built
/// on the spot, see `order_or_build`).
const SETTLE_LIMIT: Duration = Duration::from_secs(1);

/// The latest published snapshot whose orders are in place.
///
/// While no window shows, the writer merges a batch into the sort orders in
/// place when no reader holds them, instead of copying all four (16 B per
/// entry, about 51 MB at 3.2M entries) while the published snapshot still
/// holds the old ones. For the few milliseconds of the merge it publishes
/// the previous snapshot without its orders (`orders_pending`); this waits
/// for the snapshot that follows, so a reader never has to sort the whole
/// index itself.
pub(crate) fn settled(current: &ArcSwap<EngineSnapshot>) -> Arc<EngineSnapshot> {
    let mut waiting_since = None;
    loop {
        let snap = current.load_full();
        if !snap.orders_pending {
            return snap;
        }
        let since = *waiting_since.get_or_insert_with(Instant::now);
        if since.elapsed() >= SETTLE_LIMIT {
            return snap;
        }
        drop(snap);
        thread::sleep(Duration::from_micros(200));
    }
}

impl EngineSnapshot {
    /// The index as of this snapshot.
    pub fn index(&self) -> &fx_index::Snapshot {
        &self.index
    }

    /// The merged order for `key`, or `None` if the engine does not keep it
    /// (see [`ENGINE_SORT_KEYS`]). Also `None` for every key in the one
    /// case where the engine publishes a snapshot without its orders: its
    /// writer failed while merging a batch into them in place.
    pub fn order(&self, key: SortKey) -> Option<&Order> {
        self.orders.iter().find(|o| o.key == key)
    }

    /// The change-source position covered by this snapshot: every change
    /// before it is applied. `None` when the index was never synchronised
    /// with a change source.
    pub fn cursor(&self) -> Option<ResumeCursor> {
        self.cursor
    }

    /// Increases by one with every published batch; the snapshot available
    /// right after start has generation 0.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The entries of this snapshot in the priority folders ("Your files
    /// first", see
    /// [`EngineConfig::with_priority_folders`](crate::EngineConfig::with_priority_folders)),
    /// by entry ID of this snapshot's index. `None` when the engine has no
    /// priority folders.
    pub fn priority_group(&self) -> Option<&MatchSet> {
        self.priority.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(generation: u64, orders_pending: bool) -> EngineSnapshot {
        EngineSnapshot {
            index: fx_index::Index::new().snapshot(),
            orders: Arc::new(Vec::new()),
            cursor: None,
            generation,
            priority: None,
            orders_pending,
        }
    }

    #[test]
    fn settled_waits_for_the_snapshot_after_a_merge_in_place() {
        let current = Arc::new(ArcSwap::from_pointee(snapshot(1, true)));
        let writer = {
            let current = Arc::clone(&current);
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(20));
                current.store(Arc::new(snapshot(2, false)));
            })
        };
        let snap = settled(&current);
        assert!(!snap.orders_pending);
        assert_eq!(snap.generation, 2);
        writer.join().expect("the writer stored a snapshot");
    }

    #[test]
    fn settled_returns_a_settled_snapshot_at_once() {
        let current = ArcSwap::from_pointee(snapshot(4, false));
        assert_eq!(settled(&current).generation, 4);
    }

    #[test]
    fn settled_gives_up_on_a_merge_that_never_ends() {
        let current = ArcSwap::from_pointee(snapshot(1, true));
        let start = Instant::now();
        let snap = settled(&current);
        assert!(snap.orders_pending, "the only snapshot there is");
        assert!(start.elapsed() >= SETTLE_LIMIT);
    }
}
