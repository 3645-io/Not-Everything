//! The app's main menu bar as plain data, so the AppKit layer only
//! renders it.
//!
//! Menus, in order: the app menu (About, Settings…, Hide, Hide Others, Show
//! All, Quit), File (Close Window), Edit (Undo, Redo, Cut, Copy, Paste,
//! Select All) and Window (Minimize, Zoom, Bring All to Front). Every item
//! sends a standard AppKit action down the responder chain, except About and
//! Settings, which the window controller answers (About shows the standard
//! About panel with the version label and build number,
//! [`crate::build_info::BuildInfo::about`]). Copy copies the search
//! field's selected text, or, while the results table has the focus, the
//! selected files themselves, as Finder's Copy does (the table answers
//! `copy:`); Option+Cmd+C copies their full paths.
//!
//! The MVP is read-only: there is deliberately no delete, rename, move or
//! trash command in [`Command`].

#![forbid(unsafe_code)]

use crate::FixedList;

/// What a main menu item does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    /// Show the standard About panel with the version label and build
    /// number.
    About,
    /// Show the Settings window.
    Settings,
    /// Hide the app.
    Hide,
    /// Hide every other app.
    HideOthers,
    /// Show every hidden app.
    ShowAll,
    /// Quit the app.
    Quit,
    /// Close (hide) the key window.
    CloseWindow,
    /// Undo the last text edit.
    Undo,
    /// Redo the last undone text edit.
    Redo,
    /// Cut the selected text (the results table does not answer it).
    Cut,
    /// Copy the selected text, or the selected files while the results
    /// have the focus.
    Copy,
    /// Paste text.
    Paste,
    /// Select all text or all results.
    SelectAll,
    /// Minimize the key window to the Dock.
    Minimize,
    /// Zoom the key window.
    Zoom,
    /// Bring all the app's windows to the front.
    BringAllToFront,
}

/// A key equivalent: `key` with Command, plus Shift and Option when set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Shortcut {
    /// The key, as AppKit's `keyEquivalent` string (lower case).
    pub key: &'static str,
    /// Whether Shift is held too.
    pub shift: bool,
    /// Whether Option is held too.
    pub option: bool,
}

impl Shortcut {
    const fn cmd(key: &'static str) -> Shortcut {
        Shortcut {
            key,
            shift: false,
            option: false,
        }
    }
}

impl Command {
    /// Every command.
    pub const ALL: FixedList<Command, 16> = FixedList([
        Command::About,
        Command::Settings,
        Command::Hide,
        Command::HideOthers,
        Command::ShowAll,
        Command::Quit,
        Command::CloseWindow,
        Command::Undo,
        Command::Redo,
        Command::Cut,
        Command::Copy,
        Command::Paste,
        Command::SelectAll,
        Command::Minimize,
        Command::Zoom,
        Command::BringAllToFront,
    ]);

    /// The Objective-C action the item sends.
    pub const fn selector(self) -> &'static str {
        match self {
            Command::About => "showAbout:",
            Command::Settings => "showSettings:",
            Command::Hide => "hide:",
            Command::HideOthers => "hideOtherApplications:",
            Command::ShowAll => "unhideAllApplications:",
            Command::Quit => "terminate:",
            Command::CloseWindow => "performClose:",
            Command::Undo => "undo:",
            Command::Redo => "redo:",
            Command::Cut => "cut:",
            Command::Copy => "copy:",
            Command::Paste => "paste:",
            Command::SelectAll => "selectAll:",
            Command::Minimize => "performMiniaturize:",
            Command::Zoom => "performZoom:",
            Command::BringAllToFront => "arrangeInFront:",
        }
    }

    /// The standard key equivalent, if the command has one.
    pub const fn shortcut(self) -> Option<Shortcut> {
        Some(match self {
            Command::Settings => Shortcut::cmd(","),
            Command::Hide => Shortcut::cmd("h"),
            Command::HideOthers => Shortcut {
                key: "h",
                shift: false,
                option: true,
            },
            Command::Quit => Shortcut::cmd("q"),
            Command::CloseWindow => Shortcut::cmd("w"),
            Command::Undo => Shortcut::cmd("z"),
            Command::Redo => Shortcut {
                key: "z",
                shift: true,
                option: false,
            },
            Command::Cut => Shortcut::cmd("x"),
            Command::Copy => Shortcut::cmd("c"),
            Command::Paste => Shortcut::cmd("v"),
            Command::SelectAll => Shortcut::cmd("a"),
            Command::Minimize => Shortcut::cmd("m"),
            Command::About | Command::ShowAll | Command::Zoom | Command::BringAllToFront => {
                return None;
            }
        })
    }
}

/// Which menu of the menu bar a [`Submenu`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MenuKind {
    /// The app menu, titled with the app name.
    App,
    /// File.
    File,
    /// Edit.
    Edit,
    /// Window; AppKit adds the list of open windows to it.
    Window,
}

/// A menu item: a command or a separator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    /// A separator line.
    Separator,
    /// A command.
    Command {
        /// The item title.
        title: String,
        /// What the item does.
        command: Command,
    },
}

/// One menu of the menu bar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Submenu {
    /// Which menu this is.
    pub kind: MenuKind,
    /// The menu title.
    pub title: String,
    /// The items, in order.
    pub items: Vec<Item>,
}

fn item(title: impl Into<String>, command: Command) -> Item {
    Item::Command {
        title: title.into(),
        command,
    }
}

/// The main menu bar for the app named `app_name`. Settings… is included
/// when `settings` is set (the resident app; the measurement modes have no
/// settings).
pub fn main_menu(app_name: &str, settings: bool) -> Vec<Submenu> {
    let mut app = vec![item(format!("About {app_name}"), Command::About)];
    if settings {
        app.push(Item::Separator);
        app.push(item("Settings\u{2026}", Command::Settings));
    }
    app.extend([
        Item::Separator,
        item(format!("Hide {app_name}"), Command::Hide),
        item("Hide Others", Command::HideOthers),
        item("Show All", Command::ShowAll),
        Item::Separator,
        item(format!("Quit {app_name}"), Command::Quit),
    ]);
    vec![
        Submenu {
            kind: MenuKind::App,
            title: app_name.to_string(),
            items: app,
        },
        Submenu {
            kind: MenuKind::File,
            title: "File".to_string(),
            items: vec![item("Close Window", Command::CloseWindow)],
        },
        Submenu {
            kind: MenuKind::Edit,
            title: "Edit".to_string(),
            items: vec![
                item("Undo", Command::Undo),
                item("Redo", Command::Redo),
                Item::Separator,
                item("Cut", Command::Cut),
                item("Copy", Command::Copy),
                item("Paste", Command::Paste),
                item("Select All", Command::SelectAll),
            ],
        },
        Submenu {
            kind: MenuKind::Window,
            title: "Window".to_string(),
            items: vec![
                item("Minimize", Command::Minimize),
                item("Zoom", Command::Zoom),
                Item::Separator,
                item("Bring All to Front", Command::BringAllToFront),
            ],
        },
    ]
}
