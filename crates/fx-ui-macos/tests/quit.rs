//! What the app does when it quits: Cmd+Q quits at once.
//!
//! Contract these tests rely on:
//!
//! - `fx_ui_macos::quit::quit_steps(status_item: bool) -> Vec<QuitStep>`
//!   lists the quit's steps in the order the AppKit layer performs them:
//!   `RemoveStatusItem` (only when the app shows a menu-bar item), then
//!   `HideWindows`, then `SaveIndex`, which calls
//!   `ResultsSource::save_for_quit` (the engine decides whether anything is
//!   written; see fx-engine's `checkpoint_for_quit`).

use fx_ui_macos::quit::{QuitStep, quit_steps};

#[test]
fn the_resident_app_hides_its_menu_bar_item_and_windows_before_saving() {
    assert_eq!(
        quit_steps(true),
        vec![
            QuitStep::RemoveStatusItem,
            QuitStep::HideWindows,
            QuitStep::SaveIndex
        ]
    );
}

#[test]
fn without_a_menu_bar_item_only_the_windows_are_hidden_before_saving() {
    assert_eq!(
        quit_steps(false),
        vec![QuitStep::HideWindows, QuitStep::SaveIndex]
    );
}

#[test]
fn the_index_is_saved_exactly_once_and_last() {
    for status_item in [false, true] {
        let steps = quit_steps(status_item);
        let saves = steps.iter().filter(|&&s| s == QuitStep::SaveIndex).count();
        assert_eq!(saves, 1, "{steps:?}");
        assert_eq!(steps.last(), Some(&QuitStep::SaveIndex), "{steps:?}");
    }
}
