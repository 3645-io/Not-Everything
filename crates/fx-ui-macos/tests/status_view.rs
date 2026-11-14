//! Contract tests for `fx_ui_macos::status_view`: the status bar's three
//! slots (counts, hints or a note, the index) from the window's state, by
//! the canvas and the owner's choices.

use std::time::Duration;

use fx_ui_macos::actions::Focus;
use fx_ui_macos::key_caps::RESULT_HINTS;
use fx_ui_macos::onboarding::{
    INDEXING_PROTECTED_SUFFIX, IndexScope, LIMITED_MODE_SUFFIX, ScopeNote,
};
use fx_ui_macos::status_view::{Copied, Counts, IndexSlot, Middle, StatusInput, status_view};

fn ready(results: usize) -> StatusInput<'static> {
    StatusInput {
        has_query: true,
        results,
        objects: 2_014_377,
        search_time: Some(Duration::from_millis(9)),
        ..StatusInput::default()
    }
}

#[test]
fn the_main_boards_status_bar() {
    let v = status_view(&StatusInput {
        focus: Focus::Results,
        selected: 1,
        ..ready(8)
    });
    assert_eq!(
        v.counts,
        Counts {
            figure: "8".into(),
            rest: " results \u{b7} 2,014,377 objects".into(),
        }
    );
    assert_eq!(v.counts.text(), "8 results · 2,014,377 objects");
    assert_eq!(v.middle, Middle::Hints(&RESULT_HINTS));
    assert_eq!(
        v.index,
        IndexSlot::UpToDate("Up to date \u{b7} 9 ms".into())
    );
}

#[test]
fn counts_without_a_query_and_in_the_singular() {
    let v = status_view(&StatusInput {
        has_query: false,
        results: 2_014_377,
        ..ready(0)
    });
    assert_eq!(v.counts.text(), "2,014,377 objects");
    assert_eq!(v.counts.figure, "2,014,377");
    let v = status_view(&StatusInput {
        objects: 1,
        ..ready(1)
    });
    assert_eq!(v.counts.text(), "1 result · 1 object");
    let v = status_view(&StatusInput {
        objects: 12_345,
        ..ready(1_234)
    });
    assert_eq!(v.counts.text(), "1,234 results · 12,345 objects");
    assert_eq!(
        status_view(&ready(0)).counts.text(),
        "0 results · 2,014,377 objects"
    );
}

#[test]
fn the_index_slot_while_indexing() {
    // The FirstRun board: the previous index held about 2,010,000 entries.
    let v = status_view(&StatusInput {
        objects: 1_204_311,
        indexing: true,
        previous_objects: Some(2_010_000),
        ..ready(5)
    });
    let IndexSlot::Indexing { text, progress } = v.index else {
        panic!("{:?}", v.index);
    };
    assert_eq!(text, "Indexing\u{2026} 1,204,311 of about 2,010,000");
    let progress = progress.expect("a known total gives a fraction");
    assert!((progress - 1_204_311.0 / 2_010_000.0).abs() < 1e-12);
    assert_eq!(v.counts.text(), "5 results · 1,204,311 objects");

    // A first run knows no total: the count alone, and no fraction.
    let v = status_view(&StatusInput {
        objects: 1_204_311,
        indexing: true,
        previous_objects: None,
        ..ready(5)
    });
    assert_eq!(
        v.index,
        IndexSlot::Indexing {
            text: "Indexing\u{2026} 1,204,311".into(),
            progress: None,
        }
    );

    // Past the previous size the estimate is wrong: the count alone.
    for previous in [Some(1_000_000), Some(1_204_311), Some(0)] {
        let v = status_view(&StatusInput {
            objects: 1_204_311,
            indexing: true,
            previous_objects: previous,
            ..ready(5)
        });
        assert_eq!(
            v.index,
            IndexSlot::Indexing {
                text: "Indexing\u{2026} 1,204,311".into(),
                progress: None,
            },
            "{previous:?}"
        );
    }
}

#[test]
fn up_to_date_with_and_without_a_search_time() {
    let at = |t: Option<Duration>| {
        status_view(&StatusInput {
            search_time: t,
            ..ready(3)
        })
        .index
    };
    assert_eq!(at(None), IndexSlot::UpToDate("Up to date".into()));
    assert_eq!(
        at(Some(Duration::from_micros(300))),
        IndexSlot::UpToDate("Up to date \u{b7} <1 ms".into())
    );
    assert_eq!(
        at(Some(Duration::from_millis(1_250))),
        IndexSlot::UpToDate("Up to date \u{b7} 1,250 ms".into())
    );
}

#[test]
fn errors_replace_the_index_state() {
    let e = "Cannot index protected folders: disk full";
    let v = status_view(&StatusInput {
        error: Some(e),
        ..ready(3)
    });
    assert_eq!(v.index, IndexSlot::Error(e.into()));
    let v = status_view(&StatusInput {
        error: Some(e),
        indexing: true,
        previous_objects: Some(3_000_000),
        ..ready(3)
    });
    assert_eq!(v.index, IndexSlot::Error(e.into()));
}

#[test]
fn hints_only_while_the_results_have_the_focus_and_a_selection() {
    let middle = |focus, selected| {
        status_view(&StatusInput {
            focus,
            selected,
            ..ready(8)
        })
        .middle
    };
    assert_eq!(middle(Focus::Results, 1), Middle::Hints(&RESULT_HINTS));
    assert_eq!(middle(Focus::Results, 3), Middle::Hints(&RESULT_HINTS));
    assert_eq!(middle(Focus::Results, 0), Middle::Empty);
    // In the search field Return only moves to the results: no hints.
    assert_eq!(
        middle(Focus::SearchField, 1),
        Middle::Note("1 selected".into())
    );
    assert_eq!(middle(Focus::SearchField, 0), Middle::Empty);
}

#[test]
fn notes_fill_the_middle_slot_when_no_hint_applies() {
    let login = "Launch at Login requires approval in System Settings \u{203a} Login Items";
    let v = status_view(&StatusInput {
        selected: 1_200,
        scope: Some(ScopeNote::IndexingProtected),
        note: Some(login),
        ..ready(8)
    });
    assert_eq!(
        v.middle,
        Middle::Note(format!(
            "1,200 selected \u{b7} {INDEXING_PROTECTED_SUFFIX} \u{b7} {login}"
        ))
    );
    // With the hints showing, the notes wait.
    let v = status_view(&StatusInput {
        focus: Focus::Results,
        selected: 2,
        scope: Some(ScopeNote::IndexingProtected),
        note: Some(login),
        ..ready(8)
    });
    assert_eq!(v.middle, Middle::Hints(&RESULT_HINTS));
}

#[test]
fn limited_mode_is_noted_only_after_not_now() {
    let middle = |banner_shown| {
        status_view(&StatusInput {
            scope: Some(ScopeNote::LimitedMode),
            banner_shown,
            ..ready(8)
        })
        .middle
    };
    // The callout above the results says it while it shows.
    assert_eq!(middle(true), Middle::Empty);
    assert_eq!(middle(false), Middle::Note(LIMITED_MODE_SUFFIX.into()));
    // The other scope note shows either way.
    let v = status_view(&StatusInput {
        scope: Some(ScopeNote::IndexingProtected),
        banner_shown: true,
        ..ready(8)
    });
    assert_eq!(v.middle, Middle::Note(INDEXING_PROTECTED_SUFFIX.into()));
}

#[test]
fn a_copy_cut_at_the_limit_is_noted_even_over_the_hints() {
    let v = status_view(&StatusInput {
        focus: Focus::Results,
        selected: 250_000,
        copy_limit: Some(Copied::Paths(100_000)),
        ..ready(300_000)
    });
    assert_eq!(
        v.middle,
        Middle::Note("Copied the first 100,000 paths only (copy limit)".into())
    );
}

#[test]
fn a_file_copy_cut_at_the_limit_says_items() {
    // Cmd+C copies the files themselves; Finder calls them items, since
    // folders are copied too.
    let v = status_view(&StatusInput {
        focus: Focus::Results,
        selected: 250_000,
        copy_limit: Some(Copied::Files(100_000)),
        ..ready(300_000)
    });
    assert_eq!(
        v.middle,
        Middle::Note("Copied the first 100,000 items only (copy limit)".into())
    );
}

#[test]
fn scope_notes_follow_the_onboarding_rule() {
    assert_eq!(ScopeNote::of(IndexScope::Full, false), None);
    assert_eq!(
        ScopeNote::of(IndexScope::Limited, false),
        Some(ScopeNote::LimitedMode)
    );
    assert_eq!(
        ScopeNote::of(IndexScope::Full, true),
        Some(ScopeNote::LimitedMode)
    );
    assert_eq!(
        ScopeNote::of(IndexScope::Upgrading, false),
        Some(ScopeNote::IndexingProtected)
    );
    assert_eq!(ScopeNote::LimitedMode.text(), LIMITED_MODE_SUFFIX);
    assert_eq!(
        ScopeNote::IndexingProtected.text(),
        INDEXING_PROTECTED_SUFFIX
    );
}

#[test]
fn the_default_input_is_an_empty_ready_window() {
    let input = StatusInput::default();
    assert!(!input.has_query && !input.indexing && !input.banner_shown);
    assert_eq!(input.focus, Focus::SearchField);
    let v = status_view(&input);
    assert_eq!(v.counts.text(), "0 objects");
    assert_eq!(v.middle, Middle::Empty);
    assert_eq!(v.index, IndexSlot::UpToDate("Up to date".into()));
}
