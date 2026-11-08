//! The date functions `dm:` / `datemodified:`, `dc:` /
//! `datecreated:` and `da:` / `dateaccessed:`, with date keywords, ISO 8601
//! dates, comparisons and ranges.
//!
//! Readings pinned here (our own words, after voidtools' public Search
//! Functions page and docs/PLAN.md):
//!
//! - Dates are read in the evaluation context's local time: `now` plus a
//!   fixed `utc_offset_secs` (no time-zone database, no daylight saving).
//! - A date value is a period. `YYYY` is the whole year, `YYYY-MM` the whole
//!   month, `YYYY-MM-DD` the whole day, `YYYY-MM-DDTHH:MM` the whole minute
//!   and `YYYY-MM-DDTHH:MM:SS` one second.
//! - Keywords are whole calendar periods around `now`: `today`,
//!   `yesterday`, `thisweek`, `lastweek`, `thismonth`, `lastmonth`,
//!   `thisyear`, `lastyear`. Weeks run Monday to Sunday, and `thisweek`
//!   covers the whole current week (Monday 00:00 up to the next Monday),
//!   just as `thismonth` covers the whole month. Keywords are
//!   case-insensitive.
//! - `=` (or no operator) means inside the period, `!=` outside it, `<`
//!   before its start, `<=` before its end, `>` from its end on (after the
//!   period), `>=` from its start on. `==` matches the period's first
//!   instant and nothing outside the period (only those points are checked
//!   here). A range `a..b` runs from the start of `a` to the end of `b`.
//! - Invalid dates (`dm:2026-13`, `dm:2026-02-30`, `T24:00`, ...) give an
//!   `InvalidArgument` diagnostic, an empty argument `MissingArgument`, and
//!   the word is dropped as if it were not typed. Nothing panics.
//!
//! Every test uses an explicit, fixed `now`, so results never depend on the
//! machine's clock or time zone.

use fx_query::eval::{Candidate, EvalContext, matches_in};
use fx_query::{DiagnosticKind, Node, Parsed, SearchOptions, parse};

const HOUR: i64 = 3600;
const DAY: i64 = 86_400;

// ---------------------------------------------------------------------------
// An independent calendar oracle (year-by-year loop, not days-from-civil).
// ---------------------------------------------------------------------------

fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn month_len(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(y) => 29,
        2 => 28,
        _ => panic!("bad month {m}"),
    }
}

fn year_len(y: i32) -> i64 {
    if is_leap(y) { 366 } else { 365 }
}

/// Unix seconds for a UTC date-time.
fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> i64 {
    assert!((1..=12).contains(&mo) && (1..=month_len(y, mo)).contains(&d));
    let mut days: i64 = 0;
    if y >= 1970 {
        for yr in 1970..y {
            days += year_len(yr);
        }
    } else {
        for yr in y..1970 {
            days -= year_len(yr);
        }
    }
    for m in 1..mo {
        days += i64::from(month_len(y, m));
    }
    days += i64::from(d) - 1;
    days * DAY + i64::from(h) * HOUR + i64::from(mi) * 60 + i64::from(s)
}

/// Unix seconds for a local date-time in `ctx`'s time zone.
fn local(ctx: &EvalContext, y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> i64 {
    utc(y, mo, d, h, mi, s) - i64::from(ctx.utc_offset_secs)
}

#[test]
fn oracle_matches_known_timestamps() {
    assert_eq!(utc(1970, 1, 1, 0, 0, 0), 0);
    assert_eq!(utc(2000, 3, 1, 0, 0, 0), 951_868_800);
    assert_eq!(utc(2024, 2, 29, 0, 0, 0), 1_709_164_800);
    assert_eq!(utc(2026, 11, 3, 10, 0, 0), EvalContext::DEFAULT_NOW);
    assert_eq!(utc(1969, 12, 31, 23, 59, 59), -1);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Tuesday 2026-11-03 10:00 in UTC.
fn ctx_utc() -> EvalContext {
    EvalContext {
        now: utc(2026, 11, 3, 10, 0, 0),
        utc_offset_secs: 0,
    }
}

/// A context whose local clock reads `y-mo-d h:00` at `offset` seconds east
/// of UTC.
fn ctx_at(y: i32, mo: u32, d: u32, h: u32, offset: i32) -> EvalContext {
    EvalContext {
        now: utc(y, mo, d, h, 0, 0) - i64::from(offset),
        utc_offset_secs: offset,
    }
}

fn p(query: &str) -> Parsed {
    parse(query, &SearchOptions::default())
}

/// A file with all three timestamps set to `t`.
fn at(t: i64) -> Candidate<'static> {
    Candidate {
        name: "a.txt",
        parent_path: "/Users/me",
        is_dir: false,
        modified: Some(t),
        created: Some(t),
        accessed: Some(t),
        ..Default::default()
    }
}

/// Whether `query` matches a file stamped `t` under `ctx`. Also checks the
/// query parsed without diagnostics.
fn m(query: &str, t: i64, ctx: &EvalContext) -> bool {
    let parsed = p(query);
    assert!(
        parsed.diagnostics.is_empty(),
        "{query:?} should parse cleanly: {:?}",
        parsed.diagnostics
    );
    matches_in(&parsed.query, &at(t), ctx)
        .unwrap_or_else(|e| panic!("{query:?}: unsupported {e:?}"))
}

/// The single filter `query` parses to, as `(name, args)`.
fn filter_of(query: &str) -> (String, String) {
    let parsed = p(query);
    assert!(
        parsed.diagnostics.is_empty(),
        "{query:?}: {:?}",
        parsed.diagnostics
    );
    match &parsed.query {
        Node::Function(f) => (f.name().to_owned(), f.args()),
        other => panic!("{query:?} should parse to one function, got {other:?}"),
    }
}

/// Asserts that `query` matches at each `inside` time and not at each
/// `outside` time.
fn check(query: &str, ctx: &EvalContext, inside: &[i64], outside: &[i64]) {
    for &t in inside {
        assert!(m(query, t, ctx), "{query:?} should match t={t}");
    }
    for &t in outside {
        assert!(!m(query, t, ctx), "{query:?} should not match t={t}");
    }
}

fn check_spans(query: &str, parsed: &Parsed) {
    for d in &parsed.diagnostics {
        assert!(
            d.span.start <= d.span.end && d.span.end <= query.len(),
            "{query:?}: span {:?} out of range",
            d.span
        );
        assert!(
            query.is_char_boundary(d.span.start) && query.is_char_boundary(d.span.end),
            "{query:?}: span {:?} not on char boundaries",
            d.span
        );
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

#[test]
fn function_names_and_aliases() {
    for (q, name) in [
        ("dm:2026", "dm"),
        ("datemodified:2026", "dm"),
        ("DM:2026", "dm"),
        ("DateModified:2026", "dm"),
        ("dc:2026", "dc"),
        ("datecreated:2026", "dc"),
        ("da:2026", "da"),
        ("dateaccessed:2026", "da"),
    ] {
        assert_eq!(filter_of(q), (name.to_owned(), "2026".to_owned()), "{q:?}");
    }
}

#[test]
fn canonical_args() {
    for (q, args) in [
        ("dm:2026-11", "2026-11"),
        ("dm:2026-11-03", "2026-11-03"),
        ("dm:2026-11-03T09:30", "2026-11-03T09:30"),
        ("dm:2026-11-03T09:30:15", "2026-11-03T09:30:15"),
        ("dm:=2026", "2026"),
        ("dm:==2026-11-03", "==2026-11-03"),
        ("dm:!=2026", "!=2026"),
        ("dm:<2026-11", "<2026-11"),
        ("dm:<=2026-11", "<=2026-11"),
        ("dm:>2026-11", ">2026-11"),
        ("dm:>=2026-01-05", ">=2026-01-05"),
        ("dm:2026-01..2026-03", "2026-01..2026-03"),
        ("dm:today", "today"),
        ("dm:Yesterday", "yesterday"),
        ("dm:THISWEEK", "thisweek"),
        ("dm:lastweek", "lastweek"),
        ("dm:thismonth", "thismonth"),
        ("dm:lastmonth", "lastmonth"),
        ("dm:thisyear", "thisyear"),
        ("dm:lastyear", "lastyear"),
        ("dm:>=lastweek", ">=lastweek"),
        ("dm:yesterday..today", "yesterday..today"),
    ] {
        assert_eq!(filter_of(q).1, args, "{q:?}");
    }
}

#[test]
fn quoted_function_name_stays_literal_text() {
    let parsed = p("\"dm:2026\"");
    assert!(matches!(parsed.query, Node::Term(_)), "{:?}", parsed.query);
}

// ---------------------------------------------------------------------------
// Which timestamp each function reads
// ---------------------------------------------------------------------------

#[test]
fn each_function_reads_its_own_timestamp() {
    let ctx = ctx_utc();
    let c = Candidate {
        name: "a.txt",
        parent_path: "/Users/me",
        modified: Some(utc(2026, 6, 1, 12, 0, 0)),
        created: Some(utc(2020, 6, 1, 12, 0, 0)),
        accessed: Some(utc(2010, 6, 1, 12, 0, 0)),
        ..Default::default()
    };
    let ev = |q: &str| matches_in(&p(q).query, &c, &ctx).unwrap();
    assert!(ev("dm:2026") && ev("datemodified:2026"));
    assert!(!ev("dm:2020") && !ev("dm:2010"));
    assert!(ev("dc:2020") && ev("datecreated:2020"));
    assert!(!ev("dc:2026") && !ev("dc:2010"));
    assert!(ev("da:2010") && ev("dateaccessed:2010"));
    assert!(!ev("da:2026") && !ev("da:2020"));
}

#[test]
fn missing_timestamp_never_matches() {
    let ctx = ctx_utc();
    let c = Candidate::file("a.txt", "/Users/me");
    for q in [
        "dm:2026",
        "dm:!=2026",
        "dm:<2026",
        "dm:>2026",
        "dc:today",
        "da:thisyear",
        "dm:2000..2100",
    ] {
        assert_eq!(matches_in(&p(q).query, &c, &ctx), Ok(false), "{q:?}");
    }
}

// ---------------------------------------------------------------------------
// ISO 8601 periods
// ---------------------------------------------------------------------------

#[test]
fn partial_dates_cover_their_whole_period() {
    let c = ctx_utc();
    check(
        "dm:2026",
        &c,
        &[utc(2026, 1, 1, 0, 0, 0), utc(2026, 12, 31, 23, 59, 59)],
        &[utc(2025, 12, 31, 23, 59, 59), utc(2027, 1, 1, 0, 0, 0)],
    );
    check(
        "dm:2026-11",
        &c,
        &[utc(2026, 11, 1, 0, 0, 0), utc(2026, 11, 30, 23, 59, 59)],
        &[utc(2026, 10, 31, 23, 59, 59), utc(2026, 12, 1, 0, 0, 0)],
    );
    check(
        "dm:2026-12",
        &c,
        &[utc(2026, 12, 31, 23, 59, 59)],
        &[utc(2027, 1, 1, 0, 0, 0)],
    );
    check(
        "dm:2026-11-03",
        &c,
        &[utc(2026, 11, 3, 0, 0, 0), utc(2026, 11, 3, 23, 59, 59)],
        &[utc(2026, 11, 2, 23, 59, 59), utc(2026, 11, 4, 0, 0, 0)],
    );
    check(
        "dm:2026-11-03T09:30",
        &c,
        &[utc(2026, 11, 3, 9, 30, 0), utc(2026, 11, 3, 9, 30, 59)],
        &[utc(2026, 11, 3, 9, 29, 59), utc(2026, 11, 3, 9, 31, 0)],
    );
    check(
        "dm:2026-11-03T09:30:15",
        &c,
        &[utc(2026, 11, 3, 9, 30, 15)],
        &[utc(2026, 11, 3, 9, 30, 14), utc(2026, 11, 3, 9, 30, 16)],
    );
}

#[test]
fn dates_around_the_epoch() {
    let c = ctx_utc();
    check("dm:1970-01-01", &c, &[0, DAY - 1], &[-1, DAY]);
    check("dm:1969-12-31", &c, &[-1, -DAY], &[0, -DAY - 1]);
    check("dm:1969", &c, &[-1, utc(1969, 1, 1, 0, 0, 0)], &[0]);
}

// ---------------------------------------------------------------------------
// Comparisons and ranges
// ---------------------------------------------------------------------------

#[test]
fn comparisons_against_the_period() {
    let c = ctx_utc();
    let before = utc(2026, 10, 31, 23, 59, 59);
    let first = utc(2026, 11, 1, 0, 0, 0);
    let mid = utc(2026, 11, 15, 12, 0, 0);
    let last = utc(2026, 11, 30, 23, 59, 59);
    let after = utc(2026, 12, 1, 0, 0, 0);
    check("dm:2026-11", &c, &[first, mid, last], &[before, after]);
    check("dm:=2026-11", &c, &[first, mid, last], &[before, after]);
    check("dm:!=2026-11", &c, &[before, after], &[first, mid, last]);
    check("dm:<2026-11", &c, &[before], &[first, mid, last, after]);
    check("dm:<=2026-11", &c, &[before, first, mid, last], &[after]);
    check("dm:>2026-11", &c, &[after], &[before, first, mid, last]);
    check("dm:>=2026-11", &c, &[first, mid, last, after], &[before]);
    check("dm:==2026-11", &c, &[first], &[before, after]);
    check(
        "dm:==2026-11-03",
        &c,
        &[utc(2026, 11, 3, 0, 0, 0)],
        &[utc(2026, 11, 2, 23, 59, 59), utc(2026, 11, 4, 0, 0, 0)],
    );
    check(
        "dm:>2026",
        &c,
        &[utc(2027, 1, 1, 0, 0, 0)],
        &[utc(2026, 12, 31, 23, 59, 59)],
    );
}

#[test]
fn ranges_include_both_end_periods() {
    let c = ctx_utc();
    check(
        "dm:2026-01..2026-03",
        &c,
        &[
            utc(2026, 1, 1, 0, 0, 0),
            utc(2026, 2, 14, 12, 0, 0),
            utc(2026, 3, 31, 23, 59, 59),
        ],
        &[utc(2025, 12, 31, 23, 59, 59), utc(2026, 4, 1, 0, 0, 0)],
    );
    check(
        "dm:2025..2026-02",
        &c,
        &[utc(2025, 1, 1, 0, 0, 0), utc(2026, 2, 28, 23, 59, 59)],
        &[utc(2024, 12, 31, 23, 59, 59), utc(2026, 3, 1, 0, 0, 0)],
    );
    check(
        "dc:2026-11-01..2026-11-03",
        &c,
        &[utc(2026, 11, 1, 0, 0, 0), utc(2026, 11, 3, 23, 59, 59)],
        &[utc(2026, 10, 31, 23, 59, 59), utc(2026, 11, 4, 0, 0, 0)],
    );
    check(
        "da:yesterday..today",
        &c,
        &[utc(2026, 11, 2, 0, 0, 0), utc(2026, 11, 3, 23, 59, 59)],
        &[utc(2026, 11, 1, 23, 59, 59), utc(2026, 11, 4, 0, 0, 0)],
    );
}

#[test]
fn comparisons_work_inside_groups_and_with_not() {
    let c = ctx_utc();
    let y2025 = utc(2025, 6, 1, 0, 0, 0);
    let y2026 = utc(2026, 6, 1, 0, 0, 0);
    let y2027 = utc(2027, 6, 1, 0, 0, 0);
    check("<dm:<2026 | dm:>2026>", &c, &[y2025, y2027], &[y2026]);
    check("!dm:2026", &c, &[y2025, y2027], &[y2026]);
    check("<dm:>=2026 a.txt>", &c, &[y2026, y2027], &[y2025]);
}

// ---------------------------------------------------------------------------
// Keywords (now = Tuesday 2026-11-03 10:00)
// ---------------------------------------------------------------------------

#[test]
fn day_keywords() {
    let c = ctx_utc();
    check(
        "dm:today",
        &c,
        &[utc(2026, 11, 3, 0, 0, 0), utc(2026, 11, 3, 23, 59, 59)],
        &[utc(2026, 11, 2, 23, 59, 59), utc(2026, 11, 4, 0, 0, 0)],
    );
    check(
        "dm:yesterday",
        &c,
        &[utc(2026, 11, 2, 0, 0, 0), utc(2026, 11, 2, 23, 59, 59)],
        &[utc(2026, 11, 1, 23, 59, 59), utc(2026, 11, 3, 0, 0, 0)],
    );
    check("dm:Today", &c, &[utc(2026, 11, 3, 9, 0, 0)], &[]);
}

#[test]
fn week_keywords_start_on_monday() {
    // 2026-11-03 is a Tuesday: this week is Mon 11-02 .. Sun 11-08.
    let c = ctx_utc();
    check(
        "dm:thisweek",
        &c,
        &[
            utc(2026, 11, 2, 0, 0, 0),
            utc(2026, 11, 3, 10, 0, 0),
            utc(2026, 11, 8, 23, 59, 59),
        ],
        &[utc(2026, 11, 1, 23, 59, 59), utc(2026, 11, 9, 0, 0, 0)],
    );
    check(
        "dm:lastweek",
        &c,
        &[utc(2026, 10, 26, 0, 0, 0), utc(2026, 11, 1, 23, 59, 59)],
        &[utc(2026, 10, 25, 23, 59, 59), utc(2026, 11, 2, 0, 0, 0)],
    );
    check(
        "dm:ThisWeek",
        &c,
        &[utc(2026, 11, 2, 0, 0, 0)],
        &[utc(2026, 11, 1, 0, 0, 0)],
    );
    check(
        "dm:<thisweek",
        &c,
        &[utc(2026, 11, 1, 23, 59, 59)],
        &[utc(2026, 11, 2, 0, 0, 0)],
    );
}

#[test]
fn week_keywords_on_monday_and_sunday() {
    // Monday 2026-11-02 08:00: the week starts today.
    let mon = ctx_at(2026, 11, 2, 8, 0);
    check(
        "dm:thisweek",
        &mon,
        &[utc(2026, 11, 2, 0, 0, 0)],
        &[utc(2026, 11, 1, 23, 59, 59)],
    );
    check(
        "dm:lastweek",
        &mon,
        &[utc(2026, 10, 26, 0, 0, 0), utc(2026, 11, 1, 23, 59, 59)],
        &[utc(2026, 11, 2, 0, 0, 0)],
    );
    // Sunday 2026-11-08 23:00: still the week that began Monday 11-02.
    let sun = ctx_at(2026, 11, 8, 23, 0);
    check(
        "dm:thisweek",
        &sun,
        &[utc(2026, 11, 2, 0, 0, 0), utc(2026, 11, 8, 23, 30, 0)],
        &[utc(2026, 11, 1, 23, 59, 59), utc(2026, 11, 9, 0, 0, 0)],
    );
    check(
        "dm:lastweek",
        &sun,
        &[utc(2026, 10, 26, 0, 0, 0)],
        &[utc(2026, 11, 2, 0, 0, 0)],
    );
}

#[test]
fn month_and_year_keywords() {
    let c = ctx_utc();
    check(
        "dm:thismonth",
        &c,
        &[utc(2026, 11, 1, 0, 0, 0), utc(2026, 11, 30, 23, 59, 59)],
        &[utc(2026, 10, 31, 23, 59, 59), utc(2026, 12, 1, 0, 0, 0)],
    );
    check(
        "dm:lastmonth",
        &c,
        &[utc(2026, 10, 1, 0, 0, 0), utc(2026, 10, 31, 23, 59, 59)],
        &[utc(2026, 9, 30, 23, 59, 59), utc(2026, 11, 1, 0, 0, 0)],
    );
    check(
        "dm:thisyear",
        &c,
        &[utc(2026, 1, 1, 0, 0, 0), utc(2026, 12, 31, 23, 59, 59)],
        &[utc(2025, 12, 31, 23, 59, 59), utc(2027, 1, 1, 0, 0, 0)],
    );
    check(
        "dm:lastyear",
        &c,
        &[utc(2025, 1, 1, 0, 0, 0), utc(2025, 12, 31, 23, 59, 59)],
        &[utc(2024, 12, 31, 23, 59, 59), utc(2026, 1, 1, 0, 0, 0)],
    );
}

#[test]
fn keywords_across_year_boundaries() {
    // Thursday 2026-01-01 09:00: last week is Mon 2025-12-22 .. Sun 12-28,
    // this week Mon 2025-12-29 .. Sun 2026-01-04.
    let c = ctx_at(2026, 1, 1, 9, 0);
    check(
        "dm:yesterday",
        &c,
        &[utc(2025, 12, 31, 12, 0, 0)],
        &[utc(2026, 1, 1, 0, 0, 0)],
    );
    check(
        "dm:thisweek",
        &c,
        &[utc(2025, 12, 29, 0, 0, 0), utc(2026, 1, 4, 23, 59, 59)],
        &[utc(2025, 12, 28, 23, 59, 59), utc(2026, 1, 5, 0, 0, 0)],
    );
    check(
        "dm:lastweek",
        &c,
        &[utc(2025, 12, 22, 0, 0, 0), utc(2025, 12, 28, 23, 59, 59)],
        &[utc(2025, 12, 21, 23, 59, 59), utc(2025, 12, 29, 0, 0, 0)],
    );
    check(
        "dm:lastmonth",
        &c,
        &[utc(2025, 12, 1, 0, 0, 0), utc(2025, 12, 31, 23, 59, 59)],
        &[utc(2025, 11, 30, 23, 59, 59), utc(2026, 1, 1, 0, 0, 0)],
    );
    check(
        "dm:lastyear",
        &c,
        &[utc(2025, 1, 1, 0, 0, 0)],
        &[utc(2026, 1, 1, 0, 0, 0)],
    );
    // March 2024: last month is a leap February.
    let mar = ctx_at(2024, 3, 10, 12, 0);
    check(
        "dm:lastmonth",
        &mar,
        &[utc(2024, 2, 1, 0, 0, 0), utc(2024, 2, 29, 23, 59, 59)],
        &[utc(2024, 1, 31, 23, 59, 59), utc(2024, 3, 1, 0, 0, 0)],
    );
}

// ---------------------------------------------------------------------------
// Local time
// ---------------------------------------------------------------------------

#[test]
fn keywords_and_dates_use_local_time() {
    // UTC+9: 2026-11-03T20:00Z is 2026-11-04 05:00 local.
    let tokyo = ctx_at(2026, 11, 4, 5, 9 * 3600);
    assert_eq!(tokyo.now, utc(2026, 11, 3, 20, 0, 0));
    let today_start = local(&tokyo, 2026, 11, 4, 0, 0, 0);
    assert_eq!(today_start, utc(2026, 11, 3, 15, 0, 0));
    check(
        "dm:today",
        &tokyo,
        &[today_start, today_start + DAY - 1],
        &[today_start - 1, today_start + DAY],
    );
    check(
        "dm:yesterday",
        &tokyo,
        &[today_start - 1, today_start - DAY],
        &[today_start, today_start - DAY - 1],
    );
    check(
        "dm:2026-11-04",
        &tokyo,
        &[today_start, today_start + DAY - 1],
        &[today_start - 1, today_start + DAY],
    );
    check(
        "dm:2026-11-04T00:00",
        &tokyo,
        &[today_start, today_start + 59],
        &[today_start - 1, today_start + 60],
    );
    // Wednesday local: this week starts Monday 11-02 00:00 local.
    let week_start = local(&tokyo, 2026, 11, 2, 0, 0, 0);
    check(
        "dm:thisweek",
        &tokyo,
        &[week_start],
        &[week_start - 1, week_start + 7 * DAY],
    );
    check(
        "dm:2027",
        &tokyo,
        &[local(&tokyo, 2027, 1, 1, 0, 0, 0)],
        &[local(&tokyo, 2026, 12, 31, 23, 59, 59)],
    );

    // UTC-5: 2026-11-03T03:00Z is still 2026-11-02 22:00 local.
    let ny = ctx_at(2026, 11, 2, 22, -5 * 3600);
    assert_eq!(ny.now, utc(2026, 11, 3, 3, 0, 0));
    let ny_today = local(&ny, 2026, 11, 2, 0, 0, 0);
    check(
        "dm:today",
        &ny,
        &[ny_today, ny.now],
        &[ny_today - 1, ny_today + DAY],
    );
    check(
        "dm:2026-11-02",
        &ny,
        &[utc(2026, 11, 3, 4, 59, 59)],
        &[utc(2026, 11, 3, 5, 0, 0)],
    );
}

// ---------------------------------------------------------------------------
// Calendar arithmetic: leap years and month lengths
// ---------------------------------------------------------------------------

#[test]
fn leap_days() {
    let c = ctx_utc();
    for y in [2024, 2000, 1972, 2400] {
        let q = format!("dm:{y}-02-29");
        check(
            &q,
            &c,
            &[utc(y, 2, 29, 0, 0, 0), utc(y, 2, 29, 23, 59, 59)],
            &[utc(y, 2, 28, 23, 59, 59), utc(y, 3, 1, 0, 0, 0)],
        );
    }
    for y in [2023, 2026, 1900, 2100] {
        let q = format!("dm:{y}-02-29");
        let parsed = p(&q);
        assert!(
            parsed
                .diagnostics
                .iter()
                .any(|d| d.kind == DiagnosticKind::InvalidArgument),
            "{q:?} is not a real day: {:?}",
            parsed.diagnostics
        );
    }
}

#[test]
fn month_lengths() {
    let c = ctx_utc();
    for y in [2023, 2024, 2026, 1900, 2000] {
        for mo in 1..=12u32 {
            let len = month_len(y, mo);
            let month = format!("dm:{y}-{mo:02}");
            let (ny, nm) = if mo == 12 { (y + 1, 1) } else { (y, mo + 1) };
            check(
                &month,
                &c,
                &[utc(y, mo, 1, 0, 0, 0), utc(y, mo, len, 23, 59, 59)],
                &[utc(ny, nm, 1, 0, 0, 0), utc(y, mo, 1, 0, 0, 0) - 1],
            );
            let last = format!("dm:{y}-{mo:02}-{len:02}");
            check(&last, &c, &[utc(y, mo, len, 12, 0, 0)], &[]);
            let past = format!("dm:{y}-{mo:02}-{:02}", len + 1);
            let parsed = p(&past);
            assert!(
                parsed
                    .diagnostics
                    .iter()
                    .any(|d| d.kind == DiagnosticKind::InvalidArgument),
                "{past:?} is past the month's end: {:?}",
                parsed.diagnostics
            );
        }
    }
}

#[test]
fn leap_and_common_years_have_the_right_length() {
    let c = ctx_utc();
    for y in [1900, 2000, 2023, 2024, 2100] {
        let start = utc(y, 1, 1, 0, 0, 0);
        let end = start + year_len(y) * DAY;
        check(&format!("dm:{y}"), &c, &[start, end - 1], &[start - 1, end]);
    }
}

// ---------------------------------------------------------------------------
// Invalid and partial input
// ---------------------------------------------------------------------------

const INVALID: [&str; 20] = [
    "dm:2026-13",
    "dm:2026-00",
    "dm:2026-02-30",
    "dm:2026-04-31",
    "dm:2026-11-00",
    "dm:2026-11-32",
    "dm:2026-11-03T24:00",
    "dm:2026-11-03T10:60",
    "dm:2026-11-03T10:00:60",
    "dm:2026-11-03T",
    "dm:2026-11-03T10",
    "dm:notadate",
    "dm:tomorrowish",
    "dm:2026-",
    "dm:>",
    "dm:<=",
    "dm:2026..abc",
    "dm:2026-13..2026-14",
    "dc:2026-02-30",
    "da:2026-13",
];

#[test]
fn invalid_dates_give_a_diagnostic_and_are_dropped() {
    let c = ctx_utc();
    for q in INVALID {
        let parsed = p(q);
        assert!(
            parsed
                .diagnostics
                .iter()
                .any(|d| d.kind == DiagnosticKind::InvalidArgument),
            "{q:?} should give InvalidArgument: {:?}",
            parsed.diagnostics
        );
        check_spans(q, &parsed);
        assert_eq!(parsed.query, Node::MatchAll, "{q:?} should be dropped");
        let _ = matches_in(&parsed.query, &at(c.now), &c);
    }
}

#[test]
fn missing_argument_gives_a_diagnostic() {
    for q in ["dm:", "dc:", "da:", "datemodified:"] {
        let parsed = p(q);
        assert!(
            parsed
                .diagnostics
                .iter()
                .any(|d| d.kind == DiagnosticKind::MissingArgument),
            "{q:?}: {:?}",
            parsed.diagnostics
        );
        check_spans(q, &parsed);
    }
}

#[test]
fn invalid_date_does_not_swallow_other_terms() {
    let c = ctx_utc();
    let file = at(c.now);
    for q in [
        "foo dm:2026-13",
        "dm:2026-02-30 foo",
        "foo dc:>",
        "foo da:x",
    ] {
        let parsed = p(q);
        assert!(!parsed.diagnostics.is_empty(), "{q:?}");
        check_spans(q, &parsed);
        assert_eq!(
            matches_in(&parsed.query, &file, &c),
            Ok(false),
            "{q:?}: `foo` is still required"
        );
    }
    let parsed = p("a.txt dm:2026-13");
    assert_eq!(matches_in(&parsed.query, &file, &c), Ok(true));
}

#[test]
fn partial_input_never_panics() {
    let c = ctx_utc();
    let full = "dm:>=2026-11-03T09:30:15 dc:2026-01..2026-03 <da:thisweek | datemodified:lastyear> dm:!=yesterday";
    for end in 0..=full.len() {
        let q = &full[..end];
        let parsed = p(q);
        check_spans(q, &parsed);
        for t in [0, c.now, -DAY] {
            let _ = matches_in(&parsed.query, &at(t), &c);
        }
        let _ = matches_in(&parsed.query, &Candidate::file("a.txt", "/Users/me"), &c);
    }
}

#[test]
fn extreme_years_do_not_panic() {
    let c = ctx_utc();
    for q in [
        "dm:0000",
        "dm:9999-12-31T23:59:59",
        "dm:99999999999999999999",
        "dm:2026-99999999999999999999",
        "dm:0001-01-01..9999-12-31",
    ] {
        let parsed = p(q);
        check_spans(q, &parsed);
        for t in [i64::MIN, -1, 0, c.now, i64::MAX] {
            let _ = matches_in(&parsed.query, &at(t), &c);
        }
    }
    let extreme = EvalContext {
        now: i64::MAX,
        utc_offset_secs: 14 * 3600,
    };
    for q in ["dm:today", "dm:lastweek", "dm:lastmonth", "dm:lastyear"] {
        let _ = matches_in(&p(q).query, &at(0), &extreme);
    }
    let extreme = EvalContext {
        now: i64::MIN,
        utc_offset_secs: -12 * 3600,
    };
    for q in ["dm:thisweek", "dm:thismonth", "dm:thisyear", "dm:yesterday"] {
        let _ = matches_in(&p(q).query, &at(0), &extreme);
    }
}
