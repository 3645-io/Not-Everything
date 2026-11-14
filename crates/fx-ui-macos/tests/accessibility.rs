//! Contract tests for `fx_ui_macos::accessibility`: what VoiceOver reads
//! for the search window's custom-drawn parts.
//!
//! The window draws its own look, so the texts VoiceOver speaks are set by
//! the app: each cell's text reads in words, so a row, read cell by cell,
//! reads in a sensible order (name, "folder" for a folder, the folder it is
//! in, size, date modified); each cell names its column; the status bar's
//! counts, index state and key hints read in words; and the result count of
//! a new search is announced.

use std::time::Duration;

use fx_ui_macos::accessibility::{
    RowSpeech, cap_word, cell_speech, column_label, hint_label, results_announcement, row_label,
    spoken, spoken_date, spoken_folder,
};
use fx_ui_macos::actions::Focus;
use fx_ui_macos::columns::Column;
use fx_ui_macos::key_caps::RESULT_HINTS;
use fx_ui_macos::status_view::{StatusInput, status_view};
use fx_ui_macos::ui_text::REGEX_ERROR_TITLE;

// ------------------------------------------------------------------ rows

fn file<'a>(name: &'a str, folder: &'a str, size: &'a str, modified: &'a str) -> RowSpeech<'a> {
    RowSpeech {
        name,
        is_folder: false,
        folder,
        size,
        modified,
    }
}

#[test]
fn a_file_reads_name_folder_size_then_date() {
    assert_eq!(
        row_label(&file(
            "report.pdf",
            "~/Documents/Finance",
            "182 KB",
            "14 Mar 2026 09:12"
        )),
        "report.pdf, Home/Documents/Finance, 182 KB, 14 March 2026 at 09:12"
    );
}

#[test]
fn a_folder_says_so_and_has_no_size() {
    // The Size column shows "—" for a folder; spoken, it would be noise.
    let row = RowSpeech {
        name: "Finance",
        is_folder: true,
        folder: "~/Documents",
        size: "\u{2014}",
        modified: "2 Mar 2026 10:00",
    };
    assert_eq!(
        row_label(&row),
        "Finance, folder, Home/Documents, 2 March 2026 at 10:00"
    );
}

#[test]
fn a_file_outside_the_home_folder_reads_its_folder_as_shown() {
    assert_eq!(
        row_label(&file(
            "hosts",
            "/private/etc",
            "213 bytes",
            "1 Jan 2026 00:00"
        )),
        "hosts, /private/etc, 213 bytes, 1 January 2026 at 00:00"
    );
}

#[test]
fn a_file_in_the_home_folder_itself() {
    assert_eq!(
        row_label(&file(".zshrc", "~", "1.2 KB", "9 Nov 2026 23:59")),
        ".zshrc, Home, 1.2 KB, 9 November 2026 at 23:59"
    );
}

#[test]
fn missing_parts_are_left_out() {
    // A row not read yet, a file without a size or a date.
    assert_eq!(row_label(&file("a.txt", "", "", "")), "a.txt");
    assert_eq!(row_label(&file("a.txt", "~/x", "", "")), "a.txt, Home/x");
    assert_eq!(row_label(&file("a.txt", "", "4 KB", "")), "a.txt, 4 KB");
}

#[test]
fn each_cell_reads_its_text_in_words() {
    assert_eq!(
        cell_speech(Column::Name, "report.pdf", false).as_deref(),
        Some("report.pdf")
    );
    assert_eq!(
        cell_speech(Column::Name, "Finance", true).as_deref(),
        Some("Finance, folder")
    );
    assert_eq!(
        cell_speech(Column::Path, "~/Documents", false).as_deref(),
        Some("Home/Documents")
    );
    assert_eq!(
        cell_speech(Column::Size, "182 KB", false).as_deref(),
        Some("182 KB")
    );
    assert_eq!(
        cell_speech(Column::Modified, "14 Mar 2026 09:12", false).as_deref(),
        Some("14 March 2026 at 09:12")
    );
}

#[test]
fn a_folders_size_and_empty_cells_read_nothing() {
    // "—" in the Size column would read "em dash".
    assert_eq!(cell_speech(Column::Size, "\u{2014}", true), None);
    for column in Column::ALL {
        assert_eq!(cell_speech(column, "", false), None, "{column:?}");
        assert_eq!(cell_speech(column, "", true), None, "{column:?}");
    }
}

#[test]
fn folders_read_home_in_words() {
    assert_eq!(spoken_folder("~"), "Home");
    assert_eq!(spoken_folder("~/Documents"), "Home/Documents");
    assert_eq!(spoken_folder("/Users/me"), "/Users/me");
    // Only a leading "~" is the home folder.
    assert_eq!(spoken_folder("/tmp/~old"), "/tmp/~old");
    assert_eq!(spoken_folder("~old"), "~old");
    assert_eq!(spoken_folder(""), "");
}

#[test]
fn dates_read_the_month_in_full() {
    let months = [
        ("Jan", "January"),
        ("Feb", "February"),
        ("Mar", "March"),
        ("Apr", "April"),
        ("May", "May"),
        ("Jun", "June"),
        ("Jul", "July"),
        ("Aug", "August"),
        ("Sep", "September"),
        ("Oct", "October"),
        ("Nov", "November"),
        ("Dec", "December"),
    ];
    for (short, long) in months {
        assert_eq!(
            spoken_date(&format!("7 {short} 2025 08:05")),
            format!("7 {long} 2025 at 08:05")
        );
    }
}

#[test]
fn a_date_in_another_shape_is_read_as_shown() {
    assert_eq!(spoken_date(""), "");
    assert_eq!(spoken_date("yesterday"), "yesterday");
    assert_eq!(spoken_date("7 Foo 2025 08:05"), "7 Foo 2025 08:05");
}

#[test]
fn each_cell_names_its_column() {
    let labels: Vec<&str> = Column::ALL.into_iter().map(column_label).collect();
    assert_eq!(labels, ["Name", "Path", "Size", "Date Modified"]);
}

// ------------------------------------------------------------ status bar

#[test]
fn the_counts_read_without_the_dots() {
    let v = status_view(&StatusInput {
        has_query: true,
        results: 8,
        objects: 2_014_377,
        search_time: Some(Duration::from_millis(9)),
        ..StatusInput::default()
    });
    assert_eq!(v.counts.text(), "8 results \u{b7} 2,014,377 objects");
    assert_eq!(spoken(&v.counts.text()), "8 results, 2,014,377 objects");
}

#[test]
fn the_counts_without_a_query() {
    let v = status_view(&StatusInput {
        objects: 1,
        ..StatusInput::default()
    });
    assert_eq!(spoken(&v.counts.text()), "1 object");
}

#[test]
fn the_index_slot_reads_without_the_dots() {
    assert_eq!(spoken("Up to date \u{b7} 9 ms"), "Up to date, 9 ms");
    assert_eq!(
        spoken("Indexing\u{2026} 1,200 of about 2,000"),
        "Indexing\u{2026} 1,200 of about 2,000"
    );
    assert_eq!(spoken("2 selected \u{b7} Limited"), "2 selected, Limited");
}

#[test]
fn key_caps_read_as_words() {
    assert_eq!(cap_word("\u{2318}"), "Command");
    assert_eq!(cap_word("\u{2325}"), "Option");
    assert_eq!(cap_word("\u{21e7}"), "Shift");
    assert_eq!(cap_word("\u{2303}"), "Control");
    assert_eq!(cap_word("\u{21a9}"), "Return");
    assert_eq!(cap_word("C"), "C");
    assert_eq!(cap_word("Space"), "Space");
}

#[test]
fn each_hint_reads_its_action_then_its_keys() {
    let spoken: Vec<String> = RESULT_HINTS.iter().map(hint_label).collect();
    assert_eq!(
        spoken,
        [
            "Open, Return",
            "Reveal in Finder, Command Return",
            "Copy, Command C",
            "Copy path, Option Command C",
        ]
    );
}

#[test]
fn the_hints_show_with_the_results_focused() {
    // The hints VoiceOver reads are the ones the status bar shows.
    let v = status_view(&StatusInput {
        has_query: true,
        results: 3,
        focus: Focus::Results,
        selected: 1,
        ..StatusInput::default()
    });
    assert_eq!(
        v.middle,
        fx_ui_macos::status_view::Middle::Hints(&RESULT_HINTS)
    );
}

// --------------------------------------------------------- announcement

#[test]
fn a_new_search_announces_its_count() {
    assert_eq!(
        results_announcement(true, 1_234, false).as_deref(),
        Some("1,234 results")
    );
    assert_eq!(
        results_announcement(true, 1, false).as_deref(),
        Some("1 result")
    );
}

#[test]
fn a_search_without_results_announces_no_matches() {
    assert_eq!(
        results_announcement(true, 0, false).as_deref(),
        Some("No matches")
    );
}

#[test]
fn an_invalid_regular_expression_is_announced() {
    assert_eq!(
        results_announcement(true, 0, true).as_deref(),
        Some(REGEX_ERROR_TITLE)
    );
}

#[test]
fn an_emptied_search_announces_nothing() {
    // Clearing the field lists every file again; the count of the whole
    // index is in the status bar, and announcing it would only interrupt.
    assert_eq!(results_announcement(false, 2_000_000, false), None);
}
