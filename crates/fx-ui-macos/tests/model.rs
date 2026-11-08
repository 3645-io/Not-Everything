//! Contract tests for `fx_ui_macos::model`: the virtual row model the
//! spike's table reads from. Its cells use `fx_ui_macos::formats` (tested in
//! `tests/formats.rs`), with dates in UTC.
//!
//! Row `i` of `RowModel::new(n, seed)` is entry `i` of
//! `fx_bench::generate_tree(n, seed, TreeShape::default())`.
//!
//! Built with the `measure` feature only.

#![cfg(feature = "measure")]

use fx_bench::{TreeShape, generate_tree};
use fx_ui_macos::formats::{UTC, format_date, format_size};
use fx_ui_macos::model::{Column, RowModel};

const SEED: u64 = 7;

fn text(m: &RowModel, row: usize, col: Column) -> Option<String> {
    m.cell_text(row, col).map(|c| c.into_owned())
}

#[test]
fn row_count_matches_requested_rows() {
    assert_eq!(RowModel::new(1_000, SEED).row_count(), 1_000);
    assert_eq!(RowModel::new(1, SEED).row_count(), 1);
}

#[test]
fn model_holds_two_million_rows() {
    let m = RowModel::new(2_000_000, 1);
    assert_eq!(m.row_count(), 2_000_000);
    let last = 1_999_999;
    for col in [Column::Name, Column::Path, Column::Size, Column::Modified] {
        assert!(m.cell_text(last, col).is_some());
    }
    assert!(m.cell_text(2_000_000, Column::Name).is_none());
}

#[test]
fn name_column_is_the_tree_entry_name() {
    let tree = generate_tree(5_000, SEED, TreeShape::default());
    let m = RowModel::new(5_000, SEED);
    for i in [0usize, 1, 2, 17, 999, 2_500, 4_999] {
        assert_eq!(
            text(&m, i, Column::Name).as_deref(),
            Some(tree.names.get(i)),
            "row {i}"
        );
    }
}

#[test]
fn path_column_is_the_parent_folder_path() {
    let tree = generate_tree(5_000, SEED, TreeShape::default());
    let m = RowModel::new(5_000, SEED);
    for i in [1usize, 2, 17, 999, 2_500, 4_999] {
        let parent = tree.parent[i] as usize;
        assert_eq!(
            text(&m, i, Column::Path),
            Some(tree.path(parent)),
            "row {i}"
        );
    }
    // The root has no parent folder; it must still answer without panicking.
    assert!(m.cell_text(0, Column::Path).is_some());
}

#[test]
fn size_and_modified_cells_use_the_formatters() {
    let m = RowModel::new(3_000, SEED);
    for i in [0usize, 1, 5, 100, 2_999] {
        let size = m.size_bytes(i).expect("size in range");
        let modified = m.modified_unix(i).expect("modified in range");
        let size_text = if m.is_dir(i) == Some(true) {
            "\u{2014}".to_string()
        } else {
            format_size(size)
        };
        assert_eq!(text(&m, i, Column::Size), Some(size_text), "row {i}");
        assert_eq!(
            text(&m, i, Column::Modified),
            Some(format_date(modified, &UTC))
        );
    }
}

#[test]
fn synthetic_size_and_modified_are_deterministic_per_seed() {
    let a = RowModel::new(2_000, SEED);
    let b = RowModel::new(2_000, SEED);
    for i in 0..2_000 {
        assert_eq!(a.size_bytes(i), b.size_bytes(i), "row {i}");
        assert_eq!(a.modified_unix(i), b.modified_unix(i), "row {i}");
        assert_eq!(text(&a, i, Column::Size), text(&b, i, Column::Size));
        assert_eq!(text(&a, i, Column::Modified), text(&b, i, Column::Modified));
    }
    // Synthetic values vary across rows rather than being one constant.
    let first = a.size_bytes(1);
    assert!((2..2_000).any(|i| a.size_bytes(i) != first));
    let first = a.modified_unix(1);
    assert!((2..2_000).any(|i| a.modified_unix(i) != first));
}

#[test]
fn modified_cells_look_like_dates() {
    let m = RowModel::new(1_000, SEED);
    for i in 0..1_000 {
        let s = text(&m, i, Column::Modified).unwrap();
        assert_date_shape(&s);
    }
}

/// "14 Mar 2026 09:12": a day of one or two digits, a short month name, a
/// four-digit year and a 24-hour time.
fn assert_date_shape(s: &str) {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let parts: Vec<&str> = s.split(' ').collect();
    assert_eq!(parts.len(), 4, "{s:?}");
    let digits = |p: &str| !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit());
    assert!(digits(parts[0]) && parts[0].len() <= 2, "{s:?}");
    assert!(!parts[0].starts_with('0'), "{s:?}");
    assert!(MONTHS.contains(&parts[1]), "{s:?}");
    assert!(digits(parts[2]) && parts[2].len() == 4, "{s:?}");
    let time = parts[3].as_bytes();
    assert_eq!(time.len(), 5, "{s:?}");
    assert_eq!(time[2], b':', "{s:?}");
    assert!(digits(&parts[3][..2]) && digits(&parts[3][3..]), "{s:?}");
}

#[test]
fn out_of_range_rows_return_none_and_never_panic() {
    let m = RowModel::new(100, SEED);
    for row in [100usize, 101, 1_000_000, usize::MAX] {
        for col in [Column::Name, Column::Path, Column::Size, Column::Modified] {
            assert!(m.cell_text(row, col).is_none(), "row {row}");
        }
        assert!(m.size_bytes(row).is_none());
        assert!(m.modified_unix(row).is_none());
    }
}

#[test]
fn write_cell_fills_a_caller_buffer() {
    let m = RowModel::new(1_000, SEED);
    let mut buf = String::from("stale contents");
    for col in [Column::Name, Column::Path, Column::Size, Column::Modified] {
        assert!(m.write_cell(42, col, &mut buf));
        assert_eq!(Some(buf.clone()), text(&m, 42, col));
    }
    buf.push_str("stale");
    assert!(!m.write_cell(1_000, Column::Name, &mut buf));
    assert!(buf.is_empty(), "out-of-range write leaves the buffer empty");
    assert!(!m.write_cell(usize::MAX, Column::Path, &mut buf));
    assert!(buf.is_empty());
}

#[test]
fn empty_filter_returns_every_row_in_order() {
    let m = RowModel::new(500, SEED);
    let all = m.filter("");
    assert_eq!(all, (0..500).collect::<Vec<usize>>());
}

#[test]
fn filter_is_ascii_case_insensitive_contains_on_names() {
    let tree = generate_tree(5_000, SEED, TreeShape::default());
    let m = RowModel::new(5_000, SEED);
    // Needles taken from real names, plus case variants of them.
    let sample = tree.names.get(1234).to_string();
    let needle: String = sample.chars().take(3).collect();
    let needles = [
        needle.clone(),
        needle.to_ascii_uppercase(),
        needle.to_ascii_lowercase(),
        "a".to_string(),
        "E".to_string(),
        ".txt".to_string(),
    ];
    for n in &needles {
        let lower = n.to_ascii_lowercase();
        let expected: Vec<usize> = (0..tree.len())
            .filter(|&i| tree.names.get(i).to_ascii_lowercase().contains(&lower))
            .collect();
        assert_eq!(m.filter(n), expected, "needle {n:?}");
    }
    assert!(m.filter(&needle).contains(&1234));
    assert!(m.filter(&needle.to_ascii_uppercase()).contains(&1234));
}

#[test]
fn filter_with_no_match_is_empty() {
    let m = RowModel::new(2_000, SEED);
    assert!(m.filter("\u{1}no such name\u{1}").is_empty());
}
