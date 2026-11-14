//! Contract tests for `fx_ui_macos::context_menu`: the right-click menu on
//! the results, after the mockups' "Right-click a file" and "Right-click a
//! folder" boards (chosen 9 Nov 2026), in Finder's order, without the
//! boards' Move to Trash and Rename (the app is read-only).
//!
//! - What the menu acts on: the whole selection when the click is inside
//!   it, the clicked row alone when it is outside, nothing (no menu) on
//!   empty space; from the keyboard or VoiceOver, the selection.
//! - Which items show: Open With for files only (a package counts as a
//!   file), New Terminal at Folder for folders only, neither for a mixed
//!   selection or an app; items that don't apply are hidden, and the
//!   separators around them collapse.
//! - The shortcuts the menu shows are the ones the results answer, and the
//!   status bar's hints show the same caps.
//! - Open With's apps: the default first ("Preview (default)"), then the
//!   others alphabetically, then App Store… and Other….

use fx_ui_macos::actions::{self, Action, Key, Modifiers};
use fx_ui_macos::context_menu::{
    AppChoice, ContextItem, MAX_KIND_CHECKS, MAX_SHARE_ITEMS, MenuEntry, MenuRequest, MenuTargets,
    OpenWithEntry, TargetKind, TargetMix, common_apps, common_default, is_application_bundle,
    item_limit, menu_entries, menu_targets, open_with_entries, target_mix,
};
use fx_ui_macos::controller::{MAX_COPY_PATHS, MAX_KEPT_SELECTION};
use fx_ui_macos::key_caps::RESULT_HINTS;
use fx_ui_macos::quick_look::MAX_PREVIEW_ITEMS;

use ContextItem::*;

fn item(i: ContextItem) -> MenuEntry {
    MenuEntry::Item(i)
}

const SEP: MenuEntry = MenuEntry::Separator;

// ------------------------------------------------------------- targets

#[test]
fn a_click_inside_the_selection_acts_on_the_whole_selection() {
    let request = MenuRequest::Pointer {
        row: Some(4),
        row_selected: true,
    };
    assert_eq!(menu_targets(request, 3), Some(MenuTargets::Selection));
    assert_eq!(MenuTargets::Selection.count(3), 3);
}

#[test]
fn a_click_outside_the_selection_acts_on_the_clicked_row_only() {
    for selected in [0, 1, 250] {
        let request = MenuRequest::Pointer {
            row: Some(7),
            row_selected: false,
        };
        assert_eq!(
            menu_targets(request, selected),
            Some(MenuTargets::Row(7)),
            "{selected} selected"
        );
    }
    assert_eq!(MenuTargets::Row(7).count(250), 1);
}

#[test]
fn a_click_on_empty_space_shows_no_menu() {
    for selected in [0, 1, 250] {
        for row_selected in [false, true] {
            let request = MenuRequest::Pointer {
                row: None,
                row_selected,
            };
            assert_eq!(menu_targets(request, selected), None, "{selected}");
        }
    }
}

#[test]
fn the_keyboard_and_voiceover_act_on_the_selection() {
    assert_eq!(
        menu_targets(MenuRequest::Keyboard, 1),
        Some(MenuTargets::Selection)
    );
    assert_eq!(
        menu_targets(MenuRequest::Keyboard, 40),
        Some(MenuTargets::Selection)
    );
    // Nothing selected: nothing to act on, so no menu.
    assert_eq!(menu_targets(MenuRequest::Keyboard, 0), None);
}

// ----------------------------------------------------------- the items

#[test]
fn files_get_the_file_board() {
    let want = vec![
        item(Open),
        item(OpenWith),
        item(RevealInFinder),
        SEP,
        item(QuickLook),
        item(Copy),
        item(CopyPath),
        SEP,
        item(Share),
    ];
    assert_eq!(menu_entries(TargetMix::Files), want);
    assert_eq!(target_mix([TargetKind::File]), TargetMix::Files);
    assert_eq!(
        target_mix([TargetKind::File, TargetKind::File, TargetKind::File]),
        TargetMix::Files
    );
}

#[test]
fn folders_get_the_folder_board() {
    let want = vec![
        item(Open),
        item(RevealInFinder),
        SEP,
        item(QuickLook),
        item(Copy),
        item(CopyPath),
        SEP,
        item(Share),
        SEP,
        item(NewTerminal),
    ];
    assert_eq!(menu_entries(TargetMix::Folders), want);
    assert_eq!(target_mix([TargetKind::Folder]), TargetMix::Folders);
    assert_eq!(
        target_mix([TargetKind::Folder, TargetKind::Folder]),
        TargetMix::Folders
    );
}

#[test]
fn a_mixed_selection_gets_neither_open_with_nor_terminal() {
    let want = vec![
        item(Open),
        item(RevealInFinder),
        SEP,
        item(QuickLook),
        item(Copy),
        item(CopyPath),
        SEP,
        item(Share),
    ];
    assert_eq!(menu_entries(TargetMix::Other), want);
    assert_eq!(
        target_mix([TargetKind::File, TargetKind::Folder]),
        TargetMix::Other
    );
    assert_eq!(
        target_mix([TargetKind::Folder, TargetKind::Package]),
        TargetMix::Other
    );
}

#[test]
fn a_package_counts_as_a_file() {
    // A package (a .pages document, an .rtfd) is a folder on disk that
    // Finder shows and opens as one file.
    assert_eq!(target_mix([TargetKind::Package]), TargetMix::Files);
    assert_eq!(
        target_mix([TargetKind::File, TargetKind::Package]),
        TargetMix::Files
    );
}

#[test]
fn an_app_gets_neither_open_with_nor_terminal() {
    assert_eq!(target_mix([TargetKind::Application]), TargetMix::Other);
    assert_eq!(
        target_mix([TargetKind::File, TargetKind::Application]),
        TargetMix::Other
    );
    assert_eq!(
        target_mix([TargetKind::Folder, TargetKind::Application]),
        TargetMix::Other
    );
}

#[test]
fn unknown_kinds_get_neither() {
    // A selection too large to check (more than MAX_KIND_CHECKS) has no
    // kinds to go on.
    assert_eq!(target_mix(std::iter::empty()), TargetMix::Other);
    assert_eq!(MAX_KIND_CHECKS, MAX_KEPT_SELECTION);
}

#[test]
fn no_menu_starts_or_ends_with_a_separator_or_has_two_in_a_row() {
    for mix in [TargetMix::Files, TargetMix::Folders, TargetMix::Other] {
        let entries = menu_entries(mix);
        assert_ne!(entries.first(), Some(&SEP), "{mix:?}");
        assert_ne!(entries.last(), Some(&SEP), "{mix:?}");
        for pair in entries.windows(2) {
            assert!(!(pair[0] == SEP && pair[1] == SEP), "{mix:?}: {entries:?}");
        }
    }
}

#[test]
fn the_titles_are_finders() {
    let titles: Vec<&str> = ContextItem::ALL.iter().map(|i| i.title()).collect();
    assert_eq!(
        titles,
        [
            "Open",
            "Open With",
            "Reveal in Finder",
            "Quick Look",
            "Copy",
            "Copy Full Path",
            "Share\u{2026}",
            "New Terminal at Folder",
        ]
    );
}

#[test]
fn every_item_carries_the_boards_glyph() {
    let glyphs: Vec<&str> = ContextItem::ALL.iter().map(|i| i.glyph()).collect();
    assert_eq!(
        glyphs,
        [
            "open", "openWith", "reveal", "look", "copy", "path", "share", "terminal"
        ]
    );
    for i in ContextItem::ALL {
        assert!(!i.symbol().is_empty(), "{i:?} has no SF Symbol");
    }
}

#[test]
fn tags_round_trip() {
    for i in ContextItem::ALL {
        assert_eq!(ContextItem::from_tag(i.tag()), Some(i), "{i:?}");
    }
    assert_eq!(ContextItem::from_tag(-1), None);
    assert_eq!(ContextItem::from_tag(99), None);
}

#[test]
fn no_destructive_item() {
    // The boards' Move to Trash and Rename wait for run 2.13 and the owner
    // lifting the read-only rule.
    for i in ContextItem::ALL {
        let words = format!("{i:?} {}", i.title()).to_lowercase();
        for banned in ["delete", "remove", "rename", "move", "trash", "erase"] {
            assert!(!words.contains(banned), "{i:?} looks destructive");
        }
    }
}

// ----------------------------------------------------------- shortcuts

fn plain() -> Modifiers {
    Modifiers::default()
}

fn cmd() -> Modifiers {
    Modifiers {
        command: true,
        ..Modifiers::default()
    }
}

fn option_cmd() -> Modifiers {
    Modifiers {
        option: true,
        ..cmd()
    }
}

#[test]
fn the_menu_shows_finders_shortcuts() {
    let shown: Vec<(ContextItem, Vec<&str>)> = ContextItem::ALL
        .iter()
        .filter_map(|&i| i.shortcut().map(|s| (i, s.caps())))
        .collect();
    assert_eq!(
        shown,
        [
            (Open, vec!["↩"]),
            (RevealInFinder, vec!["⌘", "↩"]),
            (QuickLook, vec!["Space"]),
            (Copy, vec!["⌘", "C"]),
            (CopyPath, vec!["⌥", "⌘", "C"]),
        ]
    );
    for i in [OpenWith, Share, NewTerminal] {
        assert_eq!(i.shortcut(), None, "{i:?}");
    }
}

#[test]
fn every_shortcut_the_menu_shows_does_what_the_item_does() {
    for i in ContextItem::ALL {
        let Some(s) = i.shortcut() else { continue };
        let action = i.action().expect("an item with a shortcut has an action");
        assert_eq!(
            actions::action_for_key(s.key, s.mods),
            Some(action),
            "{i:?}"
        );
    }
}

#[test]
fn the_key_equivalents_are_appkits() {
    let equivalents: Vec<(ContextItem, &str, bool, bool)> = ContextItem::ALL
        .iter()
        .filter_map(|&i| {
            i.shortcut()
                .map(|s| (i, s.key_equivalent(), s.mods.command, s.mods.option))
        })
        .collect();
    assert_eq!(
        equivalents,
        [
            (Open, "\r", false, false),
            (RevealInFinder, "\r", true, false),
            (QuickLook, " ", false, false),
            (Copy, "c", true, false),
            (CopyPath, "c", true, true),
        ]
    );
}

#[test]
fn the_status_bar_hints_show_the_menus_caps() {
    for hint in RESULT_HINTS {
        let i = ContextItem::ALL
            .iter()
            .copied()
            .find(|i| i.action() == Some(hint.action))
            .unwrap_or_else(|| panic!("no menu item for {:?}", hint.action));
        let caps = i.shortcut().expect("a hinted item has a shortcut").caps();
        assert_eq!(caps, hint.keys, "{:?}", hint.action);
    }
}

#[test]
fn the_items_map_to_the_read_only_actions() {
    let mapped: Vec<(ContextItem, Option<Action>)> =
        ContextItem::ALL.iter().map(|&i| (i, i.action())).collect();
    assert_eq!(
        mapped,
        [
            (Open, Some(Action::Open)),
            (OpenWith, None),
            (RevealInFinder, Some(Action::RevealInFinder)),
            (QuickLook, Some(Action::QuickLook)),
            (Copy, Some(Action::Copy)),
            (CopyPath, Some(Action::CopyPath)),
            (Share, None),
            (NewTerminal, None),
        ]
    );
}

#[test]
fn cmd_c_copies_the_files_in_the_results_only() {
    assert_eq!(
        actions::action_for_key(Key::Char('c'), cmd()),
        Some(Action::Copy)
    );
    // The search field keeps Cmd+C for its text.
    assert_eq!(actions::search_field_key(Key::Char('c'), cmd()), None);
    // Shift and Control give nothing; Option is Copy Full Path.
    for extra in [
        Modifiers {
            shift: true,
            ..cmd()
        },
        Modifiers {
            control: true,
            ..cmd()
        },
    ] {
        assert_eq!(actions::action_for_key(Key::Char('c'), extra), None);
    }
    assert_eq!(
        actions::action_for_key(Key::Char('c'), option_cmd()),
        Some(Action::CopyPath)
    );
    assert_eq!(actions::action_for_key(Key::Char('c'), plain()), None);
}

// -------------------------------------------------------------- limits

#[test]
fn each_item_acts_on_a_bounded_number_of_targets() {
    assert_eq!(item_limit(Open), actions::MAX_OPEN);
    assert_eq!(item_limit(RevealInFinder), actions::MAX_OPEN);
    assert_eq!(item_limit(OpenWith), actions::MAX_OPEN);
    assert_eq!(item_limit(NewTerminal), actions::MAX_OPEN);
    assert_eq!(item_limit(QuickLook), MAX_PREVIEW_ITEMS);
    assert_eq!(item_limit(Copy), MAX_COPY_PATHS);
    assert_eq!(item_limit(CopyPath), MAX_COPY_PATHS);
    assert_eq!(item_limit(Share), MAX_SHARE_ITEMS);
    assert_eq!(actions::MAX_OPEN, 20);
    assert_eq!(MAX_SHARE_ITEMS, 1_000);
}

// ----------------------------------------------------------- Open With

fn app(name: &str, version: Option<&str>, path: &str) -> AppChoice {
    AppChoice {
        name: name.into(),
        version: version.map(Into::into),
        path: path.into(),
    }
}

fn titles(entries: &[OpenWithEntry]) -> Vec<String> {
    entries
        .iter()
        .map(|e| match e {
            OpenWithEntry::App { title, .. } => title.clone(),
            OpenWithEntry::Separator => "--".into(),
            OpenWithEntry::AppStore => "App Store\u{2026}".into(),
            OpenWithEntry::Other => "Other\u{2026}".into(),
        })
        .collect()
}

#[test]
fn open_with_lists_the_default_first_then_alphabetically() {
    let apps = vec![
        app(
            "TextEdit",
            Some("1.20"),
            "/System/Applications/TextEdit.app",
        ),
        app("Preview", Some("11.0"), "/System/Applications/Preview.app"),
        app("Xcode", Some("26.0"), "/Applications/Xcode.app"),
        app("BBEdit", Some("15.5"), "/Applications/BBEdit.app"),
        app("affinity", None, "/Applications/affinity.app"),
    ];
    let entries = open_with_entries(Some("/System/Applications/Preview.app"), apps);
    assert_eq!(
        titles(&entries),
        [
            "Preview (default)",
            "affinity",
            "BBEdit",
            "TextEdit",
            "Xcode",
            "--",
            "App Store…",
            "Other…",
        ]
    );
    assert_eq!(
        entries[0],
        OpenWithEntry::App {
            title: "Preview (default)".into(),
            path: "/System/Applications/Preview.app".into(),
            default: true,
        }
    );
    assert!(matches!(
        &entries[1],
        OpenWithEntry::App { default: false, .. }
    ));
}

#[test]
fn open_with_shows_versions_of_same_name_apps() {
    let apps = vec![
        app("Xcode", Some("26.0"), "/Applications/Xcode.app"),
        app("Xcode", Some("16.4"), "/Applications/Xcode-16.app"),
        app(
            "TextEdit",
            Some("1.20"),
            "/System/Applications/TextEdit.app",
        ),
    ];
    let entries = open_with_entries(None, apps);
    assert_eq!(
        titles(&entries),
        [
            "TextEdit",
            "Xcode (16.4)",
            "Xcode (26.0)",
            "--",
            "App Store…",
            "Other…"
        ]
    );
    // Without a version, the app's folder tells them apart.
    let apps = vec![
        app("Tool", None, "/Applications/Tool.app"),
        app("Tool", None, "/Users/me/Applications/Tool.app"),
    ];
    assert_eq!(
        titles(&open_with_entries(None, apps))[..2],
        ["Tool (/Applications)", "Tool (/Users/me/Applications)"]
    );
}

#[test]
fn the_default_keeps_its_plain_name_beside_a_same_name_app() {
    let apps = vec![
        app("Xcode", Some("16.4"), "/Applications/Xcode-16.app"),
        app("Xcode", Some("26.0"), "/Applications/Xcode.app"),
    ];
    let entries = open_with_entries(Some("/Applications/Xcode.app"), apps);
    assert_eq!(titles(&entries)[..2], ["Xcode (default)", "Xcode (16.4)"]);
}

#[test]
fn open_with_drops_duplicate_apps() {
    let apps = vec![
        app("Preview", Some("11.0"), "/System/Applications/Preview.app"),
        app("Preview", Some("11.0"), "/System/Applications/Preview.app"),
    ];
    assert_eq!(
        titles(&open_with_entries(None, apps)),
        ["Preview", "--", "App Store…", "Other…"]
    );
}

#[test]
fn open_with_without_apps_still_offers_the_store_and_other() {
    assert_eq!(
        titles(&open_with_entries(None, Vec::new())),
        ["App Store…", "Other…"]
    );
    // A default the list doesn't hold isn't shown.
    assert_eq!(
        titles(&open_with_entries(
            Some("/Applications/Gone.app"),
            Vec::new()
        )),
        ["App Store…", "Other…"]
    );
}

#[test]
fn open_with_offers_the_apps_every_target_can_open() {
    let a = vec!["/A.app".to_string(), "/B.app".into(), "/C.app".into()];
    let b = vec!["/C.app".to_string(), "/A.app".into()];
    assert_eq!(common_apps(&[a.clone(), b]), ["/A.app", "/C.app"]);
    assert_eq!(common_apps(std::slice::from_ref(&a)), a);
    assert!(common_apps(&[]).is_empty());
    assert!(common_apps(&[a, Vec::new()]).is_empty());
}

#[test]
fn the_default_is_marked_only_when_every_target_shares_it() {
    let p = || Some("/System/Applications/Preview.app".to_string());
    assert_eq!(common_default(&[p(), p()]), p());
    assert_eq!(
        common_default(&[p(), Some("/Applications/Xcode.app".into())]),
        None
    );
    assert_eq!(common_default(&[p(), None]), None);
    assert_eq!(common_default(&[]), None);
}

#[test]
fn other_offers_applications_only() {
    assert!(is_application_bundle("/Applications/Preview.app"));
    assert!(is_application_bundle(
        "/Applications/Utilities/Terminal.APP"
    ));
    assert!(!is_application_bundle("/Applications/Utilities"));
    assert!(!is_application_bundle("/Users/me/notes.app.txt"));
    assert!(!is_application_bundle(".app"));
}
