//! FSEvents resume cursor and the start decision (every OS).
//!
//! A cursor is only meaningful together with the FSEvents database it came
//! from, so it stores the volume's database UUID
//! (`FSEventsCopyUUIDForDevice`) next to the last processed event ID. On
//! start, [`decide_start`] compares the saved cursor with the volume's
//! current state and either resumes from the saved ID or reports a gap, in
//! which case the caller must rescan the volume and stream from "now".
//!
//! The cursor itself is the shared
//! [`fx_model::ResumeCursor::FsEvents`] and the outcome is
//! [`fx_model::StartOutcome`]. [`FsEventsCursor`] stays only as a thin
//! named-field view of that variant (`From` / `TryFrom`) plus its 24-byte
//! encoding.

use std::fmt;

use fx_model::{ResumeCursor, StartOutcome};

/// `kFSEventStreamEventIdSinceNow` from `FSEvents.h`: start with events
/// that happen after the stream is created.
pub const EVENT_ID_SINCE_NOW: u64 = 0xFFFF_FFFF_FFFF_FFFF;

/// Length of [`FsEventsCursor::to_bytes`].
pub const CURSOR_LEN: usize = 24;

/// Named-field view of [`ResumeCursor::FsEvents`] for one volume's stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FsEventsCursor {
    /// Last event ID that was fully applied to the index.
    pub event_id: u64,
    /// UUID of the volume's FSEvents database the ID belongs to.
    pub db_uuid: [u8; 16],
}

/// Error from [`FsEventsCursor::from_bytes`]: the input was not
/// [`CURSOR_LEN`] bytes long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorLenError {
    /// Length of the rejected input.
    pub len: usize,
}

impl fmt::Display for CursorLenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "FSEvents cursor must be {CURSOR_LEN} bytes, got {}",
            self.len
        )
    }
}

impl std::error::Error for CursorLenError {}

impl FsEventsCursor {
    /// Encodes the cursor: bytes 0..8 are `event_id` little-endian, bytes
    /// 8..24 are `db_uuid` verbatim.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; CURSOR_LEN] {
        let mut out = [0u8; CURSOR_LEN];
        out[..8].copy_from_slice(&self.event_id.to_le_bytes());
        out[8..].copy_from_slice(&self.db_uuid);
        out
    }

    /// Decodes [`FsEventsCursor::to_bytes`] output. Any length other than
    /// [`CURSOR_LEN`] is rejected.
    pub fn from_bytes(bytes: &[u8]) -> Result<FsEventsCursor, CursorLenError> {
        let bytes: &[u8; CURSOR_LEN] = bytes
            .try_into()
            .map_err(|_| CursorLenError { len: bytes.len() })?;
        let mut id = [0u8; 8];
        id.copy_from_slice(&bytes[..8]);
        let mut db_uuid = [0u8; 16];
        db_uuid.copy_from_slice(&bytes[8..]);
        Ok(FsEventsCursor {
            event_id: u64::from_le_bytes(id),
            db_uuid,
        })
    }
}

impl From<FsEventsCursor> for ResumeCursor {
    fn from(c: FsEventsCursor) -> ResumeCursor {
        ResumeCursor::FsEvents {
            event_id: c.event_id,
            db_uuid: c.db_uuid,
        }
    }
}

/// Error from `FsEventsCursor::try_from(ResumeCursor)`: the cursor belongs
/// to another kind of change source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotFsEvents(pub ResumeCursor);

impl fmt::Display for NotFsEvents {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "not an FSEvents resume cursor: {:?}", self.0)
    }
}

impl std::error::Error for NotFsEvents {}

impl TryFrom<ResumeCursor> for FsEventsCursor {
    type Error = NotFsEvents;

    fn try_from(c: ResumeCursor) -> Result<FsEventsCursor, NotFsEvents> {
        match c {
            ResumeCursor::FsEvents { event_id, db_uuid } => {
                Ok(FsEventsCursor { event_id, db_uuid })
            }
            other => Err(NotFsEvents(other)),
        }
    }
}

/// Decides where a volume's stream starts.
///
/// Returns the outcome and the `sinceWhen` event ID for the stream:
///
/// * no saved cursor: [`StartOutcome::NoCursorSupport`], since now;
/// * a saved cursor of another kind (`Usn`, `None`): gap, since now;
/// * no current UUID: gap, since now;
/// * UUID differs: gap, since now;
/// * saved ID greater than `current_event_id`: gap, since now;
/// * otherwise [`StartOutcome::Resumed`] from the saved ID.
#[must_use]
pub fn decide_start(
    saved: Option<&ResumeCursor>,
    current_uuid: Option<[u8; 16]>,
    current_event_id: u64,
) -> (StartOutcome, u64) {
    let Some(saved) = saved else {
        return (StartOutcome::NoCursorSupport, EVENT_ID_SINCE_NOW);
    };
    let gap = (StartOutcome::GapDetected, EVENT_ID_SINCE_NOW);
    let Ok(saved) = FsEventsCursor::try_from(*saved) else {
        return gap;
    };
    match current_uuid {
        Some(uuid) if uuid == saved.db_uuid && saved.event_id <= current_event_id => {
            (StartOutcome::Resumed, saved.event_id)
        }
        _ => gap,
    }
}

/// The pure bookkeeping behind a stream's resume cursor.
///
/// Replay from a saved cursor arrives roughly in path order, not ID order,
/// so the highest ID seen during replay says nothing about which lower IDs
/// were already delivered. Until the HistoryDone marker the cursor ID
/// therefore stays at the replay start (the saved cursor's event ID): a
/// cursor taken mid-replay resumes from the same point and skips nothing.
/// The marker moves the cursor to the highest ID seen or the marker's ID,
/// whichever is larger (never below the replay start). After that, live
/// events advance it monotonically. The `kFSEventStreamEventIdSinceNow`
/// sentinel ([`EVENT_ID_SINCE_NOW`]) is never recorded.
///
/// A replaying tracker also decides which events are delivered: FSEvents
/// may replay events at or before the saved event ID, which were already
/// applied. [`CursorTracker::on_event`] rejects those and counts them in
/// [`CursorTracker::dropped_before_cursor`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorTracker {
    /// The event ID a resume cursor may hold.
    cursor_id: u64,
    /// Highest ID seen during replay (before HistoryDone).
    highest_seen: u64,
    history_done: bool,
    /// The saved cursor's event ID for a replaying stream; events at or
    /// below it are not delivered.
    replay_start: Option<u64>,
    /// Events rejected because they were at or before `replay_start`.
    dropped_before_cursor: u64,
}

impl CursorTracker {
    /// A stream that starts from now: history is done at once and the
    /// cursor ID is `now` (the volume's current event ID).
    #[must_use]
    pub fn since_now(now: u64) -> CursorTracker {
        CursorTracker {
            cursor_id: now,
            highest_seen: now,
            history_done: true,
            replay_start: None,
            dropped_before_cursor: 0,
        }
    }

    /// A stream that replays from `since` (the saved cursor's event ID).
    #[must_use]
    pub fn replay(since: u64) -> CursorTracker {
        CursorTracker {
            cursor_id: since,
            highest_seen: since,
            history_done: false,
            replay_start: Some(since),
            dropped_before_cursor: 0,
        }
    }

    /// Records one event that is not the HistoryDone marker.
    ///
    /// Returns whether the event is to be delivered: `false` (and the event
    /// is counted in [`CursorTracker::dropped_before_cursor`]) for a
    /// replaying stream's event at or before the replay start.
    pub fn on_event(&mut self, id: u64) -> bool {
        if matches!(self.replay_start, Some(start) if id <= start) {
            self.dropped_before_cursor += 1;
            return false;
        }
        if id == EVENT_ID_SINCE_NOW {
            return true;
        }
        if self.history_done {
            self.cursor_id = self.cursor_id.max(id);
        } else {
            self.highest_seen = self.highest_seen.max(id);
        }
        true
    }

    /// How many replayed events at or before the replay start were
    /// rejected by [`CursorTracker::on_event`] instead of delivered. Always
    /// 0 for a stream that starts from now.
    #[must_use]
    pub fn dropped_before_cursor(&self) -> u64 {
        self.dropped_before_cursor
    }

    /// Records the HistoryDone marker with its event ID.
    pub fn on_history_done(&mut self, marker_id: u64) {
        if self.history_done {
            if marker_id != EVENT_ID_SINCE_NOW {
                self.cursor_id = self.cursor_id.max(marker_id);
            }
            return;
        }
        let mut id = self.cursor_id.max(self.highest_seen);
        if marker_id != EVENT_ID_SINCE_NOW {
            id = id.max(marker_id);
        }
        self.cursor_id = id;
        self.history_done = true;
    }

    /// The event ID a resume cursor may hold right now.
    #[must_use]
    pub fn cursor_id(&self) -> u64 {
        self.cursor_id
    }

    /// True once the HistoryDone marker was recorded (at once for
    /// [`CursorTracker::since_now`]).
    #[must_use]
    pub fn history_done(&self) -> bool {
        self.history_done
    }
}
