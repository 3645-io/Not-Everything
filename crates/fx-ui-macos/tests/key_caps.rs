//! Contract tests for `fx_ui_macos::key_caps`: the key caps of the summon
//! hotkey (Settings) and of the status bar's hints, in Apple's modifier
//! order (the owner's choice: ⌥⇧Space).

use fx_channel::Channel;
use fx_ui_macos::actions::{self, Action, Modifiers};
use fx_ui_macos::hotkey::{HotkeySpec, Key};
use fx_ui_macos::key_caps::{RESULT_HINTS, key_caps, key_label, spoken};

fn spec(s: &str) -> HotkeySpec {
    HotkeySpec::parse(s).expect("a valid hotkey")
}

#[test]
fn the_default_hotkeys() {
    assert_eq!(
        key_caps(HotkeySpec::default_for(Channel::Release)),
        ["⌥", "Space"]
    );
    // Apple's order puts Option before Shift.
    assert_eq!(
        key_caps(HotkeySpec::default_for(Channel::Rc)),
        ["⌥", "⇧", "Space"]
    );
    assert_eq!(
        key_caps(HotkeySpec::default_for(Channel::Beta)),
        ["⌥", "⇧", "Space"]
    );
}

#[test]
fn modifiers_in_apple_order_whatever_the_spec_order() {
    assert_eq!(
        key_caps(spec("cmd+shift+option+control+k")),
        ["⌃", "⌥", "⇧", "⌘", "K"]
    );
    assert_eq!(key_caps(spec("shift+cmd+f12")), ["⇧", "⌘", "F12"]);
    assert_eq!(key_caps(spec("control+1")), ["⌃", "1"]);
}

#[test]
fn every_key_has_a_cap() {
    for key in Key::all() {
        let want = match key {
            Key::Space => "Space".to_string(),
            k => k.name().to_ascii_uppercase(),
        };
        assert_eq!(key_label(key), want, "{key:?}");
    }
}

#[test]
fn spoken_labels_for_accessibility() {
    assert_eq!(spoken(spec("option+space")), "Option Space");
    assert_eq!(spoken(spec("shift+option+space")), "Option Shift Space");
    assert_eq!(
        spoken(spec("cmd+shift+option+control+k")),
        "Control Option Shift Command K"
    );
}

#[test]
fn the_result_hints_match_the_key_map() {
    let shown: Vec<(&[&str], &str)> = RESULT_HINTS.iter().map(|h| (h.keys, h.label)).collect();
    assert_eq!(
        shown,
        [
            (&["↩"][..], "Open"),
            (&["⌘", "↩"][..], "Reveal in Finder"),
            (&["⌥", "⌘", "C"][..], "Copy path"),
        ]
    );
    // No Quick Look hint: Quick Look is not built.
    assert!(RESULT_HINTS.iter().all(|h| !h.label.contains("Quick Look")));

    let plain = Modifiers::default();
    let cmd = Modifiers {
        command: true,
        ..Modifiers::default()
    };
    let option_cmd = Modifiers {
        option: true,
        ..cmd
    };
    let actions: Vec<Action> = RESULT_HINTS.iter().map(|h| h.action).collect();
    assert_eq!(
        actions,
        [Action::Open, Action::RevealInFinder, Action::CopyPath]
    );
    assert_eq!(
        actions::action_for_key(actions::Key::Return, plain),
        Some(Action::Open)
    );
    assert_eq!(
        actions::action_for_key(actions::Key::Return, cmd),
        Some(Action::RevealInFinder)
    );
    assert_eq!(
        actions::action_for_key(actions::Key::Char('c'), option_cmd),
        Some(Action::CopyPath)
    );
    // Cmd+C alone copies nothing in the results, so no hint shows it.
    assert_eq!(actions::action_for_key(actions::Key::Char('c'), cmd), None);
}
