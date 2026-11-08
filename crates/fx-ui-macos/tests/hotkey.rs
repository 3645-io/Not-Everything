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
