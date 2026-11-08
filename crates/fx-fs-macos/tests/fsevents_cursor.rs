//! Cross-platform tests for `fx_fs_macos::fsevents::cursor`.
//!
//! Contract pinned here:
//! - `FsEventsCursor { pub event_id: u64, pub db_uuid: [u8; 16] }` derives
//!   `Debug`, `Clone`, `PartialEq` and `Eq`.
//! - `FsEventsCursor::to_bytes(&self) -> [u8; 24]`: bytes 0..8 hold
//!   `event_id` little-endian, bytes 8..24 hold `db_uuid` verbatim.
//! - `FsEventsCursor::from_bytes(&[u8]) -> Result<FsEventsCursor, E>` (with
//!   `E: Debug`) accepts exactly 24 bytes and rejects any other length.
//! - `FsEventsCursor` is only a thin view of `ResumeCursor::FsEvents` plus
//!   that encoding; `ResumeCursor::FsEvents` converts back with `TryFrom`.
//! - `decide_start(saved: Option<&fx_model::ResumeCursor>, current_uuid:
//!   Option<[u8; 16]>, current_event_id: u64) -> (fx_model::StartOutcome,
//!   u64)`. The `u64` is the stream's start ID; "since now" is
//!   `kFSEventStreamEventIdSinceNow` (`0xFFFF_FFFF_FFFF_FFFF`). A saved
//!   cursor of another kind (`Usn`, `None`) is a gap. The local
//!   `decide_start` below converts the thin `FsEventsCursor` through
//!   `From<FsEventsCursor> for ResumeCursor` and calls it.
//! - `CursorTracker` is the pure bookkeeping behind `VolumeStream::cursor()`:
//!   - `CursorTracker::since_now(now: u64)` starts with history done and
//!     cursor ID `now`;
//!   - `CursorTracker::replay(since: u64)` starts with history not done and
//!     cursor ID `since` (the saved cursor's event ID);
//!   - `on_event(&mut self, id: u64)` records one delivered, non-marker
//!     event;
//!   - `on_history_done(&mut self, marker_id: u64)` records the HistoryDone
//!     marker;
//!   - `cursor_id(&self) -> u64` is the event ID a resume cursor may hold;
//!   - `history_done(&self) -> bool`.
//!
//!   Replay arrives in path order, not ID order, so before HistoryDone the
//!   cursor ID stays at the replay start; the marker moves it to the highest
//!   ID seen or the marker's ID, whichever is larger; afterwards live events
//!   advance it monotonically. The SinceNow sentinel is never recorded.

use fx_fs_macos::fsevents::cursor::{CursorTracker, FsEventsCursor};
use fx_model::{ResumeCursor, StartOutcome};

fn decide_start(
    saved: Option<&FsEventsCursor>,
    current_uuid: Option<[u8; 16]>,
    current_event_id: u64,
) -> (StartOutcome, u64) {
    let saved: Option<ResumeCursor> = saved.map(|c| ResumeCursor::from(*c));
    fx_fs_macos::fsevents::cursor::decide_start(saved.as_ref(), current_uuid, current_event_id)
}

#[test]
fn fs_events_cursor_converts_to_and_from_resume_cursor() {
    let c = FsEventsCursor {
        event_id: 42,
        db_uuid: UUID_A,
    };
    let r = ResumeCursor::from(c);
    assert_eq!(
        r,
        ResumeCursor::FsEvents {
            event_id: 42,
            db_uuid: UUID_A
        }
    );
    assert_eq!(FsEventsCursor::try_from(r).expect("FsEvents variant"), c);
    assert!(FsEventsCursor::try_from(ResumeCursor::None { snapshot_epoch: 1 }).is_err());
}

#[test]
fn non_fsevents_saved_cursor_is_a_gap() {
    for other in [
        ResumeCursor::Usn {
            journal_id: 1,
            next_usn: 2,
        },
        ResumeCursor::None { snapshot_epoch: 3 },
    ] {
        assert_eq!(
            fx_fs_macos::fsevents::cursor::decide_start(Some(&other), Some(UUID_A), 5_000),
            (StartOutcome::GapDetected, SINCE_NOW),
            "{other:?}"
        );
    }
}

/// `kFSEventStreamEventIdSinceNow` from FSEvents.h.
const SINCE_NOW: u64 = 0xFFFF_FFFF_FFFF_FFFF;

const UUID_A: [u8; 16] = [
    0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54, 0x32, 0x10,
];
const UUID_B: [u8; 16] = [
    0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xab, 0xac, 0xad, 0xae, 0xaf,
];

fn cursor(event_id: u64, db_uuid: [u8; 16]) -> FsEventsCursor {
    FsEventsCursor { event_id, db_uuid }
}

// ------------------------------------------------------------ byte format

#[test]
fn to_bytes_layout_is_le_event_id_then_uuid() {
    let c = cursor(0x0102_0304_0506_0708, UUID_A);
    let bytes: [u8; 24] = c.to_bytes();
    assert_eq!(
        &bytes[..8],
        &[0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]
    );
    assert_eq!(&bytes[8..], &UUID_A);
}

#[test]
fn bytes_round_trip() {
    for c in [
        cursor(0, [0; 16]),
        cursor(1, UUID_A),
        cursor(123_456_789_012, UUID_B),
        cursor(u64::MAX - 1, [0xff; 16]),
    ] {
        let bytes = c.to_bytes();
        let back = FsEventsCursor::from_bytes(&bytes).expect("24 bytes decode");
        assert_eq!(back, c);
    }
}

#[test]
fn from_bytes_decodes_known_layout() {
    let mut raw = vec![0x2a, 0, 0, 0, 0, 0, 0, 0];
    raw.extend_from_slice(&UUID_B);
    let c = FsEventsCursor::from_bytes(&raw).expect("24 bytes decode");
    assert_eq!(c.event_id, 42);
    assert_eq!(c.db_uuid, UUID_B);
}

#[test]
fn from_bytes_rejects_wrong_length() {
    let good = cursor(7, UUID_A).to_bytes();
    for len in [0usize, 1, 8, 16, 23] {
        assert!(
            FsEventsCursor::from_bytes(&good[..len]).is_err(),
            "length {len} must be rejected"
        );
    }
    let mut long = good.to_vec();
    long.push(0);
    assert!(
        FsEventsCursor::from_bytes(&long).is_err(),
        "25 bytes rejected"
    );
    long.extend_from_slice(&[0; 7]);
    assert!(
        FsEventsCursor::from_bytes(&long).is_err(),
        "32 bytes rejected"
    );
}

// ------------------------------------------------------------ decide_start

#[test]
fn no_saved_cursor_starts_since_now() {
    assert_eq!(
        decide_start(None, Some(UUID_A), 5_000),
        (StartOutcome::NoCursorSupport, SINCE_NOW)
    );
    assert_eq!(
        decide_start(None, None, 5_000),
        (StartOutcome::NoCursorSupport, SINCE_NOW)
    );
}

#[test]
fn matching_uuid_and_older_id_resumes_from_saved_id() {
    let saved = cursor(1_000, UUID_A);
    assert_eq!(
        decide_start(Some(&saved), Some(UUID_A), 5_000),
        (StartOutcome::Resumed, 1_000)
    );
}

#[test]
fn saved_id_equal_to_current_resumes() {
    let saved = cursor(5_000, UUID_A);
    assert_eq!(
        decide_start(Some(&saved), Some(UUID_A), 5_000),
        (StartOutcome::Resumed, 5_000)
    );
}

#[test]
fn uuid_mismatch_is_a_gap() {
    let saved = cursor(1_000, UUID_A);
    assert_eq!(
        decide_start(Some(&saved), Some(UUID_B), 5_000),
        (StartOutcome::GapDetected, SINCE_NOW)
    );
}

#[test]
fn missing_current_uuid_is_a_gap() {
    let saved = cursor(1_000, UUID_A);
    assert_eq!(
        decide_start(Some(&saved), None, 5_000),
        (StartOutcome::GapDetected, SINCE_NOW)
    );
}

#[test]
fn saved_id_ahead_of_current_is_a_gap() {
    let saved = cursor(5_001, UUID_A);
    assert_eq!(
        decide_start(Some(&saved), Some(UUID_A), 5_000),
        (StartOutcome::GapDetected, SINCE_NOW)
    );
    let saved = cursor(u64::MAX - 1, UUID_A);
    assert_eq!(
        decide_start(Some(&saved), Some(UUID_A), 0),
        (StartOutcome::GapDetected, SINCE_NOW)
    );
}

#[test]
fn uuid_mismatch_and_id_ahead_is_still_a_gap() {
    let saved = cursor(9_999, UUID_A);
    let (outcome, start) = decide_start(Some(&saved), Some(UUID_B), 5_000);
    assert!(
        matches!(outcome, StartOutcome::GapDetected),
        "got {outcome:?}"
    );
    assert_eq!(start, SINCE_NOW);
}

#[test]
fn decide_start_survives_a_byte_round_trip() {
    let saved = cursor(77_777, UUID_B);
    let restored = FsEventsCursor::from_bytes(&saved.to_bytes()).expect("decode");
    assert_eq!(
        decide_start(Some(&restored), Some(UUID_B), 80_000),
        (StartOutcome::Resumed, 77_777)
    );
}

// --------------------------------------------------------- cursor tracker

#[test]
fn since_now_tracker_starts_done_at_now() {
    let t = CursorTracker::since_now(5_000);
    assert!(t.history_done());
    assert_eq!(t.cursor_id(), 5_000);
}

#[test]
fn since_now_tracker_advances_with_live_events() {
    let mut t = CursorTracker::since_now(5_000);
    t.on_event(5_001);
    assert_eq!(t.cursor_id(), 5_001);
    t.on_event(5_010);
    assert_eq!(t.cursor_id(), 5_010);
    // Never moves backwards.
    t.on_event(5_004);
    assert_eq!(t.cursor_id(), 5_010);
    assert!(t.history_done());
}

#[test]
fn replay_tracker_starts_at_the_saved_id_without_history() {
    let t = CursorTracker::replay(1_000);
    assert!(!t.history_done());
    assert_eq!(t.cursor_id(), 1_000);
}

#[test]
fn replay_events_do_not_move_the_cursor_before_history_done() {
    let mut t = CursorTracker::replay(1_000);
    // Replay arrives in path order: a high ID can come before lower ones.
    for id in [1_900, 1_200, 1_750, 1_001, 1_500] {
        t.on_event(id);
        assert_eq!(
            t.cursor_id(),
            1_000,
            "cursor moved to {} during replay after id {id}",
            t.cursor_id()
        );
        assert!(!t.history_done());
    }
}

#[test]
fn history_done_moves_cursor_to_highest_seen_when_above_marker() {
    let mut t = CursorTracker::replay(1_000);
    for id in [1_900, 1_200, 1_750] {
        t.on_event(id);
    }
    t.on_history_done(1_800);
    assert!(t.history_done());
    assert_eq!(t.cursor_id(), 1_900);
}

#[test]
fn history_done_moves_cursor_to_marker_when_above_highest_seen() {
    let mut t = CursorTracker::replay(1_000);
    for id in [1_300, 1_100] {
        t.on_event(id);
    }
    t.on_history_done(2_500);
    assert!(t.history_done());
    assert_eq!(t.cursor_id(), 2_500);
}

#[test]
fn history_done_without_replayed_events_uses_marker() {
    let mut t = CursorTracker::replay(1_000);
    t.on_history_done(1_400);
    assert!(t.history_done());
    assert_eq!(t.cursor_id(), 1_400);
}

#[test]
fn history_done_never_moves_cursor_below_replay_start() {
    let mut t = CursorTracker::replay(1_000);
    t.on_history_done(900);
    assert!(t.history_done());
    assert!(t.cursor_id() >= 1_000, "cursor {} < 1000", t.cursor_id());
}

#[test]
fn live_events_after_history_done_advance_the_cursor() {
    let mut t = CursorTracker::replay(1_000);
    t.on_event(1_600);
    t.on_history_done(1_500);
    assert_eq!(t.cursor_id(), 1_600);
    t.on_event(1_700);
    assert_eq!(t.cursor_id(), 1_700);
    t.on_event(1_650);
    assert_eq!(t.cursor_id(), 1_700, "live cursor never moves backwards");
}

#[test]
fn since_now_sentinel_is_never_recorded() {
    let mut t = CursorTracker::replay(1_000);
    t.on_event(SINCE_NOW);
    t.on_history_done(SINCE_NOW);
    assert_ne!(t.cursor_id(), SINCE_NOW);
    assert!(t.cursor_id() >= 1_000);

    let mut live = CursorTracker::since_now(42);
    live.on_event(SINCE_NOW);
    assert_eq!(live.cursor_id(), 42);
}

#[test]
fn tracker_cursor_id_resumes_through_decide_start() {
    // A cursor taken mid-replay resumes from the same point: nothing that
    // was not yet replayed is skipped.
    let mut t = CursorTracker::replay(77_777);
    t.on_event(79_000);
    let saved = cursor(t.cursor_id(), UUID_A);
    assert_eq!(
        decide_start(Some(&saved), Some(UUID_A), 80_000),
        (StartOutcome::Resumed, 77_777)
    );
}
