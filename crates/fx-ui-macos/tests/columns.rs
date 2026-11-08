//! The results table's columns and the header-click sort state.

use std::collections::HashSet;

use fx_sort::{Direction, SortKey};
use fx_ui_macos::columns::{Column, SortState};

#[test]
fn the_four_mock_ui_columns_in_display_order() {
    let all: Vec<Column> = Column::ALL.iter().copied().collect();
    assert_eq!(
        all,
        vec![Column::Name, Column::Path, Column::Size, Column::Modified]
    );
}

#[test]
fn titles_match_the_mock_ui() {
    assert_eq!(Column::Name.title(), "Name");
    assert_eq!(Column::Path.title(), "Path");
    assert_eq!(Column::Size.title(), "Size");
    assert_eq!(Column::Modified.title(), "Date Modified");
}

#[test]
fn each_column_sorts_by_its_key() {
    assert_eq!(Column::Name.sort_key(), SortKey::Name);
    assert_eq!(Column::Path.sort_key(), SortKey::Path);
    assert_eq!(Column::Size.sort_key(), SortKey::Size);
    assert_eq!(Column::Modified.sort_key(), SortKey::Modified);
}

#[test]
fn identifiers_are_unique_non_empty_and_round_trip() {
    let mut seen = HashSet::new();
    for column in Column::ALL.iter().copied() {
        let id = column.identifier();
        assert!(!id.is_empty(), "{column:?} has an empty identifier");
        assert!(seen.insert(id), "identifier {id:?} is used twice");
        assert_eq!(Column::from_identifier(id), Some(column));
    }
    assert_eq!(Column::from_identifier("no-such-column"), None);
}

#[test]
fn default_widths_are_positive() {
    for column in Column::ALL.iter().copied() {
        let width = column.default_width();
        assert!(
            width > 0.0 && width.is_finite(),
            "{column:?} has width {width}"
        );
    }
}

#[test]
fn default_sort_is_name_ascending() {
    let sort = SortState::default();
    assert_eq!(sort.column, Column::Name);
    assert_eq!(sort.direction, Direction::Ascending);
}

#[test]
fn clicking_another_header_selects_it_ascending() {
    let mut sort = SortState::default();
    sort.click(Column::Size);
    assert_eq!(sort.column, Column::Size);
    assert_eq!(sort.direction, Direction::Ascending);

    // Even when the previous column was descending.
    sort.click(Column::Size);
    assert_eq!(sort.direction, Direction::Descending);
    sort.click(Column::Modified);
    assert_eq!(sort.column, Column::Modified);
    assert_eq!(sort.direction, Direction::Ascending);
}

#[test]
fn settings_value_is_the_column_identifier_and_direction() {
    // The `sort` line of the settings file, written on every header click.
    let size_descending = SortState {
        column: Column::Size,
        direction: Direction::Descending,
    };
    assert_eq!(size_descending.to_string(), "size:descending");
    assert_eq!(SortState::default().to_string(), "name:ascending");
    for column in Column::ALL.iter().copied() {
        for (direction, word) in [
            (Direction::Ascending, "ascending"),
            (Direction::Descending, "descending"),
        ] {
            let sort = SortState { column, direction };
            let value = sort.to_string();
            assert_eq!(value, format!("{}:{word}", column.identifier()));
            assert_eq!(SortState::parse(&value), Some(sort), "{value}");
        }
    }
}

#[test]
fn a_header_click_gives_the_value_saved() {
    let mut sort = SortState::default();
    sort.click(Column::Modified);
    assert_eq!(sort.to_string(), "modified:ascending");
    sort.click(Column::Modified);
    assert_eq!(sort.to_string(), "modified:descending");
}

#[test]
fn unreadable_settings_values_parse_to_none() {
    for value in [
        "",
        "name",
        "name:",
        ":ascending",
        "name:up",
        "title:ascending",
        "Name:Ascending",
        "name:ascending:x",
        " name:ascending",
    ] {
        assert_eq!(SortState::parse(value), None, "{value:?}");
    }
}

#[test]
fn clicking_the_same_header_toggles_direction() {
    let mut sort = SortState::default();
    sort.click(Column::Name);
    assert_eq!(sort.column, Column::Name);
    assert_eq!(sort.direction, Direction::Descending);
    sort.click(Column::Name);
    assert_eq!(sort.column, Column::Name);
    assert_eq!(sort.direction, Direction::Ascending);
}
