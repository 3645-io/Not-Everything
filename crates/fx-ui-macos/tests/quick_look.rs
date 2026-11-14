//! Contract tests for `fx_ui_macos::quick_look`: what Space and Escape do
//! to the Quick Look panel, which selected rows it shows, and when it has
//! to reload.
//!
//! As in Finder, Space in the results opens the system Quick Look panel on
//! the selection and Space again closes it; Escape closes it too, and only
//! while it is open does Escape mean anything to it (otherwise Escape keeps
//! clearing the search and hiding the window). While the panel is open the
//! arrow keys move the selection and the panel follows, reloading only
//! when the selected files change.

use fx_ui_macos::actions::{Key, Modifiers};
use fx_ui_macos::controller::CopiedPaths;
use fx_ui_macos::quick_look::{
    MAX_PREVIEW_ITEMS, PreviewCommand, PreviewItems, preview_command, preview_rows,
};

const OPEN: bool = true;
const CLOSED: bool = false;

fn none() -> Modifiers {
    Modifiers::default()
}

/// Every combination of the four modifiers but none.
fn with_modifiers() -> Vec<Modifiers> {
    (1..16u8)
        .map(|i| Modifiers {
            command: i & 1 != 0,
            shift: i & 2 != 0,
            option: i & 4 != 0,
            control: i & 8 != 0,
        })
        .collect()
}

// ------------------------------------------------------------------- keys

#[test]
fn space_opens_the_panel_when_it_is_closed() {
    assert_eq!(
        preview_command(Key::Space, none(), CLOSED),
        Some(PreviewCommand::Open)
    );
}

#[test]
fn space_again_closes_it() {
    assert_eq!(
        preview_command(Key::Space, none(), OPEN),
        Some(PreviewCommand::Close)
    );
}

#[test]
fn escape_closes_the_open_panel() {
    assert_eq!(
        preview_command(Key::Escape, none(), OPEN),
        Some(PreviewCommand::Close)
    );
}

#[test]
fn escape_with_the_panel_closed_is_left_to_the_window() {
    // It still clears the search or hides the window (`escape_action`).
    assert_eq!(preview_command(Key::Escape, none(), CLOSED), None);
}

#[test]
fn space_and_escape_with_modifiers_do_nothing_to_the_panel() {
    for mods in with_modifiers() {
        for open in [OPEN, CLOSED] {
            assert_eq!(
                preview_command(Key::Space, mods, open),
                None,
                "{mods:?} {open}"
            );
            assert_eq!(
                preview_command(Key::Escape, mods, open),
                None,
                "{mods:?} {open}"
            );
        }
    }
}

#[test]
fn other_keys_do_nothing_to_the_panel() {
    // Return still opens and Cmd+Return still reveals; the arrow keys
    // (`Key::Other`) move the selection, which the panel follows.
    for key in [
        Key::Return,
        Key::Enter,
        Key::Backspace,
        Key::Delete,
        Key::Char('y'),
        Key::Char(' '),
        Key::Other,
    ] {
        for open in [OPEN, CLOSED] {
            assert_eq!(preview_command(key, none(), open), None, "{key:?}");
        }
    }
}

// ------------------------------------------------------- what it shows

#[test]
fn the_panel_shows_the_selection_in_row_order() {
    assert_eq!(preview_rows(&[2..3, 5..8]), vec![2..3, 5..8]);
}

#[test]
fn nothing_selected_shows_nothing() {
    assert!(preview_rows(&[]).is_empty());
    assert!(preview_rows(std::slice::from_ref(&(4..4))).is_empty());
}

#[test]
fn a_large_selection_shows_its_first_rows_only() {
    // Select All over two million results resolves at most this many
    // paths, as many as the window keeps selected across refreshes.
    assert_eq!(MAX_PREVIEW_ITEMS, 1_000);
    let all = 0..2_000_000;
    let shown = preview_rows(std::slice::from_ref(&all));
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0], 0..MAX_PREVIEW_ITEMS);
    assert_eq!(
        preview_rows(&[10..20, 100..2_000]),
        vec![10..20, 100..100 + MAX_PREVIEW_ITEMS - 10]
    );
}

// ------------------------------------------------------------ reloading

fn resolved(paths: &[&str]) -> CopiedPaths {
    CopiedPaths {
        paths: paths.iter().map(|p| (*p).to_owned()).collect(),
        ..CopiedPaths::default()
    }
}

#[test]
fn new_files_reload_the_panel() {
    let mut items = PreviewItems::default();
    assert!(items.is_empty());
    assert!(items.update(resolved(&["/Users/me/a.txt", "/Users/me/b.png"])));
    assert_eq!(items.len(), 2);
    assert_eq!(items.get(0), Some("/Users/me/a.txt"));
    assert_eq!(items.get(1), Some("/Users/me/b.png"));
    assert_eq!(items.get(2), None);
}

#[test]
fn the_same_files_do_not_reload_it() {
    // A refresh while indexing selects the same files again: the preview
    // must not flicker.
    let mut items = PreviewItems::default();
    items.update(resolved(&["/Users/me/a.txt"]));
    assert!(!items.update(resolved(&["/Users/me/a.txt"])));
    assert_eq!(items.len(), 1);
}

#[test]
fn an_outdated_answer_keeps_what_is_shown() {
    // The rows could only be read from newer results than the table shows;
    // the next page brings them, so the panel keeps its files until then.
    let mut items = PreviewItems::default();
    items.update(resolved(&["/Users/me/a.txt"]));
    let outdated = CopiedPaths {
        outdated: true,
        ..CopiedPaths::default()
    };
    assert!(!items.update(outdated));
    assert_eq!(items.get(0), Some("/Users/me/a.txt"));
}

#[test]
fn an_emptied_selection_empties_the_panel() {
    let mut items = PreviewItems::default();
    items.update(resolved(&["/Users/me/a.txt"]));
    assert!(items.update(resolved(&[])));
    assert!(items.is_empty());
    // Already empty: nothing to reload.
    assert!(!items.update(resolved(&[])));
}

#[test]
fn clearing_forgets_the_files() {
    let mut items = PreviewItems::default();
    items.update(resolved(&["/Users/me/a.txt"]));
    items.clear();
    assert!(items.is_empty());
    // After a clear the same files count as new again.
    assert!(items.update(resolved(&["/Users/me/a.txt"])));
}
