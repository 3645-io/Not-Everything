//! The read-only action layer: the key maps, the clipboard text and the
//! drag-out operation mask.
//!
//! In the search field, Return or Enter moves to the results
//! (`SearchFieldKey::FocusResults`) instead of opening the first result, so
//! the arrow keys can pick the file and Return in the results opens it.
//! Cmd+Return there still reveals in Finder.
//!
//! Escape backs out one step, as in Spotlight (resident app only): with
//! text in the search field it clears the search, and with the field empty
//! it hides the window (`escape_action`). Hiding the window, by Escape or
//! the summon hotkey, hands the focus back to the app the user was using
//! before (`hide_mode`).

use fx_ui_macos::actions::{
    Action, DRAG_OPERATION_MASK, EscapeAction, Focus, HideMode, Key, Modifiers, SearchFieldKey,
    action_for_key, copy_text, escape_action, hide_mode, key_from_code, search_field_key,
};

fn none() -> Modifiers {
    Modifiers::default()
}

fn cmd() -> Modifiers {
    Modifiers {
        command: true,
        ..Modifiers::default()
    }
}

#[test]
fn return_and_enter_open_the_selection() {
    assert_eq!(action_for_key(Key::Return, none()), Some(Action::Open));
    assert_eq!(action_for_key(Key::Enter, none()), Some(Action::Open));
}

#[test]
fn cmd_return_reveals_in_finder() {
    assert_eq!(
        action_for_key(Key::Return, cmd()),
        Some(Action::RevealInFinder)
    );
    assert_eq!(
        action_for_key(Key::Enter, cmd()),
        Some(Action::RevealInFinder)
    );
}

fn option_cmd() -> Modifiers {
    Modifiers {
        command: true,
        option: true,
        ..Modifiers::default()
    }
}

#[test]
fn option_cmd_c_copies_the_full_path() {
    // Finder's shortcut for copying a pathname.
    assert_eq!(
        action_for_key(Key::Char('c'), option_cmd()),
        Some(Action::CopyPath)
    );
}

#[test]
fn cmd_c_in_the_results_copies_nothing() {
    // Cmd+C is kept for copying the files themselves, as in Finder, which
    // is not built yet; until then only Option+Cmd+C copies, and only paths.
    for mods in all_modifiers() {
        if mods == option_cmd() {
            continue;
        }
        assert_eq!(action_for_key(Key::Char('c'), mods), None, "{mods:?}");
    }
}

#[test]
fn plain_c_is_not_an_action() {
    // Typing a letter without Command belongs to the search field.
    assert_eq!(action_for_key(Key::Char('c'), none()), None);
}

#[test]
fn delete_keys_map_to_no_action() {
    for key in [Key::Backspace, Key::Delete] {
        for mods in [
            none(),
            cmd(),
            Modifiers {
                shift: true,
                ..Modifiers::default()
            },
            Modifiers {
                option: true,
                ..Modifiers::default()
            },
            Modifiers {
                control: true,
                ..Modifiers::default()
            },
            Modifiers {
                command: true,
                option: true,
                ..Modifiers::default()
            },
        ] {
            assert_eq!(
                action_for_key(key, mods),
                None,
                "{key:?} with {mods:?} must not trigger any action"
            );
        }
    }
}

#[test]
fn action_set_has_the_read_only_actions() {
    let all: Vec<Action> = Action::ALL.iter().copied().collect();
    for wanted in [Action::Open, Action::RevealInFinder, Action::CopyPath] {
        assert!(all.contains(&wanted), "Action::ALL lacks {wanted:?}");
    }
}

#[test]
fn no_destructive_action_variant() {
    for action in Action::ALL.iter() {
        let name = format!("{action:?}").to_lowercase();
        for banned in ["delete", "remove", "rename", "move", "trash", "erase"] {
            assert!(
                !name.contains(banned),
                "{action:?} looks destructive ({banned}); the MVP is read-only"
            );
        }
    }
}

#[test]
fn copy_text_joins_paths_with_newlines() {
    let paths = vec![
        "/Users/me/alpha.txt".to_string(),
        "/Users/me/src/beta.rs".to_string(),
    ];
    assert_eq!(
        copy_text(&paths),
        "/Users/me/alpha.txt\n/Users/me/src/beta.rs"
    );
}

#[test]
fn copy_text_of_one_path_has_no_trailing_newline() {
    let paths = vec!["/tmp/a b.txt".to_string()];
    assert_eq!(copy_text(&paths), "/tmp/a b.txt");
}

#[test]
fn copy_text_of_nothing_is_empty() {
    let paths: Vec<String> = Vec::new();
    assert_eq!(copy_text(&paths), "");
}

// Raw NSDragOperation values from Apple's AppKit documentation.
const NS_DRAG_OPERATION_COPY: u64 = 1;
const NS_DRAG_OPERATION_LINK: u64 = 2;
const NS_DRAG_OPERATION_GENERIC: u64 = 4;
const NS_DRAG_OPERATION_PRIVATE: u64 = 8;
const NS_DRAG_OPERATION_MOVE: u64 = 16;
const NS_DRAG_OPERATION_DELETE: u64 = 32;

#[test]
fn drag_mask_is_exactly_copy_and_link() {
    assert_eq!(
        DRAG_OPERATION_MASK,
        NS_DRAG_OPERATION_COPY | NS_DRAG_OPERATION_LINK
    );
}

#[test]
fn drag_mask_has_move_delete_generic_and_private_clear() {
    for (bit, name) in [
        (NS_DRAG_OPERATION_MOVE, "move"),
        (NS_DRAG_OPERATION_DELETE, "delete"),
        (NS_DRAG_OPERATION_GENERIC, "generic"),
        (NS_DRAG_OPERATION_PRIVATE, "private"),
    ] {
        assert_eq!(DRAG_OPERATION_MASK & bit, 0, "the {name} bit must be clear");
    }
}

#[test]
fn return_and_enter_in_the_search_field_move_to_the_results() {
    for key in [Key::Return, Key::Enter] {
        assert_eq!(
            search_field_key(key, none()),
            Some(SearchFieldKey::FocusResults),
            "{key:?}"
        );
    }
}

#[test]
fn cmd_return_in_the_search_field_reveals_in_finder() {
    for key in [Key::Return, Key::Enter] {
        assert_eq!(
            search_field_key(key, cmd()),
            Some(SearchFieldKey::Act(Action::RevealInFinder)),
            "{key:?}"
        );
    }
}

#[test]
fn the_search_field_never_opens_on_return() {
    let mut combos = vec![none(), cmd()];
    for i in 0..16u8 {
        combos.push(Modifiers {
            command: i & 1 != 0,
            shift: i & 2 != 0,
            option: i & 4 != 0,
            control: i & 8 != 0,
        });
    }
    for mods in combos {
        for key in [Key::Return, Key::Enter] {
            assert_ne!(
                search_field_key(key, mods),
                Some(SearchFieldKey::Act(Action::Open)),
                "{key:?} {mods:?}"
            );
        }
    }
}

#[test]
fn other_keys_belong_to_the_search_field() {
    // Typing, deleting, Cmd+C (copy the field's text) and Option+Cmd+C stay
    // the field's.
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    for (key, mods) in [
        (Key::Char('c'), cmd()),
        (Key::Char('c'), option_cmd()),
        (Key::Char('a'), none()),
        (Key::Backspace, none()),
        (Key::Delete, cmd()),
        (Key::Other, none()),
        (Key::Return, shift),
    ] {
        assert_eq!(search_field_key(key, mods), None, "{key:?} {mods:?}");
    }
}

#[test]
fn return_in_the_results_still_opens() {
    assert_eq!(action_for_key(Key::Return, none()), Some(Action::Open));
}

// ------------------------------------------------------------------ Escape

/// Every combination of the four modifiers.
fn all_modifiers() -> Vec<Modifiers> {
    (0..16u8)
        .map(|i| Modifiers {
            command: i & 1 != 0,
            shift: i & 2 != 0,
            option: i & 4 != 0,
            control: i & 8 != 0,
        })
        .collect()
}

const RESIDENT: bool = true;
const MEASURING: bool = false;

#[test]
fn escape_in_an_empty_search_field_hides_the_window() {
    assert_eq!(
        escape_action(Key::Escape, none(), Focus::SearchField, "", RESIDENT),
        Some(EscapeAction::HideWindow)
    );
}

#[test]
fn escape_in_a_search_field_with_text_is_left_to_the_field() {
    // NSSearchField clears its text on Escape and runs the empty query;
    // that stays the field's own.
    for text in ["report", "a", " ", "ext:pdf invoice"] {
        assert_eq!(
            escape_action(Key::Escape, none(), Focus::SearchField, text, RESIDENT),
            None,
            "{text:?}"
        );
    }
}

#[test]
fn escape_in_the_results_with_an_empty_search_field_hides_the_window() {
    assert_eq!(
        escape_action(Key::Escape, none(), Focus::Results, "", RESIDENT),
        Some(EscapeAction::HideWindow)
    );
}

#[test]
fn escape_in_the_results_with_a_search_clears_it() {
    for text in ["report", " "] {
        assert_eq!(
            escape_action(Key::Escape, none(), Focus::Results, text, RESIDENT),
            Some(EscapeAction::ClearSearch),
            "{text:?}"
        );
    }
}

#[test]
fn escape_backs_out_one_step_at_a_time() {
    // From the results with a search: the first Escape clears the search
    // and puts the focus in the (now empty) field; the second hides.
    assert_eq!(
        escape_action(Key::Escape, none(), Focus::Results, "notes", RESIDENT),
        Some(EscapeAction::ClearSearch)
    );
    assert_eq!(
        escape_action(Key::Escape, none(), Focus::SearchField, "", RESIDENT),
        Some(EscapeAction::HideWindow)
    );
}

#[test]
fn escape_keeps_todays_behaviour_in_the_measurement_modes() {
    for focus in [Focus::SearchField, Focus::Results] {
        for text in ["", "report"] {
            assert_eq!(
                escape_action(Key::Escape, none(), focus, text, MEASURING),
                None,
                "{focus:?} {text:?}"
            );
        }
    }
}

#[test]
fn escape_with_modifiers_keeps_todays_behaviour() {
    for mods in all_modifiers().into_iter().filter(|m| *m != none()) {
        for focus in [Focus::SearchField, Focus::Results] {
            for text in ["", "report"] {
                assert_eq!(
                    escape_action(Key::Escape, mods, focus, text, RESIDENT),
                    None,
                    "{mods:?} {focus:?} {text:?}"
                );
            }
        }
    }
}

#[test]
fn only_escape_backs_out() {
    for key in [
        Key::Return,
        Key::Enter,
        Key::Backspace,
        Key::Delete,
        Key::Char('c'),
        Key::Char('\u{1b}'),
        Key::Other,
    ] {
        for focus in [Focus::SearchField, Focus::Results] {
            for text in ["", "report"] {
                assert_eq!(
                    escape_action(key, none(), focus, text, RESIDENT),
                    None,
                    "{key:?} {focus:?} {text:?}"
                );
            }
        }
    }
}

#[test]
fn escape_is_not_a_result_action_or_a_search_field_key() {
    for mods in all_modifiers() {
        assert_eq!(action_for_key(Key::Escape, mods), None, "{mods:?}");
        assert_eq!(search_field_key(Key::Escape, mods), None, "{mods:?}");
    }
}

// --------------------------------------------------------------- key codes

// Virtual key codes from Apple's HIToolbox Events.h (kVK_*).
const KVK_RETURN: u16 = 36;
const KVK_DELETE: u16 = 51;
const KVK_ESCAPE: u16 = 53;
const KVK_ANSI_KEYPAD_ENTER: u16 = 76;
const KVK_FORWARD_DELETE: u16 = 117;
const KVK_ANSI_C: u16 = 8;

#[test]
fn escape_is_recognised_by_its_key_code() {
    assert_eq!(key_from_code(KVK_ESCAPE, Some("\u{1b}")), Key::Escape);
    assert_eq!(key_from_code(KVK_ESCAPE, None), Key::Escape);
}

#[test]
fn named_keys_are_recognised_by_their_key_codes() {
    assert_eq!(key_from_code(KVK_RETURN, Some("\r")), Key::Return);
    assert_eq!(
        key_from_code(KVK_ANSI_KEYPAD_ENTER, Some("\u{3}")),
        Key::Enter
    );
    assert_eq!(key_from_code(KVK_DELETE, Some("\u{7f}")), Key::Backspace);
    assert_eq!(
        key_from_code(KVK_FORWARD_DELETE, Some("\u{f728}")),
        Key::Delete
    );
}

#[test]
fn other_keys_are_their_lowercase_character() {
    assert_eq!(key_from_code(KVK_ANSI_C, Some("c")), Key::Char('c'));
    assert_eq!(key_from_code(KVK_ANSI_C, Some("C")), Key::Char('c'));
    assert_eq!(key_from_code(KVK_ANSI_C, None), Key::Other);
    assert_eq!(key_from_code(KVK_ANSI_C, Some("")), Key::Other);
}

// ------------------------------------------------------- hiding the window

/// The user was in another app when the search window was summoned.
const FROM_ANOTHER_APP: bool = false;
/// The user was already in Not-Everything when it was summoned.
const FROM_THIS_APP: bool = true;
const SETTINGS_OPEN: bool = true;
const NO_OTHER_WINDOW: bool = false;

#[test]
fn hiding_returns_to_the_app_the_user_came_from() {
    // Summoned over another app: hide the app, so macOS activates that app
    // again, as Spotlight does.
    assert_eq!(
        hide_mode(FROM_ANOTHER_APP, NO_OTHER_WINDOW),
        HideMode::HideApp
    );
}

#[test]
fn hiding_returns_to_the_other_app_even_with_settings_open_behind() {
    // Settings was open but another app was in front: that app was the
    // one in use, so the focus goes back to it.
    assert_eq!(
        hide_mode(FROM_ANOTHER_APP, SETTINGS_OPEN),
        HideMode::HideApp
    );
}

#[test]
fn hiding_keeps_settings_in_front_when_the_user_came_from_it() {
    // Summoned from inside the app with Settings open: only the search
    // window goes, and Settings keeps the focus. It is never hidden.
    assert_eq!(
        hide_mode(FROM_THIS_APP, SETTINGS_OPEN),
        HideMode::WindowOnly
    );
}

#[test]
fn hiding_never_leaves_the_app_active_without_a_window() {
    // Before, the window was only ordered out and Not-Everything stayed
    // the active app with no window. With nothing else of the app on
    // screen, the app hides and the previous app comes back.
    assert_eq!(hide_mode(FROM_THIS_APP, NO_OTHER_WINDOW), HideMode::HideApp);
}
