//! Contract tests for `fx_ui_macos::chips`: the search window's three
//! option chips (the canvas's ToggleChip): Aa (match case), Path
//! (match the full path) and .* (the whole search is one regular
//! expression), their state as the search's `QueryOptions`, their look and
//! their width.

use fx_query::QueryOptions;
use fx_ui_macos::chips::{Chip, chip_surface, chip_text, chip_width};
use fx_ui_macos::surface::Surface;
use fx_ui_macos::theme::{Token, metrics};
use fx_ui_macos::ui_text::OPTION_CHIPS;

#[test]
fn three_chips_in_the_boards_order() {
    let all: Vec<Chip> = Chip::ALL.into_iter().collect();
    assert_eq!(all, [Chip::MatchCase, Chip::MatchPath, Chip::Regex]);
    for (i, chip) in Chip::ALL.into_iter().enumerate() {
        assert_eq!(chip.index(), i);
        assert_eq!(Chip::from_index(i), Some(chip));
        assert_eq!(*chip.text(), OPTION_CHIPS[i]);
    }
    assert_eq!(Chip::from_index(3), None);
    assert_eq!(Chip::MatchCase.text().label, "Aa");
    assert_eq!(Chip::MatchPath.text().title, "Match full path");
    assert!(Chip::Regex.text().mono);
}

#[test]
fn every_chip_is_off_by_default() {
    let options = QueryOptions::default();
    for chip in Chip::ALL {
        assert!(!chip.is_on(options), "{chip:?}");
    }
}

#[test]
fn a_click_toggles_one_option_only() {
    let none = QueryOptions::default();
    let case = Chip::MatchCase.toggle(none);
    assert_eq!(
        case,
        QueryOptions {
            match_case: true,
            ..QueryOptions::default()
        }
    );
    let both = Chip::Regex.toggle(case);
    assert_eq!(
        both,
        QueryOptions {
            match_case: true,
            regex: true,
            ..QueryOptions::default()
        }
    );
    assert!(Chip::MatchCase.is_on(both) && Chip::Regex.is_on(both));
    assert!(!Chip::MatchPath.is_on(both));
    let path = Chip::MatchPath.toggle(none);
    assert!(path.match_path && !path.match_case && !path.regex);
    // A second click turns it off again.
    assert_eq!(Chip::Regex.toggle(both), case);
    for chip in Chip::ALL {
        assert_eq!(chip.toggle(chip.toggle(both)), both);
    }
}

#[test]
fn the_settings_keys() {
    let keys: Vec<&str> = Chip::ALL.into_iter().map(Chip::settings_key).collect();
    assert_eq!(keys, ["match_case", "match_path", "regex"]);
}

/// Off: raised on `panel`; on: pressed, washed and rimmed in green.
#[test]
fn surfaces_follow_the_state() {
    assert_eq!(chip_surface(false), Surface::ChipOff);
    assert_eq!(chip_surface(true), Surface::ChipOn);
}

/// `.ne-toggle { color: ink-strong }`, `:hover { color: green-ink }` and
/// `[aria-pressed=true] { color: ink-strong }`, which comes later in the
/// stylesheet and so wins over the hover.
#[test]
fn label_colours() {
    assert_eq!(chip_text(false, false), Token::InkStrong);
    assert_eq!(chip_text(false, true), Token::GreenInk);
    assert_eq!(chip_text(true, false), Token::InkStrong);
    assert_eq!(chip_text(true, true), Token::InkStrong);
}

/// `height 40; min-width 48; padding 0 14px; border 2px` (border-box).
#[test]
fn widths_are_the_label_plus_padding_and_rim_at_least_48() {
    let pad = 2.0 * (metrics::TOGGLE_PADDING_X + metrics::SELECTED_BORDER);
    assert_eq!(pad, 32.0);
    assert_eq!(chip_width(30.0), 62.0);
    assert_eq!(chip_width(16.0), metrics::TOGGLE_MIN_WIDTH);
    assert_eq!(chip_width(0.0), 48.0);
    // Fractional text widths round up to whole points.
    assert_eq!(chip_width(18.4), 51.0);
}
