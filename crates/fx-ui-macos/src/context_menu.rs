//! The right-click menu on the results as plain data, so the AppKit layer
//! only renders it and acts on the item chosen.
//!
//! The mockups' "Right-click a file" and "Right-click a folder" boards
//! (chosen 9 Nov 2026), in Finder's order. The boards' Move to Trash and
//! Rename are left out, since the app is read-only, and the separators
//! around them collapse to one:
//!
//! - File(s): Open, Open With ▸, Reveal in Finder, ──, Quick Look, Copy,
//!   Copy Full Path, ──, Share….
//! - Folder(s): Open, Reveal in Finder, ──, Quick Look, Copy, Copy Full
//!   Path, ──, Share…, ──, New Terminal at Folder.
//!
//! A mixed selection, an app or a selection too large to check gets
//! neither Open With nor New Terminal at Folder ([`target_mix`]). Items
//! that don't apply are left out, not greyed.
//!
//! The menu acts on the clicked row, or on the whole selection when the
//! click is inside it ([`menu_targets`]). The shortcuts it shows
//! ([`ContextItem::shortcut`]) are the ones the results answer
//! ([`crate::actions::action_for_key`]) and the status bar's hints show.
//!
//! Open With ▸ lists the apps that can open every target
//! ([`common_apps`]), the default first ("Preview (default)"), then the
//! others alphabetically, then App Store… and Other…
//! ([`open_with_entries`]).
//!
//! There is deliberately no delete, rename, move or trash item in
//! [`ContextItem`].

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::FixedList;
use crate::actions::{Action, Key, MAX_OPEN, Modifiers};
use crate::controller::{MAX_COPY_PATHS, MAX_KEPT_SELECTION};
use crate::key_caps::{COMMAND, OPTION, RETURN};
use crate::quick_look::MAX_PREVIEW_ITEMS;

/// Where the menu was asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MenuRequest {
    /// A right-click (or Control-click) on the table: `row` is the row
    /// under the pointer (`None` on empty space) and `row_selected` whether
    /// it is selected.
    Pointer {
        /// The row clicked, if any.
        row: Option<usize>,
        /// Whether that row is selected.
        row_selected: bool,
    },
    /// The keyboard or VoiceOver's Show Menu, with no row under a pointer.
    Keyboard,
}

/// What the menu acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MenuTargets {
    /// Every selected row.
    Selection,
    /// One row outside the selection, which stays as it is.
    Row(usize),
}

impl MenuTargets {
    /// How many rows the menu acts on, with `selected` rows selected.
    pub fn count(self, selected: usize) -> usize {
        match self {
            MenuTargets::Selection => selected,
            MenuTargets::Row(_) => 1,
        }
    }
}

/// What a menu asked for by `request` acts on, with `selected` rows
/// selected; `None` shows no menu.
///
/// - A click on a selected row: the whole selection, as in Finder.
/// - A click on a row outside the selection: that row alone; the selection
///   stays as it is.
/// - A click on empty space: nothing, so no menu.
/// - The keyboard or VoiceOver: the selection, or nothing when none.
pub fn menu_targets(request: MenuRequest, selected: usize) -> Option<MenuTargets> {
    match request {
        MenuRequest::Pointer { row: None, .. } => None,
        MenuRequest::Pointer {
            row: Some(_),
            row_selected: true,
        } => Some(MenuTargets::Selection),
        MenuRequest::Pointer {
            row: Some(row),
            row_selected: false,
        } => Some(MenuTargets::Row(row)),
        MenuRequest::Keyboard => (selected > 0).then_some(MenuTargets::Selection),
    }
}

/// What a target is on disk, as far as the menu cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TargetKind {
    /// A file.
    File,
    /// A folder that isn't a package.
    Folder,
    /// A package other than an app (a `.pages` document, an `.rtfd`): a
    /// folder on disk that Finder shows and opens as one file.
    Package,
    /// An app.
    Application,
}

/// Most targets whose kinds the menu reads (on disk) when it opens: as many
/// as the window keeps selected across refreshes, whose paths come without
/// a search. A larger selection shows neither Open With nor New Terminal at
/// Folder.
pub const MAX_KIND_CHECKS: usize = MAX_KEPT_SELECTION;

/// What the targets are, together.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TargetMix {
    /// Every target is a file or a package: Open With shows.
    Files,
    /// Every target is a folder: New Terminal at Folder shows.
    Folders,
    /// A mix, an app, or kinds not known: neither shows.
    Other,
}

/// What `kinds` are together ([`TargetMix`]). No kinds at all (a selection
/// too large to check) is [`TargetMix::Other`].
pub fn target_mix(kinds: impl IntoIterator<Item = TargetKind>) -> TargetMix {
    let mut mix = None;
    for kind in kinds {
        let this = match kind {
            TargetKind::File | TargetKind::Package => TargetMix::Files,
            TargetKind::Folder => TargetMix::Folders,
            TargetKind::Application => return TargetMix::Other,
        };
        match mix {
            None => mix = Some(this),
            Some(m) if m == this => {}
            Some(_) => return TargetMix::Other,
        }
    }
    mix.unwrap_or(TargetMix::Other)
}

/// An item of the right-click menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ContextItem {
    /// Open with the default app.
    Open,
    /// The Open With submenu (files only).
    OpenWith,
    /// Reveal in Finder.
    RevealInFinder,
    /// Show in the Quick Look panel.
    QuickLook,
    /// Copy the files to the pasteboard, as Finder's Copy does.
    Copy,
    /// Copy the full paths, one per line.
    CopyPath,
    /// The system's share picker.
    Share,
    /// Open a Terminal window at each folder (folders only).
    NewTerminal,
}

/// A shortcut an item shows: the key and the modifiers held with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MenuShortcut {
    /// The key.
    pub key: Key,
    /// The modifiers.
    pub mods: Modifiers,
}

impl MenuShortcut {
    const fn new(key: Key, command: bool, option: bool) -> MenuShortcut {
        MenuShortcut {
            key,
            mods: Modifiers {
                command,
                shift: false,
                option,
                control: false,
            },
        }
    }

    /// AppKit's `keyEquivalent` for the key: `"\r"` for Return, `" "` for
    /// the space bar, the lowercase letter otherwise.
    pub fn key_equivalent(self) -> &'static str {
        match self.key {
            Key::Return | Key::Enter => "\r",
            Key::Space => " ",
            Key::Char('c') => "c",
            _ => "",
        }
    }

    /// The caps, as the status bar's hints show them: the modifiers in
    /// Apple's order, then the key.
    pub fn caps(self) -> Vec<&'static str> {
        let mut caps = Vec::new();
        if self.mods.option {
            caps.push(OPTION);
        }
        if self.mods.command {
            caps.push(COMMAND);
        }
        caps.push(match self.key {
            Key::Return | Key::Enter => RETURN,
            Key::Space => "Space",
            Key::Char('c') => "C",
            _ => "",
        });
        caps
    }
}

impl ContextItem {
    /// Every item, in the boards' order.
    pub const ALL: FixedList<ContextItem, 8> = FixedList([
        ContextItem::Open,
        ContextItem::OpenWith,
        ContextItem::RevealInFinder,
        ContextItem::QuickLook,
        ContextItem::Copy,
        ContextItem::CopyPath,
        ContextItem::Share,
        ContextItem::NewTerminal,
    ]);

    /// The item's title, as Finder writes it.
    pub const fn title(self) -> &'static str {
        match self {
            ContextItem::Open => "Open",
            ContextItem::OpenWith => "Open With",
            ContextItem::RevealInFinder => "Reveal in Finder",
            ContextItem::QuickLook => "Quick Look",
            ContextItem::Copy => "Copy",
            ContextItem::CopyPath => "Copy Full Path",
            ContextItem::Share => "Share\u{2026}",
            ContextItem::NewTerminal => "New Terminal at Folder",
        }
    }

    /// The boards' glyph for the item.
    pub const fn glyph(self) -> &'static str {
        match self {
            ContextItem::Open => "open",
            ContextItem::OpenWith => "openWith",
            ContextItem::RevealInFinder => "reveal",
            ContextItem::QuickLook => "look",
            ContextItem::Copy => "copy",
            ContextItem::CopyPath => "path",
            ContextItem::Share => "share",
            ContextItem::NewTerminal => "terminal",
        }
    }

    /// The SF Symbol closest to the boards' glyph, shown as a template
    /// image: a native menu can't draw the boards' own glyph paths.
    pub const fn symbol(self) -> &'static str {
        match self {
            ContextItem::Open => "arrow.up.forward.app",
            ContextItem::OpenWith => "square.grid.2x2",
            ContextItem::RevealInFinder => "folder",
            ContextItem::QuickLook => "eye",
            ContextItem::Copy => "doc.on.doc",
            ContextItem::CopyPath => "point.topleft.down.to.point.bottomright.curvepath",
            ContextItem::Share => "square.and.arrow.up",
            ContextItem::NewTerminal => "terminal",
        }
    }

    /// The read-only action the item performs, for those the results also
    /// answer from the keyboard.
    pub const fn action(self) -> Option<Action> {
        match self {
            ContextItem::Open => Some(Action::Open),
            ContextItem::RevealInFinder => Some(Action::RevealInFinder),
            ContextItem::QuickLook => Some(Action::QuickLook),
            ContextItem::Copy => Some(Action::Copy),
            ContextItem::CopyPath => Some(Action::CopyPath),
            ContextItem::OpenWith | ContextItem::Share | ContextItem::NewTerminal => None,
        }
    }

    /// The shortcut the item shows: Finder's, and the one the results
    /// answer. Space is Quick Look's key in the results, as in Finder.
    pub const fn shortcut(self) -> Option<MenuShortcut> {
        Some(match self {
            ContextItem::Open => MenuShortcut::new(Key::Return, false, false),
            ContextItem::RevealInFinder => MenuShortcut::new(Key::Return, true, false),
            ContextItem::QuickLook => MenuShortcut::new(Key::Space, false, false),
            ContextItem::Copy => MenuShortcut::new(Key::Char('c'), true, false),
            ContextItem::CopyPath => MenuShortcut::new(Key::Char('c'), true, true),
            ContextItem::OpenWith | ContextItem::Share | ContextItem::NewTerminal => {
                return None;
            }
        })
    }

    /// The item's tag on its `NSMenuItem`: its place in [`ContextItem::ALL`].
    pub fn tag(self) -> isize {
        ContextItem::ALL
            .iter()
            .position(|&i| i == self)
            .and_then(|i| isize::try_from(i).ok())
            .unwrap_or(-1)
    }

    /// The item with `tag`, if any.
    pub fn from_tag(tag: isize) -> Option<ContextItem> {
        let i = usize::try_from(tag).ok()?;
        ContextItem::ALL.as_slice().get(i).copied()
    }
}

/// Most targets Share… hands the share picker: as many as Quick Look
/// shows.
pub const MAX_SHARE_ITEMS: usize = MAX_PREVIEW_ITEMS;

/// Most targets `item` acts on; the first ones in row order.
///
/// Open, Open With, Reveal in Finder and New Terminal at Folder take at
/// most [`MAX_OPEN`], so a large selection can't open thousands of windows;
/// Quick Look and Share… [`MAX_PREVIEW_ITEMS`]; Copy and Copy Full Path
/// [`MAX_COPY_PATHS`], with a note in the status bar when they stop there.
pub const fn item_limit(item: ContextItem) -> usize {
    match item {
        ContextItem::Open
        | ContextItem::OpenWith
        | ContextItem::RevealInFinder
        | ContextItem::NewTerminal => MAX_OPEN,
        ContextItem::QuickLook => MAX_PREVIEW_ITEMS,
        ContextItem::Copy | ContextItem::CopyPath => MAX_COPY_PATHS,
        ContextItem::Share => MAX_SHARE_ITEMS,
    }
}

/// An entry of the menu: an item or a separator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MenuEntry {
    /// An item.
    Item(ContextItem),
    /// A separator line.
    Separator,
}

/// The boards' layout without Move to Trash and Rename: every item and
/// separator, before the ones that don't apply are left out.
const LAYOUT: [MenuEntry; 11] = [
    MenuEntry::Item(ContextItem::Open),
    MenuEntry::Item(ContextItem::OpenWith),
    MenuEntry::Item(ContextItem::RevealInFinder),
    MenuEntry::Separator,
    MenuEntry::Item(ContextItem::QuickLook),
    MenuEntry::Item(ContextItem::Copy),
    MenuEntry::Item(ContextItem::CopyPath),
    MenuEntry::Separator,
    MenuEntry::Item(ContextItem::Share),
    MenuEntry::Separator,
    MenuEntry::Item(ContextItem::NewTerminal),
];

/// The menu for targets that are `mix`: the items that apply, in the
/// boards' order, with no separator first, last or next to another.
pub fn menu_entries(mix: TargetMix) -> Vec<MenuEntry> {
    let shows = |item: ContextItem| match item {
        ContextItem::OpenWith => mix == TargetMix::Files,
        ContextItem::NewTerminal => mix == TargetMix::Folders,
        _ => true,
    };
    let mut out: Vec<MenuEntry> = Vec::with_capacity(LAYOUT.len());
    for entry in LAYOUT {
        match entry {
            MenuEntry::Item(item) if shows(item) => out.push(entry),
            MenuEntry::Item(_) => {}
            MenuEntry::Separator => {
                if matches!(out.last(), Some(MenuEntry::Item(_))) {
                    out.push(entry);
                }
            }
        }
    }
    if out.last() == Some(&MenuEntry::Separator) {
        out.pop();
    }
    out
}

/// An app that can open the targets, for Open With.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppChoice {
    /// Its display name, without ".app".
    pub name: String,
    /// Its version (`CFBundleShortVersionString`), if any.
    pub version: Option<String>,
    /// Its full path.
    pub path: String,
}

/// An entry of the Open With submenu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenWithEntry {
    /// An app.
    App {
        /// "Preview (default)", "TextEdit", "Xcode (16.4)".
        title: String,
        /// The app's full path.
        path: String,
        /// Whether it is the targets' default app.
        default: bool,
    },
    /// A separator line.
    Separator,
    /// App Store…: opens the Mac App Store.
    AppStore,
    /// Other…: choose an app to open the targets with, once.
    Other,
}

/// The Open With submenu for `apps` (the apps that can open every target)
/// and the targets' shared `default` app, by path.
///
/// The default comes first as "Preview (default)", as Finder writes it,
/// then the others in alphabetical order (ignoring case); apps sharing a
/// name show their versions ("Xcode (16.4)"), or their folders when no
/// version tells them apart. An app listed twice shows once. Then a
/// separator, App Store… and Other…; with no app at all, only those two.
pub fn open_with_entries(default: Option<&str>, apps: Vec<AppChoice>) -> Vec<OpenWithEntry> {
    let mut apps = apps;
    let mut seen = std::collections::HashSet::new();
    apps.retain(|a| seen.insert(a.path.clone()));
    let mut names: HashMap<String, usize> = HashMap::new();
    for a in &apps {
        *names.entry(a.name.to_lowercase()).or_default() += 1;
    }
    let default_app = default.and_then(|d| {
        let i = apps.iter().position(|a| a.path == d)?;
        Some(apps.remove(i))
    });
    apps.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.version.cmp(&b.version))
            .then_with(|| a.path.cmp(&b.path))
    });
    let mut out = Vec::with_capacity(apps.len() + 4);
    if let Some(a) = default_app {
        out.push(OpenWithEntry::App {
            title: format!("{} (default)", a.name),
            path: a.path,
            default: true,
        });
    }
    for a in apps {
        let shared = names.get(&a.name.to_lowercase()).copied().unwrap_or(0) > 1;
        let title = match (&a.version, shared) {
            (_, false) => a.name.clone(),
            (Some(v), true) => format!("{} ({v})", a.name),
            (None, true) => format!("{} ({})", a.name, folder_of(&a.path)),
        };
        out.push(OpenWithEntry::App {
            title,
            path: a.path,
            default: false,
        });
    }
    if !out.is_empty() {
        out.push(OpenWithEntry::Separator);
    }
    out.push(OpenWithEntry::AppStore);
    out.push(OpenWithEntry::Other);
    out
}

/// The folder holding `path`.
fn folder_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) => "/",
        Some(i) => &path[..i],
        None => "",
    }
}

/// The apps (by path) that every target can be opened with: those in every
/// one of `lists`, in the first list's order. No lists gives none.
pub fn common_apps(lists: &[Vec<String>]) -> Vec<String> {
    let Some((first, rest)) = lists.split_first() else {
        return Vec::new();
    };
    first
        .iter()
        .filter(|app| rest.iter().all(|list| list.contains(app)))
        .cloned()
        .collect()
}

/// The targets' default app, when they all have the same one.
pub fn common_default(defaults: &[Option<String>]) -> Option<String> {
    let (first, rest) = defaults.split_first()?;
    let first = first.as_ref()?;
    rest.iter()
        .all(|d| d.as_ref() == Some(first))
        .then(|| first.clone())
}

/// Whether `path` names an app bundle (`….app`), which Other… lets the user
/// choose.
pub fn is_application_bundle(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let lower = name.to_ascii_lowercase();
    lower.len() > ".app".len() && lower.ends_with(".app")
}
