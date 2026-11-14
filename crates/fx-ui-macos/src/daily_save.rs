//! When the resident app asks the engine to save the index: once a day, at
//! a moment nobody is using the keyboard or mouse.
//!
//! A save rewrites the whole index (about 251 MB for a whole disk) and
//! flushes it to the SSD, so the app saves it rarely, as Everything does by
//! default: once a day while it runs, when it quits, and after a full
//! rescan. A crash loses nothing: the next launch replays the changes made
//! since the saved cursor. The AppKit layer checks [`daily_save_due`] every
//! [`DAILY_SAVE_CHECK`] and asks the engine for a save when it says so.
//!
//! The day counts from the store's last write, whatever wrote it: a save
//! after a compaction, a full rescan or a quit restarts it too. The engine
//! adds a backstop of its own, a save at the latest 48 hours of awake time
//! after the first unsaved change (`fx_engine::DEFAULT_CHECKPOINT_MAX_DELAY`),
//! for a Mac that is never idle for a minute at a check.
//!
//! The decision takes the clock as an argument ([`daily_save_due_at`]), so
//! `tests/daily_save_day.rs` can run it through simulated days.

#![forbid(unsafe_code)]

use std::time::{Duration, SystemTime};

/// How long after the last save the next daily save is due.
pub const DAILY_SAVE_INTERVAL: Duration = Duration::from_secs(24 * 3600);

/// How long nobody must have used the keyboard or mouse before the daily
/// save starts, so it does not compete with someone at the Mac.
pub const DAILY_SAVE_INPUT_IDLE: Duration = Duration::from_secs(60);

/// How often the app checks whether the daily save is due.
pub const DAILY_SAVE_CHECK: Duration = Duration::from_secs(5 * 60);

/// Whether the daily save is due: the index was saved `saved_ago` ago
/// (`None` without a saved index, whose first save the engine makes by
/// itself), and nobody has used the keyboard or mouse for `input_idle`.
pub fn daily_save_due(saved_ago: Option<Duration>, input_idle: Duration) -> bool {
    saved_ago.is_some_and(|age| age >= DAILY_SAVE_INTERVAL) && input_idle >= DAILY_SAVE_INPUT_IDLE
}

/// How long before `now` an index saved at `saved_at` was saved (the store
/// file's modification time, `fx_engine::Engine::saved_at`). `None` without
/// a saved index, and for a save dated after `now`: the clock was set back
/// since, and the daily save waits until it has caught up.
pub fn saved_ago(now: SystemTime, saved_at: Option<SystemTime>) -> Option<Duration> {
    now.duration_since(saved_at?).ok()
}

/// [`daily_save_due`] at `now` for an index saved at `saved_at`: what the
/// timer decides, with the clock passed in.
pub fn daily_save_due_at(
    now: SystemTime,
    saved_at: Option<SystemTime>,
    input_idle: Duration,
) -> bool {
    daily_save_due(saved_ago(now, saved_at), input_idle)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: Duration = Duration::from_secs(60);
    const HOUR: Duration = Duration::from_secs(3600);

    #[test]
    fn not_due_before_a_day_has_passed() {
        assert!(!daily_save_due(Some(23 * HOUR), 10 * MINUTE));
        assert!(!daily_save_due(Some(Duration::ZERO), 10 * MINUTE));
    }

    #[test]
    fn due_after_a_day_only_after_a_minute_without_input() {
        assert!(!daily_save_due(Some(25 * HOUR), Duration::from_secs(5)));
        assert!(!daily_save_due(Some(25 * HOUR), Duration::from_secs(59)));
        assert!(daily_save_due(Some(25 * HOUR), MINUTE));
        assert!(daily_save_due(Some(24 * HOUR), 2 * MINUTE));
    }

    #[test]
    fn not_due_again_right_after_a_save() {
        // The save resets the age the check reads.
        assert!(daily_save_due(Some(30 * HOUR), 5 * MINUTE));
        assert!(!daily_save_due(Some(MINUTE), 5 * MINUTE));
    }

    #[test]
    fn not_due_without_a_store() {
        // The engine saves the first scan by itself.
        assert!(!daily_save_due(None, 5 * MINUTE));
    }

    #[test]
    fn the_age_is_read_from_the_clock_passed_in() {
        let saved = SystemTime::UNIX_EPOCH + 1_000 * HOUR;
        assert_eq!(saved_ago(saved + 3 * HOUR, Some(saved)), Some(3 * HOUR));
        assert_eq!(saved_ago(saved, Some(saved)), Some(Duration::ZERO));
        assert_eq!(saved_ago(saved, None), None);
        assert!(daily_save_due_at(saved + 24 * HOUR, Some(saved), MINUTE));
        assert!(!daily_save_due_at(
            saved + 24 * HOUR - Duration::from_secs(1),
            Some(saved),
            MINUTE
        ));
        assert!(!daily_save_due_at(saved + 48 * HOUR, None, MINUTE));
    }

    #[test]
    fn a_save_dated_in_the_future_is_waited_out() {
        // The clock was set back after the save: no age, so not due until
        // the clock reaches the save's time and then a day more.
        let saved = SystemTime::UNIX_EPOCH + 1_000 * HOUR;
        assert_eq!(saved_ago(saved - HOUR, Some(saved)), None);
        assert!(!daily_save_due_at(saved - HOUR, Some(saved), 10 * MINUTE));
    }
}
