//! Bulk export and restore of an index as dense columns, so a store can
//! persist an index without replaying per-entry upserts.
//!
//! [`Snapshot::to_parts`](crate::Snapshot::to_parts) materializes the base,
//! the delta and the deletion bitset into one [`IndexParts`];
//! [`Index::from_parts`](crate::Index::from_parts) validates the columns and
//! rebuilds an index with the same entry IDs (dead slots included), so sort
//! orders saved next to the columns stay valid after a reload.
//!
//! Clean-room: written from docs/PLAN.md ("Data layout", "Updates and
//! concurrency") and the in-repo fx-index code only.

use std::fmt;
use std::ops::Range;
use std::sync::Arc;

use bytemuck::Pod;
use fx_model::EntryId;

use crate::column::{Backing, Column};
use crate::extensions::{ExtId, ExtensionTable};
use crate::names::NameBuffer;
use crate::store::{Base, Bitset, Delta, State};

/// Bits of a [`PackedMeta`](crate::PackedMeta) that are never set: the reserved bits 62..=63.
const META_RESERVED: u64 = 0b11 << 62;
/// The unused kind value 3 in bits 60..=61.
const META_KIND_UNUSED: u64 = 0b11 << 60;

/// An index as owned dense columns of plain integers and bytes.
///
/// Every per-entry column has one element per entry ID `0..id_bound`, live
/// or removed, so entry IDs survive an export and restore unchanged. All
/// columns are plain `Vec`s of POD types so another crate can cast them to
/// and from bytes (for example with `bytemuck`) without copying.
///
/// Made by [`Snapshot::to_parts`](crate::Snapshot::to_parts) and consumed
/// by [`Index::from_parts`](crate::Index::from_parts), which validates
/// every column.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexParts {
    /// Every entry's name as UTF-8, joined by single NUL bytes with no
    /// trailing NUL (the [`NameBuffer`] layout).
    pub name_bytes: Vec<u8>,
    /// Start offset of each entry's name in [`name_bytes`](Self::name_bytes);
    /// one per entry.
    pub name_offsets: Vec<u32>,
    /// Parent entry ID per entry; `u32::MAX` ([`EntryId::NONE`]) for a root.
    pub parents: Vec<u32>,
    /// `FileKey::volume` per entry.
    pub volumes: Vec<u32>,
    /// `FileKey::file_id` per entry.
    pub file_ids: Vec<u64>,
    /// Packed size, kind and flags per entry (see
    /// [`PackedMeta`](crate::PackedMeta)).
    pub meta: Vec<u64>,
    /// Last modification time per entry, seconds since the Unix epoch.
    pub modified: Vec<i64>,
    /// Creation time per entry, seconds since the Unix epoch.
    pub created: Vec<i64>,
    /// [`ExtId`] per entry, as its raw `u16`.
    pub ext_ids: Vec<u16>,
    /// The extension table's lowercased extensions in ID order, joined by
    /// single NUL bytes (see [`ExtensionTable::to_raw_parts`]).
    pub ext_bytes: Vec<u8>,
    /// Start offset of each extension in [`ext_bytes`](Self::ext_bytes);
    /// offset `i` belongs to [`ExtId`] `i + 1`.
    pub ext_offsets: Vec<u32>,
    /// Live bitmap: bit `i % 64` of word `i / 64` is set when entry `i` is
    /// live. Needs at least `id_bound.div_ceil(64)` words; bits at or past
    /// `id_bound` and any extra words are ignored on restore.
    pub live: Vec<u64>,
}

impl IndexParts {
    /// Number of entry IDs the parts describe (live or removed): the length
    /// of the per-entry columns.
    pub fn id_bound(&self) -> usize {
        self.parents.len()
    }

    /// Whether entry `i` is marked live in [`live`](Self::live). `false`
    /// for IDs the bitmap does not cover.
    pub fn is_live(&self, i: usize) -> bool {
        self.live
            .get(i / 64)
            .is_some_and(|w| (w >> (i % 64)) & 1 == 1)
    }
}

/// Why [`Index::from_parts`](crate::Index::from_parts) rejected a set of
/// columns. Carried by [`IndexError::InvalidParts`](crate::IndexError::InvalidParts).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartsError {
    /// A per-entry column's length differs from the `parents` column.
    ColumnLength {
        /// Name of the column.
        column: &'static str,
        /// Length of `parents` (the entry count).
        expected: usize,
        /// Length of the column.
        found: usize,
    },
    /// More entries than the index can address with `u32` IDs.
    TooManyEntries {
        /// The entry count.
        count: usize,
    },
    /// The live bitmap has fewer words than the entry count needs.
    LiveBitmapTooShort {
        /// Words needed, `entries.div_ceil(64)`.
        needed: usize,
        /// Words present.
        found: usize,
    },
    /// The name bytes and offsets do not form a valid name buffer.
    NameBuffer {
        /// What is wrong.
        reason: &'static str,
    },
    /// The extension bytes and offsets do not form a valid table.
    ExtensionTable {
        /// What is wrong.
        reason: &'static str,
    },
    /// A parent ID is out of range, or a live entry's parent is removed.
    BadParent {
        /// The entry.
        id: u32,
        /// Its parent ID.
        parent: u32,
    },
    /// Following parent pointers from a live entry leads back to it.
    ParentCycle {
        /// An entry on the cycle.
        id: u32,
    },
    /// A packed meta value uses reserved bits or the unused kind value.
    BadMeta {
        /// The entry.
        id: u32,
        /// The packed value.
        meta: u64,
    },
    /// An extension ID is neither `NONE`, `OVERFLOW` nor in the table.
    BadExtId {
        /// The entry.
        id: u32,
        /// The extension ID.
        ext: u16,
    },
    /// A shared column's byte range is out of bounds or not a whole number
    /// of values ([`Index::from_shared`](crate::Index::from_shared)).
    SharedColumn {
        /// Name of the column.
        column: &'static str,
    },
    /// Two live entries share a `FileKey`.
    DuplicateKey {
        /// The earlier entry.
        first: u32,
        /// The later entry.
        second: u32,
    },
}

impl fmt::Display for PartsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PartsError::ColumnLength {
                column,
                expected,
                found,
            } => write!(
                f,
                "column {column} has {found} elements, expected {expected}"
            ),
            PartsError::TooManyEntries { count } => {
                write!(f, "{count} entries exceed the u32 entry ID space")
            }
            PartsError::LiveBitmapTooShort { needed, found } => {
                write!(f, "live bitmap has {found} words, needs {needed}")
            }
            PartsError::NameBuffer { reason } => write!(f, "invalid name buffer: {reason}"),
            PartsError::ExtensionTable { reason } => {
                write!(f, "invalid extension table: {reason}")
            }
            PartsError::BadParent { id, parent } => {
                write!(f, "entry {id} has an invalid parent {parent}")
            }
            PartsError::ParentCycle { id } => write!(f, "entry {id} is on a parent cycle"),
            PartsError::BadMeta { id, meta } => {
                write!(f, "entry {id} has invalid packed metadata {meta:#018x}")
            }
            PartsError::BadExtId { id, ext } => {
                write!(f, "entry {id} has extension ID {ext}, not in the table")
            }
            PartsError::SharedColumn { column } => {
                write!(
                    f,
                    "shared column {column} is out of bounds or not whole values"
                )
            }
            PartsError::DuplicateKey { first, second } => {
                write!(f, "live entries {first} and {second} share a file key")
            }
        }
    }
}

impl std::error::Error for PartsError {}

/// Materializes `state` (base, delta overrides, new rows and deletions)
/// into dense columns indexed by entry ID.
pub(crate) fn export(state: &State) -> IndexParts {
    let base = &state.base;
    let delta = &state.delta;
    let total = state.total();

    // Start from copies of the base columns, then patch overrides and
    // append the new rows.
    let mut parents = base.parent.to_vec();
    let mut volumes = base.volume.to_vec();
    let mut file_ids = base.file_id.to_vec();
    let mut meta = base.meta.to_vec();
    let mut modified = base.modified.to_vec();
    let mut created = base.created.to_vec();
    let mut ext_ids = base.ext.to_vec();
    for (&id, row) in &delta.overrides {
        let i = id as usize;
        parents[i] = row.parent;
        volumes[i] = row.key.volume.0;
        file_ids[i] = row.key.file_id;
        meta[i] = row.meta.0;
        modified[i] = row.modified;
        created[i] = row.created;
        ext_ids[i] = row.ext.0;
    }
    let extra = delta.new.len();
    for row in &delta.new {
        parents.push(row.parent);
        volumes.push(row.key.volume.0);
        file_ids.push(row.key.file_id);
        meta.push(row.meta.0);
        modified.push(row.modified);
        created.push(row.created);
        ext_ids.push(row.ext.0);
    }

    let (name_bytes, name_offsets) = if delta.overrides.is_empty() {
        let mut bytes = base.names.bytes().to_vec();
        let mut offsets = base.names.offsets().to_vec();
        let more: usize = delta.new.iter().map(|r| r.name.len() + 1).sum();
        bytes.reserve_exact(more);
        offsets.reserve_exact(extra);
        for row in &delta.new {
            push_name(&mut bytes, &mut offsets, &row.name);
        }
        (bytes, offsets)
    } else {
        let mut bytes = Vec::with_capacity(base.names.bytes().len() + delta.name_bytes);
        let mut offsets = Vec::with_capacity(total);
        for i in 0..total {
            push_name(&mut bytes, &mut offsets, state.name(EntryId(i as u32)));
        }
        (bytes, offsets)
    };

    let words = total.div_ceil(64);
    let mut live: Vec<u64> = state.deleted.words.iter().take(words).map(|w| !w).collect();
    if total % 64 != 0 {
        if let Some(last) = live.last_mut() {
            *last &= (1u64 << (total % 64)) - 1;
        }
    }

    let (ext_bytes, ext_offsets) = state.exts.to_raw_parts();
    IndexParts {
        name_bytes,
        name_offsets,
        parents,
        volumes,
        file_ids,
        meta,
        modified,
        created,
        ext_ids,
        ext_bytes,
        ext_offsets,
        live,
    }
}

/// Appends one name in the NUL-separated layout. The index keeps the whole
/// buffer within the u32 offset space, so the cast does not truncate.
fn push_name(bytes: &mut Vec<u8>, offsets: &mut Vec<u32>, name: &str) {
    if !offsets.is_empty() {
        bytes.push(0);
    }
    offsets.push(bytes.len() as u32);
    bytes.extend_from_slice(name.as_bytes());
}

/// Validates `parts` and builds the index state plus the live child count
/// per entry. Never panics on bad input.
pub(crate) fn restore(parts: IndexParts) -> Result<State, PartsError> {
    let n = parts.parents.len();
    check_lengths(
        n,
        [
            ("name_offsets", parts.name_offsets.len()),
            ("volumes", parts.volumes.len()),
            ("file_ids", parts.file_ids.len()),
            ("meta", parts.meta.len()),
            ("modified", parts.modified.len()),
            ("created", parts.created.len()),
            ("ext_ids", parts.ext_ids.len()),
            ("parents", n),
        ],
    )?;
    let deleted = live_to_deleted(&parts.live, n)?;
    let exts = ExtensionTable::from_raw_parts(parts.ext_bytes, parts.ext_offsets)?;
    check_rows(
        &parts.meta,
        &parts.ext_ids,
        exts.len(),
        &parts.parents,
        &deleted,
    )?;
    let names = NameBuffer::from_raw_parts(parts.name_bytes, parts.name_offsets)?;
    let base = Base {
        names,
        parent: parts.parents.into(),
        meta: parts.meta.into(),
        modified: parts.modified.into(),
        created: parts.created.into(),
        ext: parts.ext_ids.into(),
        volume: parts.volumes.into(),
        file_id: parts.file_ids.into(),
        table: Vec::new(),
    };
    finish(base, deleted, exts)
}

/// An index's columns inside a [`Backing`], such as a saved index's memory
/// map, as byte ranges, for [`Index::from_shared`](crate::Index::from_shared).
/// Every per-entry column has one value per entry ID, as in [`IndexParts`].
/// The extension table and the live bitmap are small, so they come owned.
#[derive(Debug, Clone)]
pub struct SharedParts {
    /// The buffer every range below points into.
    pub backing: Arc<dyn Backing>,
    /// Names joined by NUL, no trailing separator.
    pub name_bytes: Range<usize>,
    /// `u32` start offset of each name.
    pub name_offsets: Range<usize>,
    /// `u32` parent ID per entry.
    pub parents: Range<usize>,
    /// `u32` volume per entry.
    pub volumes: Range<usize>,
    /// `u64` file ID per entry.
    pub file_ids: Range<usize>,
    /// `u64` packed size, kind and flags per entry.
    pub meta: Range<usize>,
    /// `i64` modified time per entry.
    pub modified: Range<usize>,
    /// `i64` created time per entry.
    pub created: Range<usize>,
    /// `u16` extension ID per entry.
    pub ext_ids: Range<usize>,
    /// The extension table's bytes, as in [`IndexParts::ext_bytes`].
    pub ext_bytes: Vec<u8>,
    /// The extension table's offsets, as in [`IndexParts::ext_offsets`].
    pub ext_offsets: Vec<u32>,
    /// Live bitmap, as in [`IndexParts::live`].
    pub live: Vec<u64>,
}

/// [`restore`] for columns that stay in their backing: the same checks,
/// without copying the columns.
pub(crate) fn restore_shared(parts: SharedParts) -> Result<State, PartsError> {
    fn column<T: Pod>(
        backing: &Arc<dyn Backing>,
        range: Range<usize>,
        name: &'static str,
    ) -> Result<Column<T>, PartsError> {
        Column::shared(backing, range).ok_or(PartsError::SharedColumn { column: name })
    }
    let b = &parts.backing;
    let parent: Column<u32> = column(b, parts.parents, "parents")?;
    let volume: Column<u32> = column(b, parts.volumes, "volumes")?;
    let file_id: Column<u64> = column(b, parts.file_ids, "file_ids")?;
    let meta: Column<u64> = column(b, parts.meta, "meta")?;
    let modified: Column<i64> = column(b, parts.modified, "modified")?;
    let created: Column<i64> = column(b, parts.created, "created")?;
    let ext: Column<u16> = column(b, parts.ext_ids, "ext_ids")?;
    let names = NameBuffer::from_shared(b, parts.name_bytes, parts.name_offsets)?;
    let n = parent.len();
    check_lengths(
        n,
        [
            ("name_offsets", names.len()),
            ("volumes", volume.len()),
            ("file_ids", file_id.len()),
            ("meta", meta.len()),
            ("modified", modified.len()),
            ("created", created.len()),
            ("ext_ids", ext.len()),
            ("parents", n),
        ],
    )?;
    let deleted = live_to_deleted(&parts.live, n)?;
    let exts = ExtensionTable::from_raw_parts(parts.ext_bytes, parts.ext_offsets)?;
    check_rows(&meta, &ext, exts.len(), &parent, &deleted)?;
    let base = Base {
        names,
        parent,
        meta,
        modified,
        created,
        ext,
        volume,
        file_id,
        table: Vec::new(),
    };
    finish(base, deleted, exts)
}

/// Every per-entry column has `n` values, and `n` fits the index's IDs.
fn check_lengths(n: usize, columns: [(&'static str, usize); 8]) -> Result<(), PartsError> {
    for (column, found) in columns {
        if found != n {
            return Err(PartsError::ColumnLength {
                column,
                expected: n,
                found,
            });
        }
    }
    // Upsert refuses to hand out more than u32::MAX - 1 IDs.
    if n > (u32::MAX - 1) as usize {
        return Err(PartsError::TooManyEntries { count: n });
    }
    Ok(())
}

/// The deletion bitset of `n` entries from the live bitmap.
fn live_to_deleted(live: &[u64], n: usize) -> Result<Bitset, PartsError> {
    let needed = n.div_ceil(64);
    if live.len() < needed {
        return Err(PartsError::LiveBitmapTooShort {
            needed,
            found: live.len(),
        });
    }
    Ok(deleted_bitset(&live[..needed], n))
}

/// Meta values, extension IDs and parents are valid, and live entries'
/// parents are live and acyclic.
fn check_rows(
    meta: &[u64],
    ext_ids: &[u16],
    ext_count: usize,
    parents: &[u32],
    deleted: &Bitset,
) -> Result<(), PartsError> {
    let n = parents.len();
    let is_live = |i: usize| !deleted.get(i);
    for (i, &m) in meta.iter().enumerate() {
        if m & META_RESERVED != 0 || m & META_KIND_UNUSED == META_KIND_UNUSED {
            return Err(PartsError::BadMeta {
                id: i as u32,
                meta: m,
            });
        }
    }
    for (i, &e) in ext_ids.iter().enumerate() {
        let ext = ExtId(e);
        if ext.is_real() && usize::from(e) > ext_count {
            return Err(PartsError::BadExtId {
                id: i as u32,
                ext: e,
            });
        }
    }
    // Parents: in range for every entry; live and acyclic for live ones.
    for (i, &p) in parents.iter().enumerate() {
        if p == u32::MAX {
            continue;
        }
        let pi = p as usize;
        if pi >= n || (is_live(i) && !is_live(pi)) {
            return Err(PartsError::BadParent {
                id: i as u32,
                parent: p,
            });
        }
    }
    check_acyclic(parents, deleted)
}

/// The state over `base`: its key table built, the delta empty.
fn finish(mut base: Base, deleted: Bitset, exts: ExtensionTable) -> Result<State, PartsError> {
    base.build_live_table(&deleted)
        .map_err(|(first, second)| PartsError::DuplicateKey { first, second })?;
    let live = base.len() - deleted.ones;
    Ok(State {
        base: Arc::new(base),
        delta: Arc::new(Delta::default()),
        deleted: Arc::new(deleted),
        exts: Arc::new(exts),
        live,
    })
}

/// The deletion bitset for `len` IDs from live-bitmap words (exactly
/// `len.div_ceil(64)` of them): inverted, with bits past `len` cleared.
fn deleted_bitset(live: &[u64], len: usize) -> Bitset {
    let mut words: Vec<u64> = live.iter().map(|w| !w).collect();
    if len % 64 != 0 {
        if let Some(last) = words.last_mut() {
            *last &= (1u64 << (len % 64)) - 1;
        }
    }
    let ones = words.iter().map(|w| w.count_ones() as usize).sum();
    Bitset { words, len, ones }
}

/// Checks that no live entry is its own ancestor, with one memoized pass.
/// Parents are already known to be in range, and live entries' parents
/// live.
fn check_acyclic(parents: &[u32], deleted: &Bitset) -> Result<(), PartsError> {
    const UNKNOWN: u8 = 0;
    const ON_PATH: u8 = 1;
    const DONE: u8 = 2;
    let n = parents.len();
    let mut mark = vec![UNKNOWN; n];
    let mut chain = Vec::new();
    for i in 0..n {
        if mark[i] != UNKNOWN || deleted.get(i) {
            continue;
        }
        let mut cur = i;
        loop {
            match mark[cur] {
                UNKNOWN => {}
                ON_PATH => return Err(PartsError::ParentCycle { id: cur as u32 }),
                _ => break,
            }
            mark[cur] = ON_PATH;
            chain.push(cur);
            let p = parents[cur];
            if p == u32::MAX {
                break;
            }
            cur = p as usize;
        }
        for j in chain.drain(..) {
            mark[j] = DONE;
        }
    }
    Ok(())
}
