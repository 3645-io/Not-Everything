//! The widths the user gives the results columns with the dividers
//! between their titles (the Main board's column dividers).
//!
//! A divider sits after Name, Path and Size. Dragging one resizes the
//! column on its left, never below that column's minimum (Name 140, Path
//! 120, Size 64 points) nor above 800; the other two keep their widths and
//! Date Modified takes the rest, never less than 140
//! ([`crate::main_layout::resized_column_widths`]). Left or Right moves a
//! focused divider 8 points, 32 with Shift. Double-clicking one returns to
//! the default layout, in which Name and Path share the free width
//! ([`crate::main_layout::column_widths`]).
//!
//! [`ColumnSizes`] holds the three widths in whole points; `None` in its
//! place means the default layout. The app remembers it as the
//! `column_widths` line of the settings file ([`crate::settings`]), such
//! as `300,480,88`.

#![forbid(unsafe_code)]

use std::fmt;

use crate::theme::{Token, metrics};

/// How many columns have a divider on their right: Name, Path and Size.
pub const RESIZABLE: usize = 3;

/// The narrowest and widest a divider makes column `column` (0 Name, 1
/// Path, 2 Size).
///
/// # Panics
///
/// If `column` is not below [`RESIZABLE`].
pub fn range(column: usize) -> (f64, f64) {
    (
        metrics::RESIZE_MIN_WIDTHS[column],
        metrics::RESIZE_MAX_WIDTH,
    )
}

/// `width` rounded to whole points and kept within `column`'s
/// [`range`]; not a number counts as the minimum.
fn fit(column: usize, width: f64) -> u32 {
    let (min, max) = range(column);
    let w = if width.is_nan() { min } else { width.round() };
    // Within 64..=800, so the cast is exact.
    w.clamp(min, max) as u32
}

/// The widths the user gave Name, Path and Size, in whole points, each
/// within its column's [`range`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ColumnSizes([u32; RESIZABLE]);

impl ColumnSizes {
    /// `widths` rounded to whole points and kept within each column's
    /// range.
    pub fn new(widths: [f64; RESIZABLE]) -> ColumnSizes {
        ColumnSizes([fit(0, widths[0]), fit(1, widths[1]), fit(2, widths[2])])
    }

    /// The three widths in points.
    pub fn widths(self) -> [f64; RESIZABLE] {
        self.0.map(f64::from)
    }

    /// Reads a settings-file value (see [`Display`](fmt::Display)): three
    /// whole numbers separated by commas, each kept within its column's
    /// range as a drag would. `None` for anything else.
    pub fn parse(s: &str) -> Option<ColumnSizes> {
        let mut parts = s.split(',').map(|p| p.trim().parse::<u32>().ok());
        let mut widths = [0.0; RESIZABLE];
        for w in &mut widths {
            *w = f64::from(parts.next()??);
        }
        if parts.next().is_some() {
            return None;
        }
        Some(ColumnSizes::new(widths))
    }
}

impl fmt::Display for ColumnSizes {
    /// The settings-file value, such as `300,480,88`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c] = self.0;
        write!(f, "{a},{b},{c}")
    }
}

/// The first three of four laid-out column widths.
fn resizable(widths: [f64; 4]) -> [f64; RESIZABLE] {
    [widths[0], widths[1], widths[2]]
}

/// A drag of a divider, from the mouse-down on it to the mouse-up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Drag {
    divider: usize,
    start: [f64; RESIZABLE],
    moved: bool,
}

impl Drag {
    /// A drag of the divider after column `divider` (0 Name, 1 Path, 2
    /// Size) over the columns as laid out, `widths`, which may be the
    /// default layout's.
    ///
    /// # Panics
    ///
    /// If `divider` is not below [`RESIZABLE`].
    pub fn new(divider: usize, widths: [f64; 4]) -> Drag {
        assert!(divider < RESIZABLE, "no divider after column {divider}");
        Drag {
            divider,
            start: resizable(widths),
            moved: false,
        }
    }

    /// The divider being dragged.
    pub fn divider(&self) -> usize {
        self.divider
    }

    /// The sizes with the pointer `dx` points right of where the drag
    /// started (left when negative): the dragged column's start width plus
    /// `dx`, the others as they started. `None` until the pointer has
    /// moved a whole point, so a click alone changes nothing; after that,
    /// always the user's sizes, also back at the start.
    pub fn to(&mut self, dx: f64) -> Option<ColumnSizes> {
        if !self.moved && dx.round() == 0.0 {
            return None;
        }
        self.moved = true;
        let mut widths = self.start;
        widths[self.divider] += dx;
        Some(ColumnSizes::new(widths))
    }
}

/// Which way Left or Right moves a focused divider.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Step {
    /// Left: the column on the divider's left gets narrower.
    Narrower,
    /// Right: it gets wider.
    Wider,
}

/// The sizes after one key press on the focused divider after column
/// `divider` over the columns as laid out, `widths`: 8 points, or 32 with
/// Shift (`large`), within the column's range.
///
/// # Panics
///
/// If `divider` is not below [`RESIZABLE`].
pub fn step(widths: [f64; 4], divider: usize, step: Step, large: bool) -> ColumnSizes {
    let (small, big) = metrics::RESIZE_STEP;
    let by = if large { big } else { small };
    let mut w = resizable(widths);
    w[divider] += match step {
        Step::Narrower => -by,
        Step::Wider => by,
    };
    ColumnSizes::new(w)
}

/// The colour of a divider's line: `edge` at rest, `green-ink` while it is
/// hovered, focused or dragged.
pub fn divider_token(active: bool) -> Token {
    if active { Token::GreenInk } else { Token::Edge }
}
