//! The read-only view of entries that orders are built from.
//!
//! Clean-room: written from docs/PLAN.md ("Sorting") and the public fx-index
//! API only.

use fx_model::{EntryId, EntryKind};

/// Read access to the entry columns a sort needs.
///
/// IDs run from `0` to [`id_bound`](SortSource::id_bound); only those for
/// which [`is_live`](SortSource::is_live) holds appear in an order. The
/// signatures match [`fx_index::Snapshot`]'s accessors, which implements this
/// trait; tests and benchmarks can use any in-memory table instead.
pub trait SortSource {
    /// One past the largest ID (live or removed).
    fn id_bound(&self) -> usize;

    /// Whether `id` is in range and not removed.
    fn is_live(&self, id: EntryId) -> bool;

    /// The entry's own name (one path component).
    fn name(&self, id: EntryId) -> &str;

    /// The containing folder, or [`EntryId::NONE`] for a root.
    ///
    /// An entry that names itself as its parent is also treated as a root.
    fn parent(&self, id: EntryId) -> EntryId;

    /// Size in bytes.
    fn size(&self, id: EntryId) -> u64;

    /// Last modification time, seconds since the Unix epoch.
    fn modified(&self, id: EntryId) -> i64;

    /// Creation time, seconds since the Unix epoch.
    fn created(&self, id: EntryId) -> i64;

    /// File, directory or symlink.
    fn kind(&self, id: EntryId) -> EntryKind;
}

impl SortSource for fx_index::Snapshot {
    fn id_bound(&self) -> usize {
        fx_index::Snapshot::id_bound(self)
    }

    fn is_live(&self, id: EntryId) -> bool {
        fx_index::Snapshot::is_live(self, id)
    }

    fn name(&self, id: EntryId) -> &str {
        fx_index::Snapshot::name(self, id)
    }

    fn parent(&self, id: EntryId) -> EntryId {
        fx_index::Snapshot::parent(self, id)
    }

    fn size(&self, id: EntryId) -> u64 {
        fx_index::Snapshot::size(self, id)
    }

    fn modified(&self, id: EntryId) -> i64 {
        fx_index::Snapshot::modified(self, id)
    }

    fn created(&self, id: EntryId) -> i64 {
        fx_index::Snapshot::created(self, id)
    }

    fn kind(&self, id: EntryId) -> EntryKind {
        fx_index::Snapshot::kind(self, id)
    }
}

/// The parent of `id`, or `None` for a root (no parent or itself).
pub(crate) fn parent_of<S: SortSource + ?Sized>(src: &S, id: EntryId) -> Option<EntryId> {
    let p = src.parent(id);
    if p.is_none() || p == id {
        None
    } else {
        Some(p)
    }
}
