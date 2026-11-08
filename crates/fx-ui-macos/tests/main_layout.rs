//! Contract tests for `fx_ui_macos::main_layout`: where the search window's
//! parts go (the Main board at 1280 × 800 and the `ne-` component
//! stylesheet), in points with y running down from the top.

use fx_ui_macos::main_layout::{
    CardLayout, Rect, banner, card, clear_button, column_widths, empty_content_width, empty_state,
    hints, initial_size, key_width, status_bar, title_row, top_bar, window,
};

const CHIPS: [f64; 3] = [51.0, 62.0, 48.0];

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

/// The owner's choice: 1280 × 800 to start, smaller on a smaller
/// screen, never under 760 × 480.
#[test]
fn the_first_window_size_fits_the_screen() {
    assert_eq!(initial_size(1512.0, 944.0), (1280.0, 800.0));
    assert_eq!(initial_size(1280.0, 775.0), (1232.0, 727.0));
    assert_eq!(initial_size(700.0, 400.0), (760.0, 480.0));
}

#[test]
fn rects_know_their_edges() {
    let r = Rect::new(16.0, 44.0, 100.0, 46.0);
    assert_eq!((r.right(), r.bottom()), (116.0, 90.0));
}

/// The board: 10 points above a 24-point title row, 10 below it, the
/// 46-point search field and 12 points under the toolbar.
#[test]
fn the_top_bar_at_the_boards_size() {
    let t = top_bar(1280.0, None, CHIPS);
    assert_eq!(t.title_row, Rect::new(16.0, 10.0, 1248.0, 24.0));
    assert!(!t.wrapped);
    // The chips sit at the right, 8 points apart; the field takes the rest.
    assert_eq!(t.search, Rect::new(16.0, 44.0, 1059.0, 46.0));
    assert_eq!(
        t.chips,
        [
            Rect::new(1087.0, 47.0, 51.0, 40.0),
            Rect::new(1146.0, 47.0, 62.0, 40.0),
            Rect::new(1216.0, 47.0, 48.0, 40.0),
        ]
    );
    assert_eq!(t.chips[2].right(), 1280.0 - 16.0);
    assert_eq!(t.height, 102.0);
}

/// macOS keeps the traffic lights where it puts them; the title row is
/// centred on them and the toolbar follows it.
#[test]
fn the_title_row_lines_up_with_the_traffic_lights() {
    let t = top_bar(1280.0, Some(14.0), CHIPS);
    assert_eq!(t.title_row.y, 2.0);
    assert_eq!(t.title_row.y + t.title_row.height / 2.0, 14.0);
    assert_eq!(t.search.y, 36.0);
    assert_eq!(t.height, 94.0);
    // Lights lower than the design's row move it down.
    let low = top_bar(1280.0, Some(26.0), CHIPS);
    assert_eq!((low.title_row.y, low.height), (14.0, 106.0));
    // Never above the window's top.
    assert_eq!(top_bar(1280.0, Some(4.0), CHIPS).title_row.y, 0.0);
}

/// `.ne-title` and `.ne-channel`, centred together: the title's 20-point
/// line box 2 points into the row, then 8 points on a Preview build's tag,
/// 18 points tall with 7 points each side and centred on the row. The tag's
/// 14-point line box is centred in it (`align-items: center`), 2 points
/// from its top and its bottom: AppKit's label had put the text 2 points
/// low.
#[test]
fn the_channel_tag_centres_its_text() {
    let row = Rect::new(16.0, 10.0, 1248.0, 24.0);
    let t = title_row(1280.0, row, 177.0, Some(80.0));
    // 177 + 8 + 94 = 279 points, centred in 1280 and rounded.
    assert_eq!(t.title, Rect::new(501.0, 12.0, 177.0, 20.0));
    let tag = t.tag.expect("a Preview build has a tag");
    assert_eq!(tag, Rect::new(686.0, 13.0, 94.0, 18.0));
    let text = t.tag_text.expect("the tag has text");
    assert_eq!(text, Rect::new(693.0, 15.0, 80.0, 14.0));
    assert_eq!(text.y - tag.y, tag.bottom() - text.bottom());
    assert_eq!(text.x - tag.x, tag.right() - text.right());
}

/// A release build has no tag: the title is centred alone.
#[test]
fn a_release_title_is_centred_alone() {
    let row = Rect::new(16.0, 10.0, 1248.0, 24.0);
    let t = title_row(1280.0, row, 177.0, None);
    assert_eq!(t.title, Rect::new(552.0, 12.0, 177.0, 20.0));
    assert_eq!((t.tag, t.tag_text), (None, None));
}

/// `.ne-search { flex: 1 1 320px }` and `.ne-toolbar { flex-wrap: wrap;
/// gap: 12px }`: the chips move under the field when the field would be
/// narrower than 320 points.
#[test]
fn chips_wrap_under_the_field_when_narrow() {
    // 320 + 12 + (51 + 8 + 62 + 8 + 48) = 509 points inside the padding.
    let fits = top_bar(509.0 + 32.0, None, CHIPS);
    assert!(!fits.wrapped);
    assert_eq!(fits.search.width, 320.0);
    let t = top_bar(508.0 + 32.0, None, CHIPS);
    assert!(t.wrapped);
    assert_eq!(t.search, Rect::new(16.0, 44.0, 508.0, 46.0));
    assert_eq!(
        t.chips,
        [
            Rect::new(16.0, 102.0, 51.0, 40.0),
            Rect::new(75.0, 102.0, 62.0, 40.0),
            Rect::new(145.0, 102.0, 48.0, 40.0),
        ]
    );
    assert_eq!(t.height, 102.0 + 40.0 + 12.0);
}

#[test]
fn the_window_stacks_top_bar_card_and_status_bar() {
    let w = window(1280.0, 800.0, 102.0, None, 44.0);
    assert_eq!(w.top_bar, Rect::new(0.0, 0.0, 1280.0, 102.0));
    assert_eq!(w.banner, None);
    assert_eq!(w.card, Rect::new(16.0, 118.0, 1248.0, 638.0));
    assert_eq!(w.status, Rect::new(0.0, 756.0, 1280.0, 44.0));
}

/// `.ne-banner { margin: 16px 16px 0 }` above the card, which keeps its
/// own 16-point margin.
#[test]
fn the_access_callout_sits_between_the_top_bar_and_the_card() {
    let w = window(1280.0, 800.0, 102.0, Some(74.0), 44.0);
    assert_eq!(w.banner, Some(Rect::new(16.0, 118.0, 1248.0, 74.0)));
    assert_eq!(w.card, Rect::new(16.0, 208.0, 1248.0, 548.0));
    // A window too short for the card gives it no height, never less.
    let tiny = window(760.0, 200.0, 102.0, Some(74.0), 44.0);
    assert_eq!(tiny.card.height, 0.0);
}

/// `grid-template-columns: minmax(220px, 1.25fr) minmax(200px, 1.5fr) 88px
/// 156px; column-gap: 16px`.
#[test]
fn columns_share_the_free_width_one_and_a_quarter_to_one_and_a_half() {
    // The board: a 1,248-point card, 16 points of padding a side.
    let w = column_widths(1216.0);
    assert!(close(w[0], 420.0) && close(w[1], 504.0), "{w:?}");
    assert_eq!((w[2], w[3]), (88.0, 156.0));
    assert!(close(w.iter().sum::<f64>() + 3.0 * 16.0, 1216.0));
    // From 484 points of free width down, Name stays at its 220...
    let w = column_widths(292.0 + 484.0);
    assert!(close(w[0], 220.0) && close(w[1], 264.0), "{w:?}");
    let w = column_widths(292.0 + 450.0);
    assert!(close(w[0], 220.0) && close(w[1], 230.0), "{w:?}");
    // ...and below 420 points Path gives way rather than the table
    // scrolling sideways: at the smallest window (760, a 728-point card)
    // it is 184 points wide.
    let w = column_widths(696.0);
    assert!(close(w[0], 220.0) && close(w[1], 184.0), "{w:?}");
    assert!(close(w.iter().sum::<f64>() + 48.0, 696.0));
}

/// The header's 16-point padding equals the rows' 8 points of inset plus
/// their 2-point rim and 6-point padding, so headers and cells line up.
#[test]
fn the_card_puts_the_header_over_the_rows() {
    let c = card(1248.0, 638.0, 0.0);
    assert_eq!(c.header, Rect::new(0.0, 0.0, 1248.0, 44.0));
    assert_eq!(c.rows, Rect::new(0.0, 44.0, 1248.0, 594.0));
    assert!(close(c.columns.x[0], 16.0));
    assert!(close(c.columns.x[1], 16.0 + 420.0 + 16.0));
    assert!(close(c.columns.x[2], 452.0 + 504.0 + 16.0));
    assert!(close(c.columns.x[3], 972.0 + 88.0 + 16.0));
    assert!(close(c.columns.x[3] + c.columns.widths[3], 1248.0 - 16.0));
    assert_eq!(card(1248.0, 20.0, 0.0).rows.height, 0.0);
}

/// The rows' table is exactly as wide as the part of the scroll view that
/// shows it. A legacy scroller (17 points, shown when a mouse is connected
/// or "Show scroll bars" is Always) takes its width out of Name and Path,
/// which share the loss as CSS grid shares a classic scrollbar's, so the
/// selected row's rim shows whole at the right instead of cut off under
/// the scroller. Overlay scrollers float over the rows
/// and take nothing.
#[test]
fn the_rows_fit_beside_a_legacy_scroller() {
    // The table: the columns plus the 16-point gap, which NSTableView adds
    // once per column.
    let table = |c: &CardLayout| c.columns.widths.iter().sum::<f64>() + 4.0 * 16.0;
    // The scroll view: the card less 8 points of inset a side.
    let overlay = card(1068.0, 600.0, 0.0);
    assert!(close(table(&overlay), 1068.0 - 16.0));
    let legacy = card(1068.0, 600.0, 17.0);
    assert!(close(table(&legacy), 1068.0 - 16.0 - 17.0));
    let (o, l) = (overlay.columns.widths, legacy.columns.widths);
    assert!(close(l[0] + l[1], o[0] + o[1] - 17.0));
    assert!(close(l[0] / l[1], 1.25 / 1.5) && l[0] < o[0]);
    assert_eq!(l[2..], o[2..]);
    // The column titles stay over their cells.
    assert!(close(
        legacy.columns.x[3] + legacy.columns.widths[3],
        1068.0 - 16.0 - 17.0
    ));
}

/// `.ne-search .clear`: a 38-point square 4 points inside the field's top
/// and right, its × centred in it by the mark's ink. A CSS line box would
/// leave SF's × (ink 0.97 to 11.55 points above the baseline at 22 points,
/// the baseline 19 points into the line) 1.74 points low, and AppKit's
/// label put it about 6 points high: it belongs centred.
#[test]
fn the_clear_mark_is_centred_in_the_field() {
    let search = Rect::new(16.0, 44.0, 1059.0, 46.0);
    let c = clear_button(search, 22.0, 19.0, (0.97, 11.55));
    assert_eq!(c.button, Rect::new(1033.0, 48.0, 38.0, 38.0));
    assert_eq!(
        (c.mark.x, c.mark.width, c.mark.height),
        (1033.0, 38.0, 22.0)
    );
    // The ink's middle, drawn with the baseline 19 points into the line.
    let ink_middle = c.mark.y + 19.0 - (0.97 + 11.55) / 2.0;
    assert!(close(ink_middle, search.y + search.height / 2.0));
    assert!(close(ink_middle, c.button.y + c.button.height / 2.0));
    // A mark whose ink sits on its line's middle keeps its line centred.
    let even = clear_button(search, 22.0, 16.0, (2.0, 8.0));
    assert!(close(even.mark.y, 48.0 + 8.0));
}

/// `.ne-banner`: a 1-point caution rim, 12 × 16 points of padding, the
/// text (`flex: 1 1 360px`, with the callout's 4-point paragraph margins)
/// and the two 36-point buttons 8 apart, 16 after the text; centred in the
/// row.
#[test]
fn the_callout_keeps_its_buttons_beside_the_text() {
    let b = banner(
        1248.0,
        |w| if w >= 1000.0 { 20.0 } else { 40.0 },
        [193.0, 86.0],
    );
    assert_eq!(b.text, Rect::new(17.0, 17.0, 911.0, 40.0));
    assert_eq!(
        b.buttons,
        [
            Rect::new(944.0, 19.0, 193.0, 36.0),
            Rect::new(1145.0, 19.0, 86.0, 36.0),
        ]
    );
    assert_eq!(b.buttons[1].right(), 1248.0 - 17.0);
    assert_eq!(b.height, 74.0);
    // One line of text: the buttons set the row's height.
    let one = banner(1248.0, |_| 20.0, [193.0, 86.0]);
    assert_eq!(one.text, Rect::new(17.0, 21.0, 911.0, 20.0));
    assert_eq!(one.buttons[0].y, 13.0);
    assert_eq!(one.height, 62.0);
}

#[test]
fn the_callout_puts_its_buttons_under_the_text_when_narrow() {
    // 360 + 16 + 287 = 663 points inside the rim and padding.
    let b = banner(663.0 + 34.0 - 1.0, |_| 40.0, [193.0, 86.0]);
    assert_eq!(b.text, Rect::new(17.0, 17.0, 662.0, 40.0));
    assert_eq!(
        b.buttons[0],
        Rect::new(17.0, 13.0 + 48.0 + 12.0, 193.0, 36.0)
    );
    assert_eq!(b.buttons[1].x, 17.0 + 193.0 + 8.0);
    assert_eq!(b.height, 1.0 + 12.0 + 48.0 + 12.0 + 36.0 + 12.0 + 1.0);
}

/// `.ne-status { padding: 10px 24px 12px; gap: 8px 24px; flex-wrap: wrap }`
/// with the index pushed right (`margin-left: auto`). Lines are 22 points,
/// a key cap's height, whether or not the hints show, so the bar keeps its
/// height when the focus moves.
#[test]
fn the_status_bar_flows_left_to_right_with_the_index_at_the_right() {
    let s = status_bar(1280.0, 230.0, Some(371.0), 121.0);
    assert_eq!(s.counts, Rect::new(24.0, 10.0, 230.0, 22.0));
    assert_eq!(s.middle, Some(Rect::new(278.0, 10.0, 371.0, 22.0)));
    assert_eq!(s.index, Rect::new(1135.0, 10.0, 121.0, 22.0));
    assert_eq!(s.height, 44.0);
    let none = status_bar(1280.0, 230.0, None, 121.0);
    assert_eq!(none.middle, None);
    assert_eq!(none.index, s.index);
    assert_eq!(none.height, 44.0);
}

#[test]
fn the_status_bar_wraps_what_does_not_fit() {
    let s = status_bar(760.0, 230.0, Some(371.0), 121.0);
    assert_eq!(s.middle, Some(Rect::new(278.0, 10.0, 371.0, 22.0)));
    assert_eq!(s.index, Rect::new(615.0, 40.0, 121.0, 22.0));
    assert_eq!(s.height, 10.0 + 22.0 + 8.0 + 22.0 + 12.0);
    // A note wider than the bar starts a line and is cut to its width.
    let s = status_bar(760.0, 230.0, Some(1000.0), 121.0);
    assert_eq!(s.middle, Some(Rect::new(24.0, 40.0, 712.0, 22.0)));
    assert_eq!(s.index, Rect::new(615.0, 70.0, 121.0, 22.0));
}

/// `.ne-key { min-width: 22px; padding: 0 6px }`.
#[test]
fn key_caps_are_at_least_square() {
    assert_eq!(key_width(7.0), 22.0);
    assert_eq!(key_width(10.0), 22.0);
    assert_eq!(key_width(30.2), 43.0);
}

/// `.ne-hints { gap: 6px 16px }`, `.ne-hint { gap: 6px }`, `.ne-keys {
/// gap: 3px }`.
#[test]
fn hints_place_keys_then_their_label() {
    let h = hints(&[(vec![22.0], 35.0), (vec![22.0, 22.0], 105.0)]);
    assert_eq!(h.keys, vec![vec![0.0], vec![79.0, 104.0]]);
    assert_eq!(h.labels, vec![28.0, 132.0]);
    assert_eq!(h.width, 237.0);
    assert_eq!(hints(&[]).width, 0.0);
}

/// `.ne-empty { max-width: 600px; margin: 0 auto; padding: 56px 16px 32px
/// }`: the title, 6 points, the text, 24 points, "SEARCH SYNTAX", 12
/// points, then rows of a 24-point token and its meaning, 10 apart, the
/// meanings 16 points after the widest token.
#[test]
fn the_empty_state_is_a_centred_column() {
    assert_eq!(empty_content_width(1248.0), 568.0);
    assert_eq!(empty_content_width(500.0), 468.0);
    let e = empty_state(1248.0, 26.0, 24.0, &[62.0, 94.0, 86.0, 102.0, 125.0, 117.0]);
    assert_eq!(e.title, Rect::new(340.0, 56.0, 568.0, 26.0));
    assert_eq!(e.text, Rect::new(340.0, 88.0, 568.0, 24.0));
    assert_eq!(e.label, Some(Rect::new(340.0, 136.0, 568.0, 16.0)));
    assert_eq!(e.tokens.len(), 6);
    assert_eq!(e.tokens[0], Rect::new(340.0, 164.0, 62.0, 24.0));
    assert_eq!(e.tokens[5], Rect::new(340.0, 334.0, 117.0, 24.0));
    assert_eq!(e.meanings[0], Rect::new(481.0, 166.0, 427.0, 20.0));
    assert_eq!(e.meanings[1].y, 200.0);
    assert_eq!(e.height, 358.0 + 32.0);
}

/// The invalid regular expression: a title and one line, no examples.
#[test]
fn the_regex_error_has_no_examples() {
    let e = empty_state(800.0, 26.0, 20.0, &[]);
    assert_eq!(e.title, Rect::new(116.0, 56.0, 568.0, 26.0));
    assert_eq!(e.text, Rect::new(116.0, 88.0, 568.0, 20.0));
    assert_eq!(e.label, None);
    assert!(e.tokens.is_empty() && e.meanings.is_empty());
    assert_eq!(e.height, 108.0 + 32.0);
}
