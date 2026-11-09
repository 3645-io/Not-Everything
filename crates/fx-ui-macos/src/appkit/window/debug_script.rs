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
//! them), `key:down` and `key:up` (an arrow key) and `type:TEXT` (each
//! character as a key press, 80 ms apart). Each step logs where the focus
//! is.
//!
//! The column dividers: `divider:I:drag:DX` (a mouse-down on divider I,
//! 0 Name, 1 Path, 2 Size, dragged DX points and released, as the hardware
//! sends them), `divider:I:double` (a double-click), `divider:I:key:right`,
//! `divider:I:key:shift-left` and so on (an arrow key handed to the
//! divider), and `scrollx:X` (the rows scrolled X points sideways).

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
    Type(String),
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
                    _ => None,
                },
                "type" => Some(Step::Type(arg.to_owned())),
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
}
