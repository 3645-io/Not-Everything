//! When the resident app asks the engine to save the index: once a day, at
//! a moment nobody is using the keyboard or mouse.
//!
//! A save rewrites the whole index (about 251 MB for a whole disk) and
//! flushes it to the SSD, so the app saves it rarely, as Everything does by
//! default: once a day while it runs, when it quits, and after a full
//! rescan. A crash loses nothing: the next launch replays the changes made
//! since the saved cursor. The AppKit layer checks [`daily_save_due`] every
//! [`DAILY_SAVE_CHECK`] and asks the engine for a save when it says so.

#![forbid(unsafe_code)]

use std::time::Duration;

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
}
