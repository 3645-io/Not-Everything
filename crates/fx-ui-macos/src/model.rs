//! The virtual row model behind the spike's results table.
//!
//! A [`RowModel`] wraps a synthetic [`fx_bench::Tree`]: row `i` is entry `i`
//! of `generate_tree(rows, seed, TreeShape::default())`. The table asks for
//! cell text only for the rows on screen, so nothing per row is formatted up
//! front; 2M rows cost the tree's name buffer and parent column only.
//!
//! - Name is the entry's name, borrowed from the tree.
//! - Path is the parent folder's path, built on demand by following parent
//!   pointers up to the root (empty for the root itself).
//! - Size and Modified are synthetic, derived from the seed and the row
//!   index with a SplitMix64-style hash, so they are deterministic per seed
//!   and need no storage. Directories have size 0 and show "—".
//! - Cells are written with [`crate::formats`]: sizes in 1,024-based
//!   units, and dates as "14 Mar 2026 09:12" in UTC, since the synthetic
//!   times are UTC and the spike stays the same on every Mac.
//!
//! [`RowModel::write_cell`] fills a caller-owned buffer so per-frame calls
//! can reuse one `String`; [`RowModel::cell_text`] borrows names and
//! allocates only for the other columns. Out-of-range rows give `None` (or
//! `false`), never a panic.

#![forbid(unsafe_code)]

use std::borrow::Cow;

use fx_bench::{Tree, TreeShape, generate_tree};

use crate::formats::{UTC, write_date, write_size_cell};

/// Salt mixed into the seed for synthetic sizes.
const SIZE_SALT: u64 = 0x7369_7a65_7369_7a65;
/// Salt mixed into the seed for synthetic modification times.
const TIME_SALT: u64 = 0x6d74_696d_656d_7469;
/// Earliest synthetic modification time: 2015-01-01 00:00 UTC.
const MODIFIED_FROM: i64 = 1_420_070_400;
/// Span of synthetic modification times: ten years up to 2025-01-01 UTC.
const MODIFIED_SPAN: u64 = 1_735_689_600 - 1_420_070_400;
/// Largest synthetic file size, as a power of two (16 GiB).
const MAX_SIZE_BITS: u64 = 34;

/// The results table's columns, defined in [`crate::columns`] and
/// re-exported here for the spike's code.
pub use crate::columns::Column;

/// Read-only rows over a synthetic file tree.
#[derive(Clone, Debug)]
pub struct RowModel {
    tree: Tree,
    seed: u64,
}

impl RowModel {
    /// Generates a model of `rows` rows from `seed`: row `i` is entry `i`
    /// of `fx_bench::generate_tree(rows, seed, TreeShape::default())`.
    ///
    /// # Panics
    ///
    /// Panics if `rows` does not fit the tree's `u32` indices (`rows` must
    /// be below `u32::MAX`); `spike_args` rejects such counts up front.
    pub fn new(rows: usize, seed: u64) -> RowModel {
        RowModel {
            tree: generate_tree(rows, seed, TreeShape::default()),
            seed,
        }
    }

    /// Number of rows.
    pub fn row_count(&self) -> usize {
        self.tree.len()
    }

    /// The seed the model was generated from.
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// The underlying synthetic tree.
    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    /// Whether `row` is a directory, or `None` if out of range.
    pub fn is_dir(&self, row: usize) -> Option<bool> {
        self.tree.is_dir.get(row).copied()
    }

    /// Name of `row`, or `None` if out of range.
    pub fn name(&self, row: usize) -> Option<&str> {
        (row < self.row_count()).then(|| self.tree.names.get(row))
    }

    /// Synthetic size of `row` in bytes (0 for directories), or `None` if
    /// out of range. Deterministic per seed and row.
    pub fn size_bytes(&self, row: usize) -> Option<u64> {
        let is_dir = self.is_dir(row)?;
        if is_dir {
            return Some(0);
        }
        let h = mix(self.seed ^ SIZE_SALT, row as u64);
        // Roughly log-uniform: pick a bit length, then a value below it.
        let bits = h % (MAX_SIZE_BITS + 1);
        let mask = (1u64 << bits) - 1;
        Some((h >> 8) & mask)
    }

    /// Synthetic modification time of `row` in Unix seconds (UTC), between
    /// 2015 and 2025, or `None` if out of range. Deterministic per seed and
    /// row.
    pub fn modified_unix(&self, row: usize) -> Option<i64> {
        if row >= self.row_count() {
            return None;
        }
        let h = mix(self.seed ^ TIME_SALT, row as u64);
        Some(MODIFIED_FROM + (h % MODIFIED_SPAN) as i64)
    }

    /// Text of one cell, or `None` if `row` is out of range. Names are
    /// borrowed; the other columns are formatted into a new `String`.
    pub fn cell_text(&self, row: usize, col: Column) -> Option<Cow<'_, str>> {
        if row >= self.row_count() {
            return None;
        }
        match col {
            Column::Name => self.name(row).map(Cow::Borrowed),
            _ => {
                let mut buf = String::new();
                self.write_cell(row, col, &mut buf);
                Some(Cow::Owned(buf))
            }
        }
    }

    /// Clears `buf` and writes the text of one cell into it. Returns
    /// `false`, leaving `buf` empty, if `row` is out of range. Reusing one
    /// buffer avoids allocating per cell once it has grown.
    pub fn write_cell(&self, row: usize, col: Column, buf: &mut String) -> bool {
        buf.clear();
        if row >= self.row_count() {
            return false;
        }
        match col {
            Column::Name => buf.push_str(self.tree.names.get(row)),
            Column::Path => {
                let parent = self.tree.parent[row];
                if parent != u32::MAX {
                    self.write_path(parent as usize, buf);
                }
            }
            Column::Size => {
                let is_dir = self.is_dir(row).unwrap_or(false);
                write_size_cell(self.size_bytes(row), is_dir, buf);
            }
            Column::Modified => {
                if let Some(t) = self.modified_unix(row) {
                    write_date(t, &UTC, buf);
                }
            }
        }
        true
    }

    /// Appends the full relative path of entry `i` (names from the root
    /// down, joined with `/`). Recursion depth is the entry's depth, which
    /// the default tree shape bounds at 32.
    fn write_path(&self, i: usize, buf: &mut String) {
        let parent = self.tree.parent[i];
        if parent != u32::MAX {
            self.write_path(parent as usize, buf);
            buf.push('/');
        }
        buf.push_str(self.tree.names.get(i));
    }

    /// Indices of the rows whose name contains `needle`, ignoring ASCII
    /// case, in ascending order. An empty needle matches every row.
    ///
    /// This is a plain linear scan for the spike's search field only; the
    /// real search engine lives elsewhere.
    pub fn filter(&self, needle: &str) -> Vec<usize> {
        if needle.is_empty() {
            return (0..self.row_count()).collect();
        }
        let needle = needle.as_bytes();
        self.tree
            .names
            .iter()
            .enumerate()
            .filter(|(_, name)| contains_ignore_ascii_case(name.as_bytes(), needle))
            .map(|(i, _)| i)
            .collect()
    }
}

/// Whether `hay` contains `needle`, comparing ASCII letters case-blind and
/// all other bytes exactly. On UTF-8 input this matches
/// `hay.to_ascii_lowercase().contains(&needle.to_ascii_lowercase())`.
fn contains_ignore_ascii_case(hay: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > hay.len() {
        return false;
    }
    hay.windows(needle.len())
        .any(|w| w.eq_ignore_ascii_case(needle))
}

/// SplitMix64 finalizer over a seed and an index.
fn mix(seed: u64, i: u64) -> u64 {
    let mut z = seed.wrapping_add(i.wrapping_add(1).wrapping_mul(0x9e37_79b9_7f4a_7c15));
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_matches_ascii_case_blind() {
        assert!(contains_ignore_ascii_case(b"Hello.TXT", b"lo.t"));
        assert!(!contains_ignore_ascii_case(b"ab", b"abc"));
        assert!(contains_ignore_ascii_case(b"x", b""));
    }
}
