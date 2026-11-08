//! Tests for `fx_ui_macos::hotkey`: the platform-agnostic summon
//! hotkey spec, its string form and its mapping to the macOS virtual key code
//! and Carbon modifier mask that the AppKit layer feeds into `global-hotkey`.
//!
//! Contract exercised here:
//!
//! - `HotkeySpec { modifiers: Modifiers { cmd, shift, option, control }, key }`
//!   with public fields; `Key::Space` is the space bar.
//! - `HotkeySpec::parse(&str) -> Result<HotkeySpec, HotkeyError>`:
//!   `+`-separated, case-insensitive, modifiers in any order; rejects a
//!   spec with no modifier, an unknown key or modifier, a duplicate
//!   modifier, no key, or more than one key.
//! - `Display` formats the canonical lower-case form with modifiers in the
//!   order cmd, shift, option, control, then the key (`"cmd+shift+space"`).
//! - `HotkeySpec::default_for(channel)` is that build channel's default
//!   summon hotkey: option+space for the release app,
//!   shift+option+space for the Preview app (release candidates and betas),
//!   so both can run side by side. `HotkeySpec::default()` is this build's
//!   channel's, which `DEFAULT_SUMMON_HOTKEY` spells out.
//! - `HotkeySpec::to_carbon() -> (u32, u32)`: the macOS virtual key code
//!   (`kVK_*` from Carbon's `Events.h`) and the Carbon modifier mask
//!   (`cmdKey` 0x100, `shiftKey` 0x200, `optionKey` 0x800, `controlKey`
//!   0x1000). Plain integers, no `global-hotkey` types.

use fx_channel::Channel;
use fx_ui_macos::hotkey::{DEFAULT_SUMMON_HOTKEY, HotkeySpec, Key, Modifiers};

const CMD_KEY: u32 = 1 << 8;
const SHIFT_KEY: u32 = 1 << 9;
const OPTION_KEY: u32 = 1 << 11;
const CONTROL_KEY: u32 = 1 << 12;

const KVK_SPACE: u32 = 0x31;

fn parse(s: &str) -> HotkeySpec {
    HotkeySpec::parse(s).unwrap_or_else(|e| panic!("{s:?} should parse, got error: {e}"))
}

#[test]
fn parses_cmd_shift_space_into_fields() {
    let spec = parse("cmd+shift+space");
    assert_eq!(
        spec.modifiers,
        Modifiers {
            cmd: true,
            shift: true,
            option: false,
            control: false,
        }
    );
    assert_eq!(spec.key, Key::Space);
}

#[test]
fn parses_option_space_into_fields() {
    let spec = parse("option+space");
    assert!(spec.modifiers.option);
    assert!(!spec.modifiers.cmd);
    assert!(!spec.modifiers.shift);
    assert!(!spec.modifiers.control);
    assert_eq!(spec.key, Key::Space);
}

#[test]
fn parsing_is_case_insensitive() {
    assert_eq!(parse("CMD+Shift+SPACE"), parse("cmd+shift+space"));
    assert_eq!(parse("Option+Space"), parse("option+space"));
    assert_eq!(parse("Control+K"), parse("control+k"));
}

#[test]
fn modifier_order_does_not_matter() {
    let canonical = parse("cmd+shift+option+control+space");
    for s in [
        "control+option+shift+cmd+space",
        "shift+cmd+control+option+space",
        "option+control+cmd+shift+space",
    ] {
        assert_eq!(parse(s), canonical, "{s}");
    }
}

#[test]
fn formats_canonical_order_and_lower_case() {
    assert_eq!(parse("shift+cmd+space").to_string(), "cmd+shift+space");
    assert_eq!(parse("Option+Space").to_string(), "option+space");
    assert_eq!(
        parse("control+option+shift+cmd+space").to_string(),
        "cmd+shift+option+control+space"
    );
    assert_eq!(parse("CONTROL+K").to_string(), "control+k");
}

#[test]
fn format_parse_round_trips() {
    for s in [
        "option+space",
        "cmd+shift+space",
        "cmd+k",
        "option+control+f",
        "cmd+shift+option+control+space",
        "cmd+1",
        "option+0",
        "cmd+f1",
        "control+f12",
    ] {
        let spec = parse(s);
        let text = spec.to_string();
        assert_eq!(parse(&text), spec, "{s} -> {text}");
        assert_eq!(text, s, "{s} is already canonical");
    }
}

#[test]
fn every_letter_and_digit_round_trips() {
    for c in ('a'..='z').chain('0'..='9') {
        let s = format!("cmd+{c}");
        assert_eq!(parse(&s).to_string(), s);
    }
}

#[test]
fn rejects_spec_without_modifier() {
    for s in ["space", "k", "f1", "SPACE"] {
        assert!(HotkeySpec::parse(s).is_err(), "{s} has no modifier");
    }
}

#[test]
fn rejects_unknown_keys_and_modifiers() {
    for s in [
        "cmd+banana",
        "cmd+spacebar",
        "hyper+space",
        "cmd+f99",
        "cmd+é",
        "cmd+ab",
    ] {
        assert!(HotkeySpec::parse(s).is_err(), "{s} should be rejected");
    }
}

#[test]
fn rejects_duplicate_modifiers() {
    for s in ["cmd+cmd+space", "option+shift+option+space", "CMD+cmd+k"] {
        assert!(HotkeySpec::parse(s).is_err(), "{s} has a duplicate");
    }
}

#[test]
fn rejects_missing_or_extra_keys_and_empty_input() {
    for s in [
        "",
        "+",
        "cmd",
        "cmd+shift",
        "cmd+",
        "cmd++space",
        "cmd+space+k",
        "cmd+space+space",
    ] {
        assert!(HotkeySpec::parse(s).is_err(), "{s:?} should be rejected");
    }
}

#[test]
fn errors_have_a_message() {
    let err = HotkeySpec::parse("cmd+banana").unwrap_err();
    assert!(!err.to_string().is_empty());
}

#[test]
fn release_summon_hotkey_is_option_space() {
    let default = HotkeySpec::default_for(Channel::Release);
    assert_eq!(default, parse("option+space"));
    assert_eq!(default.to_string(), "option+space");
}

#[test]
fn preview_summon_hotkey_is_shift_option_space() {
    for channel in [Channel::Rc, Channel::Beta] {
        let default = HotkeySpec::default_for(channel);
        assert_eq!(default, parse("option+shift+space"), "{channel:?}");
        assert_eq!(default.to_string(), "shift+option+space", "{channel:?}");
    }
}

/// Each channel's hotkey string is the canonical form of its spec, so the
/// settings file writes back what the channel names.
#[test]
fn channel_hotkeys_are_canonical() {
    for channel in Channel::ALL {
        let spec = HotkeySpec::default_for(channel);
        assert_eq!(spec.to_string(), channel.default_hotkey(), "{channel:?}");
    }
}

#[test]
fn default_is_this_build_s_channel() {
    assert_eq!(
        HotkeySpec::default(),
        HotkeySpec::default_for(Channel::CURRENT)
    );
    assert_eq!(DEFAULT_SUMMON_HOTKEY, Channel::CURRENT.default_hotkey());
    assert_eq!(parse(DEFAULT_SUMMON_HOTKEY), HotkeySpec::default());
}

#[test]
fn carbon_conversion_for_default_hotkeys() {
    assert_eq!(
        HotkeySpec::default_for(Channel::Release).to_carbon(),
        (KVK_SPACE, OPTION_KEY)
    );
    assert_eq!(
        HotkeySpec::default_for(Channel::Rc).to_carbon(),
        (KVK_SPACE, OPTION_KEY | SHIFT_KEY)
    );
}

#[test]
fn carbon_modifier_bits() {
    assert_eq!(parse("cmd+space").to_carbon().1, CMD_KEY);
    assert_eq!(parse("shift+cmd+space").to_carbon().1, CMD_KEY | SHIFT_KEY);
    assert_eq!(parse("control+space").to_carbon().1, CONTROL_KEY);
    assert_eq!(
        parse("cmd+shift+option+control+space").to_carbon().1,
        CMD_KEY | SHIFT_KEY | OPTION_KEY | CONTROL_KEY
    );
}

#[test]
fn carbon_key_codes_for_known_keys() {
    // kVK_* values from Carbon's HIToolbox Events.h (ANSI layout positions).
    let cases: [(&str, u32); 10] = [
        ("cmd+space", 0x31),
        ("cmd+a", 0x00),
        ("cmd+f", 0x03),
        ("cmd+k", 0x28),
        ("cmd+q", 0x0C),
        ("cmd+m", 0x2E),
        ("cmd+1", 0x12),
        ("cmd+0", 0x1D),
        ("cmd+f1", 0x7A),
        ("cmd+f12", 0x6F),
    ];
    for (s, code) in cases {
        assert_eq!(parse(s).to_carbon().0, code, "{s}");
    }
}

#[test]
fn every_letter_and_digit_has_a_distinct_key_code() {
    let mut codes: Vec<u32> = ('a'..='z')
        .chain('0'..='9')
        .map(|c| parse(&format!("option+{c}")).to_carbon().0)
        .collect();
    assert!(!codes.contains(&KVK_SPACE));
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), 36);
}

// ------------------------------------------------- recording and the rules
//
// Run 2.10 adds, for the Settings recorder:
//
// - `Key::from_carbon_code(u32) -> Option<Key>`: the key at a macOS virtual
//   key code (`NSEvent.keyCode`), the inverse of `Key::carbon_code`; `None`
//   for a key a shortcut can't use.
// - `Modifiers::from_event_flags(u64)`: the modifiers in `NSEvent`'s
//   `modifierFlags` (Shift 1 << 17, Control 1 << 18, Option 1 << 19,
//   Command 1 << 20), ignoring Caps Lock, the numeric pad, Help and fn.
// - `HotkeySpec::check() -> Result<(), Refusal>`: the rules a shortcut
//   picked in Settings must pass before macOS is asked. `Refusal`'s
//   `Display` is the reason the Settings row shows.

use fx_ui_macos::hotkey::{
    EVENT_COMMAND, EVENT_CONTROL, EVENT_OPTION, EVENT_SHIFT, KVK_ESCAPE, Refusal,
};

/// `NSEventModifierFlagCapsLock`, `NumericPad`, `Help` and `Function`.
const OTHER_FLAGS: u64 = (1 << 16) | (1 << 21) | (1 << 22) | (1 << 23);

#[test]
fn every_key_reads_back_from_its_key_code() {
    for key in Key::all() {
        assert_eq!(Key::from_carbon_code(key.carbon_code()), Some(key), "{key}");
    }
}

#[test]
fn keys_a_shortcut_cannot_use_read_as_none() {
    // kVK_Return, Tab, Delete, Escape, the arrows, ANSI minus, comma and
    // grave, keypad 1, F13 and a code past the table.
    for code in [
        0x24, 0x30, 0x33, 0x35, 0x7B, 0x7C, 0x7D, 0x7E, 0x1B, 0x2B, 0x32, 0x53, 0x69, 0x200,
    ] {
        assert_eq!(Key::from_carbon_code(code), None, "{code:#x}");
    }
    assert_eq!(KVK_ESCAPE, 0x35);
}

#[test]
fn event_flags_map_to_modifiers() {
    assert_eq!(EVENT_SHIFT, 1 << 17);
    assert_eq!(EVENT_CONTROL, 1 << 18);
    assert_eq!(EVENT_OPTION, 1 << 19);
    assert_eq!(EVENT_COMMAND, 1 << 20);
    assert_eq!(Modifiers::from_event_flags(0), Modifiers::default());
    assert_eq!(
        Modifiers::from_event_flags(OTHER_FLAGS),
        Modifiers::default()
    );
    assert_eq!(
        Modifiers::from_event_flags(EVENT_OPTION | EVENT_SHIFT | OTHER_FLAGS),
        Modifiers {
            cmd: false,
            shift: true,
            option: true,
            control: false,
        }
    );
    assert_eq!(
        Modifiers::from_event_flags(EVENT_COMMAND | EVENT_CONTROL),
        Modifiers {
            cmd: true,
            shift: false,
            option: false,
            control: true,
        }
    );
    // The low device-dependent bits (left or right key) are ignored too.
    assert_eq!(
        Modifiers::from_event_flags(EVENT_COMMAND | 0x8),
        parse("cmd+a").modifiers
    );
}

#[test]
fn both_defaults_pass_the_rules() {
    for channel in Channel::ALL {
        assert_eq!(
            HotkeySpec::default_for(channel).check(),
            Ok(()),
            "{channel:?}"
        );
    }
}

#[test]
fn shortcuts_the_rules_allow() {
    for s in [
        "option+space",
        "shift+option+space",
        "control+k",
        "control+option+k",
        "cmd+shift+k",
        "cmd+option+k",
        "cmd+control+1",
        "cmd+space+shift",
        "control+shift+space",
        "cmd+f5",
        "shift+f5",
        "option+f12",
        "shift+option+f1",
        "control+f2",
    ] {
        assert_eq!(parse(s).check(), Ok(()), "{s}");
    }
}

#[test]
fn command_alone_with_a_letter_or_digit_is_every_app_s() {
    for s in [
        "cmd+q", "cmd+w", "cmd+h", "cmd+m", "cmd+c", "cmd+1", "cmd+0",
    ] {
        let spec = parse(s);
        assert_eq!(spec.check(), Err(Refusal::UsedByApps(spec)), "{s}");
    }
}

#[test]
fn shortcuts_that_type_a_character_are_refused() {
    for s in [
        "shift+k",
        "shift+1",
        "shift+space",
        "option+k",
        "option+7",
        "shift+option+k",
        "shift+option+2",
    ] {
        let spec = parse(s);
        assert_eq!(spec.check(), Err(Refusal::TypesCharacter(spec)), "{s}");
    }
}

#[test]
fn macos_s_own_shortcuts_are_refused_with_the_feature() {
    let cases = [
        ("cmd+space", "Spotlight"),
        ("cmd+option+space", "Finder search"),
        ("control+space", "input sources"),
        ("option+control+space", "input sources"),
        ("cmd+control+space", "emoji and symbols"),
        ("cmd+shift+3", "screenshots"),
        ("cmd+shift+4", "screenshots"),
        ("cmd+shift+5", "screenshots"),
        ("cmd+shift+6", "screenshots"),
        ("cmd+control+q", "Lock Screen"),
        ("cmd+shift+q", "Log Out"),
        ("cmd+option+d", "hiding the Dock"),
        ("cmd+control+f", "full screen"),
    ];
    for (s, feature) in cases {
        let spec = parse(s);
        assert_eq!(spec.check(), Err(Refusal::System(spec, feature)), "{s}");
    }
}

#[test]
fn refusals_read_as_the_settings_row_s_reasons() {
    let cases = [
        (
            Refusal::UsedByApps(parse("cmd+q")),
            "\u{2318}Q is used by every app. Try another shortcut.",
        ),
        (
            Refusal::TypesCharacter(parse("shift+option+k")),
            "\u{2325}\u{21e7}K types a character. Try another shortcut.",
        ),
        (
            Refusal::System(parse("cmd+space"), "Spotlight"),
            "macOS uses \u{2318}Space for Spotlight. Try another shortcut.",
        ),
        (
            Refusal::UnsupportedKey,
            "That key can\u{2019}t be used. Use a letter, digit, Space or F1\u{2013}F12.",
        ),
        (
            Refusal::NotAccepted(parse("control+option+k")),
            "macOS didn\u{2019}t accept \u{2303}\u{2325}K. Another app may be using it.",
        ),
        (
            Refusal::NotAcceptedAtLaunch(parse("option+space")),
            "macOS didn\u{2019}t accept \u{2325}Space when the app opened. Click the keys to \
             pick another.",
        ),
    ];
    for (refusal, text) in cases {
        assert_eq!(refusal.to_string(), text, "{refusal:?}");
    }
}
