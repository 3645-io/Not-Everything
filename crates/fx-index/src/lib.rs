//! The in-memory file-name index.
//!
//! This crate holds the column store, the name buffer, the extension table,
//! the delta of pending changes, compaction and read-only snapshots.
//!
//! - [`Index`] is the single-writer store. Base columns are indexed by
//!   [`EntryId`](fx_model::EntryId): name (through a [`NameBuffer`]),
//!   parent, packed size/kind/flags ([`PackedMeta`]), modified and created
//!   times, and [`ExtId`], plus a `FileKey -> EntryId` table.
//! - New, renamed, moved and updated entries go to a small delta; removed
//!   ones set a bit in a deletion bitset. [`Index::compact`] rebuilds a
//!   dense base and returns the ID remap; it also runs automatically when
//!   the delta passes [`Index::compact_fraction`] of the live entries.
//! - [`Snapshot`] is an immutable `Send + Sync` view behind an `Arc`.
//! - [`Snapshot::to_parts`] exports the index as dense [`IndexParts`]
//!   columns with entry IDs preserved, and [`Index::from_parts`] validates
//!   and restores them, so a store can persist an index in bulk.
//!
//! Clean-room: written from docs/PLAN.md only.

#![forbid(unsafe_code)]

mod extensions;
mod index;
pub mod meta;
mod names;
mod parts;
mod snapshot;
mod store;
#[cfg(test)]
mod tests;

pub use extensions::{ExtId, ExtensionTable, extension_of};
pub use index::{Applied, DEFAULT_COMPACT_FRACTION, Index, IndexError};
pub use meta::PackedMeta;
pub use names::{NameBuffer, NameError};
pub use parts::{IndexParts, PartsError};
pub use snapshot::Snapshot;
