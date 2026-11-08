//! Where the search window's parts go: the layout maths of the
//! Main board and the canvas's component stylesheet (`ne-` classes),
//! transcribed from CSS flex and grid rules into plain functions of the
//! window's width and the measured text. Points, with y running down from
//! the top of the containing view (the AppKit views are flipped).
//!
//! - [`top_bar`]: the title row, centred on the window's traffic lights,
//!   then the search field and the option chips, which move under the field
//!   when it would be narrower than 320 points (`flex: 1 1 320px`).
//! - [`title_row`]: the title and a Preview build's channel tag, centred
//!   together, each line box centred on its box.
//! - [`window`]: the top bar, the access callout, the results card and the
//!   status bar, stacked.
//! - [`card`] and [`column_widths`]: the header over the rows and the four
//!   columns (`minmax(220px, 1.25fr) minmax(200px, 1.5fr) 88px 156px`).
//! - [`banner`]: the access callout's text and buttons.
//! - [`status_bar`], [`hints`] and [`key_width`]: the status bar's three
//!   slots, flowing and wrapping like the board's flex row.
//! - [`empty_state`]: "No matches" with its syntax examples, and the
//!   invalid regular expression message.

#![forbid(unsafe_code)]

use crate::theme::{SPACE_2, SPACE_3, SPACE_4, SPACE_5, SPACE_6, metrics, styles};

/// A rectangle, y running down.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

impl Rect {
    /// The rectangle at (`x`, `y`) of `width` × `height`.
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    /// The right edge.
    pub fn right(&self) -> f64 {
        self.x + self.width
    }

    /// The bottom edge.
    pub fn bottom(&self) -> f64 {
        self.y + self.height
    }
}

/// Room left round a first window on a screen too small for the design's
/// size.
const SCREEN_MARGIN: f64 = 48.0;

/// The window's first size on a screen whose usable area is
/// `screen_width` × `screen_height`: the design's 1280 × 800, or the
/// screen less 48 points when smaller, never under the 760 × 480 minimum.
pub fn initial_size(screen_width: f64, screen_height: f64) -> (f64, f64) {
    (
        metrics::WINDOW_WIDTH
            .min(screen_width - SCREEN_MARGIN)
            .max(metrics::WINDOW_MIN_WIDTH),
        metrics::WINDOW_HEIGHT
            .min(screen_height - SCREEN_MARGIN)
            .max(metrics::WINDOW_MIN_HEIGHT),
    )
}

/// The search field's flex basis: narrower, the chips wrap.
pub const SEARCH_BASIS: f64 = 320.0;

/// The top bar's parts. See [`top_bar`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TopBarLayout {
    /// The bar's height.
    pub height: f64,
    /// The title row (the title is centred in it).
    pub title_row: Rect,
    /// The search field.
    pub search: Rect,
    /// The chips, in [`crate::chips::Chip::ALL`]'s order.
    pub chips: [Rect; 3],
    /// Whether the chips moved under the field.
    pub wrapped: bool,
}

/// The top bar of a window `width` points wide (`.ne-topbar`,
/// `.ne-titlebar`, `.ne-toolbar`).
///
/// The title row is centred on the traffic lights, `lights_center_y` points
/// from the window's top (the design's 22 when `None`), but never above the
/// top; the toolbar follows 10 points under it. The chips, `chip_widths`
/// wide and 8 points apart, end at the right padding and are centred on the
/// 46-point field; when the field would be narrower than
/// [`SEARCH_BASIS`] they wrap to a row of their own, 12 points under it.
pub fn top_bar(width: f64, lights_center_y: Option<f64>, chip_widths: [f64; 3]) -> TopBarLayout {
    let [pad_top, pad_right, pad_bottom, pad_left] = metrics::TOPBAR_PADDING;
    let row = metrics::TITLE_ROW_HEIGHT;
    let center = lights_center_y.unwrap_or(pad_top + row / 2.0);
    let title_y = (center - row / 2.0).max(0.0);
    let inner = (width - pad_left - pad_right).max(0.0);
    let title_row = Rect::new(pad_left, title_y, inner, row);
    let top = title_row.bottom() + metrics::TITLE_ROW_GAP;

    let gap = metrics::TOGGLE_GAP;
    let group = chip_widths.iter().sum::<f64>() + gap * (chip_widths.len() - 1) as f64;
    let wrapped = SEARCH_BASIS + metrics::TOOLBAR_GAP + group > inner;
    let field = metrics::SEARCH_HEIGHT;
    let chip_h = metrics::TOGGLE_HEIGHT;
    let (search, mut x, chip_y, bottom) = if wrapped {
        let chips_y = top + field + metrics::TOOLBAR_GAP;
        (
            Rect::new(pad_left, top, inner, field),
            pad_left,
            chips_y,
            chips_y + chip_h,
        )
    } else {
        let search_w = inner - metrics::TOOLBAR_GAP - group;
        (
            Rect::new(pad_left, top, search_w, field),
            pad_left + search_w + metrics::TOOLBAR_GAP,
            top + (field - chip_h) / 2.0,
            top + field,
        )
    };
    let chips = chip_widths.map(|w| {
        let r = Rect::new(x, chip_y, w, chip_h);
        x += w + gap;
        r
    });
    TopBarLayout {
        height: bottom + pad_bottom,
        title_row,
        search,
        chips,
        wrapped,
    }
}

/// The search field's clear button. See [`clear_button`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClearButtonLayout {
    /// The click target, a square inside the field.
    pub button: Rect,
    /// The line box its × is drawn in, as wide as the button.
    pub mark: Rect,
}

/// The clear button of the field at `search` (`.ne-search .clear`): a
/// [`metrics::CLEAR_SIZE`] square [`metrics::CLEAR_INSET`] points inside
/// the field's top and right, and its × in a line box `line_height` tall
/// whose baseline sits `baseline` points down it (where CSS puts it,
/// [`crate::settings_view::layout::css_baseline`]).
///
/// The line box is placed so the mark's ink, from `ink.0` to `ink.1`
/// points above the baseline, is centred in the button. Centring the line
/// box instead leaves a × off centre by however far the font draws it from
/// the line's middle.
pub fn clear_button(
    search: Rect,
    line_height: f64,
    baseline: f64,
    ink: (f64, f64),
) -> ClearButtonLayout {
    let size = metrics::CLEAR_SIZE;
    let button = Rect::new(
        search.right() - metrics::CLEAR_INSET - size,
        search.y + metrics::CLEAR_INSET,
        size,
        size,
    );
    let ink_middle = baseline - (ink.0 + ink.1) / 2.0;
    let mark = Rect::new(
        button.x,
        button.y + button.height / 2.0 - ink_middle,
        button.width,
        line_height,
    );
    ClearButtonLayout { button, mark }
}

/// The title row's parts. See [`title_row`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TitleRowLayout {
    /// The title's line box.
    pub title: Rect,
    /// A Preview build's channel tag.
    pub tag: Option<Rect>,
    /// The tag's line box, centred in it.
    pub tag_text: Option<Rect>,
}

/// The title, `title_width` wide, and a Preview build's channel tag, whose
/// text is `tag_text_width` wide, centred together across a window `width`
/// points wide in `row`, the title row (`.ne-title`, `.ne-channel`).
///
/// Each line box is centred on its box, as `align-items: center` centres
/// it: the title's in the row, the tag's text in the 18-point tag, which
/// has 7 points each side and follows the title 8 points on. The text is
/// drawn in these line boxes with CSS baselines
/// ([`crate::settings_view::layout::css_baseline`]).
pub fn title_row(
    width: f64,
    row: Rect,
    title_width: f64,
    tag_text_width: Option<f64>,
) -> TitleRowLayout {
    let pad = metrics::CHANNEL_TAG_PADDING_X;
    let tag_h = metrics::CHANNEL_TAG_HEIGHT;
    let tag_w = tag_text_width.map(|w| w + 2.0 * pad);
    let group = title_width + tag_w.map_or(0.0, |w| metrics::TITLE_GAP + w);
    let x = ((width - group) / 2.0).round();
    let title_line = styles::WINDOW_TITLE.line_height;
    let title = Rect::new(
        x,
        row.y + (row.height - title_line) / 2.0,
        title_width,
        title_line,
    );
    let tag = tag_w.map(|w| {
        Rect::new(
            title.right() + metrics::TITLE_GAP,
            row.y + (row.height - tag_h) / 2.0,
            w,
            tag_h,
        )
    });
    let tag_line = styles::CHANNEL_TAG.line_height;
    let tag_text = tag
        .zip(tag_text_width)
        .map(|(t, w)| Rect::new(t.x + pad, t.y + (t.height - tag_line) / 2.0, w, tag_line));
    TitleRowLayout {
        title,
        tag,
        tag_text,
    }
}

/// The window's parts. See [`window`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowLayout {
    /// The top bar, across the window's top.
    pub top_bar: Rect,
    /// The access callout, when it shows.
    pub banner: Option<Rect>,
    /// The results card.
    pub card: Rect,
    /// The status bar, across the window's bottom.
    pub status: Rect,
}

/// The parts of a `width` × `height` window: the top bar, then the access
/// callout (`banner_height`, when it shows) and the results card, each 16
/// points in from the sides and 16 under what is above, and the status
/// bar at the bottom. The card takes the height left, never less than 0.
pub fn window(
    width: f64,
    height: f64,
    top_bar_height: f64,
    banner_height: Option<f64>,
    status_height: f64,
) -> WindowLayout {
    let m = metrics::CARD_MARGIN;
    let top_bar = Rect::new(0.0, 0.0, width, top_bar_height);
    let banner = banner_height.map(|h| Rect::new(m, top_bar.bottom() + m, width - 2.0 * m, h));
    let card_y = banner.map_or(top_bar.bottom(), |b| b.bottom()) + m;
    let status = Rect::new(0.0, height - status_height, width, status_height);
    let card = Rect::new(m, card_y, width - 2.0 * m, (status.y - card_y).max(0.0));
    WindowLayout {
        top_bar,
        banner,
        card,
        status,
    }
}

/// The widths of the Name, Path, Size and Date Modified columns in
/// `content` points (the card less its 16-point padding a side), 16 points
/// apart.
///
/// Size and Date Modified are fixed. Name and Path share the rest 1.25 to
/// 1.5, as CSS grid shares `fr` units: a track whose share falls under its
/// minimum (Name 220, Path 200) is fixed at it and the other takes the
/// rest. Where the rest can't hold both minimums (windows under 776 points
/// wide) Path gives way, so the table never scrolls sideways; CSS would
/// scroll the grid, whose `min-width` is the window's 760.
pub fn column_widths(content: f64) -> [f64; 4] {
    let (name_min, name_fr) = metrics::NAME_COLUMN;
    let (path_min, path_fr) = metrics::PATH_COLUMN;
    let fixed = metrics::SIZE_COLUMN + metrics::DATE_COLUMN + 3.0 * metrics::COLUMN_GAP;
    let flex = (content - fixed).max(0.0);
    let fr = flex / (name_fr + path_fr);
    let (name, path) = if name_fr * fr < name_min {
        // Name is fixed first (its minimum is the larger share); Path takes
        // the rest, even under its own minimum.
        (name_min, (flex - name_min).max(0.0))
    } else if path_fr * fr < path_min {
        ((flex - path_min).max(0.0), path_min)
    } else {
        (name_fr * fr, path_fr * fr)
    };
    [name, path, metrics::SIZE_COLUMN, metrics::DATE_COLUMN]
}

/// The four columns: widths and where each one's content starts in the
/// card.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Columns {
    /// [`column_widths`].
    pub widths: [f64; 4],
    /// Each column's left edge, from the card's left edge.
    pub x: [f64; 4],
}

/// The results card's parts. See [`card`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CardLayout {
    /// The header row (`.ne-thead`, 44 points).
    pub header: Rect,
    /// The rows' scroll view, under the header.
    pub rows: Rect,
    /// The columns.
    pub columns: Columns,
}

/// The parts of a `width` × `height` results card whose rows' vertical
/// scroller takes `scroller` points of their width (a legacy scroller's
/// width; 0 for overlay scrollers, which float over the rows).
///
/// The columns share the card's width less its padding and the scroller,
/// so the rows' table is exactly as wide as the part of the scroll view
/// that shows it and every row, the selected one's rim included, shows
/// whole. The column titles use the same columns.
pub fn card(width: f64, height: f64, scroller: f64) -> CardLayout {
    let pad = metrics::HEADER_PADDING_X;
    let header = Rect::new(0.0, 0.0, width, metrics::HEADER_HEIGHT);
    let rows = Rect::new(
        0.0,
        header.bottom(),
        width,
        (height - header.height).max(0.0),
    );
    let widths = column_widths(width - 2.0 * pad - scroller.max(0.0));
    let mut x = [pad; 4];
    for i in 1..4 {
        x[i] = x[i - 1] + widths[i - 1] + metrics::COLUMN_GAP;
    }
    CardLayout {
        header,
        rows,
        columns: Columns { widths, x },
    }
}

/// The callout's rim.
const BANNER_BORDER: f64 = 1.0;
/// The callout text's top and bottom margin: `.callout p { margin: 4px 0
/// }`, which outranks `.ne-banner-text { margin: 0 }` in the stylesheet.
const BANNER_TEXT_MARGIN: f64 = 4.0;
/// The callout text's flex basis.
const BANNER_TEXT_BASIS: f64 = 360.0;
/// A button's height: `.btn`'s 20-point line, 6 points of padding and a
/// 2-point rim, top and bottom.
pub const BUTTON_HEIGHT: f64 = 20.0 + 2.0 * (6.0 + 2.0);

/// The access callout's parts. See [`banner`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BannerLayout {
    /// The callout's height.
    pub height: f64,
    /// The text.
    pub text: Rect,
    /// Grant Full Disk Access… and Not Now.
    pub buttons: [Rect; 2],
}

/// The access callout `width` points wide (`.ne-banner`): the text, whose
/// height for a width is `text_height`, then the two buttons
/// (`button_widths`) on the same row when the text keeps its 360-point
/// basis, else on a row of their own; everything centred in its row.
pub fn banner(
    width: f64,
    text_height: impl Fn(f64) -> f64,
    button_widths: [f64; 2],
) -> BannerLayout {
    let [pad_y, pad_x] = metrics::BANNER_PADDING;
    let left = BANNER_BORDER + pad_x;
    let top = BANNER_BORDER + pad_y;
    let inner = (width - 2.0 * left).max(0.0);
    let gap = metrics::BUTTON_GAP;
    let actions = button_widths[0] + gap + button_widths[1];
    let col_gap = SPACE_4;
    let one_row = BANNER_TEXT_BASIS + col_gap + actions <= inner;
    let text_w = if one_row {
        inner - col_gap - actions
    } else {
        inner
    };
    let th = text_height(text_w);
    let text_box = th + 2.0 * BANNER_TEXT_MARGIN;
    let (text_y, buttons_x, buttons_y, rows_h) = if one_row {
        let row = text_box.max(BUTTON_HEIGHT);
        (
            top + (row - text_box) / 2.0 + BANNER_TEXT_MARGIN,
            left + text_w + col_gap,
            top + (row - BUTTON_HEIGHT) / 2.0,
            row,
        )
    } else {
        let buttons_y = top + text_box + SPACE_3;
        (
            top + BANNER_TEXT_MARGIN,
            left,
            buttons_y,
            text_box + SPACE_3 + BUTTON_HEIGHT,
        )
    };
    let first = Rect::new(buttons_x, buttons_y, button_widths[0], BUTTON_HEIGHT);
    let second = Rect::new(
        first.right() + gap,
        buttons_y,
        button_widths[1],
        BUTTON_HEIGHT,
    );
    BannerLayout {
        height: 2.0 * top + rows_h,
        text: Rect::new(left, text_y, text_w, th),
        buttons: [first, second],
    }
}

/// The height of one line of the status bar: a key cap's. The board's bar
/// grows from 18-point text lines to this while the hints show; the window
/// keeps it, so the bar doesn't change height when the focus moves.
pub const STATUS_LINE: f64 = metrics::KEY_HEIGHT;
/// Between wrapped lines of the status bar.
const STATUS_GAP_Y: f64 = SPACE_2;

/// The status bar's slots. See [`status_bar`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StatusLayout {
    /// The bar's height.
    pub height: f64,
    /// The counts.
    pub counts: Rect,
    /// The hints or note, when there is one.
    pub middle: Option<Rect>,
    /// The index state.
    pub index: Rect,
}

/// The status bar `width` points wide (`.ne-status`): the counts
/// (`counts_width`), the middle slot (`middle_width`, cut to the bar's
/// width) and the index state (`index_width`) pushed to the right, each on
/// the current line when it fits and on a new line when not.
pub fn status_bar(
    width: f64,
    counts_width: f64,
    middle_width: Option<f64>,
    index_width: f64,
) -> StatusLayout {
    let [pad_top, pad_right, pad_bottom, pad_left] = metrics::STATUS_PADDING;
    let gap = metrics::STATUS_GAP_X;
    let end = width - pad_right;
    let line_step = STATUS_LINE + STATUS_GAP_Y;
    let mut y = pad_top;
    let counts = Rect::new(pad_left, y, counts_width, STATUS_LINE);
    let mut x = counts.right();
    let middle = middle_width.map(|w| {
        let w = w.min((end - pad_left).max(0.0));
        if x + gap + w > end {
            y += line_step;
            x = pad_left;
        } else {
            x += gap;
        }
        let r = Rect::new(x, y, w, STATUS_LINE);
        x = r.right();
        r
    });
    if x + gap + index_width > end {
        y += line_step;
    }
    let index = Rect::new(end - index_width, y, index_width, STATUS_LINE);
    StatusLayout {
        height: y + STATUS_LINE + pad_bottom,
        counts,
        middle,
        index,
    }
}

/// A key cap's width for a label `label_width` points wide: the label and
/// 6 points a side, rounded up, at least 22.
pub fn key_width(label_width: f64) -> f64 {
    (label_width + 2.0 * metrics::KEY_PADDING_X)
        .ceil()
        .max(metrics::KEY_MIN_WIDTH)
}

/// Where the key hints' parts go. See [`hints`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HintsLayout {
    /// Each hint's keys' left edges.
    pub keys: Vec<Vec<f64>>,
    /// Each hint's label's left edge.
    pub labels: Vec<f64>,
    /// The whole group's width.
    pub width: f64,
}

/// The key hints, each given as its keys' widths and its label's width:
/// keys 3 points apart, the label 6 points after them, hints 16 apart.
pub fn hints(items: &[(Vec<f64>, f64)]) -> HintsLayout {
    let mut out = HintsLayout::default();
    let mut x = 0.0;
    for (i, (keys, label)) in items.iter().enumerate() {
        if i > 0 {
            x += metrics::HINT_GAP;
        }
        let mut lefts = Vec::with_capacity(keys.len());
        for (k, w) in keys.iter().enumerate() {
            if k > 0 {
                x += metrics::KEY_GAP;
            }
            lefts.push(x);
            x += w;
        }
        x += metrics::HINT_KEY_GAP;
        out.keys.push(lefts);
        out.labels.push(x);
        x += label;
    }
    out.width = x;
    out
}

/// The empty state's widest box (`.ne-empty`).
const EMPTY_PADDING_X: f64 = SPACE_4;
/// Under the empty state's title.
const EMPTY_TITLE_GAP: f64 = 6.0;
/// Under "SEARCH SYNTAX".
const EMPTY_LABEL_GAP: f64 = SPACE_3;
/// The height of "SEARCH SYNTAX".
const EMPTY_LABEL_HEIGHT: f64 = 16.0;
/// A syntax token's height: a 20-point line and 2 points of padding.
const TOKEN_HEIGHT: f64 = 20.0 + 2.0 * metrics::CODE_PADDING_Y;
/// A meaning's line.
const MEANING_HEIGHT: f64 = 20.0;
/// Between syntax rows.
const SYNTAX_ROW_GAP: f64 = 10.0;

/// The width the empty state's text is set in, in a card `width` wide.
pub fn empty_content_width(width: f64) -> f64 {
    (width.min(metrics::EMPTY_MAX_WIDTH) - 2.0 * EMPTY_PADDING_X).max(0.0)
}

/// The empty state's parts. See [`empty_state`].
#[derive(Clone, Debug, PartialEq)]
pub struct EmptyLayout {
    /// The whole state's height, its padding included.
    pub height: f64,
    /// The title.
    pub title: Rect,
    /// The text under it.
    pub text: Rect,
    /// "SEARCH SYNTAX", when there are examples.
    pub label: Option<Rect>,
    /// The example tokens.
    pub tokens: Vec<Rect>,
    /// What each example finds.
    pub meanings: Vec<Rect>,
}

/// The empty state in a card `width` wide: centred, at most 600 points
/// wide with 16 points of padding a side and 56 above; the title
/// (`title_height` at [`empty_content_width`]), 6 points, the text
/// (`text_height`), then, with examples (`token_widths`), 24 points,
/// "SEARCH SYNTAX", 12 points and a row per example: the 24-point token and
/// its meaning, 16 points after the widest token, rows 10 apart. 32 points
/// of padding end it.
pub fn empty_state(
    width: f64,
    title_height: f64,
    text_height: f64,
    token_widths: &[f64],
) -> EmptyLayout {
    let column = width.min(metrics::EMPTY_MAX_WIDTH);
    let x = (width - column) / 2.0 + EMPTY_PADDING_X;
    let cw = empty_content_width(width);
    let title = Rect::new(x, metrics::EMPTY_PADDING_TOP, cw, title_height);
    let text = Rect::new(x, title.bottom() + EMPTY_TITLE_GAP, cw, text_height);
    let mut y = text.bottom();
    let mut label = None;
    let mut tokens = Vec::with_capacity(token_widths.len());
    let mut meanings = Vec::with_capacity(token_widths.len());
    if !token_widths.is_empty() {
        y += SPACE_5;
        let l = Rect::new(x, y, cw, EMPTY_LABEL_HEIGHT);
        y = l.bottom() + EMPTY_LABEL_GAP;
        label = Some(l);
        let dt = token_widths.iter().copied().fold(0.0, f64::max);
        let dd_x = x + dt + SPACE_4;
        for (i, w) in token_widths.iter().enumerate() {
            if i > 0 {
                y += SYNTAX_ROW_GAP;
            }
            tokens.push(Rect::new(x, y, *w, TOKEN_HEIGHT));
            meanings.push(Rect::new(
                dd_x,
                y + (TOKEN_HEIGHT - MEANING_HEIGHT) / 2.0,
                (x + cw - dd_x).max(0.0),
                MEANING_HEIGHT,
            ));
            y += TOKEN_HEIGHT;
        }
    }
    EmptyLayout {
        height: y + SPACE_6,
        title,
        text,
        label,
        tokens,
        meanings,
    }
}
