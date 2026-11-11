//! Quick Look on Space, as far as it is a rule: what Space and Escape do to
//! the system Quick Look panel, which selected rows it shows and when it has
//! to reload. The panel itself is macOS's own (`QLPreviewPanel`, driven from
//! `appkit::window::quick_look`).
//!
//! As in Finder: Space in the results opens the panel on the selection and
//! Space again closes it; Escape closes it too. While it is open the arrow
//! keys move the selection and the panel follows. Space in the search field
//! stays a space ([`crate::actions::search_field_key`]).

#![forbid(unsafe_code)]

use std::ops::Range;

use crate::actions::{Key, Modifiers};
use crate::controller::{CopiedPaths, MAX_KEPT_SELECTION};

/// Most selected files the panel shows: as many as the window keeps
/// selected across refreshes, whose paths come without a search, so Select
/// All over millions of results resolves no more than this.
pub const MAX_PREVIEW_ITEMS: usize = MAX_KEPT_SELECTION;

/// What a key does to the Quick Look panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PreviewCommand {
    /// Show the selection in the panel.
    Open,
    /// Close the panel.
    Close,
}

/// What a key press in the results does to the Quick Look panel, given
/// whether it is `open`.
///
/// - Space: [`PreviewCommand::Open`] when closed, [`PreviewCommand::Close`]
///   when open.
/// - Escape: [`PreviewCommand::Close`] when open; with the panel closed it
///   is left to [`crate::actions::escape_action`].
///
/// With any modifier held, and for every other key, `None`: Return still
/// opens, and the arrow keys move the selection, which the panel follows.
pub fn preview_command(key: Key, mods: Modifiers, open: bool) -> Option<PreviewCommand> {
    let plain = !mods.command && !mods.shift && !mods.option && !mods.control;
    if !plain {
        return None;
    }
    match (key, open) {
        (Key::Space, false) => Some(PreviewCommand::Open),
        (Key::Space, true) | (Key::Escape, true) => Some(PreviewCommand::Close),
        _ => None,
    }
}

/// The selected rows the panel shows: the selection's `ranges` (in row
/// order, as the table gives them), cut after [`MAX_PREVIEW_ITEMS`] rows.
pub fn preview_rows(ranges: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut left = MAX_PREVIEW_ITEMS;
    let mut out = Vec::new();
    for r in ranges {
        if left == 0 {
            break;
        }
        let len = r.end.saturating_sub(r.start).min(left);
        if len > 0 {
            out.push(r.start..r.start + len);
            left -= len;
        }
    }
    out
}

/// The files the panel shows, by full path, in the selection's order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PreviewItems {
    paths: Vec<String>,
}

impl PreviewItems {
    /// Takes the selection's newly resolved paths and returns whether the
    /// panel must reload.
    ///
    /// The same files again (a refresh re-selecting them while the index
    /// changes) don't reload it, so the preview never flickers; an
    /// outdated answer (rows only readable from newer results than the
    /// table shows) keeps what is shown until the next page.
    pub fn update(&mut self, resolved: CopiedPaths) -> bool {
        if resolved.outdated || resolved.paths == self.paths {
            return false;
        }
        self.paths = resolved.paths;
        true
    }

    /// Forgets the files (the panel closed).
    pub fn clear(&mut self) {
        self.paths.clear();
    }

    /// How many files the panel shows.
    pub fn len(&self) -> usize {
        self.paths.len()
    }

    /// Whether the panel shows no file.
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// The full path of the `index`th file.
    pub fn get(&self, index: usize) -> Option<&str> {
        self.paths.get(index).map(String::as_str)
    }
}
