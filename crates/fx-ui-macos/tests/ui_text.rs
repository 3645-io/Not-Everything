//! Contract tests for the interface's copy: `fx_ui_macos::ui_text`
//! (search window), the Settings copy in `settings_view` and the
//! channel tag in `build_info`, as written on the canvas (with the owner's
//! changes).

use fx_channel::Channel;
use fx_query::{Filter, Node, SearchOptions, parse};
use fx_sort::Direction;
use fx_ui_macos::build_info::{BuildInfo, ChannelTag};
use fx_ui_macos::settings_view;
use fx_ui_macos::theme::Token;
use fx_ui_macos::ui_text::{self, OPTION_CHIPS, SYNTAX_EXAMPLES};

#[test]
fn the_search_field_and_chips() {
    assert_eq!(ui_text::SEARCH_PLACEHOLDER, "Search");
    assert_eq!(ui_text::SEARCH_LABEL, "Search file and folder names");
    assert_eq!(ui_text::CLEAR_LABEL, "Clear search");
    assert_eq!(ui_text::CLEAR_GLYPH, "\u{d7}");
    assert_eq!(ui_text::OPTIONS_LABEL, "Search options");
    let chips: Vec<(&str, &str, bool)> = OPTION_CHIPS
        .iter()
        .map(|c| (c.label, c.title, c.mono))
        .collect();
    assert_eq!(
        chips,
        [
            ("Aa", "Match case", false),
            ("Path", "Match full path", false),
            (".*", "Regular expression", true),
        ]
    );
}

#[test]
fn sort_header_labels() {
    assert_eq!(ui_text::RESULTS_LABEL, "Results");
    assert_eq!(ui_text::sort_label("Name", None), "Sort by Name");
    assert_eq!(
        ui_text::sort_label("Date Modified", Some(Direction::Descending)),
        "Sort by Date Modified, descending"
    );
    assert_eq!(
        ui_text::sort_label("Size", Some(Direction::Ascending)),
        "Sort by Size, ascending"
    );
}

#[test]
fn the_no_matches_state() {
    assert_eq!(
        ui_text::no_matches_title("quarterly reprot"),
        "No matches for \u{201c}quarterly reprot\u{201d}"
    );
    assert_eq!(
        ui_text::no_matches_title("  invoice 2026 "),
        "No matches for \u{201c}invoice 2026\u{201d}"
    );
    assert_eq!(
        ui_text::NO_MATCHES_TEXT,
        "Check the spelling or try fewer words. To narrow a search instead, add one of these:"
    );
    assert_eq!(ui_text::SYNTAX_LABEL, "SEARCH SYNTAX");
    let examples: Vec<(&str, &str)> = SYNTAX_EXAMPLES
        .iter()
        .map(|e| (e.query, e.meaning))
        .collect();
    assert_eq!(
        examples,
        [
            ("ext:pdf", "Files with this extension"),
            ("dm:thisweek", "Modified this week"),
            ("size:>10mb", "Larger than 10 MB"),
            ("path:Finance", "Matches the full path, not only the name"),
            ("report|summary", "Either word"),
            ("\"tax return\"", "The words together, in this order"),
        ]
    );
}

fn parsed(q: &str) -> Node {
    let p = parse(q, &SearchOptions::default());
    assert!(p.diagnostics.is_empty(), "{q}: {:?}", p.diagnostics);
    p.query
}

#[test]
fn every_syntax_example_parses_to_what_it_says() {
    for e in SYNTAX_EXAMPLES {
        parsed(e.query);
    }
    let ext = parsed("ext:pdf");
    assert!(
        matches!(ext, Node::Function(Filter::Ext(ref v)) if v == &["pdf"]),
        "{ext:?}"
    );
    let Node::Function(dm) = parsed("dm:thisweek") else {
        panic!("dm:thisweek is a function")
    };
    assert_eq!(dm.name(), "dm");
    let Node::Function(Filter::Size(size)) = parsed("size:>10mb") else {
        panic!("size:>10mb is a size test")
    };
    // A written "10mb" stands for the sizes that read as 10 MB at whole-MB
    // precision (10 MiB up to 11 MiB - 1, fx-query's size buckets), so
    // ">10mb" starts at 11 MiB.
    assert!(size.matches_bytes(11 * 1_048_576));
    assert!(size.matches_bytes(500 * 1_048_576));
    assert!(!size.matches_bytes(11 * 1_048_576 - 1));
    assert!(!size.matches_bytes(1_048_576));
    let Node::Term(path) = parsed("path:Finance") else {
        panic!("path:Finance is a term")
    };
    assert!(path.options.match_path);
    assert_eq!(path.text, "Finance");
    let Node::Or(words) = parsed("report|summary") else {
        panic!("report|summary is an OR")
    };
    assert_eq!(words.len(), 2);
    let Node::Term(phrase) = parsed("\"tax return\"") else {
        panic!("a quoted phrase is one term")
    };
    assert_eq!(phrase.text, "tax return");
}

#[test]
fn the_invalid_regex_state() {
    assert_eq!(
        ui_text::REGEX_ERROR_TITLE,
        "That isn\u{2019}t a valid regular expression"
    );
    assert_eq!(
        ui_text::REGEX_ERROR_TEXT,
        "Check its brackets and escapes, or turn off .* to search for the text as typed."
    );
}

#[test]
fn the_access_callout_uses_the_main_boards_text() {
    assert_eq!(ui_text::BANNER_LEAD, "Full Disk Access is off.");
    assert_eq!(
        ui_text::BANNER_TEXT,
        "Desktop, Documents, Downloads, iCloud Drive, Mail and other protected folders are left \
         out of the index until you grant it."
    );
    assert_eq!(ui_text::BANNER_GRANT, "Grant Full Disk Access\u{2026}");
    assert_eq!(ui_text::BANNER_NOT_NOW, "Not Now");
}

#[test]
fn the_channel_tag() {
    let build = |channel, number| BuildInfo {
        channel,
        number,
        version: "0.2.0",
    };
    assert_eq!(build(Channel::Release, 0).channel_tag(), None);
    assert_eq!(
        build(Channel::Rc, 1).channel_tag(),
        Some(ChannelTag {
            label: "0.2.0-RC-1".into(),
            fill: Token::Green,
            text: Token::OnGreen,
        })
    );
    assert_eq!(
        build(Channel::Beta, 2).channel_tag(),
        Some(ChannelTag {
            label: "0.2.0-beta-2".into(),
            fill: Token::Vol,
            text: Token::OnVol,
        })
    );
    // Set in capitals, as on the Components board.
    assert_eq!(
        build(Channel::Beta, 2)
            .channel_tag()
            .unwrap()
            .label
            .to_uppercase(),
        "0.2.0-BETA-2"
    );
}

#[test]
fn the_new_settings_copy() {
    use settings_view::*;
    assert_eq!(
        SECTIONS,
        [
            "General",
            "Search",
            "Appearance",
            "App Icon",
            "Full Disk Access"
        ]
    );
    assert_eq!(HOTKEY_LABEL, "Show the search window");
    assert_eq!(HOTKEY_CAPTION, "Press this shortcut in any app.");
    assert_eq!(LOGIN_LABEL, "Launch at Login");
    assert_eq!(
        login_caption("Not-Everything"),
        "Starts Not-Everything in the menu bar when you log in."
    );
    // The canvas's caption, keeping the earlier caption's last sentence.
    assert_eq!(
        YOUR_FILES_FIRST_CAPTION,
        "Lists matches in your home folder, iCloud Drive and cloud storage above system and \
         app files, in every sort. Hidden files and Library are not counted."
    );
    assert_eq!(ACCESS_GRANTED_LABEL, "Granted");
    assert_eq!(
        ACCESS_GRANTED_CAPTION,
        "Every folder is indexed, including Desktop, Documents, Downloads and iCloud Drive."
    );
    assert_eq!(ACCESS_SETTINGS_BUTTON, "Open System Settings");
    // Not drawn on the canvas: derived, for the owner's screenshot review.
    assert_eq!(ACCESS_OFF_LABEL, "Off");
    assert_eq!(
        ACCESS_OFF_CAPTION,
        "Desktop, Documents, Downloads, iCloud Drive, Mail and other protected folders are \
         left out of the index."
    );
    assert_eq!(
        APPEARANCE_NOTE,
        "System follows the light or dark setting of your Mac."
    );
}

#[test]
fn the_settings_footnote() {
    let release = BuildInfo {
        channel: Channel::Release,
        number: 0,
        version: "0.2.0",
    };
    assert_eq!(
        settings_view::footnote(release),
        "Not-Everything 0.2.0 (200) \u{b7} 3645"
    );
    let rc = BuildInfo {
        channel: Channel::Rc,
        number: 1,
        version: "0.3.0",
    };
    assert_eq!(
        settings_view::footnote(rc),
        "Not-Everything Preview 0.3.0-RC-1 (300) \u{b7} 3645"
    );
}
