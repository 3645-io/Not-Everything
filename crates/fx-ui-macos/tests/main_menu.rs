//! Tests for `fx_ui_macos::main_menu`: the app's main menu bar as
//! plain data, so the AppKit layer only renders it.
//!
//! Contract exercised here:
//!
//! - `main_menu(app_name, settings) -> Vec<Submenu>` gives, in order, the
//!   app menu, File, Edit and Window (`MenuKind`). Each `Submenu` has a
//!   `title` and `items`, each item `Item::Separator` or
//!   `Item::Command { title, command }`.
//! - App menu: About <app>, Settings… (only when `settings`), Hide <app>,
//!   Hide Others, Show All, Quit <app>. File: Close Window. Edit: Undo,
//!   Redo, Cut, Copy, Paste, Select All. Window: Minimize, Zoom, Bring All
//!   to Front.
//! - `Command::selector()` is the standard AppKit action (Settings is the
//!   window controller's `showSettings:`, and About is its
//!   `showAbout:`, which shows the standard About panel with the version
//!   label and build number); `Command::shortcut()` is the
//!   standard key equivalent, always with Command (⌘, for Settings, ⌘Q,
//!   ⌘H, ⌥⌘H, ⌘W, ⌘Z, ⇧⌘Z, ⌘X, ⌘C, ⌘V, ⌘A, ⌘M).
//! - The MVP is read-only: no command deletes, renames, moves or trashes a
//!   file.

use fx_ui_macos::main_menu::{Command, Item, MenuKind, Shortcut, Submenu, main_menu};

const APP: &str = "Not-Everything";

fn commands(menu: &Submenu) -> Vec<Command> {
    menu.items
        .iter()
        .filter_map(|i| match i {
            Item::Command { command, .. } => Some(*command),
            Item::Separator => None,
        })
        .collect()
}

fn titles(menu: &Submenu) -> Vec<&str> {
    menu.items
        .iter()
        .filter_map(|i| match i {
            Item::Command { title, .. } => Some(title.as_str()),
            Item::Separator => None,
        })
        .collect()
}

fn cmd(key: &'static str) -> Option<Shortcut> {
    Some(Shortcut {
        key,
        shift: false,
        option: false,
    })
}

#[test]
fn menus_in_order() {
    let menus = main_menu(APP, true);
    let kinds: Vec<MenuKind> = menus.iter().map(|m| m.kind).collect();
    assert_eq!(
        kinds,
        [
            MenuKind::App,
            MenuKind::File,
            MenuKind::Edit,
            MenuKind::Window
        ]
    );
    assert_eq!(menus[0].title, APP);
    assert_eq!(menus[1].title, "File");
    assert_eq!(menus[2].title, "Edit");
    assert_eq!(menus[3].title, "Window");
}

#[test]
fn app_menu_items() {
    let menus = main_menu(APP, true);
    assert_eq!(
        titles(&menus[0]),
        [
            "About Not-Everything",
            "Settings\u{2026}",
            "Hide Not-Everything",
            "Hide Others",
            "Show All",
            "Quit Not-Everything",
        ]
    );
    assert_eq!(
        commands(&menus[0]),
        [
            Command::About,
            Command::Settings,
            Command::Hide,
            Command::HideOthers,
            Command::ShowAll,
            Command::Quit,
        ]
    );
    // About, then Settings, then the Hide group, then Quit, each set apart.
    let separators: Vec<usize> = menus[0]
        .items
        .iter()
        .enumerate()
        .filter(|(_, i)| matches!(i, Item::Separator))
        .map(|(n, _)| n)
        .collect();
    assert_eq!(separators, [1, 3, 7]);
}

#[test]
fn settings_item_only_when_asked() {
    let menus = main_menu(APP, false);
    assert!(!commands(&menus[0]).contains(&Command::Settings));
    assert!(commands(&menus[0]).contains(&Command::Quit));
}

#[test]
fn file_edit_and_window_items() {
    let menus = main_menu(APP, true);
    assert_eq!(titles(&menus[1]), ["Close Window"]);
    assert_eq!(
        titles(&menus[2]),
        ["Undo", "Redo", "Cut", "Copy", "Paste", "Select All"]
    );
    assert_eq!(
        titles(&menus[3]),
        ["Minimize", "Zoom", "Bring All to Front"]
    );
}

#[test]
fn standard_selectors() {
    let expected = [
        (Command::About, "showAbout:"),
        (Command::Settings, "showSettings:"),
        (Command::Hide, "hide:"),
        (Command::HideOthers, "hideOtherApplications:"),
        (Command::ShowAll, "unhideAllApplications:"),
        (Command::Quit, "terminate:"),
        (Command::CloseWindow, "performClose:"),
        (Command::Undo, "undo:"),
        (Command::Redo, "redo:"),
        (Command::Cut, "cut:"),
        (Command::Copy, "copy:"),
        (Command::Paste, "paste:"),
        (Command::SelectAll, "selectAll:"),
        (Command::Minimize, "performMiniaturize:"),
        (Command::Zoom, "performZoom:"),
        (Command::BringAllToFront, "arrangeInFront:"),
    ];
    assert_eq!(Command::ALL.len(), expected.len());
    for (command, selector) in expected {
        assert!(Command::ALL.contains(&command), "{command:?}");
        assert_eq!(command.selector(), selector, "{command:?}");
    }
}

#[test]
fn standard_shortcuts() {
    assert_eq!(Command::Settings.shortcut(), cmd(","));
    assert_eq!(Command::Quit.shortcut(), cmd("q"));
    assert_eq!(Command::Hide.shortcut(), cmd("h"));
    assert_eq!(
        Command::HideOthers.shortcut(),
        Some(Shortcut {
            key: "h",
            shift: false,
            option: true
        })
    );
    assert_eq!(Command::CloseWindow.shortcut(), cmd("w"));
    assert_eq!(Command::Undo.shortcut(), cmd("z"));
    assert_eq!(
        Command::Redo.shortcut(),
        Some(Shortcut {
            key: "z",
            shift: true,
            option: false
        })
    );
    assert_eq!(Command::Cut.shortcut(), cmd("x"));
    assert_eq!(Command::Copy.shortcut(), cmd("c"));
    assert_eq!(Command::Paste.shortcut(), cmd("v"));
    assert_eq!(Command::SelectAll.shortcut(), cmd("a"));
    assert_eq!(Command::Minimize.shortcut(), cmd("m"));
    for none in [
        Command::About,
        Command::ShowAll,
        Command::Zoom,
        Command::BringAllToFront,
    ] {
        assert_eq!(none.shortcut(), None, "{none:?}");
    }
}

#[test]
fn shortcuts_are_unique() {
    let mut seen = Vec::new();
    for command in Command::ALL {
        if let Some(s) = command.shortcut() {
            assert!(!seen.contains(&s), "{command:?} repeats {s:?}");
            seen.push(s);
        }
    }
}

#[test]
fn every_command_is_in_the_menu_once() {
    let menus = main_menu(APP, true);
    let all: Vec<Command> = menus.iter().flat_map(commands).collect();
    for command in Command::ALL {
        assert_eq!(
            all.iter().filter(|&&c| c == command).count(),
            1,
            "{command:?}"
        );
    }
}

#[test]
fn no_destructive_commands() {
    const FORBIDDEN: [&str; 6] = ["delete", "rename", "move", "trash", "remove", "erase"];
    for command in Command::ALL {
        let selector = command.selector().to_lowercase();
        let name = format!("{command:?}").to_lowercase();
        for word in FORBIDDEN {
            assert!(!selector.contains(word), "{selector} looks destructive");
            assert!(!name.contains(word), "{name} looks destructive");
        }
    }
    for menu in main_menu(APP, true) {
        for title in titles(&menu) {
            let title = title.to_lowercase();
            for word in FORBIDDEN {
                assert!(!title.contains(word), "{title} looks destructive");
            }
        }
    }
}
