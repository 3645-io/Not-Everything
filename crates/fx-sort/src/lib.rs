//! Result sort orders.
//!
//! Clean-room: written from docs/PLAN.md ("Sorting", "Updates and
//! concurrency") and the public fx-model / fx-index APIs only.
//!
//! The index keeps one pre-sorted [`Order`] per [`SortKey`]: a permutation
//! of the live entry IDs, 4 bytes per entry. Showing results never sorts
//! them; [`emit`] walks an order once, filtered by a query's [`MatchSet`],
//! and returns the total match count plus only the visible window, in either
//! [`Direction`].
//!
//! Each key is a total, deterministic order (see [`SortKey`]): the primary
//! value, then the Name rule (case-folded name, raw name bytes, EntryId).
//! The Path order compares paths component-wise from the root, so every
//! folder's subtree is the contiguous range right after it.
//!
//! Orders read entries through the small [`SortSource`] trait, implemented
//! for [`fx_index::Snapshot`].
//!
//! "Your files first": [`group_subtrees`] reads the entries below a
//! few listed folders off the Path order as a [`MatchSet`], leaving out the
//! subtrees of the [`hidden_entries`] (names starting with `.`, read off the
//! Name order), and [`emit_tiered`] lists the matches in such a set before
//! the others, each part in the chosen order and direction.
//!
//! Changes reach an order in batches: [`Order::apply`] merges a [`Batch`]
//! of removed, upserted and moved IDs with one filter pass, one sort of the
//! batch and one linear merge. For the Path order a folder recorded with
//! [`Batch::moved`] (renamed or reparented) carries its whole subtree; a
//! plain [`Batch::upsert`] (attribute change) re-sorts only the entry.

mod group;
mod key;
mod merge;
mod order;
mod source;

pub use group::{group_subtrees, hidden_entries};
pub use key::{SortKey, cmp_folded, cmp_name_strs, compare_names, compare_paths};
pub use merge::Batch;
pub use order::{Direction, MatchSet, Order, OrderError, Page, emit, emit_tiered};
pub use source::SortSource;
