//! The read-only action layer: the key maps of the result list and the
//! search field, what Escape does, the clipboard text and the drag-out
//! operation mask.
//!
//! The MVP is read-only, so [`Action`] has exactly three variants: open,
//! reveal in Finder and copy the full path. There is deliberately no delete,
//! rename, move or trash action, and the delete keys map to nothing. Quick
//! Look on Space is not built yet. Escape ([`escape_action`]) only clears
//! the search or hides the window; it never acts on a file.

#![forbid(unsafe_code)]

use crate::FixedList;

/// A user action on the selected results.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    /// Open the selected items with their default application.
    Open,
    /// Reveal the selected items in Finder.
    RevealInFinder,
    /// Copy the full paths of the selected items to the clipboard.
    CopyPath,
}

impl Action {
    /// Every action.
    pub const ALL: FixedList<Action, 3> =
        FixedList([Action::Open, Action::RevealInFinder, Action::CopyPath]);
}

/// A key, as far as the action map cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// The main Return key.
    Return,
    /// The keypad Enter key.
    Enter,
    /// Backspace (labelled "delete" on Mac keyboards).
    Backspace,
    /// Forward delete.
    Delete,
    /// Escape.
    Escape,
    /// A character key, as typed without modifiers (lowercase letters).
    Char(char),
    /// Any other key.
    Other,
}

/// The modifier keys held with a key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    /// Command (⌘).
    pub command: bool,
    /// Shift (⇧).
    pub shift: bool,
    /// Option (⌥).
    pub option: bool,
    /// Control (⌃).
    pub control: bool,
}

/// The action a key press triggers on the result list, if any.
///
/// - Return or Enter: [`Action::Open`].
/// - Cmd+Return or Cmd+Enter: [`Action::RevealInFinder`].
/// - Option+Cmd+C: [`Action::CopyPath`], Finder's shortcut for copying a
///   pathname.
///
/// Everything else, the delete keys with any modifiers included, gives
/// `None`. Cmd+C alone copies nothing in the results: it is kept for
/// copying the files themselves, as in Finder, which is not built yet, so
/// the table doesn't answer Edit > Copy either.
pub fn action_for_key(key: Key, mods: Modifiers) -> Option<Action> {
    let plain = !mods.command && !mods.shift && !mods.option && !mods.control;
    let cmd_only = mods.command && !mods.shift && !mods.option && !mods.control;
    let option_cmd = mods.command && mods.option && !mods.shift && !mods.control;
    match key {
        Key::Return | Key::Enter if plain => Some(Action::Open),
        Key::Return | Key::Enter if cmd_only => Some(Action::RevealInFinder),
        Key::Char(c) if option_cmd && c.eq_ignore_ascii_case(&'c') => Some(Action::CopyPath),
        _ => None,
    }
}

/// What a key press in the search field does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SearchFieldKey {
    /// Move the focus to the results, selecting the first row when none is
    /// selected, so the arrow keys pick a file and Return opens it. With no
    /// results the focus stays in the field.
    FocusResults,
    /// Perform this action on the selection, or on the first row.
    Act(Action),
}

/// What a key press in the search field does, if anything beyond typing.
///
/// - Return or Enter: [`SearchFieldKey::FocusResults`]. It never opens a
///   result; that is Return in the results ([`action_for_key`]).
/// - Cmd+Return or Cmd+Enter: [`Action::RevealInFinder`].
///
/// Everything else, Cmd+C included (it copies the field's text), is the
/// field's own and gives `None`. The Down arrow also moves to the results;
/// AppKit reports it as a command, not a key.
pub fn search_field_key(key: Key, mods: Modifiers) -> Option<SearchFieldKey> {
    let plain = !mods.command && !mods.shift && !mods.option && !mods.control;
    match (key, action_for_key(key, mods)) {
        (Key::Return | Key::Enter, _) if plain => Some(SearchFieldKey::FocusResults),
        (_, Some(Action::RevealInFinder)) => Some(SearchFieldKey::Act(Action::RevealInFinder)),
        _ => None,
    }
}

/// Where the keyboard focus is in the search window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Focus {
    /// The search field.
    SearchField,
    /// The result list.
    Results,
}

/// What Escape does in the resident app's search window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EscapeAction {
    /// Empty the search field, run the empty query and move the focus to
    /// the field.
    ClearSearch,
    /// Hide the window, as the summon hotkey does, handing the focus back
    /// to the app the user was using before ([`hide_mode`]). The app keeps
    /// running in the menu bar and the hotkey shows the window again.
    HideWindow,
}

/// What a key press does because it is Escape, given where the focus is,
/// the search field's text and whether the app runs resident.
///
/// Escape backs out one step, as in Spotlight:
///
/// - With text in the search field: in the field, `None`, since
///   `NSSearchField` clears its text on Escape and runs the empty query by
///   itself; in the results, [`EscapeAction::ClearSearch`], which does the
///   same and moves the focus back to the field.
/// - With the search field empty, wherever the focus is:
///   [`EscapeAction::HideWindow`].
///
/// Any other key, Escape with a modifier held, and every key in the
/// measurement modes (`resident` false) give `None`, so they keep their
/// behaviour.
pub fn escape_action(
    key: Key,
    mods: Modifiers,
    focus: Focus,
    field_text: &str,
    resident: bool,
) -> Option<EscapeAction> {
    let plain = !mods.command && !mods.shift && !mods.option && !mods.control;
    if key != Key::Escape || !plain || !resident {
        return None;
    }
    match (field_text.is_empty(), focus) {
        (true, _) => Some(EscapeAction::HideWindow),
        (false, Focus::Results) => Some(EscapeAction::ClearSearch),
        (false, Focus::SearchField) => None,
    }
}

/// How hiding the search window (Escape, the summon hotkey) hands back the
/// focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HideMode {
    /// Hide the app, as Cmd+H does: macOS activates the app the user was
    /// using before, as Spotlight does. The app's windows come back when it
    /// is next activated (the hotkey, the Dock, Cmd+Tab).
    HideApp,
    /// Order out only the search window: the user was in another window
    /// of the app (Settings) when the search window was summoned, so that
    /// window stays in front and keeps the focus.
    WindowOnly,
}

/// How to hide the search window, given whether the app was already active
/// when the window was last summoned and whether another of its windows
/// (Settings) is on screen.
///
/// [`HideMode::WindowOnly`] only when both hold: the user came from that
/// other window, so it stays. Otherwise [`HideMode::HideApp`], so the app
/// never stays active with no window and the focus goes back to the
/// previous app.
pub fn hide_mode(active_at_summon: bool, other_window_visible: bool) -> HideMode {
    if active_at_summon && other_window_visible {
        HideMode::WindowOnly
    } else {
        HideMode::HideApp
    }
}

/// The action-map key of a key event, from its virtual key code and its
/// characters ignoring modifiers.
///
/// Return, keypad Enter, Delete (backspace), forward delete and Escape are
/// recognised by their virtual key codes from Apple's HIToolbox `Events.h`
/// (`kVK_Return` 36, `kVK_ANSI_KeypadEnter` 76, `kVK_Delete` 51,
/// `kVK_ForwardDelete` 117, `kVK_Escape` 53); any other key is its first
/// character, lowercased, or [`Key::Other`] without one.
pub fn key_from_code(key_code: u16, chars: Option<&str>) -> Key {
    match key_code {
        36 => Key::Return,
        76 => Key::Enter,
        51 => Key::Backspace,
        117 => Key::Delete,
        53 => Key::Escape,
        _ => chars
            .and_then(|s| s.chars().next())
            .map_or(Key::Other, |c| Key::Char(c.to_ascii_lowercase())),
    }
}

/// The clipboard text for [`Action::CopyPath`]: the paths joined with
/// newlines, without a trailing newline.
pub fn copy_text<S: AsRef<str>>(paths: &[S]) -> String {
    let mut out = String::new();
    for (i, p) in paths.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(p.as_ref());
    }
    out
}

/// The drag operations offered when results are dragged out of the window:
/// `NSDragOperationCopy | NSDragOperationLink`.
///
/// Raw values from Apple's `NSDragOperation` documentation
/// (<https://developer.apple.com/documentation/appkit/nsdragoperation>):
/// Copy = 1, Link = 2, Generic = 4, Private = 8, Move = 16, Delete = 32.
/// Move, Delete and Generic stay clear, so dropping a result on the Trash or
/// a Finder folder can only copy it or make a link, never move or delete the
/// file.
pub const DRAG_OPERATION_MASK: u64 = 1 | 2;
