//! The mutable index: applies entries and changes, keeps the delta and the
//! deletion bitset, compacts, and hands out snapshots.
//!
//! Clean-room: written from docs/PLAN.md ("Data layout", "Updates and
//! concurrency") only.

use std::fmt;
use std::mem::size_of;
use std::sync::Arc;

use fx_model::{Change, Entry, EntryId, FileKey};

use crate::extensions::{ExtId, extension_of};
use crate::meta::PackedMeta;
use crate::names::{NameBuffer, NameError};
use crate::parts::{self, IndexParts, PartsError};
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
/// one writer.
#[derive(Debug)]
pub struct Index {
    state: State,
    /// Live children per ID, so removing a leaf needs no scan. Writer-only;
    /// snapshots do not carry it.
    children: Vec<u32>,
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
            children: Vec::new(),
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
        let (state, children) = parts::restore(parts)?;
        Ok(Index {
            state,
            children,
            compact_fraction: DEFAULT_COMPACT_FRACTION,
            auto_remap: None,
        })
    }

    /// Number of live entries.
    pub fn live_count(&self) -> usize {
        self.state.live
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
        let id = match existing {
            Some(id) => {
                if !self.update(id, row) {
                    return Ok(id);
                }
                id
            }
            None => self.insert_new(row),
        };
        Ok(match self.maybe_compact() {
            Some(remap) => remap[id.index()],
            None => id,
        })
    }

    /// Removes the entry with this key and all its descendants. Returns how
    /// many entries were removed: 0 for an unknown or already removed key.
    ///
    /// Cost: removing a leaf is O(1) apart from any automatic compaction it
    /// triggers. Removing a non-leaf entry (one with live children) scans
    /// every ID to find the subtree, so it is O(n) in the total number of
    /// entries (live, delta and removed-but-not-compacted), however small
    /// the subtree. The 1.8 writer, which applies FSEvents batches that may
    /// remove many folders, may need per-entry child lists or a
    /// first-child/next-sibling column to make this proportional to the
    /// subtree size.
    pub fn remove(&mut self, key: FileKey) -> usize {
        let Some(id) = self.state.lookup(key) else {
            return 0;
        };
        let parent = self.state.parent(id);
        if !parent.is_none() {
            self.children[parent.index()] -= 1;
        }
        let removed = if self.children[id.index()] == 0 {
            self.delete_one(id);
            1
        } else {
            let doomed = self.subtree(id);
            for &d in &doomed {
                self.delete_one(EntryId(d));
            }
            doomed.len()
        };
        self.state.live -= removed;
        self.maybe_compact();
        removed
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
    /// column, the key table, the delta, the bitset and the child counts).
    /// Snapshots share most of it.
    pub fn heap_bytes(&self) -> usize {
        self.state.heap_bytes()
            + self.children.capacity() * size_of::<u32>()
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

    /// Replaces the row of live entry `id`. Returns false if nothing
    /// changed.
    fn update(&mut self, id: EntryId, row: Row) -> bool {
        let old = self.state.row(id);
        if *old.name == *row.name
            && old.parent == row.parent
            && old.meta == row.meta
            && old.modified == row.modified
            && old.created == row.created
        {
            return false;
        }
        if old.parent != row.parent {
            if old.parent != u32::MAX {
                self.children[old.parent as usize] -= 1;
            }
            if row.parent != u32::MAX {
                self.children[row.parent as usize] += 1;
            }
        }
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
        true
    }

    fn insert_new(&mut self, row: Row) -> EntryId {
        let id = EntryId(self.state.total() as u32);
        if row.parent != u32::MAX {
            self.children[row.parent as usize] += 1;
        }
        self.children.push(0);
        Arc::make_mut(&mut self.state.deleted).push_clear();
        let delta = Arc::make_mut(&mut self.state.delta);
        delta.name_bytes += row.name.len() + 1;
        delta.keys.insert(row.key, id.0);
        delta.new.push(row);
        self.state.live += 1;
        id
    }

    /// Marks one entry removed (no child-count or live-count bookkeeping).
    ///
    /// A removed base entry keeps any override row, so its dead slot reads
    /// back the entry's last stored values (as [`Snapshot`] documents) rather
    /// than the values from before a rename or move. That also makes a
    /// restore from [`Snapshot::to_parts`], which bakes overrides into the
    /// base, behave exactly like the original on later removals.
    fn delete_one(&mut self, id: EntryId) {
        let i = id.index();
        let base_len = self.state.base.len();
        Arc::make_mut(&mut self.state.deleted).set(i);
        if i >= base_len {
            let delta = Arc::make_mut(&mut self.state.delta);
            let key = delta.new[i - base_len].key;
            delta.keys.remove(&key);
        }
    }

    /// Live IDs of `root` and every descendant, found with one memoized pass
    /// over all IDs.
    fn subtree(&self, root: EntryId) -> Vec<u32> {
        const UNKNOWN: u8 = 0;
        const INSIDE: u8 = 1;
        const OUTSIDE: u8 = 2;
        let total = self.state.total();
        let mut mark = vec![UNKNOWN; total];
        mark[root.index()] = INSIDE;
        let mut chain = Vec::new();
        for i in 0..total {
            if mark[i] != UNKNOWN || self.state.deleted.get(i) {
                continue;
            }
            let mut cur = EntryId(i as u32);
            let verdict = loop {
                match mark[cur.index()] {
                    UNKNOWN => {}
                    m => break m,
                }
                chain.push(cur.index());
                cur = self.state.parent(cur);
                if cur.is_none() {
                    break OUTSIDE;
                }
            };
            for j in chain.drain(..) {
                mark[j] = verdict;
            }
        }
        (0..total)
            .filter(|&i| mark[i] == INSIDE)
            .map(|i| i as u32)
            .collect()
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
            parent: Vec::with_capacity(live),
            meta: Vec::with_capacity(live),
            modified: Vec::with_capacity(live),
            created: Vec::with_capacity(live),
            ext: Vec::with_capacity(live),
            volume: Vec::with_capacity(live),
            file_id: Vec::with_capacity(live),
            table: Vec::new(),
        };
        let mut children = vec![0u32; live];
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
                children[p.index()] += 1;
                p.0
            };
            base.parent.push(new_parent);
            base.meta.push(state.meta(id).0);
            base.modified.push(state.modified(id));
            base.created.push(state.created(id));
            base.ext.push(state.ext(id));
            let key = state.key(id);
            base.volume.push(key.volume.0);
            base.file_id.push(key.file_id);
        }
        base.build_table();
        self.state = State {
            base: Arc::new(base),
            delta: Arc::new(Delta::default()),
            deleted: Arc::new(Bitset::zeros(live)),
            exts: Arc::clone(&self.state.exts),
            live,
        };
        self.children = children;
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
