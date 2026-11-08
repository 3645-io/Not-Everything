//! The results table's columns and the header-click sort state.
//!
//! The four columns of `docs/img/mock-ui.png`, in display order: Name, Path,
//! Size and Date Modified. Each has a stable identifier (used as the
//! `NSTableColumn` identifier), a header title, a default width in points
//! and the [`SortKey`] the engine sorts by when its header is clicked.
//!
//! [`SortState`] follows the usual header behaviour: clicking a new header
//! sorts by it ascending, clicking the current header again flips the
//! direction. The default is Name ascending. The app remembers it between
//! launches as the `sort` line of the settings file ([`crate::settings`]),
//! written as `<column identifier>:<direction>`, such as `size:descending`.

#![forbid(unsafe_code)]

use std::fmt;

use fx_sort::{Direction, SortKey};

use crate::FixedList;

/// A column of the results table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Column {
    /// The entry's file name.
    Name,
    /// The path of the folder holding the entry, shown from "~" inside the
    /// home folder ([`crate::formats::write_folder`]).
    Path,
    /// The entry's size, formatted with [`crate::formats::write_size_cell`].
    Size,
    /// The entry's modification time, formatted with
    /// [`crate::formats::write_date`].
    Modified,
}

impl Column {
    /// All columns in display order.
    pub const ALL: FixedList<Column, 4> =
        FixedList([Column::Name, Column::Path, Column::Size, Column::Modified]);

    /// The column's header title.
    pub fn title(self) -> &'static str {
        match self {
            Column::Name => "Name",
            Column::Path => "Path",
            Column::Size => "Size",
            Column::Modified => "Date Modified",
        }
    }

    /// The stable identifier of the column (the `NSTableColumn` identifier).
    pub fn identifier(self) -> &'static str {
        match self {
            Column::Name => "name",
            Column::Path => "path",
            Column::Size => "size",
            Column::Modified => "modified",
        }
    }

    /// The column with this [`identifier`](Column::identifier), if any.
    pub fn from_identifier(identifier: &str) -> Option<Column> {
        Column::ALL
            .iter()
            .copied()
            .find(|c| c.identifier() == identifier)
    }

    /// The default width in points.
    pub fn default_width(self) -> f64 {
        match self {
            Column::Name => 300.0,
            Column::Path => 480.0,
            Column::Size => 90.0,
            Column::Modified => 140.0,
        }
    }

    /// The key the engine sorts by for this column.
    pub fn sort_key(self) -> SortKey {
        match self {
            Column::Name => SortKey::Name,
            Column::Path => SortKey::Path,
            Column::Size => SortKey::Size,
            Column::Modified => SortKey::Modified,
        }
    }
}

/// The current sort column and direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SortState {
    /// The column sorted by.
    pub column: Column,
    /// Which way the column is read.
    pub direction: Direction,
}

impl Default for SortState {
    /// Name ascending.
    fn default() -> SortState {
        SortState {
            column: Column::Name,
            direction: Direction::Ascending,
        }
    }
}

impl SortState {
    /// A header click: the same column flips the direction, another column
    /// is selected ascending.
    pub fn click(&mut self, column: Column) {
        if column == self.column {
            self.direction = match self.direction {
                Direction::Ascending => Direction::Descending,
                Direction::Descending => Direction::Ascending,
            };
        } else {
            self.column = column;
            self.direction = Direction::Ascending;
        }
    }

    /// The engine sort key of the current column.
    pub fn sort_key(self) -> SortKey {
        self.column.sort_key()
    }

    /// Reads a settings-file value (see [`Display`](fmt::Display)): a
    /// column [`identifier`](Column::identifier), a colon and `ascending` or
    /// `descending`. `None` for anything else.
    pub fn parse(s: &str) -> Option<SortState> {
        let (column, direction) = s.split_once(':')?;
        let column = Column::from_identifier(column)?;
        let direction = [Direction::Ascending, Direction::Descending]
            .into_iter()
            .find(|&d| direction_str(d) == direction)?;
        Some(SortState { column, direction })
    }
}

/// The settings-file word for `direction`.
fn direction_str(direction: Direction) -> &'static str {
    match direction {
        Direction::Ascending => "ascending",
        Direction::Descending => "descending",
    }
}

impl fmt::Display for SortState {
    /// The settings-file value, such as `size:descending`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}",
            self.column.identifier(),
            direction_str(self.direction)
        )
    }
}
