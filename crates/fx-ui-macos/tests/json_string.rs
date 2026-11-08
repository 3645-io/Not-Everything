//! Contract tests for `fx_ui_macos::bundle::json_string`, the JSON string
//! escaper behind `make_bundle --json`.

use fx_ui_macos::bundle::json_string;

#[test]
fn path_with_quote_backslash_tab_and_non_ascii_is_valid_json() {
    let path = "/Users/me/out \"x\"\\y\tz/caf\u{e9} \u{65e5}\u{672c}/Table Spike.app";
    assert_eq!(
        json_string(path),
        "\"/Users/me/out \\\"x\\\"\\\\y\\tz/caf\u{e9} \u{65e5}\u{672c}/Table Spike.app\""
    );
}

#[test]
fn other_control_characters_use_unicode_escapes() {
    assert_eq!(json_string("a\u{1}b\u{1b}c"), "\"a\\u0001b\\u001bc\"");
    assert_eq!(json_string("\u{0}"), "\"\\u0000\"");
}

#[test]
fn plain_text_is_only_quoted() {
    assert_eq!(
        json_string("target/spike-bundle/Table Spike.app"),
        "\"target/spike-bundle/Table Spike.app\""
    );
    assert_eq!(json_string(""), "\"\"");
}

#[test]
fn output_never_holds_a_raw_control_character() {
    let all: String = (0u32..0x100).filter_map(char::from_u32).collect();
    let s = json_string(&all);
    assert!(s.starts_with('"') && s.ends_with('"'));
    assert!(!s.chars().any(|c| (c as u32) < 0x20));
    // Every quote inside the literal is escaped.
    let inner = &s[1..s.len() - 1];
    let bytes = inner.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'"' {
            let backslashes = bytes[..i].iter().rev().take_while(|&&c| c == b'\\').count();
            assert_eq!(backslashes % 2, 1, "unescaped quote at byte {i}");
        }
    }
}
