//! Internal storage shared by [`Index`](crate::Index) and
//! [`Snapshot`](crate::Snapshot): the immutable base columns, the delta, the
//! deletion bitset and the read accessors over all three.
//!
//! Layout (docs/PLAN.md, "Data layout" and "Updates and concurrency"):
//!
//! - The **base** holds dense columns indexed by `EntryId` `0..base_len`. It
//!   is never mutated once built; only [`compact`](crate::Index::compact)
//!   replaces it. It is shared between the index and its snapshots by `Arc`.
//! - The **delta** holds entries appended since the last compaction (IDs
//!   `base_len..`) and overrides for base entries whose name, parent or
//!   metadata changed. It is behind an `Arc` and copied on write, so a
//!   snapshot keeps the delta it saw.
//! - The **deletion bitset** has one bit per ID (base and delta), set when
//!   the entry is removed. Also `Arc`, copy on write.
//!
//! Clean-room: written from docs/PLAN.md only.

use std::collections::HashMap;
use std::mem::size_of;
use std::sync::Arc;

use fx_model::{EntryId, FileKey, VolumeId};

use crate::extensions::{ExtId, ExtensionTable};
use crate::meta::PackedMeta;
use crate::names::NameBuffer;

const NONE: u32 = u32::MAX;

/// Dense, immutable base columns plus a read-only key table.
#[derive(Debug, Default)]
pub(crate) struct Base {
    pub(crate) names: NameBuffer,
    /// Parent ID per entry; `u32::MAX` (`EntryId::NONE`) for roots.
    pub(crate) parent: Vec<u32>,
    /// Size, kind and flags; see [`PackedMeta`].
    pub(crate) meta: Vec<u64>,
    pub(crate) modified: Vec<i64>,
    pub(crate) created: Vec<i64>,
    pub(crate) ext: Vec<ExtId>,
    /// `FileKey::volume` per entry.
    pub(crate) volume: Vec<u32>,
    /// `FileKey::file_id` per entry.
    pub(crate) file_id: Vec<u64>,
    /// Open-addressing table (linear probing) of base IDs keyed by their
    /// `FileKey`. Built once per compaction and never modified, so it needs
    /// no tombstones: removed entries are filtered by the deletion bitset.
    pub(crate) table: Vec<u32>,
}

/// SplitMix64 finalizer over the key; inode numbers are often sequential.
pub(crate) fn hash_key(key: FileKey) -> u64 {
    let mut z = key.file_id ^ u64::from(key.volume.0).rotate_left(32) ^ 0x9E37_79B9_7F4A_7C15;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Maps a hash onto `0..buckets` without a modulo (multiply-high).
pub(crate) fn bucket(hash: u64, buckets: usize) -> usize {
    ((u128::from(hash) * buckets as u128) >> 64) as usize
}

impl Base {
    pub(crate) fn len(&self) -> usize {
        self.parent.len()
    }

    pub(crate) fn key(&self, i: usize) -> FileKey {
        FileKey::new(VolumeId(self.volume[i]), self.file_id[i])
    }

    /// Builds the key table for the current columns (load factor <= 3/4).
    pub(crate) fn build_table(&mut self) {
        let n = self.len();
        if n == 0 {
            self.table = Vec::new();
            return;
        }
        let buckets = n + n / 3 + 1;
        let mut table = vec![NONE; buckets];
        for i in 0..n {
            let mut b = bucket(hash_key(self.key(i)), buckets);
            while table[b] != NONE {
                b += 1;
                if b == buckets {
                    b = 0;
                }
            }
            table[b] = i as u32;
        }
        self.table = table;
    }

    /// Builds the key table from the live entries only (those whose bit in
    /// `deleted` is clear), for a base restored with dead slots in it.
    ///
    /// Returns `Err((first, second))` if two live entries share a key. With
    /// linear probing and no removals, an earlier entry with the same key
    /// always sits in the probe run between its bucket and the first free
    /// slot, so comparing keys while probing finds every duplicate.
    pub(crate) fn build_live_table(&mut self, deleted: &Bitset) -> Result<(), (u32, u32)> {
        let n = self.len();
        let live = n - deleted.ones;
        if live == 0 {
            self.table = Vec::new();
            return Ok(());
        }
        let buckets = live + live / 3 + 1;
        let mut table = vec![NONE; buckets];
        for i in 0..n {
            if deleted.get(i) {
                continue;
            }
            let key = self.key(i);
            let mut b = bucket(hash_key(key), buckets);
            loop {
                let other = table[b];
                if other == NONE {
                    break;
                }
                if self.key(other as usize) == key {
                    return Err((other, i as u32));
                }
                b += 1;
                if b == buckets {
                    b = 0;
                }
            }
            table[b] = i as u32;
        }
        self.table = table;
        Ok(())
    }

    /// Base ID holding `key`. Tables built by [`Base::build_table`] hold
    /// every base entry, deleted or not; those built by
    /// [`Base::build_live_table`] hold only live ones. Callers filter
    /// deleted IDs either way.
    pub(crate) fn lookup(&self, key: FileKey) -> Option<u32> {
        let buckets = self.table.len();
        if buckets == 0 {
            return None;
        }
        let mut b = bucket(hash_key(key), buckets);
        loop {
            let id = self.table[b];
            if id == NONE {
                return None;
            }
            let i = id as usize;
            if self.file_id[i] == key.file_id && self.volume[i] == key.volume.0 {
                return Some(id);
            }
            b += 1;
            if b == buckets {
                b = 0;
            }
        }
    }

    /// Allocated bytes of the parent column.
    fn parent_bytes(&self) -> usize {
        self.parent.capacity() * size_of::<u32>()
    }

    /// Allocated bytes of the packed size/kind/flags column.
    fn meta_bytes(&self) -> usize {
        self.meta.capacity() * size_of::<u64>()
    }

    /// Allocated bytes of the two time columns.
    fn times_bytes(&self) -> usize {
        self.modified.capacity() * size_of::<i64>() + self.created.capacity() * size_of::<i64>()
    }

    /// Allocated bytes of the extension ID column.
    fn ext_bytes(&self) -> usize {
        self.ext.capacity() * size_of::<ExtId>()
    }

    /// Allocated bytes of the file key columns (volume and file ID).
    fn key_bytes(&self) -> usize {
        self.volume.capacity() * size_of::<u32>() + self.file_id.capacity() * size_of::<u64>()
    }

    /// Allocated bytes of the key lookup table.
    fn table_bytes(&self) -> usize {
        self.table.capacity() * size_of::<u32>()
    }
}

/// All columns of one entry, as kept in the delta.
#[derive(Debug, Clone)]
pub(crate) struct Row {
    pub(crate) key: FileKey,
    pub(crate) name: Box<str>,
    pub(crate) parent: u32,
    pub(crate) meta: PackedMeta,
    pub(crate) modified: i64,
    pub(crate) created: i64,
    pub(crate) ext: ExtId,
}

/// Changes since the last compaction.
#[derive(Debug, Clone, Default)]
pub(crate) struct Delta {
    /// Entries appended since the last compaction: `new[i]` has ID
    /// `base_len + i`. Removed ones stay until compaction (bit set).
    pub(crate) new: Vec<Row>,
    /// Replacement rows for base entries that were renamed, moved or
    /// updated. Kept when the entry is later removed, so its dead slot
    /// reads back its last stored values.
    pub(crate) overrides: HashMap<u32, Row>,
    /// Keys of the live entries in `new`.
    pub(crate) keys: HashMap<FileKey, u32>,
    /// Bytes the delta's names will take in the name buffer (name plus
    /// separator), for the u32 offset-space check.
    pub(crate) name_bytes: usize,
}

impl Delta {
    pub(crate) fn len(&self) -> usize {
        self.new.len() + self.overrides.len()
    }

    /// Bytes of the delta's rows, override map and key map, without the
    /// names the rows own (see [`name_bytes`](Delta::name_bytes)).
    pub(crate) fn row_bytes(&self) -> usize {
        let row = size_of::<Row>();
        self.new.capacity() * row
            + self.overrides.capacity() * (size_of::<u32>() + row + 1)
            + self.keys.capacity() * (size_of::<(FileKey, u32)>() + 1)
    }
}

/// Labels of [`State::heap_columns`], in report order.
pub(crate) mod heap_labels {
    pub(crate) const NAMES: &str = "names";
    pub(crate) const NAME_OFFSETS: &str = "name_offsets";
    pub(crate) const PARENTS: &str = "parents";
    pub(crate) const META: &str = "size_kind_flags";
    pub(crate) const TIMES: &str = "times";
    pub(crate) const EXT_IDS: &str = "ext_ids";
    pub(crate) const EXT_TABLE: &str = "ext_table";
    pub(crate) const FILE_KEYS: &str = "file_keys";
    pub(crate) const KEY_TABLE: &str = "key_table";
    pub(crate) const DELETED: &str = "deleted_bitset";
    pub(crate) const DELTA: &str = "delta_overlay";
}

/// One bit per entry ID; a set bit means removed.
#[derive(Debug, Clone, Default)]
pub(crate) struct Bitset {
    pub(crate) words: Vec<u64>,
    /// Number of IDs covered.
    pub(crate) len: usize,
    /// Number of set bits.
    pub(crate) ones: usize,
}

impl Bitset {
    pub(crate) fn zeros(len: usize) -> Self {
        Bitset {
            words: vec![0; len.div_ceil(64)],
            len,
            ones: 0,
        }
    }

    pub(crate) fn get(&self, i: usize) -> bool {
        self.words[i / 64] & (1 << (i % 64)) != 0
    }

    /// Sets bit `i`; returns whether it was clear.
    pub(crate) fn set(&mut self, i: usize) -> bool {
        let word = &mut self.words[i / 64];
        let mask = 1 << (i % 64);
        let was_clear = *word & mask == 0;
        *word |= mask;
        self.ones += usize::from(was_clear);
        was_clear
    }

    /// Appends a clear bit.
    pub(crate) fn push_clear(&mut self) {
        if self.len % 64 == 0 {
            self.words.push(0);
        }
        self.len += 1;
    }
}

/// Everything a reader needs, cheap to clone (four `Arc`s and a count).
#[derive(Debug, Clone, Default)]
pub(crate) struct State {
    pub(crate) base: Arc<Base>,
    pub(crate) delta: Arc<Delta>,
    pub(crate) deleted: Arc<Bitset>,
    pub(crate) exts: Arc<ExtensionTable>,
    pub(crate) live: usize,
}

/// Borrowed view of one entry's columns, wherever they live.
pub(crate) enum Slot<'a> {
    Base(usize),
    Row(&'a Row),
}

impl State {
    /// Number of IDs ever handed out since the last compaction.
    pub(crate) fn total(&self) -> usize {
        self.base.len() + self.delta.new.len()
    }

    pub(crate) fn is_live(&self, id: u32) -> bool {
        (id as usize) < self.total() && !self.deleted.get(id as usize)
    }

    /// Where entry `id`'s columns are.
    ///
    /// # Panics
    ///
    /// Panics if `id` is out of range.
    pub(crate) fn slot(&self, id: EntryId) -> Slot<'_> {
        let i = id.index();
        let base_len = self.base.len();
        if i < base_len {
            match self.delta.overrides.get(&id.0) {
                Some(row) => Slot::Row(row),
                None => Slot::Base(i),
            }
        } else {
            match self.delta.new.get(i - base_len) {
                Some(row) => Slot::Row(row),
                None => panic!("entry id {} out of range ({} ids)", id.0, self.total()),
            }
        }
    }

    pub(crate) fn lookup(&self, key: FileKey) -> Option<EntryId> {
        if let Some(&id) = self.delta.keys.get(&key) {
            return Some(EntryId(id));
        }
        self.base
            .lookup(key)
            .filter(|&id| !self.deleted.get(id as usize))
            .map(EntryId)
    }

    pub(crate) fn name(&self, id: EntryId) -> &str {
        match self.slot(id) {
            Slot::Base(i) => self.base.names.get(i as u32),
            Slot::Row(r) => &r.name,
        }
    }

    pub(crate) fn parent(&self, id: EntryId) -> EntryId {
        EntryId(match self.slot(id) {
            Slot::Base(i) => self.base.parent[i],
            Slot::Row(r) => r.parent,
        })
    }

    pub(crate) fn meta(&self, id: EntryId) -> PackedMeta {
        match self.slot(id) {
            Slot::Base(i) => PackedMeta(self.base.meta[i]),
            Slot::Row(r) => r.meta,
        }
    }

    pub(crate) fn modified(&self, id: EntryId) -> i64 {
        match self.slot(id) {
            Slot::Base(i) => self.base.modified[i],
            Slot::Row(r) => r.modified,
        }
    }

    pub(crate) fn created(&self, id: EntryId) -> i64 {
        match self.slot(id) {
            Slot::Base(i) => self.base.created[i],
            Slot::Row(r) => r.created,
        }
    }

    pub(crate) fn ext(&self, id: EntryId) -> ExtId {
        match self.slot(id) {
            Slot::Base(i) => self.base.ext[i],
            Slot::Row(r) => r.ext,
        }
    }

    pub(crate) fn key(&self, id: EntryId) -> FileKey {
        match self.slot(id) {
            Slot::Base(i) => self.base.key(i),
            Slot::Row(r) => r.key,
        }
    }

    /// The full row of entry `id`, copied out.
    pub(crate) fn row(&self, id: EntryId) -> Row {
        match self.slot(id) {
            Slot::Base(i) => Row {
                key: self.base.key(i),
                name: self.base.names.get(i as u32).into(),
                parent: self.base.parent[i],
                meta: PackedMeta(self.base.meta[i]),
                modified: self.base.modified[i],
                created: self.base.created[i],
                ext: self.base.ext[i],
            },
            Slot::Row(r) => r.clone(),
        }
    }

    /// Names from the root down to `id`, joined with `'/'`.
    pub(crate) fn path(&self, id: EntryId) -> String {
        let mut chain = Vec::new();
        let mut cur = id;
        // A live entry's ancestors are live and acyclic, so this ends; the
        // bound guards against misuse with a stale ID.
        while !cur.is_none() && chain.len() <= self.total() {
            chain.push(self.name(cur));
            cur = self.parent(cur);
        }
        let len = chain.iter().map(|s| s.len() + 1).sum::<usize>();
        let mut path = String::with_capacity(len);
        for (i, name) in chain.iter().rev().enumerate() {
            if i > 0 {
                path.push('/');
            }
            path.push_str(name);
        }
        path
    }

    pub(crate) fn live_ids(&self) -> impl Iterator<Item = EntryId> + '_ {
        let total = self.total();
        let words = &self.deleted.words;
        words.iter().enumerate().flat_map(move |(w, &bits)| {
            let start = w * 64;
            let end = (start + 64).min(total);
            (start..end)
                .filter(move |i| bits & (1 << (i % 64)) == 0)
                .map(|i| EntryId(i as u32))
        })
    }

    /// Heap bytes of the shared state, one `(label, bytes)` pair per
    /// column: allocated capacity of every base column and the key table,
    /// the delta overlay (rows and maps; the names its rows own are counted
    /// under `names`), the deletion bitset and the extension table.
    pub(crate) fn heap_columns(&self) -> Vec<(&'static str, usize)> {
        use heap_labels as l;
        let base = &self.base;
        vec![
            (
                l::NAMES,
                base.names.data_heap_bytes() + self.delta.name_bytes,
            ),
            (l::NAME_OFFSETS, base.names.offsets_heap_bytes()),
            (l::PARENTS, base.parent_bytes()),
            (l::META, base.meta_bytes()),
            (l::TIMES, base.times_bytes()),
            (l::EXT_IDS, base.ext_bytes()),
            // Each extension is held twice (list and lookup map): a boxed
            // string plus its ID and bookkeeping.
            (
                l::EXT_TABLE,
                self.exts.len() * 2 * (size_of::<Box<str>>() + 8),
            ),
            (l::FILE_KEYS, base.key_bytes()),
            (l::KEY_TABLE, base.table_bytes()),
            (l::DELETED, self.deleted.words.capacity() * size_of::<u64>()),
            (l::DELTA, self.delta.row_bytes()),
        ]
    }

    /// The sum of [`heap_columns`](State::heap_columns).
    pub(crate) fn heap_bytes(&self) -> usize {
        self.heap_columns().iter().map(|&(_, b)| b).sum()
    }
}
