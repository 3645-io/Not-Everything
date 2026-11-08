//! Tests for `fx_ui_macos::shortcut_recorder`: what the Settings row's
//! recorder does with each click, key and focus change (run 2.10).
//!
//! Contract exercised here:
//!
//! - `Recorder::default()` is not recording; `is_recording()` and `held()`
//!   (the modifiers held so far) describe it.
//! - `Recorder::handle(Input) -> Output`:
//!   - `Start` (a click on the caps, or Space or Return with the focus)
//!     starts recording: `Started`.
//!   - `Modifiers(m)` (`flagsChanged:`) while recording shows the held
//!     modifiers: `Held(m)`.
//!   - `Key { code, modifiers, repeat }` while recording: Escape cancels
//!     (`Cancelled`); a key with no modifier beeps and keeps recording
//!     (`Beep`); a key a shortcut can't use ends it with
//!     `Refused(Refusal::UnsupportedKey)`; any other ends it with
//!     `Picked(spec)` (the rules and macOS are asked afterwards). An
//!     auto-repeat does nothing.
//!   - `SummonPressed`: the current shortcut was pressed (macOS hands it to
//!     the app, not to the window): recording ends with no change,
//!     `Unchanged`.
//!   - `Cancel` (the focus left the control, the window closed or lost key
//!     status, another control was used) ends it: `Cancelled`.
//!   - Anything but `Start` while not recording does nothing: `Nothing`.

use fx_ui_macos::hotkey::{HotkeySpec, KVK_ESCAPE, Key, Modifiers, Refusal};
use fx_ui_macos::shortcut_recorder::{Input, Output, Recorder};

const NONE: Modifiers = Modifiers {
    cmd: false,
    shift: false,
    option: false,
    control: false,
};
const CONTROL_OPTION: Modifiers = Modifiers {
    cmd: false,
    shift: false,
    option: true,
    control: true,
};
const COMMAND: Modifiers = Modifiers {
    cmd: true,
    shift: false,
    option: false,
    control: false,
};

fn key(k: Key, modifiers: Modifiers) -> Input {
    Input::Key {
        code: k.carbon_code(),
        modifiers,
        repeat: false,
    }
}

fn recording() -> Recorder {
    let mut r = Recorder::default();
    assert_eq!(r.handle(Input::Start), Output::Started);
    r
}

#[test]
fn starts_idle() {
    let r = Recorder::default();
    assert!(!r.is_recording());
    assert_eq!(r.held(), NONE);
}

#[test]
fn a_click_starts_recording_once() {
    let mut r = recording();
    assert!(r.is_recording());
    assert_eq!(r.handle(Input::Start), Output::Nothing);
    assert!(r.is_recording());
}

#[test]
fn held_modifiers_show_while_recording() {
    let mut r = recording();
    assert_eq!(
        r.handle(Input::Modifiers(CONTROL_OPTION)),
        Output::Held(CONTROL_OPTION)
    );
    assert_eq!(r.held(), CONTROL_OPTION);
    assert_eq!(r.handle(Input::Modifiers(NONE)), Output::Held(NONE));
    assert_eq!(r.held(), NONE);
}

#[test]
fn a_key_with_modifiers_picks_the_shortcut() {
    let mut r = recording();
    r.handle(Input::Modifiers(CONTROL_OPTION));
    let picked = HotkeySpec {
        modifiers: CONTROL_OPTION,
        key: Key::K,
    };
    assert_eq!(
        r.handle(key(Key::K, CONTROL_OPTION)),
        Output::Picked(picked)
    );
    assert!(!r.is_recording());
    assert_eq!(r.held(), NONE);
}

#[test]
fn the_rules_are_not_the_recorder_s_job() {
    // ⌘Q comes back as picked: applying it refuses it with the reason, so
    // the recorder catches it before the menu would quit the app.
    let mut r = recording();
    assert_eq!(
        r.handle(key(Key::Q, COMMAND)),
        Output::Picked(HotkeySpec::parse("cmd+q").unwrap())
    );
}

#[test]
fn escape_cancels() {
    let mut r = recording();
    r.handle(Input::Modifiers(CONTROL_OPTION));
    let escape = Input::Key {
        code: KVK_ESCAPE,
        modifiers: NONE,
        repeat: false,
    };
    assert_eq!(r.handle(escape), Output::Cancelled);
    assert!(!r.is_recording());
    assert_eq!(r.held(), NONE);
}

#[test]
fn escape_with_modifiers_cancels_too() {
    let mut r = recording();
    let escape = Input::Key {
        code: KVK_ESCAPE,
        modifiers: COMMAND,
        repeat: false,
    };
    assert_eq!(r.handle(escape), Output::Cancelled);
}

#[test]
fn a_key_without_modifiers_beeps_and_keeps_recording() {
    let mut r = recording();
    assert_eq!(r.handle(key(Key::K, NONE)), Output::Beep);
    assert_eq!(r.handle(key(Key::Space, NONE)), Output::Beep);
    assert_eq!(r.handle(key(Key::F5, NONE)), Output::Beep);
    // Return and Tab with no modifier too.
    for code in [0x24, 0x30] {
        let input = Input::Key {
            code,
            modifiers: NONE,
            repeat: false,
        };
        assert_eq!(r.handle(input), Output::Beep, "{code:#x}");
    }
    assert!(r.is_recording());
}

#[test]
fn a_key_a_shortcut_cannot_use_is_refused() {
    let mut r = recording();
    let left_arrow = Input::Key {
        code: 0x7B,
        modifiers: COMMAND,
        repeat: false,
    };
    assert_eq!(
        r.handle(left_arrow),
        Output::Refused(Refusal::UnsupportedKey)
    );
    assert!(!r.is_recording());
}

#[test]
fn auto_repeat_is_ignored() {
    let mut r = recording();
    let repeat = Input::Key {
        code: Key::K.carbon_code(),
        modifiers: CONTROL_OPTION,
        repeat: true,
    };
    assert_eq!(r.handle(repeat), Output::Nothing);
    assert!(r.is_recording());
}

#[test]
fn pressing_the_current_shortcut_changes_nothing() {
    let mut r = recording();
    assert_eq!(r.handle(Input::SummonPressed), Output::Unchanged);
    assert!(!r.is_recording());
}

#[test]
fn losing_the_focus_cancels() {
    let mut r = recording();
    r.handle(Input::Modifiers(COMMAND));
    assert_eq!(r.handle(Input::Cancel), Output::Cancelled);
    assert!(!r.is_recording());
    assert_eq!(r.held(), NONE);
}

#[test]
fn nothing_happens_while_not_recording() {
    let mut r = Recorder::default();
    for input in [
        Input::Modifiers(COMMAND),
        key(Key::K, CONTROL_OPTION),
        key(Key::K, NONE),
        Input::SummonPressed,
        Input::Cancel,
    ] {
        assert_eq!(r.handle(input), Output::Nothing, "{input:?}");
    }
    assert!(!r.is_recording());
    assert_eq!(r.held(), NONE);
}
