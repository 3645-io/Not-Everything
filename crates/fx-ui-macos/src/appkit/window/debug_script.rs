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
//! What VoiceOver reads is best seen from outside the app, through the
//! Accessibility API (`AXUIElementCopyAttributeValue`), as VoiceOver
//! sees it: asked from inside, AppKit doesn't report every label a control
//! carries.

use objc2::msg_send;
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
    Wait,
    Click(usize),
    Arrow(Arrow),
    Key(NamedKey),
    Type(String),
    Activate,
    Announce,
    Ax,
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
                "wait" => Some(Step::Wait),
                "click" => arg.trim().parse().ok().map(Step::Click),
                "key" => match arg.trim() {
                    "down" => Some(Step::Arrow(Arrow::Down)),
                    "up" => Some(Step::Arrow(Arrow::Up)),
                    "space" => Some(Step::Key(NamedKey::Space)),
                    "escape" => Some(Step::Key(NamedKey::Escape)),
                    _ => None,
                },
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
