//! Building an index from a first scan, straight into the base columns.
//!
//! Upserting a whole volume into an empty [`Index`] puts every entry in the
//! delta (a row with its own name string each, plus a key map), then a
//! compaction copies it all into the base: for a moment the entries exist
//! twice. [`IndexBuilder`] appends each entry to the base columns as the
//! scan delivers it, so the first scan holds one copy (run 2.17).
//!
//! Clean-room: written from docs/PLAN.md and this crate's own code only.

use std::sync::Arc;

use fx_model::{Entry, EntryId, FileKey};

use crate::extensions::{ExtId, ExtensionTable, extension_of};
use crate::index::{Index, IndexError};
use crate::meta::PackedMeta;
use crate::names::NameError;
use crate::store::{Base, Bitset, Delta, State, bucket, hash_key};

/// An empty slot of the key table.
const NONE: u32 = u32::MAX;

/// Builds an [`Index`] from entries delivered parent before child, as an
/// enumerator walks a volume. See the module documentation.
#[derive(Debug)]
pub struct IndexBuilder {
    base: Base,
    exts: ExtensionTable,
    /// Open-addressing table of the IDs so far, by key, kept at most three
    /// quarters full; it finds each entry's parent.
    table: Vec<u32>,
    /// Entries whose key was already in the builder.
    duplicates: usize,
}

impl Default for IndexBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl IndexBuilder {
    /// An empty builder.
    pub fn new() -> Self {
        IndexBuilder {
            base: Base::default(),
            exts: ExtensionTable::new(),
            table: vec![NONE; 1024],
            duplicates: 0,
        }
    }

    /// Number of entries appended.
    pub fn len(&self) -> usize {
        self.base.len()
    }

    /// Whether nothing was appended.
    pub fn is_empty(&self) -> bool {
        self.base.len() == 0
    }

    /// Entries skipped because their key was already appended (a file with
    /// several hard links is listed once per link; the first one is kept).
    pub fn duplicates(&self) -> usize {
        self.duplicates
    }

    /// Appends `entry`, whose parent must already be appended (`None` for a
    /// root), and returns its ID. An entry whose key is already in the
    /// builder is skipped and the earlier one's ID returned.
    ///
    /// Errors (builder unchanged): [`IndexError::UnknownParent`],
    /// [`IndexError::InvalidName`], [`IndexError::Full`].
    pub fn push(&mut self, entry: Entry) -> Result<EntryId, IndexError> {
        let key = entry.key;
        if let Some(id) = self.find(key) {
            self.duplicates += 1;
            return Ok(EntryId(id));
        }
        let parent = match entry.parent {
            None => NONE,
            Some(pk) => self
                .find(pk)
                .ok_or(IndexError::UnknownParent { key, parent: pk })?,
        };
        if self.base.len() >= (u32::MAX - 1) as usize {
            return Err(IndexError::Full);
        }
        let id = self.base.len() as u32;
        // The name first: a rejected name leaves every column as it was.
        self.base
            .names
            .push(&entry.name)
            .map_err(|error| match error {
                NameError::Overflow => IndexError::Full,
                error => IndexError::InvalidName { key, error },
            })?;
        let ext = self.intern_ext(&entry.name);
        self.base.parent.push(parent);
        self.base
            .meta
            .push(PackedMeta::new(entry.size, entry.kind, entry.flags).0);
        self.base.modified.push(entry.modified);
        self.base.created.push(entry.created);
        self.base.ext.push(ext);
        self.base.volume.push(key.volume.0);
        self.base.file_id.push(key.file_id);
        if self.base.len() * 4 > self.table.len() * 3 {
            self.grow();
        } else {
            insert_slot(&mut self.table, &self.base, id);
        }
        Ok(EntryId(id))
    }

    /// The index of everything appended: all of it in the base, every
    /// column trimmed to its length, the delta empty.
    pub fn finish(self) -> Index {
        let IndexBuilder {
            mut base,
            exts,
            table,
            ..
        } = self;
        drop(table);
        base.names.shrink_to_fit();
        base.parent.shrink_to_fit();
        base.meta.shrink_to_fit();
        base.modified.shrink_to_fit();
        base.created.shrink_to_fit();
        base.ext.shrink_to_fit();
        base.volume.shrink_to_fit();
        base.file_id.shrink_to_fit();
        base.build_table();
        let live = base.len();
        Index::from_state(State {
            base: Arc::new(base),
            delta: Arc::new(Delta::default()),
            deleted: Arc::new(Bitset::zeros(live)),
            exts: Arc::new(exts),
            live,
        })
    }

    /// The ID appended with `key`.
    fn find(&self, key: FileKey) -> Option<u32> {
        let buckets = self.table.len();
        let mut b = bucket(hash_key(key), buckets);
        loop {
            let id = self.table[b];
            if id == NONE {
                return None;
            }
            let i = id as usize;
            if self.base.file_id[i] == key.file_id && self.base.volume[i] == key.volume.0 {
                return Some(id);
            }
            b += 1;
            if b == buckets {
                b = 0;
            }
        }
    }

    /// Doubles the key table.
    fn grow(&mut self) {
        let mut table = vec![NONE; self.table.len() * 2];
        for id in 0..self.base.len() as u32 {
            insert_slot(&mut table, &self.base, id);
        }
        self.table = table;
    }

    /// The name's extension, as [`Index`] interns it.
    fn intern_ext(&mut self, name: &str) -> ExtId {
        let Some(ext) = extension_of(name) else {
            return ExtId::NONE;
        };
        if let Some(id) = self.exts.lookup(ext) {
            return id;
        }
        if self.exts.is_full() {
            return ExtId::OVERFLOW;
        }
        self.exts.intern(name)
    }
}

/// Puts base entry `id` in the first free slot from its key's bucket.
fn insert_slot(table: &mut [u32], base: &Base, id: u32) {
    let buckets = table.len();
    let mut b = bucket(hash_key(base.key(id as usize)), buckets);
    while table[b] != NONE {
        b += 1;
        if b == buckets {
            b = 0;
        }
    }
    table[b] = id;
}
