//! A debug build's scripted states for screenshots and checks:
//! `NE_DEBUG_SCRIPT` holds steps separated by `;`, run one every 0.7 seconds
//! once the window shows, so the window can be put in a state and captured
//! without typing into it. The module is compiled into debug builds only,
//! so a release build has no way to script its window.
//!
//! Steps: `query:TEXT`, `chips:case,path,regex` (any of them, or none),
//! `select:ROW`, `focus:results` or `focus:field`, `hover:ROW`,
//! `sort:COLUMN` (a column identifier), `size:WIDTHxHEIGHT`,
//! `appearance:dark` or `appearance:light` (the app's, as Settings sets
//! it), `escape` (Escape in the search field, through its field editor),
//! `press:TAG` (clicks the window's control with that tag: a chip, a
//! column title, a syntax example), `wait`, `click:ROW` (a mouse-down and
//! mouse-up on that row, sent to the window as the hardware would send
//! them), `key:down` and `key:up` (an arrow key), `key:space` and
//! `key:escape` (a key press sent to the window, as the hardware sends it:
//! Quick Look with the results focused), `type:TEXT` (each character as a
//! key press, 80 ms apart), `activate` (activates the app and makes the
//! window key, as a summon does; Quick Look looks for its controller in the
//! key window), `announce` (posts the result count announcement now, as a
//! new search does while VoiceOver runs) and `ax` (logs the table's row
//! counts as AppKit reports them to VoiceOver, the announcement a new
//! search makes and the Quick Look panel's state). Each step logs where the focus is.
//!
//! The column dividers: `divider:I:drag:DX` (a mouse-down on divider I,
//! 0 Name, 1 Path, 2 Size, dragged DX points and released, as the hardware
//! sends them), `divider:I:double` (a double-click), `divider:I:key:right`,
//! `divider:I:key:shift-left` and so on (an arrow key handed to the
//! divider), and `scrollx:X` (the rows scrolled X points sideways).
//!
//! The right-click menu (run 2.12): `menu:ROW` fills the menu as a
//! right-click on that row would, without showing it, and logs what it
//! acts on and each item (title, hidden, enabled, shortcut, image,
//! submenu); `menu:empty` right-clicks below the rows and `menu:keyboard`
//! asks as the keyboard and VoiceOver do; `menu:ROW:ITEM` performs an item
//! (`open`, `reveal`, `look`, `copy`, `path`, `share`, `terminal`) as
//! choosing it would; `menu:ROW:open-with` fills and logs the Open With
//! submenu. `rclick:ROW:SECS` right-clicks the row with real events, which
//! shows the menu on screen, and closes it after SECS seconds (for
//! screenshots); `menukey:SECS` does the same with Control-Return.
//! `pasteboard` logs the types and files on the pasteboard copies go to
//! (in these checks a named one, `NE_DEBUG_PASTEBOARD`, never the user's
//! clipboard). `key:cmd-c`, `key:option-cmd-c`, `key:shift-cmd-c` and
//! `key:control-cmd-c` press those keys through the application, so the
//! main menu's key equivalents fire as for real keys. `status` logs where
//! the status bar's parts are.
//!
//! What VoiceOver reads is best seen from outside the app, through the
//! Accessibility API (`AXUIElementCopyAttributeValue`), as VoiceOver
//! sees it: asked from inside, AppKit doesn't report every label a control
//! carries.

use objc2::msg_send;

use crate::context_menu::ContextItem;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSArray, NSString};

/// One scripted step.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Step {
    Query(String),
    Chips { case: bool, path: bool, regex: bool },
    Select(usize),
    FocusResults,
    FocusField,
    Hover(usize),
    Sort(String),
    Size(f64, f64),
    Dark(bool),
    Escape,
    Press(isize),
    Divider(usize, DividerStep),
    ScrollX(f64),
    Wait,
    Click(usize),
    Arrow(Arrow),
    Key(NamedKey),
    Type(String),
    Activate,
    Announce,
    Ax,
    Menu(MenuAt, MenuStep),
    RightClick(usize, f64),
    MenuKey(f64),
    Pasteboard,
    Chord(Chord),
    Status,
}

/// Where a [`Step::Menu`] right-click lands.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum MenuAt {
    /// On this row.
    Row(usize),
    /// Below the last row in view.
    Empty,
    /// Nowhere: the keyboard or VoiceOver asks.
    Keyboard,
}

/// What a [`Step::Menu`] does with the menu it filled.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum MenuStep {
    /// Logs it.
    Log,
    /// Performs the item.
    Perform(ContextItem),
    /// Fills and logs the Open With submenu.
    OpenWith,
}

/// A key chord with C for [`Step::Chord`], sent through the application.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Chord {
    CmdC,
    OptionCmdC,
    ShiftCmdC,
    ControlCmdC,
}

/// The short name of a [`ContextItem`] in a `menu:ROW:ITEM` step.
fn item_name(item: ContextItem) -> &'static str {
    match item {
        ContextItem::Open => "open",
        ContextItem::OpenWith => "open-with",
        ContextItem::RevealInFinder => "reveal",
        ContextItem::QuickLook => "look",
        ContextItem::Copy => "copy",
        ContextItem::CopyPath => "path",
        ContextItem::Share => "share",
        ContextItem::NewTerminal => "terminal",
    }
}

/// A `menu:` step's argument: `ROW`, `empty` or `keyboard`, then
/// optionally `:ITEM` or `:open-with`.
fn menu_step(arg: &str) -> Option<Step> {
    let mut parts = arg.trim().splitn(2, ':');
    let at = match parts.next()? {
        "empty" => MenuAt::Empty,
        "keyboard" => MenuAt::Keyboard,
        row => MenuAt::Row(row.parse().ok()?),
    };
    let step = match parts.next() {
        None => MenuStep::Log,
        Some("open-with") => MenuStep::OpenWith,
        Some(name) => MenuStep::Perform(
            ContextItem::ALL
                .iter()
                .copied()
                .find(|&i| i != ContextItem::OpenWith && item_name(i) == name)?,
        ),
    };
    Some(Step::Menu(at, step))
}

/// What [`Step::Divider`] does to a column divider.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum DividerStep {
    /// Pressed, dragged this many points and released.
    Drag(f64),
    /// Double-clicked.
    DoubleClick,
    /// Left or Right, with Shift or not.
    Key { right: bool, shift: bool },
}

/// A `divider:` step's argument: `I:drag:DX`, `I:double` or
/// `I:key:[shift-]left|right`.
fn divider_step(arg: &str) -> Option<Step> {
    let mut parts = arg.trim().splitn(3, ':');
    let index = parts.next()?.parse().ok()?;
    let step = match (parts.next()?, parts.next()) {
        ("drag", Some(dx)) => DividerStep::Drag(dx.parse().ok()?),
        ("double", None) => DividerStep::DoubleClick,
        ("key", Some(key)) => {
            let (shift, key) = match key.strip_prefix("shift-") {
                Some(key) => (true, key),
                None => (false, key),
            };
            let right = match key {
                "right" => true,
                "left" => false,
                _ => return None,
            };
            DividerStep::Key { right, shift }
        }
        _ => return None,
    };
    Some(Step::Divider(index, step))
}

/// An arrow key for [`Step::Arrow`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Arrow {
    Up,
    Down,
}

/// A key for [`Step::Key`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum NamedKey {
    Space,
    Escape,
}

/// How many elements the table's accessibility attribute `name` lists
/// (`AXRows`, `AXVisibleRows`, `AXSelectedRows`), and how long asking took.
pub(crate) fn ax_count(element: &AnyObject, name: &str) -> (Option<usize>, std::time::Duration) {
    let start = std::time::Instant::now();
    let name = NSString::from_str(name);
    // SAFETY: `accessibilityAttributeValue:` takes an attribute name and
    // returns an object or nil; every AppKit object answers it.
    let value: Option<Retained<AnyObject>> =
        unsafe { msg_send![element, accessibilityAttributeValue: &*name] };
    let count = value
        .and_then(|v| v.downcast::<NSArray>().ok())
        .map(|a| a.count());
    (count, start.elapsed())
}

/// The script from the environment.
pub(crate) fn script() -> Vec<Step> {
    std::env::var("NE_DEBUG_SCRIPT")
        .map(|s| parse(&s))
        .unwrap_or_default()
}

fn parse(text: &str) -> Vec<Step> {
    text.split(';')
        .filter_map(|step| {
            let (verb, arg) = step.split_once(':').unwrap_or((step, ""));
            match verb.trim() {
                "query" => Some(Step::Query(arg.to_owned())),
                "chips" => Some(Step::Chips {
                    case: arg.contains("case"),
                    path: arg.contains("path"),
                    regex: arg.contains("regex"),
                }),
                "select" => arg.trim().parse().ok().map(Step::Select),
                "focus" if arg.trim() == "results" => Some(Step::FocusResults),
                "focus" => Some(Step::FocusField),
                "hover" => arg.trim().parse().ok().map(Step::Hover),
                "sort" => Some(Step::Sort(arg.trim().to_owned())),
                "size" => {
                    let (w, h) = arg.split_once('x')?;
                    Some(Step::Size(w.trim().parse().ok()?, h.trim().parse().ok()?))
                }
                "appearance" => Some(Step::Dark(arg.trim() == "dark")),
                "escape" => Some(Step::Escape),
                "press" => arg.trim().parse().ok().map(Step::Press),
                "divider" => divider_step(arg),
                "scrollx" => arg.trim().parse().ok().map(Step::ScrollX),
                "wait" => Some(Step::Wait),
                "click" => arg.trim().parse().ok().map(Step::Click),
                "key" => match arg.trim() {
                    "down" => Some(Step::Arrow(Arrow::Down)),
                    "up" => Some(Step::Arrow(Arrow::Up)),
                    "space" => Some(Step::Key(NamedKey::Space)),
                    "escape" => Some(Step::Key(NamedKey::Escape)),
                    "cmd-c" => Some(Step::Chord(Chord::CmdC)),
                    "option-cmd-c" => Some(Step::Chord(Chord::OptionCmdC)),
                    "shift-cmd-c" => Some(Step::Chord(Chord::ShiftCmdC)),
                    "control-cmd-c" => Some(Step::Chord(Chord::ControlCmdC)),
                    _ => None,
                },
                "menu" => menu_step(arg),
                "rclick" => {
                    let (row, secs) = arg.trim().split_once(':')?;
                    Some(Step::RightClick(row.parse().ok()?, secs.parse().ok()?))
                }
                "menukey" => arg.trim().parse().ok().map(Step::MenuKey),
                "pasteboard" => Some(Step::Pasteboard),
                "status" => Some(Step::Status),
                "type" => Some(Step::Type(arg.to_owned())),
                "activate" => Some(Step::Activate),
                "announce" => Some(Step::Announce),
                "ax" => Some(Step::Ax),
                _ => None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The right-click menu is checked in the app: built and logged
    /// without showing it, an item performed, the Open With submenu
    /// filled, shown for real for a while, and the keys and pasteboard
    /// behind Copy.
    #[test]
    fn right_click_menu_steps_parse() {
        assert_eq!(
            parse(
                "menu:3;menu:empty;menu:keyboard;menu:2:copy;menu:0:look;menu:4:open-with;\
                 rclick:1:4.5;menukey:3;pasteboard;key:cmd-c;key:option-cmd-c;\
                 key:shift-cmd-c;key:control-cmd-c;status"
            ),
            vec![
                Step::Menu(MenuAt::Row(3), MenuStep::Log),
                Step::Menu(MenuAt::Empty, MenuStep::Log),
                Step::Menu(MenuAt::Keyboard, MenuStep::Log),
                Step::Menu(MenuAt::Row(2), MenuStep::Perform(ContextItem::Copy)),
                Step::Menu(MenuAt::Row(0), MenuStep::Perform(ContextItem::QuickLook)),
                Step::Menu(MenuAt::Row(4), MenuStep::OpenWith),
                Step::RightClick(1, 4.5),
                Step::MenuKey(3.0),
                Step::Pasteboard,
                Step::Chord(Chord::CmdC),
                Step::Chord(Chord::OptionCmdC),
                Step::Chord(Chord::ShiftCmdC),
                Step::Chord(Chord::ControlCmdC),
                Step::Status,
            ]
        );
        for item in ["open", "reveal", "path", "share", "terminal"] {
            assert!(
                matches!(
                    parse(&format!("menu:0:{item}"))[..],
                    [Step::Menu(MenuAt::Row(0), MenuStep::Perform(_))]
                ),
                "{item}"
            );
        }
        assert_eq!(
            parse("menu:x;menu:0:trash;menu:0:rename;rclick:1;menukey:x"),
            vec![]
        );
    }

    #[test]
    fn existing_steps_still_parse() {
        assert_eq!(
            parse("query:budget;select:2;focus:results;escape;press:101;wait"),
            vec![
                Step::Query("budget".into()),
                Step::Select(2),
                Step::FocusResults,
                Step::Escape,
                Step::Press(101),
                Step::Wait,
            ]
        );
    }

    /// A clicked result should take the arrow keys: `click:ROW` clicks a
    /// row with real mouse events and `key:` sends an arrow key, through the
    /// window, as the hardware would, to check it.
    #[test]
    fn clicks_and_arrow_keys_parse() {
        assert_eq!(
            parse("click:3;key:down;key:up"),
            vec![
                Step::Click(3),
                Step::Arrow(Arrow::Down),
                Step::Arrow(Arrow::Up)
            ]
        );
        assert_eq!(parse("click:x;key:left"), vec![]);
    }

    /// The column dividers can be dragged, double-clicked and stepped with
    /// the arrow keys, and the rows scrolled sideways, for screenshots.
    #[test]
    fn divider_steps_parse() {
        assert_eq!(
            parse("divider:0:drag:-60.5;divider:2:double;divider:1:key:shift-right;scrollx:200"),
            vec![
                Step::Divider(0, DividerStep::Drag(-60.5)),
                Step::Divider(2, DividerStep::DoubleClick),
                Step::Divider(
                    1,
                    DividerStep::Key {
                        right: true,
                        shift: true
                    }
                ),
                Step::ScrollX(200.0),
            ]
        );
        assert_eq!(
            parse("divider:x:double;divider:0:drag;divider:0:key:up;divider:1:double:2"),
            vec![]
        );
    }

    /// `type:TEXT` types each character as a key press into whatever has
    /// the focus, 80 ms apart, so the search field's own timing applies.
    #[test]
    fn typing_parses_and_keeps_spaces() {
        assert_eq!(parse("type:bud get"), vec![Step::Type("bud get".into())]);
    }

    /// Quick Look and VoiceOver are checked in the app: `key:space` and
    /// `key:escape` press those keys through the window, and `ax` logs
    /// what AppKit reports to VoiceOver.
    #[test]
    fn quick_look_keys_and_the_accessibility_log_parse() {
        assert_eq!(
            parse("activate;key:space;ax;key:escape;announce"),
            vec![
                Step::Activate,
                Step::Key(NamedKey::Space),
                Step::Ax,
                Step::Key(NamedKey::Escape),
                Step::Announce
            ]
        );
    }
}
