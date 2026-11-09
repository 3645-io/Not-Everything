//! The search window's status bar (the canvas's StatusBar and the owner's
//! choices), as a pure function of the window's state.
//!
//! Three slots, left to right:
//!
//! 1. **Counts** ([`Counts`]): "**8** results · 2,014,377 objects" with a
//!    query, "**2,014,377** objects" without; the figure in bold.
//! 2. **The middle** ([`Middle`]): the selected row's key hints
//!    ([`RESULT_HINTS`]) while the results have the focus and a row is
//!    selected; otherwise the notes that apply, joined by " · ": "N
//!    selected", the scope of the index ([`ScopeNote`]; limited mode only
//!    once the access callout is dismissed with Not Now, since the callout
//!    says it while it shows) and the resident note (Launch at Login needs
//!    approval). A copy cut at the copy limit is noted even over the hints,
//!    because it answers the Option+Cmd+C just pressed in the results.
//! 3. **The index** ([`IndexSlot`]): an error when there is one (it replaces
//!    everything else here); else "Indexing… N of about M" with a progress
//!    fraction, M being the previous index's size (the count alone on a
//!    first run, or once N passes M); else "Up to date · 9 ms", the engine's
//!    time for the current search, with a green dot.

#![forbid(unsafe_code)]

use std::time::Duration;

use crate::actions::Focus;
use crate::controller::ResultsController;
use crate::formats::{format_millis, group_thousands};
use crate::key_caps::{KeyHint, RESULT_HINTS};
use crate::onboarding::ScopeNote;
use crate::source::ResultsSource;

/// The separator between parts of a slot.
const SEP: &str = " \u{b7} ";

/// What the status bar shows, from the window's state.
#[derive(Clone, Debug, PartialEq)]
pub struct StatusInput<'a> {
    /// Whether the search field holds a query (anything but spaces).
    pub has_query: bool,
    /// How many results the query has.
    pub results: usize,
    /// How many entries the index holds.
    pub objects: usize,
    /// Whether a scan is still filling or catching up the index.
    pub indexing: bool,
    /// The previous index's size, for "of about M" while indexing; `None`
    /// on a first run.
    pub previous_objects: Option<usize>,
    /// The engine's time for the current search, when known.
    pub search_time: Option<Duration>,
    /// Where the keyboard focus is.
    pub focus: Focus,
    /// How many rows are selected.
    pub selected: usize,
    /// A copy just stopped at the copy limit after this many paths.
    pub copy_limit: Option<usize>,
    /// The scope of the index, when it is worth a note.
    pub scope: Option<ScopeNote>,
    /// Whether the Full Disk Access callout shows above the results.
    pub banner_shown: bool,
    /// The resident note (Launch at Login needs approval).
    pub note: Option<&'a str>,
    /// An error to show (Launch at Login failed, the protected folders
    /// can't be indexed).
    pub error: Option<&'a str>,
}

impl Default for StatusInput<'_> {
    /// No query, no results, nothing indexing, the focus in the search
    /// field, nothing selected and nothing to note.
    fn default() -> Self {
        StatusInput {
            has_query: false,
            results: 0,
            objects: 0,
            indexing: false,
            previous_objects: None,
            search_time: None,
            focus: Focus::SearchField,
            selected: 0,
            copy_limit: None,
            scope: None,
            banner_shown: false,
            note: None,
            error: None,
        }
    }
}

impl StatusInput<'_> {
    /// The input as far as the results controller knows it: the query, the
    /// result count and the source's entries and indexing state. The window
    /// adds the focus, selection, search time, previous index size, notes
    /// and errors.
    pub fn from_controller<S: ResultsSource>(ctl: &ResultsController<S>) -> StatusInput<'static> {
        let status = ctl.source().status();
        StatusInput {
            has_query: !ctl.query().trim().is_empty(),
            results: ctl.row_count(),
            objects: status.entries,
            indexing: status.indexing,
            ..StatusInput::default()
        }
    }
}

/// The left slot: a figure in bold, then the rest in the status colour.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counts {
    /// The bold figure: the result count with a query, else the object
    /// count.
    pub figure: String,
    /// What follows it, starting with a space.
    pub rest: String,
}

impl Counts {
    /// The whole slot as plain text.
    pub fn text(&self) -> String {
        format!("{}{}", self.figure, self.rest)
    }
}

/// The middle slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Middle {
    /// Nothing.
    Empty,
    /// Key hints for the selected result.
    Hints(&'static [KeyHint]),
    /// A note.
    Note(String),
}

/// The right slot: the state of the index.
#[derive(Clone, Debug, PartialEq)]
pub enum IndexSlot {
    /// "Up to date · 9 ms", after a green dot.
    UpToDate(String),
    /// "Indexing… N of about M", after a progress bar filled to `progress`
    /// (from 0 to 1) when the total is known.
    Indexing {
        /// The text.
        text: String,
        /// How far the scan is, when the previous index's size is known.
        progress: Option<f64>,
    },
    /// An error, in `danger-ink`.
    Error(String),
}

/// The three slots of the status bar.
#[derive(Clone, Debug, PartialEq)]
pub struct StatusView {
    /// The left slot.
    pub counts: Counts,
    /// The middle slot.
    pub middle: Middle,
    /// The right slot.
    pub index: IndexSlot,
}

/// The status bar for `input`. See the module documentation.
pub fn status_view(input: &StatusInput<'_>) -> StatusView {
    StatusView {
        counts: counts(input),
        middle: middle(input),
        index: index(input),
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{} {}", group_thousands(n), if n == 1 { one } else { many })
}

fn counts(input: &StatusInput<'_>) -> Counts {
    let objects = plural(input.objects, "object", "objects");
    if input.has_query {
        let noun = if input.results == 1 {
            "result"
        } else {
            "results"
        };
        Counts {
            figure: group_thousands(input.results),
            rest: format!(" {noun}{SEP}{objects}"),
        }
    } else {
        let noun = if input.objects == 1 {
            "object"
        } else {
            "objects"
        };
        Counts {
            figure: group_thousands(input.objects),
            rest: format!(" {noun}"),
        }
    }
}

fn middle(input: &StatusInput<'_>) -> Middle {
    if let Some(copied) = input.copy_limit {
        return Middle::Note(format!(
            "Copied the first {} paths only (copy limit)",
            group_thousands(copied)
        ));
    }
    if input.focus == Focus::Results && input.selected > 0 {
        return Middle::Hints(&RESULT_HINTS);
    }
    let selected =
        (input.selected > 0).then(|| format!("{} selected", group_thousands(input.selected)));
    let scope = input
        .scope
        .filter(|s| !(*s == ScopeNote::LimitedMode && input.banner_shown))
        .map(|s| s.text().to_owned());
    let notes: Vec<String> = [selected, scope, input.note.map(str::to_owned)]
        .into_iter()
        .flatten()
        .collect();
    if notes.is_empty() {
        Middle::Empty
    } else {
        Middle::Note(notes.join(SEP))
    }
}

fn index(input: &StatusInput<'_>) -> IndexSlot {
    if let Some(error) = input.error {
        return IndexSlot::Error(error.to_owned());
    }
    if input.indexing {
        let count = group_thousands(input.objects);
        return match input.previous_objects {
            Some(total) if total > input.objects => IndexSlot::Indexing {
                text: format!(
                    "Indexing\u{2026} {count} of about {}",
                    group_thousands(total)
                ),
                progress: Some(input.objects as f64 / total as f64),
            },
            _ => IndexSlot::Indexing {
                text: format!("Indexing\u{2026} {count}"),
                progress: None,
            },
        };
    }
    match input.search_time {
        Some(time) => IndexSlot::UpToDate(format!("Up to date{SEP}{}", format_millis(time))),
        None => IndexSlot::UpToDate("Up to date".to_owned()),
    }
}
