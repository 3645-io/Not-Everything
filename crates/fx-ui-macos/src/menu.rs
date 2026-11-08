//! The menu-bar item's menu as plain data, so the AppKit layer only renders
//! it and dispatches [`MenuAction`]s.
//!
//! Items, in order: the app's name and version or label, greyed out
//! ("Not-Everything 0.2.0", "Not-Everything Preview 0.2.0-RC-1"), a
//! separator, Show Search Window (showing the summon hotkey), Settings…, a
//! separator, Launch at Login (checked when enabled), Grant Full
//! Disk Access… (only while access is not granted), a separator, Quit.
//!
//! The MVP is read-only: there is deliberately no delete, rename, move or
//! trash action in [`MenuAction`].

#![forbid(unsafe_code)]

use fx_fs::AccessState;

use crate::FixedList;
use crate::build_info::BuildInfo;
use crate::hotkey::HotkeySpec;

/// What a menu item does when chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MenuAction {
    /// Show (and focus) the search window.
    ShowSearchWindow,
    /// Show the Settings window.
    ShowSettings,
    /// Turn launching at login on or off.
    ToggleLaunchAtLogin,
    /// Open the Full Disk Access page in System Settings.
    GrantFullDiskAccess,
    /// Quit the app.
    Quit,
}

impl MenuAction {
    /// Every menu action.
    pub const ALL: FixedList<MenuAction, 5> = FixedList([
        MenuAction::ShowSearchWindow,
        MenuAction::ShowSettings,
        MenuAction::ToggleLaunchAtLogin,
        MenuAction::GrantFullDiskAccess,
        MenuAction::Quit,
    ]);

    /// The item title for this action.
    pub const fn title(self) -> &'static str {
        match self {
            MenuAction::ShowSearchWindow => "Show Search Window",
            MenuAction::ShowSettings => "Settings\u{2026}",
            MenuAction::ToggleLaunchAtLogin => "Launch at Login",
            MenuAction::GrantFullDiskAccess => "Grant Full Disk Access\u{2026}",
            MenuAction::Quit => "Quit",
        }
    }
}

/// The state the menu is built from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MenuState {
    /// The running build, named on the first line
    /// ([`BuildInfo::CURRENT`] in the app).
    pub build: BuildInfo,
    /// The current summon hotkey, shown on Show Search Window.
    pub hotkey: HotkeySpec,
    /// Whether launch at login is enabled.
    pub launch_at_login: bool,
    /// The current Full Disk Access state.
    pub access: AccessState,
}

/// One actionable menu item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MenuEntry {
    /// The item title.
    pub title: &'static str,
    /// What the item does.
    pub action: MenuAction,
    /// Whether the item shows a check mark.
    pub checked: bool,
    /// The global hotkey shown next to the title, if any.
    pub hotkey: Option<HotkeySpec>,
}

impl MenuEntry {
    fn new(action: MenuAction) -> MenuEntry {
        MenuEntry {
            title: action.title(),
            action,
            checked: false,
            hotkey: None,
        }
    }
}

/// A menu item: a line of information, a separator or an action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuItem {
    /// A greyed-out (disabled) line that does nothing.
    Info(String),
    /// A separator line.
    Separator,
    /// An actionable item.
    Action(MenuEntry),
}

/// Builds the menu-bar menu for `state`.
pub fn build_menu(state: &MenuState) -> Vec<MenuItem> {
    let mut items = Vec::with_capacity(9);
    items.push(MenuItem::Info(state.build.name_and_label()));
    items.push(MenuItem::Separator);
    items.push(MenuItem::Action(MenuEntry {
        hotkey: Some(state.hotkey),
        ..MenuEntry::new(MenuAction::ShowSearchWindow)
    }));
    items.push(MenuItem::Action(MenuEntry::new(MenuAction::ShowSettings)));
    items.push(MenuItem::Separator);
    items.push(MenuItem::Action(MenuEntry {
        checked: state.launch_at_login,
        ..MenuEntry::new(MenuAction::ToggleLaunchAtLogin)
    }));
    if state.access != AccessState::Granted {
        items.push(MenuItem::Action(MenuEntry::new(
            MenuAction::GrantFullDiskAccess,
        )));
    }
    items.push(MenuItem::Separator);
    items.push(MenuItem::Action(MenuEntry::new(MenuAction::Quit)));
    items
}
