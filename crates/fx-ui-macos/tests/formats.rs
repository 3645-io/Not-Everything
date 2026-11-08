//! Contract tests for `fx_ui_macos::formats`: the results table's sizes,
//! dates and paths, and the figures of the status bar (the owner's choices
//! and the canvas's examples).

use std::time::Duration;

use fx_ui_macos::formats::{
    FixedOffset, NO_VALUE, UTC, display_folder, format_date, format_millis, format_size,
    group_thousands, write_date, write_folder, write_size, write_size_cell,
};

#[test]
fn sizes_match_every_example_on_the_canvas() {
    // (bytes, text) from the Main board's rows.
    let examples = [
        (186_368, "182 KB"),
        (180_224, "176 KB"),
        (49_152, "48 KB"),
        (175_104, "171 KB"),
        (65_536, "64 KB"),
        (1_258_291, "1.2 MB"),
        (3_565_158, "3.4 MB"),
        (172_032, "168 KB"),
        (1_153_434, "1.1 MB"),
        (2_516_582, "2.4 MB"),
        (421_888, "412 KB"),
        (9_216, "9 KB"),
        (225_280, "220 KB"),
        (36_864, "36 KB"),
        (8_493_465, "8.1 MB"),
        (4_096, "4 KB"),
    ];
    for (bytes, text) in examples {
        assert_eq!(format_size(bytes), text, "{bytes} bytes");
    }
}

#[test]
fn sizes_below_a_kilobyte_are_bytes() {
    assert_eq!(format_size(0), "0 bytes");
    assert_eq!(format_size(1), "1 byte");
    assert_eq!(format_size(2), "2 bytes");
    assert_eq!(format_size(512), "512 bytes");
    assert_eq!(format_size(1_023), "1,023 bytes");
}

#[test]
fn kilobytes_are_whole_and_megabytes_up_have_one_decimal() {
    assert_eq!(format_size(1_024), "1 KB");
    assert_eq!(format_size(1_535), "1 KB");
    assert_eq!(format_size(1_536), "2 KB");
    assert_eq!(format_size(1_023 * 1_024), "1,023 KB");
    // 1023.4 KB still reads as kilobytes; 1023.5 KB would round to 1,024
    // KB, so it moves up to megabytes.
    assert_eq!(format_size(1_048_063), "1,023 KB");
    assert_eq!(format_size(1_048_064), "1.0 MB");
    assert_eq!(format_size(1_048_576), "1.0 MB");
    assert_eq!(format_size(1_000 * 1_048_576), "1,000.0 MB");
    assert_eq!(format_size(1_073_741_824 - 1), "1.0 GB");
    assert_eq!(format_size(3 * 1_073_741_824 / 2), "1.5 GB");
    assert_eq!(format_size(1_024u64.pow(4)), "1.0 TB");
    assert_eq!(format_size(1_024u64.pow(5) * 3), "3.0 PB");
    assert_eq!(format_size(u64::MAX), "16.0 EB");
}

#[test]
fn write_size_appends() {
    let mut s = String::from("size: ");
    write_size(186_368, &mut s);
    assert_eq!(s, "size: 182 KB");
}

#[test]
fn the_size_cell_shows_a_dash_for_folders() {
    let cell = |size, folder| {
        let mut s = String::from("old");
        s.clear();
        write_size_cell(size, folder, &mut s);
        s
    };
    assert_eq!(NO_VALUE, "\u{2014}");
    assert_eq!(cell(None, true), "\u{2014}");
    assert_eq!(cell(Some(4_096), true), "\u{2014}");
    assert_eq!(cell(Some(4_096), false), "4 KB");
    // A file whose size is unknown shows nothing.
    assert_eq!(cell(None, false), "");
}

#[test]
fn dates_read_as_the_canvas_writes_them() {
    assert_eq!(format_date(1_773_479_520, &UTC), "14 Mar 2026 09:12");
    assert_eq!(format_date(1_767_348_300, &UTC), "2 Jan 2026 10:05");
    assert_eq!(format_date(1_766_951_220, &UTC), "28 Dec 2025 19:47");
    assert_eq!(format_date(1_793_873_520, &UTC), "5 Nov 2026 10:12");
    assert_eq!(format_date(1_709_210_040, &UTC), "29 Feb 2024 12:34");
    assert_eq!(format_date(0, &UTC), "1 Jan 1970 00:00");
    assert_eq!(format_date(59, &UTC), "1 Jan 1970 00:00");
    assert_eq!(format_date(-60, &UTC), "31 Dec 1969 23:59");
}

#[test]
fn dates_follow_the_gregorian_calendar() {
    assert_eq!(format_date(60, &UTC), "1 Jan 1970 00:01");
    assert_eq!(format_date(946_684_799, &UTC), "31 Dec 1999 23:59");
    assert_eq!(format_date(946_684_800, &UTC), "1 Jan 2000 00:00");
    // 2000 is a leap year (divisible by 400).
    assert_eq!(format_date(951_782_400, &UTC), "29 Feb 2000 00:00");
    assert_eq!(format_date(951_868_800, &UTC), "1 Mar 2000 00:00");
    assert_eq!(format_date(1_700_000_000, &UTC), "14 Nov 2023 22:13");
    // 2100 is not a leap year: 59 days after 1 January is 1 March.
    assert_eq!(format_date(4_102_444_800, &UTC), "1 Jan 2100 00:00");
    assert_eq!(format_date(4_107_542_400, &UTC), "1 Mar 2100 00:00");
}

#[test]
fn every_month_has_its_short_name() {
    let names: Vec<String> = (0..12)
        .map(|m| {
            // The 15th of each month of 2026, at noon.
            let days = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334][m] + 14;
            format_date(1_767_225_600 + days * 86_400 + 43_200, &UTC)
        })
        .collect();
    assert_eq!(
        names,
        [
            "15 Jan 2026 12:00",
            "15 Feb 2026 12:00",
            "15 Mar 2026 12:00",
            "15 Apr 2026 12:00",
            "15 May 2026 12:00",
            "15 Jun 2026 12:00",
            "15 Jul 2026 12:00",
            "15 Aug 2026 12:00",
            "15 Sep 2026 12:00",
            "15 Oct 2026 12:00",
            "15 Nov 2026 12:00",
            "15 Dec 2026 12:00",
        ]
    );
}

#[test]
fn dates_are_in_local_time() {
    // One hour east of UTC: 08:12 UTC is 09:12 local.
    assert_eq!(
        format_date(1_773_475_920, &FixedOffset(3_600)),
        "14 Mar 2026 09:12"
    );
    // West of UTC across midnight and the year.
    assert_eq!(
        format_date(0, &FixedOffset(-5 * 3_600)),
        "31 Dec 1969 19:00"
    );
    // Half-hour zones.
    assert_eq!(
        format_date(1_773_475_920, &FixedOffset(19_800)),
        "14 Mar 2026 13:42"
    );
}

#[test]
fn the_offset_is_taken_at_each_date() {
    // A zone that moves from +1 h to +2 h at 2026-03-29 01:00 UTC, as
    // Central Europe does.
    let zone = |t: i64| if t < 1_774_746_000 { 3_600 } else { 7_200 };
    assert_eq!(format_date(1_774_745_940, &zone), "29 Mar 2026 01:59");
    assert_eq!(format_date(1_774_746_000, &zone), "29 Mar 2026 03:00");
    let mut s = String::from("at ");
    write_date(1_774_746_000, &zone, &mut s);
    assert_eq!(s, "at 29 Mar 2026 03:00");
}

#[test]
fn extreme_dates_do_not_panic() {
    for t in [i64::MIN, i64::MIN + 1, i64::MAX] {
        for offset in [-50_400, 0, 50_400] {
            assert!(!format_date(t, &FixedOffset(offset)).is_empty());
        }
    }
}

#[test]
fn the_home_folder_reads_as_a_tilde() {
    let home = Some("/Users/user");
    assert_eq!(display_folder("/Users/user", home), "~");
    assert_eq!(
        display_folder("/Users/user/Documents/Finance", home),
        "~/Documents/Finance"
    );
    assert_eq!(
        display_folder("/Users/user/Documents/Finance", Some("/Users/user/")),
        "~/Documents/Finance"
    );
    // Only whole folder names count.
    assert_eq!(display_folder("/Users/userx/a", home), "/Users/userx/a");
    assert_eq!(display_folder("/Users", home), "/Users");
    assert_eq!(
        display_folder("/Volumes/Backup/Finance", home),
        "/Volumes/Backup/Finance"
    );
    assert_eq!(display_folder("/", home), "/");
    assert_eq!(display_folder("", home), "");
    // No home, or a home that is the root or empty, changes nothing.
    assert_eq!(display_folder("/Users/user/a", None), "/Users/user/a");
    assert_eq!(display_folder("/Users/user/a", Some("/")), "/Users/user/a");
    assert_eq!(display_folder("/Users/user/a", Some("")), "/Users/user/a");

    let mut s = String::from("in ");
    write_folder("/Users/user/Code", home, &mut s);
    assert_eq!(s, "in ~/Code");
}

#[test]
fn counts_have_thousands_separators() {
    assert_eq!(group_thousands(0), "0");
    assert_eq!(group_thousands(999), "999");
    assert_eq!(group_thousands(1_000), "1,000");
    assert_eq!(group_thousands(2_014_377), "2,014,377");
    // The controller's re-export is the same function.
    assert_eq!(fx_ui_macos::controller::group_thousands(1_234), "1,234");
}

#[test]
fn search_times_in_whole_milliseconds() {
    let ms = |m: f64| format_millis(Duration::from_secs_f64(m / 1_000.0));
    assert_eq!(ms(9.0), "9 ms");
    assert_eq!(ms(9.4), "9 ms");
    assert_eq!(ms(9.5), "10 ms");
    assert_eq!(ms(0.4), "<1 ms");
    assert_eq!(ms(0.0), "<1 ms");
    assert_eq!(ms(0.6), "1 ms");
    assert_eq!(ms(1_234.0), "1,234 ms");
}
