//! Tests for `fx_ui_macos::menu`: the menu-bar item's menu as
//! plain data, so the AppKit layer only renders it.
//!
//! Contract exercised here:
//!
//! - `MenuState { build: BuildInfo, hotkey: HotkeySpec, launch_at_login:
//!   bool, access: fx_fs::AccessState }` (public fields) and
//!   `build_menu(&MenuState) -> Vec<MenuItem>`.
//! - `MenuItem::Info(String)` (a greyed-out line that does nothing),
//!   `MenuItem::Separator` or `MenuItem::Action(MenuEntry)`, where
//!   `MenuEntry { title, action: MenuAction, checked: bool, hotkey:
//!   Option<HotkeySpec> }` has public fields.
//! - Items in order: the build's app name and version or label (every
//!   channel: "Not-Everything 0.2.0", "Not-Everything Preview
//!   0.2.0-RC-1"), separator, Show Search Window (showing the summon
//!   hotkey), Settings…, separator, Launch at Login (checked when
//!   enabled), Grant Full Disk Access… (only when access is not `Granted`),
//!   separator, Quit.
//! - `MenuAction::ALL` lists every action. The MVP is read-only: no action
//!   or title deletes, renames, moves, trashes or removes anything.

use fx_channel::Channel;
use fx_fs::AccessState;
use fx_ui_macos::build_info::BuildInfo;
use fx_ui_macos::hotkey::HotkeySpec;
use fx_ui_macos::menu::{MenuAction, MenuEntry, MenuItem, MenuState, build_menu};

const ALL_ACCESS: [AccessState; 3] = [
    AccessState::Granted,
    AccessState::Denied,
    AccessState::Unknown,
];

const RELEASE: BuildInfo = BuildInfo {
    channel: Channel::Release,
    number: 0,
    version: "0.2.0",
};

fn state(access: AccessState, launch_at_login: bool) -> MenuState {
    MenuState {
        build: RELEASE,
        hotkey: HotkeySpec::default(),
        launch_at_login,
        access,
    }
}

fn entry(item: &MenuItem) -> &MenuEntry {
    match item {
        MenuItem::Action(e) => e,
        other => panic!("expected an action item, got {other:?}"),
    }
}

fn is_separator(item: &MenuItem) -> bool {
    matches!(item, MenuItem::Separator)
}

fn actions(items: &[MenuItem]) -> Vec<MenuAction> {
    items
        .iter()
        .filter_map(|i| match i {
            MenuItem::Action(e) => Some(e.action),
            MenuItem::Separator | MenuItem::Info(_) => None,
        })
        .collect()
}

#[test]
fn layout_without_full_disk_access() {
    for access in [AccessState::Denied, AccessState::Unknown] {
        let items = build_menu(&state(access, false));
        assert_eq!(items.len(), 9, "{access:?}");
        assert!(matches!(items[0], MenuItem::Info(_)));
        assert!(is_separator(&items[1]));
        assert_eq!(entry(&items[2]).action, MenuAction::ShowSearchWindow);
        assert_eq!(entry(&items[3]).action, MenuAction::ShowSettings);
        assert!(is_separator(&items[4]));
        assert_eq!(entry(&items[5]).action, MenuAction::ToggleLaunchAtLogin);
        assert_eq!(entry(&items[6]).action, MenuAction::GrantFullDiskAccess);
        assert!(is_separator(&items[7]));
        assert_eq!(entry(&items[8]).action, MenuAction::Quit);
    }
}

#[test]
fn layout_with_full_disk_access_hides_grant_item() {
    let items = build_menu(&state(AccessState::Granted, false));
    assert_eq!(items.len(), 8);
    assert!(matches!(items[0], MenuItem::Info(_)));
    assert!(is_separator(&items[1]));
    assert_eq!(entry(&items[2]).action, MenuAction::ShowSearchWindow);
    assert_eq!(entry(&items[3]).action, MenuAction::ShowSettings);
    assert!(is_separator(&items[4]));
    assert_eq!(entry(&items[5]).action, MenuAction::ToggleLaunchAtLogin);
    assert!(is_separator(&items[6]));
    assert_eq!(entry(&items[7]).action, MenuAction::Quit);
    assert!(!actions(&items).contains(&MenuAction::GrantFullDiskAccess));
}

#[test]
fn titles() {
    let items = build_menu(&state(AccessState::Denied, false));
    assert_eq!(entry(&items[2]).title, "Show Search Window");
    assert_eq!(entry(&items[3]).title, "Settings\u{2026}");
    assert_eq!(entry(&items[5]).title, "Launch at Login");
    assert_eq!(entry(&items[6]).title, "Grant Full Disk Access\u{2026}");
    assert!(entry(&items[8]).title.starts_with("Quit"));
    for item in &items {
        if let MenuItem::Action(e) = item {
            assert!(!e.title.to_lowercase().contains("everything"));
        }
    }
}

#[test]
fn first_line_names_the_build_in_every_channel() {
    let cases = [
        (RELEASE, "Not-Everything 0.2.0"),
        (
            BuildInfo {
                channel: Channel::Rc,
                number: 1,
                version: "0.2.0",
            },
            "Not-Everything Preview 0.2.0-RC-1",
        ),
        (
            BuildInfo {
                channel: Channel::Beta,
                number: 2,
                version: "0.3.0",
            },
            "Not-Everything Preview 0.3.0-beta-2",
        ),
    ];
    for (build, line) in cases {
        for access in ALL_ACCESS {
            let items = build_menu(&MenuState {
                build,
                ..state(access, false)
            });
            assert_eq!(items[0], MenuItem::Info(line.to_string()), "{build:?}");
            assert!(is_separator(&items[1]), "{build:?}");
            let infos = items
                .iter()
                .filter(|i| matches!(i, MenuItem::Info(_)))
                .count();
            assert_eq!(infos, 1, "{build:?}");
        }
    }
}

#[test]
fn show_item_shows_the_summon_hotkey() {
    let custom = HotkeySpec::parse("cmd+shift+space").unwrap();
    for hotkey in [HotkeySpec::default(), custom] {
        let items = build_menu(&MenuState {
            build: RELEASE,
            hotkey,
            launch_at_login: false,
            access: AccessState::Granted,
        });
        assert_eq!(entry(&items[2]).hotkey, Some(hotkey));
    }
}

#[test]
fn settings_item_shows_no_global_hotkey() {
    let items = build_menu(&state(AccessState::Granted, false));
    assert_eq!(entry(&items[3]).hotkey, None);
    assert!(!entry(&items[3]).checked);
}

#[test]
fn launch_at_login_checked_state() {
    for access in ALL_ACCESS {
        let on = build_menu(&state(access, true));
        assert!(entry(&on[5]).checked, "{access:?}");
        let off = build_menu(&state(access, false));
        assert!(!entry(&off[5]).checked, "{access:?}");
    }
}

#[test]
fn only_launch_at_login_is_checkable() {
    let items = build_menu(&state(AccessState::Denied, true));
    for item in &items {
        if let MenuItem::Action(e) = item {
            if e.action != MenuAction::ToggleLaunchAtLogin {
                assert!(!e.checked, "{:?} should not be checked", e.action);
            }
        }
    }
}

#[test]
fn all_lists_every_action_the_menu_uses() {
    for access in ALL_ACCESS {
        for launch in [false, true] {
            for action in actions(&build_menu(&state(access, launch))) {
                assert!(MenuAction::ALL.contains(&action), "{action:?}");
            }
        }
    }
    for action in [
        MenuAction::ShowSearchWindow,
        MenuAction::ShowSettings,
        MenuAction::ToggleLaunchAtLogin,
        MenuAction::GrantFullDiskAccess,
        MenuAction::Quit,
    ] {
        assert!(MenuAction::ALL.contains(&action), "{action:?}");
    }
}

#[test]
fn menu_has_no_destructive_actions() {
    const FORBIDDEN: [&str; 6] = ["delete", "rename", "move", "trash", "remove", "erase"];
    for action in MenuAction::ALL.iter() {
        let name = format!("{action:?}").to_lowercase();
        for word in FORBIDDEN {
            assert!(!name.contains(word), "action {name} looks destructive");
        }
    }
    for access in ALL_ACCESS {
        for launch in [false, true] {
            for item in build_menu(&state(access, launch)) {
                if let MenuItem::Action(e) = item {
                    let title = e.title.to_lowercase();
                    for word in FORBIDDEN {
                        assert!(!title.contains(word), "title {title} looks destructive");
                    }
                }
            }
        }
    }
}
