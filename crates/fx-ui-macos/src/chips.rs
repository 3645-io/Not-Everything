//! The search window's option chips (the canvas's ToggleChip):
//! Aa (match case), Path (match the full path) and .* (the whole search is
//! one regular expression), after the search field.
//!
//! Their state is the search's [`QueryOptions`] (fx-query's, which the
//! engine searches with); the window remembers it between launches in the
//! settings file, one line per chip ([`Chip::settings_key`], all off by
//! default). A chip is raised on `panel` when off and pressed, washed and
//! rimmed in green when on ([`chip_surface`]); its label turns `green-ink`
//! under the pointer while off ([`chip_text`]).

#![forbid(unsafe_code)]

pub use fx_query::QueryOptions;

use crate::FixedList;
use crate::surface::Surface;
use crate::theme::{Token, metrics};
use crate::ui_text::{OPTION_CHIPS, OptionChip};

/// One option chip.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Chip {
    /// Aa: match upper and lower case exactly.
    MatchCase,
    /// Path: match the terms against the full path.
    MatchPath,
    /// .*: the whole search is one regular expression.
    Regex,
}

impl Chip {
    /// The chips in the toolbar's order.
    pub const ALL: FixedList<Chip, 3> = FixedList([Chip::MatchCase, Chip::MatchPath, Chip::Regex]);

    /// The chip's place in [`Chip::ALL`].
    pub const fn index(self) -> usize {
        match self {
            Chip::MatchCase => 0,
            Chip::MatchPath => 1,
            Chip::Regex => 2,
        }
    }

    /// The chip at `index` in [`Chip::ALL`].
    pub fn from_index(index: usize) -> Option<Chip> {
        Chip::ALL.iter().copied().find(|c| c.index() == index)
    }

    /// The chip's label, title and font ([`OPTION_CHIPS`]).
    pub const fn text(self) -> &'static OptionChip {
        &OPTION_CHIPS[self.index()]
    }

    /// Whether the chip is on in `options`.
    pub const fn is_on(self, options: QueryOptions) -> bool {
        match self {
            Chip::MatchCase => options.match_case,
            Chip::MatchPath => options.match_path,
            Chip::Regex => options.regex,
        }
    }

    /// `options` with this chip switched over.
    pub const fn toggle(self, options: QueryOptions) -> QueryOptions {
        let mut o = options;
        match self {
            Chip::MatchCase => o.match_case = !o.match_case,
            Chip::MatchPath => o.match_path = !o.match_path,
            Chip::Regex => o.regex = !o.regex,
        }
        o
    }

    /// The chip's key in the settings file.
    pub const fn settings_key(self) -> &'static str {
        match self {
            Chip::MatchCase => "match_case",
            Chip::MatchPath => "match_path",
            Chip::Regex => "regex",
        }
    }
}

/// The chip's surface: [`Surface::ChipOn`] or [`Surface::ChipOff`].
pub const fn chip_surface(on: bool) -> Surface {
    if on {
        Surface::ChipOn
    } else {
        Surface::ChipOff
    }
}

/// The colour of a chip's label: `green-ink` under the pointer while off,
/// else `ink-strong`.
pub const fn chip_text(on: bool, hovered: bool) -> Token {
    if hovered && !on {
        Token::GreenInk
    } else {
        Token::InkStrong
    }
}

/// A chip's width for a label `label_width` points wide: the label, its
/// padding and rim, rounded up to whole points and at least
/// [`metrics::TOGGLE_MIN_WIDTH`].
pub fn chip_width(label_width: f64) -> f64 {
    let padded = label_width + 2.0 * (metrics::TOGGLE_PADDING_X + metrics::SELECTED_BORDER);
    padded.ceil().max(metrics::TOGGLE_MIN_WIDTH)
}
