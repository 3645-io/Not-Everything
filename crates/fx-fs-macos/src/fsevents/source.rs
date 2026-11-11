//! [`FsEventsSource`]: one volume's FSEvents stream as an
//! [`fx_fs::ChangeSource`] (macOS only).
//!
//! [`ChangeSource::start`] starts a [`VolumeStream`] from an optional
//! [`ResumeCursor::FsEvents`]; the outcome comes from
//! [`decide_start`](super::cursor::decide_start): `Resumed` replays the
//! history after the saved ID (delivered through
//! [`ChangeSource::next_batch`] like any other batch, up to and past the
//! HistoryDone marker), `GapDetected` means the caller must rescan the
//! volume, and `NoCursorSupport` means there was no saved cursor.
//!
//! [`ChangeSource::next_batch`] maps every raw event with
//! [`super::change::map_event`] and tidies the batch with
//! [`super::change::coalesce`]; it never drops a whole batch.
//!
//! # Cursor
//!
//! [`ChangeSource::cursor`] is the position after the last batch returned,
//! not after the last event the stream received: events still waiting in
//! the channel do not move it, so a checkpoint taken between batches never
//! skips an unapplied event. The source keeps its own
//! [`CursorTracker`] over the events it has handed out, with the same
//! replay rule as the stream (the cursor stays at the replay start until
//! the HistoryDone marker has been handed out).

use std::io;
use std::sync::mpsc::RecvTimeoutError;
use std::time::Instant;

use fx_fs::ChangeSource;
use fx_model::{Change, ResumeCursor, StartOutcome};

use super::change::{coalesce, map_event};
use super::cursor::CursorTracker;
use super::stream::{RawEvent, StreamConfig, VolumeStream};

/// An FSEvents [`ChangeSource`] for one volume. See the module docs.
pub struct FsEventsSource {
    config: StreamConfig,
    running: Option<Running>,
}

/// State of a started source.
struct Running {
    stream: VolumeStream,
    /// Cursor bookkeeping over the events handed out by `next_batch`.
    tracker: CursorTracker,
    db_uuid: [u8; 16],
}

impl FsEventsSource {
    /// A source for the volume holding `config.watch_path`. Nothing runs
    /// until [`ChangeSource::start`].
    #[must_use]
    pub fn new(config: StreamConfig) -> FsEventsSource {
        FsEventsSource {
            config,
            running: None,
        }
    }

    /// The configuration the stream is (or will be) started with.
    #[must_use]
    pub fn config(&self) -> &StreamConfig {
        &self.config
    }

    /// Replayed events at or before the resume cursor that the stream
    /// dropped ([`VolumeStream::dropped_before_cursor`]); 0 before start.
    #[must_use]
    pub fn dropped_before_cursor(&self) -> u64 {
        self.running
            .as_ref()
            .map_or(0, |r| r.stream.dropped_before_cursor())
    }

    /// True once the replayed history, up to the HistoryDone marker, has
    /// been handed out by `next_batch` (at once for a fresh start). False
    /// before start.
    #[must_use]
    pub fn history_done(&self) -> bool {
        self.running
            .as_ref()
            .is_some_and(|r| r.tracker.history_done())
    }
}

impl Running {
    /// Maps one raw event and records it in the cursor tracker.
    fn take(&mut self, ev: &RawEvent, out: &mut Vec<Change>) {
        // The stream already dropped what is not to be delivered.
        let _ = self.tracker.on_raw_event(ev.id, ev.flags);
        if let Some(change) = map_event(
            &ev.path,
            ev.flags,
            ev.rescan_subtree,
            self.stream.scope_root(),
        ) {
            out.push(change);
        }
    }
}

impl ChangeSource for FsEventsSource {
    fn start(&mut self, from: Option<&ResumeCursor>) -> fx_fs::Result<StartOutcome> {
        // Stop a previous stream before starting the next one.
        self.running = None;
        let (stream, outcome) = VolumeStream::start(self.config.clone(), from)?;
        let ResumeCursor::FsEvents { event_id, db_uuid } = stream.cursor() else {
            return Err(io::Error::other(
                "FSEvents stream returned a foreign cursor",
            ));
        };
        // The stream already drops events at or before the replay start,
        // so the source's tracker only needs the replay / live distinction.
        let tracker = if outcome == StartOutcome::Resumed {
            CursorTracker::replay(event_id)
        } else {
            CursorTracker::since_now(event_id)
        };
        self.running = Some(Running {
            stream,
            tracker,
            db_uuid,
        });
        Ok(outcome)
    }

    fn next_batch(&mut self, deadline: Instant) -> fx_fs::Result<Vec<Change>> {
        let Some(running) = self.running.as_mut() else {
            return Err(io::Error::other("FsEventsSource::next_batch before start"));
        };
        let mut out = Vec::new();
        // Wait for the first change (a HistoryDone marker alone yields no
        // change, so keep waiting until the deadline).
        while out.is_empty() {
            let left = deadline.saturating_duration_since(Instant::now());
            match running.stream.recv_timeout(left) {
                Ok(ev) => running.take(&ev, &mut out),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::other("FSEvents stream stopped"));
                }
            }
            if Instant::now() >= deadline {
                break;
            }
        }
        // Then everything already waiting, without blocking.
        let pending: Vec<RawEvent> = running.stream.try_iter().collect();
        for ev in &pending {
            running.take(ev, &mut out);
        }
        Ok(coalesce(out))
    }

    fn cursor(&self) -> ResumeCursor {
        match &self.running {
            Some(r) => ResumeCursor::FsEvents {
                event_id: r.tracker.cursor_id(),
                db_uuid: r.db_uuid,
            },
            // Not started: a cursor that can never resume (no FSEvents
            // database has an all-zero UUID), so a restart rescans.
            None => ResumeCursor::FsEvents {
                event_id: 0,
                db_uuid: [0; 16],
            },
        }
    }
}
