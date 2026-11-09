//! The memory report of a published snapshot: heap bytes of the default
//! columns and bytes per live entry (the Phase 1 memory gate, ≤100 B per
//! entry).

use std::fmt;
use std::mem::size_of;

use crate::snapshot::EngineSnapshot;

/// Heap bytes of a snapshot's default columns, from
/// [`EngineSnapshot::memory_report`].
///
/// Real heap accounting: the index columns come from
/// [`fx_index::Snapshot::heap_columns`] (the same accounting as
/// `fx_index::Index::heap_bytes`, so allocated capacity, the uncompacted
/// delta overlay, the deletion bitset and the extension table are all
/// counted), plus the merged sort orders at their allocated capacity.
/// Structures private to the writer thread (the index's children index,
/// 4 B per entry plus the delta's rows) and the refinement cache of the
/// query path are not part of a snapshot and are not counted.
///
/// `Display` prints the benchmark format documented in the crate README:
/// one `metric=value` per line (`live_entries`, `id_slots`, `total_bytes`,
/// `bytes_per_entry`, then `column.<label>` for every column).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryReport {
    /// Live entries in the snapshot.
    pub live_entries: usize,
    /// Entry ID slots (live plus removed ones not yet compacted away).
    pub id_slots: usize,
    /// Sum of the column sizes in bytes.
    pub total_bytes: usize,
    /// `(label, heap bytes)` for every column, labels distinct.
    pub columns: Vec<(&'static str, usize)>,
}

impl MemoryReport {
    /// `total_bytes / live_entries` (0 for an empty snapshot).
    pub fn bytes_per_entry(&self) -> f64 {
        if self.live_entries == 0 {
            0.0
        } else {
            self.total_bytes as f64 / self.live_entries as f64
        }
    }

    /// The size of the column labelled `label`, if listed.
    pub fn column(&self, label: &str) -> Option<usize> {
        self.columns
            .iter()
            .find(|(l, _)| *l == label)
            .map(|&(_, b)| b)
    }
}

impl fmt::Display for MemoryReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "live_entries={}", self.live_entries)?;
        writeln!(f, "id_slots={}", self.id_slots)?;
        writeln!(f, "total_bytes={}", self.total_bytes)?;
        writeln!(f, "bytes_per_entry={:.2}", self.bytes_per_entry())?;
        for (label, bytes) in &self.columns {
            writeln!(f, "column.{label}={bytes}")?;
        }
        Ok(())
    }
}

/// Labels of the columns the engine adds to fx-index's own.
pub(crate) mod labels {
    pub const FOLDED: &str = "folded_search_copy";
    pub const ORDERS: &str = "sort_orders";
    pub const PRIORITY: &str = "priority_group";
}

impl EngineSnapshot {
    /// The heap bytes of this snapshot's default columns and the bytes per
    /// live entry. See [`MemoryReport`] for what is counted.
    ///
    /// Columns: every column of [`fx_index::Snapshot::heap_columns`] (names,
    /// name offsets, parents, packed size/kind/flags, times, extension IDs
    /// and table, file keys and their lookup table, the deletion bitset and
    /// the delta overlay), then the folded search copy (0: the engine keeps
    /// none; the matcher folds names as it scans), the pre-sorted orders and
    /// the priority group's bitset (0 without priority folders).
    ///
    /// O(columns); cheap enough for diagnostics and benchmarks.
    pub fn memory_report(&self) -> MemoryReport {
        let idx = &*self.index;
        let orders: usize = self
            .orders
            .iter()
            .map(|o| o.ids.capacity() * size_of::<u32>())
            .sum();
        let mut columns = idx.heap_columns();
        columns.push((labels::FOLDED, 0));
        columns.push((labels::ORDERS, orders));
        columns.push((
            labels::PRIORITY,
            self.priority.as_ref().map_or(0, |g| g.heap_bytes()),
        ));
        let total_bytes = columns.iter().map(|&(_, b)| b).sum();
        MemoryReport {
            live_entries: idx.live_count(),
            id_slots: idx.id_bound(),
            total_bytes,
            columns,
        }
    }
}
