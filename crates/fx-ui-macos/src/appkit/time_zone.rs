//! The local time zone for the results table's dates: the offset
//! from UTC in effect at each date, daylight saving time included, from
//! Foundation's `NSTimeZone`.

use objc2::rc::Retained;
use objc2_foundation::{NSDate, NSTimeZone};

use crate::formats::LocalOffset;

/// The app's local time zone (`+[NSTimeZone localTimeZone]`, which follows
/// the default time zone, the system's unless the app sets one).
pub struct SystemTimeZone {
    zone: Retained<NSTimeZone>,
}

impl SystemTimeZone {
    /// The local time zone.
    pub fn new() -> SystemTimeZone {
        SystemTimeZone {
            zone: NSTimeZone::localTimeZone(),
        }
    }
}

impl Default for SystemTimeZone {
    fn default() -> Self {
        SystemTimeZone::new()
    }
}

impl LocalOffset for SystemTimeZone {
    fn offset_at(&self, unix_secs: i64) -> i32 {
        let date = NSDate::dateWithTimeIntervalSince1970(unix_secs as f64);
        let offset = self.zone.secondsFromGMTForDate(&date);
        // Real offsets are within ±18 hours.
        i32::try_from(offset).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::format_date;

    #[test]
    fn offsets_are_within_a_day_and_modern_ones_whole_minutes() {
        let zone = SystemTimeZone::new();
        // Before standard time (about 1900) zones used local mean time,
        // whose offsets have seconds.
        assert!(zone.offset_at(-2_000_000_000).abs() <= 18 * 3_600);
        for t in [0, 1_773_479_520, 1_783_000_000] {
            let offset = zone.offset_at(t);
            assert!(offset.abs() <= 18 * 3_600, "{offset}");
            assert_eq!(offset % 60, 0, "{offset}");
        }
        assert!(!format_date(1_773_479_520, &zone).is_empty());
    }
}
