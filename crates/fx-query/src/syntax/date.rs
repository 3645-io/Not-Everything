//! Date values for `dm:` / `datemodified:`, `dc:` / `datecreated:` and
//! `da:` / `dateaccessed:`: ISO 8601 dates, date keywords and the calendar
//! arithmetic behind them.
//!
//! Readings, in our own words after voidtools' public Search Functions page
//! and docs/PLAN.md:
//!
//! - Dates are read in local time, which here is the evaluation clock plus
//!   a fixed offset east of UTC ([`crate::eval::EvalContext`]). There is no
//!   time-zone database, so daylight-saving changes are not modelled.
//! - A date value is a period ([`DateValue::span`] gives it as a half-open
//!   range `[start, end)` of Unix seconds). `YYYY` is the whole year,
//!   `YYYY-MM` the whole month, `YYYY-MM-DD` the whole day,
//!   `YYYY-MM-DDTHH:MM` the whole minute and `YYYY-MM-DDTHH:MM:SS` one
//!   second. Every field has exactly its ISO width (four-digit year,
//!   two-digit others); the `T` may be lowercase. An hour alone
//!   (`YYYY-MM-DDTHH`) is not accepted.
//! - Keywords (case-insensitive) are whole calendar periods around the
//!   clock: `today`, `yesterday`, `thisweek`, `lastweek`, `thismonth`,
//!   `lastmonth`, `thisyear`, `lastyear`. Weeks run Monday 00:00 to the
//!   next Monday 00:00 (ISO 8601 weeks), and `thisweek` is the whole
//!   current week, including the days still to come, just as `thismonth`
//!   is the whole month.
//! - Out-of-range fields (`2026-13`, `2026-02-30`, `T24:00`, `:60`) make
//!   the date invalid; the parser then drops the word with a diagnostic.
//! - Calendar arithmetic is the proleptic Gregorian calendar, done by hand
//!   with days-from-civil and its inverse (counting years from March, so
//!   the leap day is the last day of the year). All arithmetic on the
//!   clock is checked: a period that cannot be represented in `i64`
//!   seconds resolves to `None` and matches nothing.

use std::fmt;

use super::functions::ArgError;

const DAY: i64 = 86_400;

/// Whether `year` is a leap year in the proleptic Gregorian calendar.
pub fn is_leap_year(year: i32) -> bool {
    is_leap(i64::from(year))
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Days in `month` (1..=12) of `year`; 0 for an invalid month.
pub fn days_in_month(year: i32, month: u8) -> u8 {
    month_len(i64::from(year), month)
}

fn month_len(year: i64, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date. Fields are not
/// range-checked. Exact for any year whose result fits in `i64` without
/// overflowing the intermediate products (far beyond ±10^12 years).
pub fn days_from_civil(year: i64, month: u8, day: u8) -> i64 {
    // Count years from March, so the leap day ends the year.
    let (y, m) = if month <= 2 {
        (year - 1, i64::from(month) + 9)
    } else {
        (year, i64::from(month) - 3)
    };
    let era = y.div_euclid(400);
    let yoe = y - era * 400; // 0..=399
    let doy = (153 * m + 2) / 5 + i64::from(day) - 1; // 0..=365
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // 0..=146_096
    era * 146_097 + doe - 719_468
}

/// The proleptic Gregorian date `(year, month, day)` of a day count since
/// 1970-01-01; the inverse of [`days_from_civil`].
pub fn civil_from_days(days: i64) -> (i64, u8, u8) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097; // 0..=146_096
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // 0..=399
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // 0..=365
    let mp = (5 * doy + 2) / 153; // 0..=11, from March
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    // `day` is 1..=31 and `month` 1..=12, so the casts are lossless.
    (year, month as u8, day as u8)
}

/// Seconds since 1970-01-01T00:00:00 for a proleptic Gregorian date-time,
/// with no time zone applied. Fields are not range-checked.
pub fn civil_to_unix(year: i32, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> i64 {
    days_from_civil(i64::from(year), month, day) * DAY
        + i64::from(hour) * 3_600
        + i64::from(minute) * 60
        + i64::from(second)
}

/// A local day range `[start_day, end_day)` as Unix seconds, for a local
/// time `offset` seconds ahead of UTC. `None` on overflow.
fn days_to_span(start_day: i64, end_day: i64, offset: i64) -> Option<(i64, i64)> {
    let start = start_day.checked_mul(DAY)?.checked_sub(offset)?;
    let end = end_day.checked_mul(DAY)?.checked_sub(offset)?;
    Some((start, end))
}

/// A calendar period in local time, as written in an ISO 8601 date: a
/// year, optionally narrowed to a month, day, hour, minute and second.
/// Each field is only meaningful when every coarser one is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CalendarDate {
    /// The year, such as 2026.
    pub year: i32,
    /// Month 1..=12.
    pub month: Option<u8>,
    /// Day of the month 1..=31.
    pub day: Option<u8>,
    /// Hour 0..=23.
    pub hour: Option<u8>,
    /// Minute 0..=59.
    pub minute: Option<u8>,
    /// Second 0..=59.
    pub second: Option<u8>,
}

impl CalendarDate {
    /// A whole year.
    pub fn year(year: i32) -> Self {
        CalendarDate {
            year,
            month: None,
            day: None,
            hour: None,
            minute: None,
            second: None,
        }
    }

    /// Whether every field is in range (month 1..=12, a day that exists in
    /// that month, hour 0..=23, minute and second 0..=59).
    pub fn is_valid(&self) -> bool {
        let month = self.month.unwrap_or(1);
        let day = self.day.unwrap_or(1);
        (1..=12).contains(&month)
            && day >= 1
            && day <= days_in_month(self.year, month)
            && self.hour.unwrap_or(0) <= 23
            && self.minute.unwrap_or(0) <= 59
            && self.second.unwrap_or(0) <= 59
    }

    /// The period as a half-open range `[start, end)` of Unix seconds, for
    /// a local time `utc_offset_secs` ahead of UTC. `None` if a field is
    /// out of range. A fixed offset is used for the whole period, so
    /// daylight-saving changes are not modelled.
    pub fn span(&self, utc_offset_secs: i32) -> Option<(i64, i64)> {
        if !self.is_valid() {
            return None;
        }
        let year = i64::from(self.year);
        let month = self.month.unwrap_or(1);
        let day = self.day.unwrap_or(1);
        let first_day = days_from_civil(year, month, day);
        let local = first_day * DAY
            + i64::from(self.hour.unwrap_or(0)) * 3_600
            + i64::from(self.minute.unwrap_or(0)) * 60
            + i64::from(self.second.unwrap_or(0));
        let len = if self.month.is_none() {
            (days_from_civil(year + 1, 1, 1) - first_day) * DAY
        } else if self.day.is_none() {
            let (ny, nm) = if month == 12 {
                (year + 1, 1)
            } else {
                (year, month + 1)
            };
            (days_from_civil(ny, nm, 1) - first_day) * DAY
        } else if self.hour.is_none() {
            DAY
        } else if self.minute.is_none() {
            3_600
        } else if self.second.is_none() {
            60
        } else {
            1
        };
        let start = local - i64::from(utc_offset_secs);
        Some((start, start + len))
    }
}

impl fmt::Display for CalendarDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}", self.year)?;
        let Some(month) = self.month else {
            return Ok(());
        };
        write!(f, "-{month:02}")?;
        let Some(day) = self.day else { return Ok(()) };
        write!(f, "-{day:02}")?;
        let Some(hour) = self.hour else {
            return Ok(());
        };
        write!(f, "T{hour:02}")?;
        let Some(minute) = self.minute else {
            return Ok(());
        };
        write!(f, ":{minute:02}")?;
        if let Some(second) = self.second {
            write!(f, ":{second:02}")?;
        }
        Ok(())
    }
}

/// A date keyword, resolved against the evaluation clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DateKeyword {
    /// The current local day.
    Today,
    /// The local day before today.
    Yesterday,
    /// The current week, Monday 00:00 up to the next Monday 00:00.
    ThisWeek,
    /// The week before this week.
    LastWeek,
    /// The current calendar month.
    ThisMonth,
    /// The calendar month before this month.
    LastMonth,
    /// The current calendar year.
    ThisYear,
    /// The calendar year before this year.
    LastYear,
}

impl DateKeyword {
    /// Every keyword, in a fixed order.
    pub const ALL: [DateKeyword; 8] = [
        DateKeyword::Today,
        DateKeyword::Yesterday,
        DateKeyword::ThisWeek,
        DateKeyword::LastWeek,
        DateKeyword::ThisMonth,
        DateKeyword::LastMonth,
        DateKeyword::ThisYear,
        DateKeyword::LastYear,
    ];

    /// The keyword as typed, in lowercase.
    pub fn name(self) -> &'static str {
        match self {
            DateKeyword::Today => "today",
            DateKeyword::Yesterday => "yesterday",
            DateKeyword::ThisWeek => "thisweek",
            DateKeyword::LastWeek => "lastweek",
            DateKeyword::ThisMonth => "thismonth",
            DateKeyword::LastMonth => "lastmonth",
            DateKeyword::ThisYear => "thisyear",
            DateKeyword::LastYear => "lastyear",
        }
    }

    /// The keyword for a word, case-insensitively.
    pub fn from_name(word: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|k| k.name().eq_ignore_ascii_case(word))
    }

    /// The period as a half-open range `[start, end)` of Unix seconds, for
    /// the clock `now` and a local time `utc_offset_secs` ahead of UTC.
    /// `None` if the period does not fit in `i64` seconds.
    pub fn span(self, now: i64, utc_offset_secs: i32) -> Option<(i64, i64)> {
        let offset = i64::from(utc_offset_secs);
        let today = now.checked_add(offset)?.div_euclid(DAY);
        let (start, end) = match self {
            DateKeyword::Today => (today, today + 1),
            DateKeyword::Yesterday => (today - 1, today),
            DateKeyword::ThisWeek | DateKeyword::LastWeek => {
                // Day 0 (1970-01-01) was a Thursday: index 3 from Monday.
                let monday = today - (today + 3).rem_euclid(7);
                if self == DateKeyword::ThisWeek {
                    (monday, monday + 7)
                } else {
                    (monday - 7, monday)
                }
            }
            DateKeyword::ThisMonth | DateKeyword::LastMonth => {
                let (y, m, _) = civil_from_days(today);
                let first = days_from_civil(y, m, 1);
                if self == DateKeyword::ThisMonth {
                    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
                    (first, days_from_civil(ny, nm, 1))
                } else {
                    let (py, pm) = if m == 1 { (y - 1, 12) } else { (y, m - 1) };
                    (days_from_civil(py, pm, 1), first)
                }
            }
            DateKeyword::ThisYear | DateKeyword::LastYear => {
                let (y, _, _) = civil_from_days(today);
                if self == DateKeyword::ThisYear {
                    (days_from_civil(y, 1, 1), days_from_civil(y + 1, 1, 1))
                } else {
                    (days_from_civil(y - 1, 1, 1), days_from_civil(y, 1, 1))
                }
            }
        };
        days_to_span(start, end, offset)
    }
}

/// The value of a date function: a period whose bounds may depend on the
/// clock and the local time zone, so it is resolved at evaluation time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DateValue {
    /// An ISO 8601 date or date-time, read as local time.
    Calendar(CalendarDate),
    /// A keyword such as `today`.
    Keyword(DateKeyword),
}

impl DateValue {
    /// The period as a half-open range `[start, end)` of Unix seconds.
    pub fn span(&self, now: i64, utc_offset_secs: i32) -> Option<(i64, i64)> {
        match self {
            DateValue::Calendar(date) => date.span(utc_offset_secs),
            DateValue::Keyword(kw) => kw.span(now, utc_offset_secs),
        }
    }
}

impl fmt::Display for DateValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DateValue::Calendar(date) => date.fmt(f),
            DateValue::Keyword(kw) => f.write_str(kw.name()),
        }
    }
}

/// Parses one date value: a keyword (any case) or an ISO 8601 date or
/// date-time (see the module docs). Anything else, including a date with
/// an out-of-range field, is [`ArgError::Invalid`]; an empty value is
/// [`ArgError::Missing`].
pub fn parse_date_value(text: &str) -> Result<DateValue, ArgError> {
    if text.is_empty() {
        return Err(ArgError::Missing);
    }
    if let Some(kw) = DateKeyword::from_name(text) {
        return Ok(DateValue::Keyword(kw));
    }
    parse_iso_date(text)
        .map(DateValue::Calendar)
        .ok_or(ArgError::Invalid)
}

/// Parses `YYYY`, `YYYY-MM`, `YYYY-MM-DD`, `YYYY-MM-DDTHH:MM` or
/// `YYYY-MM-DDTHH:MM:SS`, checking every field's range.
fn parse_iso_date(text: &str) -> Option<CalendarDate> {
    let bytes = text.as_bytes();
    // Reads `width` ASCII digits at `at`.
    let digits = |at: usize, width: usize| -> Option<u32> {
        let field = bytes.get(at..at + width)?;
        field.iter().try_fold(0u32, |acc, &b| {
            b.is_ascii_digit().then(|| acc * 10 + u32::from(b - b'0'))
        })
    };
    let two = |at: usize| digits(at, 2).and_then(|v| u8::try_from(v).ok());
    let sep = |at: usize, c: u8| bytes.get(at) == Some(&c);

    let year = i32::try_from(digits(0, 4)?).ok()?;
    let mut date = CalendarDate::year(year);
    let len = bytes.len();
    if len > 4 {
        if !sep(4, b'-') {
            return None;
        }
        date.month = Some(two(5)?);
    }
    if len > 7 {
        if !sep(7, b'-') {
            return None;
        }
        date.day = Some(two(8)?);
    }
    if len > 10 {
        if !(sep(10, b'T') || sep(10, b't')) {
            return None;
        }
        // The hour must come with its minutes.
        date.hour = Some(two(11)?);
        if !sep(13, b':') {
            return None;
        }
        date.minute = Some(two(14)?);
    }
    if len > 16 {
        if !sep(16, b':') {
            return None;
        }
        date.second = Some(two(17)?);
    }
    let expected = match (date.month, date.day, date.hour, date.second) {
        (None, ..) => 4,
        (Some(_), None, ..) => 7,
        (Some(_), Some(_), None, _) => 10,
        (Some(_), Some(_), Some(_), None) => 16,
        (Some(_), Some(_), Some(_), Some(_)) => 19,
    };
    (len == expected && date.is_valid()).then_some(date)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(civil_to_unix(1970, 1, 1, 0, 0, 0), 0);
        assert_eq!(civil_to_unix(2026, 11, 3, 10, 0, 0), 1_793_700_000);
        assert_eq!(civil_to_unix(2000, 3, 1, 0, 0, 0), 951_868_800);
        assert_eq!(civil_to_unix(1969, 12, 31, 23, 59, 59), -1);
    }

    #[test]
    fn civil_round_trip() {
        for days in (-800_000..800_000).step_by(37) {
            let (y, m, d) = civil_from_days(days);
            assert!((1..=12).contains(&m) && d >= 1 && d <= month_len(y, m));
            assert_eq!(days_from_civil(y, m, d), days, "{y}-{m}-{d}");
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
    }

    #[test]
    fn leap_years() {
        assert!(is_leap_year(2024) && is_leap_year(2000) && is_leap_year(2400));
        assert!(!is_leap_year(1900) && !is_leap_year(2100) && !is_leap_year(2026));
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2100, 2), 28);
        assert_eq!(days_in_month(2026, 13), 0);
    }

    #[test]
    fn calendar_spans() {
        let y = CalendarDate::year(2024);
        let (s, e) = y.span(0).unwrap();
        assert_eq!(e - s, 366 * 86_400);
        let feb = CalendarDate {
            month: Some(2),
            ..y
        };
        let (s, e) = feb.span(3600).unwrap();
        assert_eq!(e - s, 29 * 86_400);
        assert_eq!(s, civil_to_unix(2024, 2, 1, 0, 0, 0) - 3600);
        let bad = CalendarDate {
            day: Some(30),
            ..feb
        };
        assert_eq!(bad.span(0), None);
        assert_eq!(feb.to_string(), "2024-02");
        assert!(CalendarDate::year(i32::MAX).span(0).is_some());
        assert!(CalendarDate::year(i32::MIN).span(0).is_some());
    }

    #[test]
    fn keywords() {
        let now = 1_793_700_000; // Tuesday 2026-11-03T10:00Z
        let midnight = 1_793_664_000;
        let span = |k: DateKeyword| k.span(now, 0).unwrap();
        assert_eq!(span(DateKeyword::Today), (midnight, midnight + DAY));
        assert_eq!(span(DateKeyword::Yesterday), (midnight - DAY, midnight));
        assert_eq!(
            span(DateKeyword::ThisWeek),
            (midnight - DAY, midnight + 6 * DAY)
        );
        assert_eq!(
            span(DateKeyword::LastWeek),
            (midnight - 8 * DAY, midnight - DAY)
        );
        let nov = civil_to_unix(2026, 11, 1, 0, 0, 0);
        let oct = civil_to_unix(2026, 10, 1, 0, 0, 0);
        let dec = civil_to_unix(2026, 12, 1, 0, 0, 0);
        assert_eq!(span(DateKeyword::ThisMonth), (nov, dec));
        assert_eq!(span(DateKeyword::LastMonth), (oct, nov));
        let y25 = civil_to_unix(2025, 1, 1, 0, 0, 0);
        let y26 = civil_to_unix(2026, 1, 1, 0, 0, 0);
        let y27 = civil_to_unix(2027, 1, 1, 0, 0, 0);
        assert_eq!(span(DateKeyword::ThisYear), (y26, y27));
        assert_eq!(span(DateKeyword::LastYear), (y25, y26));
        // At UTC-12 it is still 2026-11-02 locally.
        assert_eq!(
            DateKeyword::Today.span(now, -12 * 3600),
            Some((midnight - DAY + 12 * 3600, midnight + 12 * 3600))
        );
        for k in DateKeyword::ALL {
            assert_eq!(DateKeyword::from_name(&k.name().to_uppercase()), Some(k));
            let _ = k.span(i64::MAX, 14 * 3600);
            let _ = k.span(i64::MIN, -12 * 3600);
            let _ = k.span(i64::MAX - 14 * 3600, 14 * 3600);
            let _ = k.span(i64::MIN + 12 * 3600, -12 * 3600);
        }
    }

    #[test]
    fn iso_dates() {
        let ok = |s: &str| match parse_date_value(s) {
            Ok(v) => v.to_string(),
            Err(e) => panic!("{s:?}: {e:?}"),
        };
        for s in [
            "2026",
            "0000",
            "2026-11",
            "2026-11-03",
            "2024-02-29",
            "2026-11-03T09:30",
            "2026-11-03T23:59:59",
        ] {
            assert_eq!(ok(s), s);
        }
        assert_eq!(ok("2026-11-03t09:30"), "2026-11-03T09:30");
        assert_eq!(ok("LastWeek"), "lastweek");
        assert_eq!(parse_date_value(""), Err(ArgError::Missing));
        for bad in [
            "202",
            "20266",
            "2026-1",
            "2026-13",
            "2026-00",
            "2026-02-29",
            "2026-11-3",
            "2026-11-03T",
            "2026-11-03T10",
            "2026-11-03T10:",
            "2026-11-03T10:0",
            "2026-11-03T24:00",
            "2026-11-03T10:60",
            "2026-11-03T10:00:60",
            "2026-11-03T10:00:",
            "2026-11-03T10:00:001",
            "2026/11/03",
            "+2026",
            "-2026",
            "２０２６",
            "tomorrow",
            "99999999999999999999",
        ] {
            assert_eq!(parse_date_value(bad), Err(ArgError::Invalid), "{bad:?}");
        }
    }
}
