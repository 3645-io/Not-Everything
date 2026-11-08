//! Immutable snapshots published by the writer thread.

use std::sync::Arc;

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
}

impl EngineSnapshot {
    /// The index as of this snapshot.
    pub fn index(&self) -> &fx_index::Snapshot {
        &self.index
    }

    /// The merged order for `key`, or `None` if the engine does not keep it
    /// (see [`ENGINE_SORT_KEYS`]).
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
