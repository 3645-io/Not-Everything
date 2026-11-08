//! How the interface writes values (the owner's choices and the canvas's
//! examples): sizes in 1,024-based units, dates in local time,
//! folders under the home folder from "~", counts with thousands
//! separators and search times in milliseconds.
//!
//! - **Sizes** ([`write_size`]): under 1 KB in bytes ("0 bytes", "1 byte",
//!   "512 bytes"), then whole kilobytes ("182 KB", "1,023 KB"), then one
//!   decimal from megabytes up ("1.2 MB", "2.5 GB", "16.0 EB"). A value
//!   moves up a unit when it would round to 1,024 of the smaller one.
//!   Halves round away from zero. The Size cell of a folder is "—"
//!   ([`write_size_cell`]).
//! - **Dates** ([`write_date`]): "14 Mar 2026 09:12": the day without a
//!   leading zero, the month's English short name, the year, and a 24-hour
//!   time, in the local time zone at that date ([`LocalOffset`]; the app
//!   asks the system time zone, tests pass fixed offsets).
//! - **Folders** ([`write_folder`]): the Path column shows the home folder
//!   as "~" ("~/Documents/Finance"); copied and dragged paths stay full.
//! - **Counts** ([`group_thousands`]): "2,014,377".
//! - **Search times** ([`format_millis`]): "9 ms", "<1 ms".
//!
//! Every writer appends to a caller-owned buffer, so the table can reuse one
//! `String` per cell as before.

#![forbid(unsafe_code)]

use std::borrow::Cow;
use std::fmt::Write as _;
use std::time::Duration;

/// The text of a cell with no value: an em dash, as the Size of a folder.
pub const NO_VALUE: &str = "\u{2014}";

/// Units above kilobytes, for one-decimal sizes.
const BIG_UNITS: [&str; 5] = ["MB", "GB", "TB", "PB", "EB"];

/// Appends `bytes` as the interface writes a size. See the module
/// documentation.
pub fn write_size(bytes: u64, out: &mut String) {
    if bytes < 1_024 {
        write_grouped(bytes, out);
        out.push_str(if bytes == 1 { " byte" } else { " bytes" });
        return;
    }
    let kb = bytes as f64 / 1_024.0;
    if kb.round() < 1_024.0 {
        write_grouped(kb.round() as u64, out);
        out.push_str(" KB");
        return;
    }
    let mut value = kb / 1_024.0;
    let mut unit = 0;
    // Move up a unit when the value would print as "1,024.0".
    while value >= 1_023.95 && unit + 1 < BIG_UNITS.len() {
        value /= 1_024.0;
        unit += 1;
    }
    let tenths = (value * 10.0).round() as u64;
    write_grouped(tenths / 10, out);
    let _ = write!(out, ".{} {}", tenths % 10, BIG_UNITS[unit]);
}

/// [`write_size`] into a new `String`.
pub fn format_size(bytes: u64) -> String {
    let mut s = String::new();
    write_size(bytes, &mut s);
    s
}

/// Appends the Size cell of an entry: [`NO_VALUE`] for a folder, nothing
/// for a file of unknown size, else [`write_size`].
pub fn write_size_cell(size: Option<u64>, is_folder: bool, out: &mut String) {
    if is_folder {
        out.push_str(NO_VALUE);
    } else if let Some(bytes) = size {
        write_size(bytes, out);
    }
}

/// The local time zone's offset from UTC.
pub trait LocalOffset {
    /// Seconds east of UTC in effect at `unix_secs` (daylight saving time
    /// included).
    fn offset_at(&self, unix_secs: i64) -> i32;
}

/// A zone whose offset never changes, in seconds east of UTC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FixedOffset(pub i32);

/// Coordinated Universal Time.
pub const UTC: FixedOffset = FixedOffset(0);

impl LocalOffset for FixedOffset {
    fn offset_at(&self, _unix_secs: i64) -> i32 {
        self.0
    }
}

impl<F: Fn(i64) -> i32> LocalOffset for F {
    fn offset_at(&self, unix_secs: i64) -> i32 {
        self(unix_secs)
    }
}

/// English short month names.
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Appends `unix_secs` as "14 Mar 2026 09:12" in `zone`'s local time.
/// Seconds are dropped (floored before 1970). Uses the proleptic Gregorian
/// calendar; never panics, even at the ends of `i64`.
pub fn write_date(unix_secs: i64, zone: &impl LocalOffset, out: &mut String) {
    let local = unix_secs.saturating_add(i64::from(zone.offset_at(unix_secs)));
    let days = local.div_euclid(86_400);
    let secs_of_day = local.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3_600;
    let minute = secs_of_day % 3_600 / 60;
    let _ = write!(
        out,
        "{day} {} {year} {hour:02}:{minute:02}",
        MONTHS[month as usize - 1]
    );
}

/// [`write_date`] into a new `String`.
pub fn format_date(unix_secs: i64, zone: &impl LocalOffset) -> String {
    let mut s = String::with_capacity(17);
    write_date(unix_secs, zone, &mut s);
    s
}

/// Converts days since 1970-01-01 to a (year, month, day) civil date.
///
/// Shifts the year to start on 1 March so the leap day falls at its end,
/// then splits days into 400-year eras of 146,097 days.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // Days from 0000-03-01 to 1970-01-01.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11], March = 0
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// `folder` as the Path column shows it: "~" for `home` itself and "~/…"
/// for folders inside it; anything else, or no home (or a home of "/" or
/// ""), unchanged. A trailing `/` on `home` is ignored.
pub fn display_folder<'a>(folder: &'a str, home: Option<&str>) -> Cow<'a, str> {
    let home = home.map(|h| h.trim_end_matches('/')).unwrap_or("");
    if home.is_empty() {
        return Cow::Borrowed(folder);
    }
    match folder.strip_prefix(home) {
        Some("") => Cow::Borrowed("~"),
        Some(rest) if rest.starts_with('/') => Cow::Owned(format!("~{rest}")),
        _ => Cow::Borrowed(folder),
    }
}

/// Appends [`display_folder`]'s text for `folder` to `out`.
pub fn write_folder(folder: &str, home: Option<&str>, out: &mut String) {
    out.push_str(&display_folder(folder, home));
}

/// `n` with commas between groups of three digits: "2,014,377".
pub fn group_thousands(n: usize) -> String {
    let mut s = String::new();
    write_grouped(n as u64, &mut s);
    s
}

/// Appends `n` with commas between groups of three digits.
pub fn write_grouped(n: u64, out: &mut String) {
    let digits = n.to_string();
    for (k, c) in digits.chars().enumerate() {
        if k > 0 && (digits.len() - k) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
}

/// A search time in whole milliseconds, rounded: "9 ms", "1,234 ms", or
/// "<1 ms" when it rounds to zero.
pub fn format_millis(time: Duration) -> String {
    let ms = (time.as_secs_f64() * 1_000.0).round() as u64;
    if ms == 0 {
        return "<1 ms".to_owned();
    }
    let mut s = String::new();
    write_grouped(ms, &mut s);
    s.push_str(" ms");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates_round_known_days() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(47_541), (2100, 3, 1));
    }
}
