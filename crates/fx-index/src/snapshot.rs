//! Immutable, thread-safe views of the index.
//!
//! Clean-room: written from docs/PLAN.md ("Updates and concurrency") only.

use fx_model::{EntryFlags, EntryId, EntryKind, FileKey};

use crate::extensions::{ExtId, ExtensionTable};
use crate::parts::{self, IndexParts};
use crate::store::State;

/// A frozen view of an [`Index`](crate::Index) at one moment.
///
/// Made by [`Index::snapshot`](crate::Index::snapshot) and handed out as an
/// `Arc<Snapshot>`. It shares the immutable base columns with the index and
/// keeps its own reference to the delta and the deletion bitset as they were;
/// the index copies those on its next write instead of changing them. Later
/// inserts, removals, renames and compactions therefore never change a
/// snapshot, and a snapshot may outlive the index. `Snapshot` is
/// `Send + Sync`, so many threads can read one snapshot at once.
///
/// # IDs
///
/// The accessors take an [`EntryId`] that is valid in *this* snapshot: one
/// from [`lookup`](Snapshot::lookup) or [`live_ids`](Snapshot::live_ids).
/// They panic if the ID is beyond [`id_bound`](Snapshot::id_bound), like
/// slice indexing. For an ID that is in range but removed they return the
/// entry's last stored values.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub(crate) state: State,
}

impl Snapshot {
    /// Number of live (not removed) entries.
    pub fn live_count(&self) -> usize {
        self.state.live
    }

    /// One past the largest ID this snapshot knows (live or removed).
    pub fn id_bound(&self) -> usize {
        self.state.total()
    }

    /// Whether `id` is in range and not removed.
    pub fn is_live(&self, id: EntryId) -> bool {
        self.state.is_live(id.0)
    }

    /// Iterates over every live entry ID in ascending order.
    pub fn live_ids(&self) -> impl Iterator<Item = EntryId> + '_ {
        self.state.live_ids()
    }

    /// The live entry with this key, if any.
    pub fn lookup(&self, key: FileKey) -> Option<EntryId> {
        self.state.lookup(key)
    }

    /// The stable key of an entry.
    pub fn key(&self, id: EntryId) -> FileKey {
        self.state.key(id)
    }

    /// The entry's own name (one path component).
    pub fn name(&self, id: EntryId) -> &str {
        self.state.name(id)
    }

    /// The containing folder, or [`EntryId::NONE`] for a root.
    pub fn parent(&self, id: EntryId) -> EntryId {
        self.state.parent(id)
    }

    /// File, directory or symlink.
    pub fn kind(&self, id: EntryId) -> EntryKind {
        self.state.meta(id).kind()
    }

    /// Attribute flags.
    pub fn flags(&self, id: EntryId) -> EntryFlags {
        self.state.meta(id).flags()
    }

    /// Size in bytes, saturated at `2^56 - 1` (see [`crate::meta`]).
    pub fn size(&self, id: EntryId) -> u64 {
        self.state.meta(id).size()
    }

    /// Last modification time, seconds since the Unix epoch.
    pub fn modified(&self, id: EntryId) -> i64 {
        self.state.modified(id)
    }

    /// Creation time, seconds since the Unix epoch.
    pub fn created(&self, id: EntryId) -> i64 {
        self.state.created(id)
    }

    /// The interned extension of the entry's name.
    pub fn ext(&self, id: EntryId) -> ExtId {
        self.state.ext(id)
    }

    /// The extension table that [`ext`](Snapshot::ext) IDs refer to.
    pub fn extensions(&self) -> &ExtensionTable {
        &self.state.exts
    }

    /// The entry's path: every name from its root down to it, joined with
    /// `'/'`. Built on demand by walking parent pointers; paths are never
    /// stored. A root's path is its own name.
    pub fn path(&self, id: EntryId) -> String {
        self.state.path(id)
    }

    /// Heap memory held by this snapshot's shared state, in bytes: the
    /// allocated capacity of the base columns and key table, the delta
    /// overlay (uncompacted updates and inserts), the deletion bitset and
    /// the extension table.
    ///
    /// The same accounting as [`Index::heap_bytes`](crate::Index::heap_bytes),
    /// minus the parts only the writer holds (the live-children counts and
    /// any pending compaction remap). Much of it is shared with the index and
    /// with other snapshots of the same base, so the bytes of several
    /// snapshots do not add up.
    pub fn heap_bytes(&self) -> usize {
        self.state.heap_bytes()
    }

    /// [`heap_bytes`](Snapshot::heap_bytes) split by column, as
    /// `(label, bytes)` pairs that sum to it. Labels are distinct:
    /// `names` (name bytes, base and delta), `name_offsets`, `parents`,
    /// `size_kind_flags`, `times`, `ext_ids`, `ext_table`, `file_keys`,
    /// `key_table`, `deleted_bitset` and `delta_overlay` (delta rows and
    /// maps, without their names).
    pub fn heap_columns(&self) -> Vec<(&'static str, usize)> {
        self.state.heap_columns()
    }

    /// Exports this snapshot as dense columns: base and delta materialized,
    /// deletions as a live bitmap. Entry IDs are kept exactly, removed slots
    /// included (with their last stored values), so orders saved alongside
    /// stay valid after [`Index::from_parts`](crate::Index::from_parts).
    pub fn to_parts(&self) -> IndexParts {
        parts::export(&self.state)
    }
}
