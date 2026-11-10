//! The mutable index: applies entries and changes, keeps the delta and the
//! deletion bitset, compacts, and hands out snapshots.
//!
//! Clean-room: written from docs/PLAN.md ("Data layout", "Updates and
//! concurrency") only.

use std::cell::OnceCell;
use std::fmt;
use std::sync::Arc;

use fx_model::{Change, Entry, EntryId, FileKey};

use crate::kids::{BaseKids, DeltaKids, name_hash};

use crate::extensions::{ExtId, extension_of};
use crate::meta::PackedMeta;
use crate::names::{NameBuffer, NameError};
use crate::parts::{self, IndexParts, PartsError, SharedParts};
use crate::snapshot::Snapshot;
use crate::store::{Base, Bitset, Delta, Row, State};

/// Default for [`Index::compact_fraction`]: compact once the delta passes
/// 10% of the live entries (docs/PLAN.md: "when the delta passes 5–10%").
pub const DEFAULT_COMPACT_FRACTION: f64 = 0.10;

/// Why the index refused a change. A refused change leaves the index as it
/// was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexError {
    /// The entry's parent key is not a live entry (never inserted, or
    /// removed). Such entries are rejected, not queued: the caller should
    /// insert parents first (enumerators walk top-down) or re-scan.
    UnknownParent {
        /// Key of the rejected entry.
        key: FileKey,
        /// The parent key that was not found.
        parent: FileKey,
    },
    /// The new parent is the entry itself or one of its descendants.
    Cycle {
        /// Key of the rejected entry.
        key: FileKey,
        /// The parent key that would create the cycle.
        parent: FileKey,
    },
    /// The name is empty or contains a NUL byte.
    InvalidName {
        /// Key of the rejected entry.
        key: FileKey,
        /// What is wrong with the name.
        error: NameError,
    },
    /// The index ran out of `u32` entry IDs or name-buffer offsets.
    Full,
    /// The change names a path (`CheckPath`, `RescanSubtree`,
    /// `RescanVolume`), which needs the file system. The writer resolves it
    /// into `Upsert` and `Remove` changes before applying them.
    NeedsFileSystem,
    /// [`Index::from_parts`] was given columns that do not describe a valid
    /// index (bad lengths, offsets, UTF-8, parents, extension IDs or
    /// duplicate live keys).
    InvalidParts(PartsError),
}

impl From<PartsError> for IndexError {
    fn from(error: PartsError) -> Self {
        IndexError::InvalidParts(error)
    }
}

impl fmt::Display for IndexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IndexError::UnknownParent { key, parent } => {
                write!(f, "parent {parent:?} of entry {key:?} is not in the index")
            }
            IndexError::Cycle { key, parent } => {
                write!(f, "moving {key:?} under {parent:?} would create a cycle")
            }
            IndexError::InvalidName { key, error } => {
                write!(f, "invalid name for entry {key:?}: {error}")
            }
            IndexError::Full => f.write_str("the index has no entry IDs or name space left"),
            IndexError::NeedsFileSystem => {
                f.write_str("path-based change must be resolved against the file system first")
            }
            IndexError::InvalidParts(error) => write!(f, "invalid index parts: {error}"),
        }
    }
}

impl std::error::Error for IndexError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IndexError::InvalidParts(error) => Some(error),
            _ => None,
        }
    }
}

/// What [`Index::apply`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// The entry was inserted or updated and now has this ID.
    Upserted(EntryId),
    /// This many entries were removed (the entry and its descendants).
    Removed(usize),
}

/// What [`Index::upsert_reporting`] changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upserted {
    /// The entry was already stored exactly so.
    Unchanged,
    /// Size, kind, flags or dates changed; name and parent did not.
    Updated,
    /// The name or the parent changed: a rename or a move.
    Moved,
    /// The key was new.
    Inserted,
}

/// The in-memory index of one or more volumes.
///
/// Columns are indexed by [`EntryId`]. IDs are stable until the next
/// compaction, explicit ([`compact`](Index::compact)) or automatic (when the
/// delta passes [`compact_fraction`](Index::compact_fraction) of the live
/// entries, checked after every change). Compaction keeps live entries in ID
/// order, so it only renumbers when something was removed. The remap of
/// automatic compactions is collected for [`take_auto_remap`](Index::take_auto_remap).
///
/// Readers use [`snapshot`](Index::snapshot); the index itself is meant for
/// one writer, which also navigates it: children by parent and by name,
/// subtrees, and entries read back by key.
#[derive(Debug)]
pub struct Index {
    state: State,
    /// The base IDs by parent, built at the first navigation after a load
    /// or a compaction (a reader that only searches, such as the command,
    /// never builds it). Writer-only; snapshots do not carry it.
    base_kids: OnceCell<BaseKids>,
    /// The live delta rows by parent and name. Writer-only.
    delta_kids: DeltaKids,
    compact_fraction: f64,
    /// Composition of the remaps of compactions since the first automatic
    /// one not yet taken, indexed by the IDs valid before it.
    auto_remap: Option<Vec<EntryId>>,
}

impl Default for Index {
    fn default() -> Self {
        Self::new()
    }
}

impl Index {
    /// Creates an empty index with the default compaction fraction.
    pub fn new() -> Self {
        Index {
            state: State::default(),
            base_kids: OnceCell::new(),
            delta_kids: DeltaKids::default(),
            compact_fraction: DEFAULT_COMPACT_FRACTION,
            auto_remap: None,
        }
    }

    /// Rebuilds an index from dense columns, as made by
    /// [`Snapshot::to_parts`].
    ///
    /// Entry IDs are kept exactly, removed slots included, so the restored
    /// index answers every [`Snapshot`] accessor, [`lookup`](Index::lookup)
    /// and [`live_ids`](Snapshot::live_ids) like the exported one, and later
    /// changes and compactions behave the same. Everything goes into the
    /// base; the delta starts empty. The compaction fraction is the default.
    ///
    /// Errors: [`IndexError::InvalidParts`] if the columns differ in length,
    /// the live bitmap is too short, the name or extension buffers are
    /// malformed or not UTF-8, a parent is out of range (or removed, or on a
    /// cycle, for a live entry), a packed meta value uses reserved bits, an
    /// extension ID is not in the table, or two live entries share a key.
    /// Never panics on bad input.
    pub fn from_parts(parts: IndexParts) -> Result<Index, IndexError> {
        let state = parts::restore(parts)?;
        Ok(Index {
            base_kids: OnceCell::new(),
            delta_kids: DeltaKids::default(),
            state,
            compact_fraction: DEFAULT_COMPACT_FRACTION,
            auto_remap: None,
        })
    }

    /// [`from_parts`](Index::from_parts) for columns that stay in a
    /// [`Backing`](crate::Backing), such as a saved index's memory map: the
    /// same checks and the same index, but the base columns are served from
    /// the backing instead of being copied to the heap. Only the key table,
    /// the extension table and the deletion bitset are built on the heap.
    ///
    /// Errors: as [`from_parts`](Index::from_parts), and
    /// [`PartsError::SharedColumn`] when a range is out of bounds or isn't a
    /// whole number of values.
    pub fn from_shared(parts: SharedParts) -> Result<Index, IndexError> {
        Ok(Index::from_state(parts::restore_shared(parts)?))
    }

    /// Serves the base columns from `restored`, an index restored from a
    /// save of `saved`, when `saved` held exactly this index's base: the
    /// same base, an empty delta and no removals, as right after a first
    /// scan or a compaction. Changes made since `saved` stay in the delta
    /// and the deletion bitset, which refer to the same entry IDs. Returns
    /// whether the base was swapped; it isn't when the base has been
    /// compacted since, or `saved` wasn't exactly the base.
    pub fn adopt_base(&mut self, saved: &Snapshot, restored: Index) -> bool {
        let saved = &saved.state;
        let exact = Arc::ptr_eq(&self.state.base, &saved.base)
            && saved.delta.len() == 0
            && saved.deleted.ones == 0;
        let restored = restored.state;
        if !exact
            || restored.base.len() != self.state.base.len()
            || restored.deleted.ones != 0
            || restored.delta.len() != 0
        {
            return false;
        }
        self.state.base = restored.base;
        true
    }

    /// Bytes of the base columns served from a backing instead of the heap
    /// ([`from_shared`](Index::from_shared)). They are part of
    /// [`heap_bytes`](Index::heap_bytes), which counts every column the
    /// index holds, so that figure keeps its meaning.
    pub fn mapped_bytes(&self) -> usize {
        self.state.base.mapped_bytes()
    }

    /// An index over `state`, whose delta is empty ([`IndexBuilder`](crate::IndexBuilder)).
    pub(crate) fn from_state(state: State) -> Index {
        Index {
            base_kids: OnceCell::new(),
            delta_kids: DeltaKids::default(),
            state,
            compact_fraction: DEFAULT_COMPACT_FRACTION,
            auto_remap: None,
        }
    }

    /// Number of live entries.
    pub fn live_count(&self) -> usize {
        self.state.live
    }

    /// One past the largest entry ID handed out since the last compaction
    /// (live and removed IDs alike).
    pub fn id_bound(&self) -> usize {
        self.state.total()
    }

    /// Whether `id` is a live entry.
    pub fn is_live(&self, id: EntryId) -> bool {
        self.state.is_live(id.0)
    }

    /// The live entry with this key, if any.
    pub fn lookup(&self, key: FileKey) -> Option<EntryId> {
        self.state.lookup(key)
    }

    /// An immutable view of the index as it is now. O(1): it shares the base
    /// and the current delta, which the index copies on its next write.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        Arc::new(Snapshot {
            state: self.state.clone(),
        })
    }

    /// Number of rows in the delta: entries appended since the last
    /// compaction plus base entries renamed, moved or updated since then.
    pub fn delta_len(&self) -> usize {
        self.state.delta.len()
    }

    /// Number of removed entries still holding an ID until compaction.
    pub fn deleted_len(&self) -> usize {
        self.state.deleted.ones
    }

    /// Fraction of the live entries the delta (or the removed entries) may
    /// reach before an automatic compaction. Default
    /// [`DEFAULT_COMPACT_FRACTION`].
    pub fn compact_fraction(&self) -> f64 {
        self.compact_fraction
    }

    /// Sets the automatic compaction threshold. Negative and NaN values
    /// count as 0 (compact after every change); `f64::INFINITY` turns
    /// automatic compaction off. Takes effect at the next change.
    pub fn set_compact_fraction(&mut self, fraction: f64) {
        self.compact_fraction = if fraction >= 0.0 { fraction } else { 0.0 };
    }

    /// Applies one change record.
    pub fn apply(&mut self, change: Change) -> Result<Applied, IndexError> {
        match change {
            Change::Upsert(entry) => self.upsert(entry).map(Applied::Upserted),
            Change::Remove { key } => Ok(Applied::Removed(self.remove(key))),
            Change::CheckPath(_) | Change::RescanSubtree(_) | Change::RescanVolume => {
                Err(IndexError::NeedsFileSystem)
            }
        }
    }

    /// Inserts a new entry or updates the one with the same key, returning
    /// its ID (after any automatic compaction this call triggered).
    ///
    /// A new key gets the next ID and goes to the delta. An existing key
    /// keeps its ID; if anything changed, its row goes to the delta. A
    /// changed name or parent is a rename or move: only that entry changes,
    /// and descendants' paths follow through their parent pointers.
    ///
    /// Errors (index unchanged): [`IndexError::UnknownParent`] if the parent
    /// key is not live, [`IndexError::Cycle`] if the entry would become its
    /// own ancestor, [`IndexError::InvalidName`], [`IndexError::Full`].
    pub fn upsert(&mut self, entry: Entry) -> Result<EntryId, IndexError> {
        self.upsert_reporting(entry).map(|(id, _)| id)
    }

    /// [`upsert`](Index::upsert), also saying what changed, so the writer
    /// knows whether the sort orders need the entry re-sorted, and whether
    /// its subtree moved with it ([`Upserted::Moved`]).
    pub fn upsert_reporting(&mut self, entry: Entry) -> Result<(EntryId, Upserted), IndexError> {
        let key = entry.key;
        if entry.name.is_empty() {
            return Err(IndexError::InvalidName {
                key,
                error: NameError::Empty,
            });
        }
        if entry.name.as_bytes().contains(&0) {
            return Err(IndexError::InvalidName {
                key,
                error: NameError::ContainsNul,
            });
        }
        let parent = match entry.parent {
            None => EntryId::NONE,
            Some(pk) => self
                .state
                .lookup(pk)
                .ok_or(IndexError::UnknownParent { key, parent: pk })?,
        };
        let existing = self.state.lookup(key);
        if let (Some(id), Some(pk)) = (existing, entry.parent) {
            let mut cur = parent;
            while !cur.is_none() {
                if cur == id {
                    return Err(IndexError::Cycle { key, parent: pk });
                }
                cur = self.state.parent(cur);
            }
        }
        let name_cost = entry.name.len() + 1;
        let name_space = self.state.base.names.bytes().len() + self.state.delta.name_bytes;
        if name_space + name_cost > u32::MAX as usize {
            return Err(IndexError::Full);
        }
        if existing.is_none() && self.state.total() >= (u32::MAX - 1) as usize {
            return Err(IndexError::Full);
        }

        let row = Row {
            key,
            ext: self.intern_ext(&entry.name),
            name: entry.name.into_boxed_str(),
            parent: parent.0,
            meta: PackedMeta::new(entry.size, entry.kind, entry.flags),
            modified: entry.modified,
            created: entry.created,
        };
        let (id, what) = match existing {
            Some(id) => {
                let what = self.update(id, row);
                if what == Upserted::Unchanged {
                    return Ok((id, what));
                }
                (id, what)
            }
            None => (self.insert_new(row), Upserted::Inserted),
        };
        let id = match self.maybe_compact() {
            Some(remap) => remap[id.index()],
            None => id,
        };
        Ok((id, what))
    }

    /// Removes the entry with this key and all its descendants. Returns how
    /// many entries were removed: 0 for an unknown or already removed key.
    ///
    /// Cost: proportional to the size of the subtree, found through the
    /// children index ([`subtree`](Index::subtree)), apart from any
    /// automatic compaction it triggers.
    pub fn remove(&mut self, key: FileKey) -> usize {
        let Some(id) = self.state.lookup(key) else {
            return 0;
        };
        let doomed = self.subtree(id);
        for &d in &doomed {
            self.delete_one(d);
        }
        self.state.live -= doomed.len();
        self.maybe_compact();
        doomed.len()
    }

    /// The key of entry `id`, live or removed but not yet compacted away.
    ///
    /// # Panics
    ///
    /// Panics if `id` is out of range.
    pub fn key(&self, id: EntryId) -> FileKey {
        self.state.key(id)
    }

    /// The live entry with this key, read back as an [`Entry`]: what the
    /// last upsert stored (sizes beyond 2^56 - 1 bytes are clamped, as
    /// stored).
    pub fn get(&self, key: FileKey) -> Option<Entry> {
        let id = self.state.lookup(key)?;
        let row = self.state.row(id);
        let parent = (row.parent != u32::MAX).then(|| self.state.key(EntryId(row.parent)));
        Some(Entry {
            key: row.key,
            parent,
            name: row.name.into_string(),
            kind: row.meta.kind(),
            flags: row.meta.flags(),
            size: row.meta.size(),
            modified: row.modified,
            created: row.created,
        })
    }

    /// The live children of `parent` ([`EntryId::NONE`]: the roots), the
    /// delta's first (the most recently added first), then the base's.
    pub fn children(&self, parent: EntryId) -> Vec<EntryId> {
        let mut out = self.delta_children(parent, None);
        out.extend(
            self.base_kids()
                .group(&self.state.base, parent.0)
                .iter()
                .copied()
                .filter(|&m| self.base_current(m))
                .map(EntryId),
        );
        out
    }

    /// The live entries with no parent, the most recently added first.
    pub fn roots(&self) -> Vec<EntryId> {
        self.children(EntryId::NONE)
    }

    /// The live children of `parent` named `name` (exact bytes), the most
    /// recently added first. Usually one; two while a replaced entry waits
    /// for its removal.
    pub fn children_named(&self, parent: EntryId, name: &str) -> Vec<EntryId> {
        let mut out = self.delta_children(parent, Some(name));
        out.extend(
            self.base_kids()
                .named(&self.state.base, parent.0, name)
                .filter(|&m| self.base_current(m))
                .map(EntryId),
        );
        out
    }

    /// The most recently added live child of `parent` named `name`.
    pub fn child(&self, parent: EntryId, name: &str) -> Option<EntryId> {
        self.children_named(parent, name).first().copied()
    }

    /// `id` and every live entry below it, each after its parent. Empty if
    /// `id` is not live.
    pub fn subtree(&self, id: EntryId) -> Vec<EntryId> {
        if !self.state.is_live(id.0) {
            return Vec::new();
        }
        let mut out = vec![id];
        let mut i = 0;
        while i < out.len() {
            let kids = self.children(out[i]);
            out.extend(kids);
            i += 1;
        }
        out
    }

    /// The base IDs by parent, built on first use.
    fn base_kids(&self) -> &BaseKids {
        self.base_kids
            .get_or_init(|| BaseKids::build(&self.state.base))
    }

    /// Whether base entry `m` is live and still stored in the base (not
    /// overridden by a delta row, which [`DeltaKids`] lists instead).
    fn base_current(&self, m: u32) -> bool {
        !self.state.deleted.get(m as usize) && !self.state.delta.overrides.contains_key(&m)
    }

    /// The live delta rows under `parent` (named `name`, if given), new
    /// rows before overrides and each newest ID first.
    fn delta_children(&self, parent: EntryId, name: Option<&str>) -> Vec<EntryId> {
        let mut out: Vec<EntryId> = match name {
            Some(name) => self
                .delta_kids
                .hashed(parent.0, name_hash(name))
                .filter(|&id| self.state.name(EntryId(id)) == name)
                .map(EntryId)
                .collect(),
            None => self.delta_kids.children(parent.0).map(EntryId).collect(),
        };
        out.retain(|id| self.state.is_live(id.0));
        let base_len = self.state.base.len();
        out.sort_unstable_by(|a, b| {
            let new_a = a.index() >= base_len;
            let new_b = b.index() >= base_len;
            new_b.cmp(&new_a).then(b.0.cmp(&a.0))
        });
        out
    }

    /// Where a delta row of `id` sits in [`DeltaKids`]: its parent and name
    /// hash; `None` when `id` has no delta row.
    fn delta_place(&self, id: EntryId) -> Option<(u32, u64)> {
        let i = id.index();
        let base_len = self.state.base.len();
        let row = if i >= base_len {
            self.state.delta.new.get(i - base_len)
        } else {
            self.state.delta.overrides.get(&id.0)
        }?;
        Some((row.parent, name_hash(&row.name)))
    }

    /// Rebuilds a dense base from the live entries, dropping removed ones
    /// and emptying the delta. Live entries keep their relative order.
    ///
    /// Returns the remap indexed by old ID: the new ID of each live entry,
    /// [`EntryId::NONE`] for removed ones. Existing snapshots are unaffected.
    pub fn compact(&mut self) -> Vec<EntryId> {
        let remap = self.compact_inner();
        if let Some(acc) = &mut self.auto_remap {
            compose(acc, &remap);
        }
        remap
    }

    /// Takes the remap of the automatic compactions since the last call
    /// (composed with any explicit [`compact`](Index::compact) after the
    /// first of them), indexed by the IDs that were valid before the first.
    /// `None` if no automatic compaction ran.
    pub fn take_auto_remap(&mut self) -> Option<Vec<EntryId>> {
        self.auto_remap.take()
    }

    /// Heap memory held by the index, in bytes (allocated capacity of every
    /// column, the key table, the delta, the bitset and the children
    /// index). Snapshots share most of it.
    pub fn heap_bytes(&self) -> usize {
        self.state.heap_bytes()
            + self.base_kids.get().map_or(0, BaseKids::heap_bytes)
            + self.delta_kids.heap_bytes()
            + self.auto_remap.as_ref().map_or(0, |r| r.capacity() * 4)
    }

    /// [`heap_bytes`](Index::heap_bytes) divided by the live entry count
    /// (the PLAN's memory budget is about 100 B per entry).
    pub fn heap_bytes_per_entry(&self) -> usize {
        self.heap_bytes() / self.state.live.max(1)
    }

    /// Interns the name's extension, copying the shared table only when the
    /// extension is new.
    fn intern_ext(&mut self, name: &str) -> ExtId {
        let Some(ext) = extension_of(name) else {
            return ExtId::NONE;
        };
        if let Some(id) = self.state.exts.lookup(ext) {
            return id;
        }
        if self.state.exts.is_full() {
            return ExtId::OVERFLOW;
        }
        Arc::make_mut(&mut self.state.exts).intern(name)
    }

    /// Replaces the row of live entry `id` and says what changed.
    fn update(&mut self, id: EntryId, row: Row) -> Upserted {
        let old = self.state.row(id);
        let moved = *old.name != *row.name || old.parent != row.parent;
        if !moved
            && old.meta == row.meta
            && old.modified == row.modified
            && old.created == row.created
        {
            return Upserted::Unchanged;
        }
        if let Some((parent, hash)) = self.delta_place(id) {
            self.delta_kids.remove(parent, hash, id.0);
        }
        self.delta_kids
            .insert(row.parent, name_hash(&row.name), id.0);
        let base_len = self.state.base.len();
        let delta = Arc::make_mut(&mut self.state.delta);
        let i = id.index();
        let old_bytes = if i < base_len {
            delta.overrides.get(&id.0).map_or(0, |r| r.name.len() + 1)
        } else {
            old.name.len() + 1
        };
        delta.name_bytes = delta.name_bytes - old_bytes + row.name.len() + 1;
        if i < base_len {
            delta.overrides.insert(id.0, row);
        } else {
            delta.new[i - base_len] = row;
        }
        if moved {
            Upserted::Moved
        } else {
            Upserted::Updated
        }
    }

    fn insert_new(&mut self, row: Row) -> EntryId {
        let id = EntryId(self.state.total() as u32);
        self.delta_kids
            .insert(row.parent, name_hash(&row.name), id.0);
        Arc::make_mut(&mut self.state.deleted).push_clear();
        let delta = Arc::make_mut(&mut self.state.delta);
        delta.name_bytes += row.name.len() + 1;
        delta.keys.insert(row.key, id.0);
        delta.new.push(row);
        self.state.live += 1;
        id
    }

    /// Marks one entry removed (no live-count bookkeeping).
    ///
    /// A removed base entry keeps any override row, so its dead slot reads
    /// back the entry's last stored values (as [`Snapshot`] documents) rather
    /// than the values from before a rename or move. That also makes a
    /// restore from [`Snapshot::to_parts`], which bakes overrides into the
    /// base, behave exactly like the original on later removals.
    fn delete_one(&mut self, id: EntryId) {
        if let Some((parent, hash)) = self.delta_place(id) {
            self.delta_kids.remove(parent, hash, id.0);
        }
        let i = id.index();
        let base_len = self.state.base.len();
        Arc::make_mut(&mut self.state.deleted).set(i);
        if i >= base_len {
            let delta = Arc::make_mut(&mut self.state.delta);
            let key = delta.new[i - base_len].key;
            delta.keys.remove(&key);
        }
    }

    /// Compacts automatically if the delta or the removed entries pass the
    /// threshold; returns the remap if it did.
    fn maybe_compact(&mut self) -> Option<Vec<EntryId>> {
        // `f64::INFINITY` turns automatic compaction off. Check it first:
        // with live == 0, INFINITY * 0 would be NaN and every comparison
        // below would fail, compacting when the caller asked us not to.
        if self.compact_fraction.is_infinite() {
            return None;
        }
        // `set_compact_fraction` maps NaN and negatives to 0, so the
        // fraction is finite and non-negative here and the product of two
        // finite non-negative values is never NaN.
        let limit = self.compact_fraction * self.state.live as f64;
        let delta = self.state.delta.len() as f64;
        let deleted = self.state.deleted.ones as f64;
        if delta <= limit && deleted <= limit {
            return None;
        }
        let remap = self.compact_inner();
        match &mut self.auto_remap {
            Some(acc) => compose(acc, &remap),
            None => self.auto_remap = Some(remap.clone()),
        }
        Some(remap)
    }

    fn compact_inner(&mut self) -> Vec<EntryId> {
        let state = &self.state;
        let total = state.total();
        let live = state.live;
        let mut remap = vec![EntryId::NONE; total];
        let mut next = 0u32;
        let mut name_bytes = 0usize;
        for (i, slot) in remap.iter_mut().enumerate() {
            if !state.deleted.get(i) {
                *slot = EntryId(next);
                next += 1;
                name_bytes += state.name(EntryId(i as u32)).len() + 1;
            }
        }
        let mut base = Base {
            names: NameBuffer::with_capacity(live, name_bytes),
            parent: Vec::with_capacity(live).into(),
            meta: Vec::with_capacity(live).into(),
            modified: Vec::with_capacity(live).into(),
            created: Vec::with_capacity(live).into(),
            ext: Vec::with_capacity(live).into(),
            volume: Vec::with_capacity(live).into(),
            file_id: Vec::with_capacity(live).into(),
            table: Vec::new(),
        };
        for i in 0..total {
            if state.deleted.get(i) {
                continue;
            }
            let id = EntryId(i as u32);
            let name = state.name(id);
            // Names were validated on upsert and the total size checked
            // against the u32 offset space, so this cannot fail.
            base.names
                .push(name)
                .expect("validated name fits the name buffer");
            let parent = state.parent(id);
            let new_parent = if parent.is_none() {
                u32::MAX
            } else {
                let p = remap[parent.index()];
                debug_assert!(!p.is_none(), "live entry with a removed parent");
                p.0
            };
            base.parent.push(new_parent);
            base.meta.push(state.meta(id).0);
            base.modified.push(state.modified(id));
            base.created.push(state.created(id));
            base.ext.push(state.ext(id).0);
            let key = state.key(id);
            base.volume.push(key.volume.0);
            base.file_id.push(key.file_id);
        }
        base.build_table();
        self.base_kids = OnceCell::new();
        self.delta_kids.clear();
        self.state = State {
            base: Arc::new(base),
            delta: Arc::new(Delta::default()),
            deleted: Arc::new(Bitset::zeros(live)),
            exts: Arc::clone(&self.state.exts),
            live,
        };
        remap
    }
}

/// Maps every target of `acc` through `step`.
fn compose(acc: &mut [EntryId], step: &[EntryId]) {
    for id in acc.iter_mut() {
        if !id.is_none() {
            *id = step[id.index()];
        }
    }
}
