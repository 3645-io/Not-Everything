//! Key caps (the canvas's KeyCap): the summon hotkey as Settings
//! shows it, and the status bar's hints for the selected result.
//!
//! Modifiers come in Apple's order, ⌃ ⌥ ⇧ ⌘, whatever order the settings
//! file wrote them in, so the Preview app's default reads ⌥ ⇧ Space (the
//! owner's choice). Letters are capitals, the space bar is
//! "Space", function keys "F1" to "F12".

#![forbid(unsafe_code)]

use crate::actions::Action;
use crate::hotkey::{HotkeySpec, Key, Modifiers};

/// Control.
pub const CONTROL: &str = "\u{2303}";
/// Option.
pub const OPTION: &str = "\u{2325}";
/// Shift.
pub const SHIFT: &str = "\u{21e7}";
/// Command.
pub const COMMAND: &str = "\u{2318}";
/// Return.
pub const RETURN: &str = "\u{21a9}";

/// Caps for [`Key::all`], in its order.
const KEY_LABELS: [&str; 49] = [
    "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R", "S",
    "T", "U", "V", "W", "X", "Y", "Z", "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "Space",
    "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
];

/// The cap of `key`.
pub fn key_label(key: Key) -> &'static str {
    Key::all()
        .position(|k| k == key)
        .and_then(|i| KEY_LABELS.get(i))
        .copied()
        .unwrap_or("")
}

/// The caps of `spec`: its modifiers in Apple's order, then its key.
pub fn key_caps(spec: HotkeySpec) -> Vec<&'static str> {
    let mut caps = modifier_caps(spec.modifiers);
    caps.push(key_label(spec.key));
    caps
}

/// The caps of the modifiers `m`, in Apple's order: what the Settings
/// recorder shows while they are held.
pub fn modifier_caps(m: Modifiers) -> Vec<&'static str> {
    [
        (m.control, CONTROL),
        (m.option, OPTION),
        (m.shift, SHIFT),
        (m.cmd, COMMAND),
    ]
    .into_iter()
    .filter_map(|(on, cap)| on.then_some(cap))
    .collect()
}

/// `spec` in words, for VoiceOver: "Option Shift Space".
pub fn spoken(spec: HotkeySpec) -> String {
    let m = spec.modifiers;
    let mut words: Vec<&str> = [
        (m.control, "Control"),
        (m.option, "Option"),
        (m.shift, "Shift"),
        (m.cmd, "Command"),
    ]
    .into_iter()
    .filter_map(|(on, word)| on.then_some(word))
    .collect();
    words.push(key_label(spec.key));
    words.join(" ")
}

/// A shortcut hint in the status bar: its caps, then what it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeyHint {
    /// The action the keys trigger on the results.
    pub action: Action,
    /// The caps, 3 points apart.
    pub keys: &'static [&'static str],
    /// The label after the caps.
    pub label: &'static str,
}

/// The hints shown while the results have the focus and a row is selected
/// (the Main board and the right-click boards, without the "Space Quick
/// Look" hint: Space works since run 2.5, but the hint waits for the owner
/// to confirm its place on the board). The right-click menu shows the same
/// caps for the same items ([`crate::context_menu::ContextItem::shortcut`]).
pub const RESULT_HINTS: [KeyHint; 4] = [
    KeyHint {
        action: Action::Open,
        keys: &[RETURN],
        label: "Open",
    },
    KeyHint {
        action: Action::RevealInFinder,
        keys: &[COMMAND, RETURN],
        label: "Reveal in Finder",
    },
    KeyHint {
        action: Action::Copy,
        keys: &[COMMAND, "C"],
        label: "Copy",
    },
    KeyHint {
        action: Action::CopyPath,
        keys: &[OPTION, COMMAND, "C"],
        label: "Copy path",
    },
];
