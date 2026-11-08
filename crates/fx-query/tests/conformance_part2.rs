//! Conformance suite, part 2 (search functions, the
//! comparison and range grammar, size constants, date keywords and ISO 8601
//! dates, macros, and their combinations with operators and modifiers).
//!
//! Every case restates, in our own words, behaviour described on voidtools'
//! public Search Functions and Searching pages or in docs/PLAN.md. No help
//! text is copied. The table machinery is shared with part 1 in
//! `tests/conformance/mod.rs`.
//!
//! Candidate notation (see `conformance/mod.rs`): a trailing `/` marks a
//! folder; a spec without other `/` is a bare name in `/Users/me`, so
//! `notes.txt` has depth 3. Metadata words follow the path:
//! `"a.bin size=1500 dm=2026-11-03T10:00 attrib=H"`. The default clock is
//! 2026-11-03T10:00Z (a Tuesday), at UTC.
//!
//! Readings chosen where the public pages leave room, continuing part 1's
//! numbering:
//!
//! 10. A written size is a bucket at its precision (`size:1kb` is
//!     1024..=2047); `==` asks for the exact first value; a range
//!     `a..b` runs from the start of `a`'s bucket to the end of `b`'s.
//! 11. Size constants are bands that include their upper bound: `tiny` up
//!     to 10 KB, `small` to 100 KB, `medium` to 1 MB, `large` to 16 MB,
//!     `huge` to 128 MB, `gigantic` above; `empty` is 0 bytes and `unknown`
//!     matches only an unknown size.
//! 12. Folders have no size, so no numeric `size:` test matches a folder.
//! 13. Metadata a candidate lacks never satisfies a function test, even a
//!     negated comparison (`dm:!=2026` skips entries without a date). A
//!     `!` in front of the function is a tree NOT and does match them.
//! 14. Dates are periods: `=` inside, `<` before the start, `<=` before
//!     the end, `>` from the end on, `>=` from the start on, `==` the first
//!     instant; weeks start on Monday; keywords use the local time zone.
//! 15. `depth:` counts path components (`/Volumes` is 1); `len:` counts
//!     characters, not bytes.
//! 16. `ext:` is case-insensitive even under global Match Case, and a bare
//!     `ext:` matches names with no extension (`.bashrc` has none).
//! 17. A malformed or missing function argument is dropped with a
//!     diagnostic, as if the word were not typed; other terms still apply.
//! 18. `count:N` limits results but filters nothing.
//! 19. A macro with an argument (`pic:holiday`) is the macro's `ext:`
//!     filter AND the argument as an ordinary term; a bare macro is the
//!     filter alone. `!pic:holiday` negates the whole pair.
//! 20. A function or macro name inside quotes (`"pic:"`) is literal text,
//!     as is an unknown name (`pics:`).

mod conformance;

use conformance::{CASE, Case, case, run_cases};

fn sizes() -> Vec<Case> {
    let g = "size";
    vec![
        case(g, "size:1500")
            .yes(&["a.bin size=1500"])
            .no(&["a.bin size=1499", "a.bin size=1501"]),
        case(g, "size:1kb")
            .yes(&["a.bin size=1024", "a.bin size=2047"])
            .no(&["a.bin size=1023", "a.bin size=2048"]),
        case(g, "size:==1kb")
            .yes(&["a.bin size=1024"])
            .no(&["a.bin size=1025"]),
        case(g, "size:>1mb")
            .yes(&["a.bin size=10485760"])
            .no(&["a.bin size=1000"]),
        case(g, "size:<1kb")
            .yes(&["a.bin size=10"])
            .no(&["a.bin size=5000"]),
        case(g, "size:>=1mb")
            .yes(&["a.bin size=1048576"])
            .no(&["a.bin size=1000"]),
        case(g, "size:<=1kb")
            .yes(&["a.bin size=100"])
            .no(&["a.bin size=1048576"]),
        case(g, "size:!=1500")
            .yes(&["a.bin size=1499"])
            .no(&["a.bin size=1500"]),
        case(g, "size:1kb..2kb")
            .yes(&["a.bin size=1024", "a.bin size=3071"])
            .no(&["a.bin size=1023", "a.bin size=4096"]),
        case(g, "size:1.5mb")
            .yes(&["a.bin size=1572864"])
            .no(&["a.bin size=1048576"]),
        case(g, "size:1KB").yes(&["a.bin size=1024"]),
        case(g, "SIZE:1kb").yes(&["a.bin size=1024"]),
        case(g, "size:>1kb").no(&["Big/ size=999999"]),
        case(g, "<size:<1kb ext:txt>")
            .yes(&["a.txt size=10"])
            .no(&["a.txt size=5000", "a.md size=10"]),
        case(g, "<size:>1mb>")
            .yes(&["a.bin size=10485760"])
            .no(&["a.bin size=10"]),
        case(g, "!size:>1mb")
            .yes(&["a.bin size=10"])
            .no(&["a.bin size=10485760"]),
        case(g, "size:tiny report")
            .yes(&["report.pdf size=100"])
            .no(&["report.pdf size=999999", "memo.pdf size=100"]),
        case(g, "\"size:1kb\"").shape("size:1kb"),
    ]
}

fn size_constants() -> Vec<Case> {
    let g = "size constants";
    vec![
        case(g, "size:empty").yes(&["a size=0"]).no(&["a size=1"]),
        case(g, "size:tiny")
            .yes(&["a size=5000", "a size=10240"])
            .no(&["a size=20000"]),
        case(g, "size:small")
            .yes(&["a size=50000"])
            .no(&["a size=5000", "a size=200000"]),
        case(g, "size:medium")
            .yes(&["a size=500000"])
            .no(&["a size=50000", "a size=2000000"]),
        case(g, "size:large")
            .yes(&["a size=5000000", "a size=16777216"])
            .no(&["a size=500000", "a size=20000000"]),
        case(g, "size:huge")
            .yes(&["a size=50000000"])
            .no(&["a size=5000000", "a size=200000000"]),
        case(g, "size:gigantic")
            .yes(&["a size=200000000"])
            .no(&["a size=50000000"]),
        case(g, "size:unknown")
            .yes(&["a.bin"])
            .no(&["a.bin size=0"]),
        case(g, "size:!=tiny")
            .yes(&["a size=20000"])
            .no(&["a size=5000"]),
        case(g, "size:large|size:huge")
            .yes(&["a size=5000000", "a size=50000000"])
            .no(&["a size=500"]),
    ]
}

fn ext() -> Vec<Case> {
    let g = "ext";
    vec![
        case(g, "ext:txt")
            .shape("fn(ext,txt)")
            .yes(&["notes.txt", "NOTES.TXT"])
            .no(&["notes.md", "txt"]),
        case(g, "ext:txt;md")
            .shape("fn(ext,txt;md)")
            .yes(&["a.txt", "b.md"])
            .no(&["c.rs"]),
        case(g, "ext:.TXT").shape("fn(ext,txt)").yes(&["a.txt"]),
        case(g, "ext:")
            .shape("fn(ext,)")
            .yes(&["Makefile", ".bashrc"])
            .no(&["a.txt"]),
        case(g, "ext:gz")
            .yes(&["archive.tar.gz"])
            .no(&["archive.tar"]),
        case(g, "ext:txt;").shape("fn(ext,txt)"),
        case(g, "!ext:txt").yes(&["a.md"]).no(&["a.txt"]),
        case(g, "EXT:txt").yes(&["a.txt"]),
        case(g, "ext:txt report")
            .yes(&["report.txt"])
            .no(&["report.md", "memo.txt"]),
        case(g, "ext:rs | ext:toml")
            .yes(&["main.rs", "Cargo.toml"])
            .no(&["README.md"]),
        case(g, "ext:TXT").opts(CASE).yes(&["a.txt", "b.TXT"]),
    ]
}

fn parent() -> Vec<Case> {
    let g = "parent";
    vec![
        case(g, "parent:/Users/me")
            .yes(&["notes.txt", "Projects/"])
            .no(&["/Users/me/src/main.rs", "/Users/notes.txt"]),
        case(g, "parent:/users/ME/").yes(&["notes.txt"]),
        case(g, "infolder:/Users/me/src")
            .yes(&["/Users/me/src/main.rs"])
            .no(&["notes.txt"]),
        case(g, "parent:/")
            .yes(&["/Volumes/", "/etc/"])
            .no(&["notes.txt"]),
        case(g, "parent:/Users/me ext:txt")
            .yes(&["notes.txt"])
            .no(&["notes.md", "/Users/me/src/a.txt"]),
        case(g, "parent:\"/Users/me/My Docs\"")
            .yes(&["/Users/me/My Docs/a.txt"])
            .no(&["notes.txt"]),
    ]
}

fn depth_and_len() -> Vec<Case> {
    let g = "depth/len";
    vec![
        case(g, "depth:1").yes(&["/Volumes/"]).no(&["notes.txt"]),
        case(g, "depth:3")
            .yes(&["notes.txt"])
            .no(&["/Users/me/src/main.rs"]),
        case(g, "depth:>3")
            .yes(&["/Users/me/src/main.rs"])
            .no(&["notes.txt"]),
        case(g, "depth:<=2")
            .yes(&["/Volumes/", "/Users/x.txt"])
            .no(&["notes.txt"]),
        case(g, "depth:2..3")
            .yes(&["/Users/x.txt", "notes.txt"])
            .no(&["/Volumes/", "/Users/me/src/main.rs"]),
        case(g, "len:5").yes(&["a.txt"]).no(&["ab.txt"]),
        case(g, "len:>10")
            .yes(&["averylongname.txt"])
            .no(&["a.txt"]),
        case(g, "len:3").yes(&["été"]).no(&["ab"]),
        case(g, "len:<=5").yes(&["a.txt"]).no(&["abc.txt"]),
        case(g, "len:4..6")
            .yes(&["ab.c", "abcd.e"])
            .no(&["a.c", "abcdefg"]),
    ]
}

fn empty_count_attrib() -> Vec<Case> {
    let g = "empty/count/attrib";
    vec![
        case(g, "empty:")
            .shape("fn(empty,)")
            .no(&["notes.txt", "Projects/"]),
        case(g, "!empty:").yes(&["notes.txt", "Projects/"]),
        case(g, "count:10 foo").shape("foo").yes(&["foo.txt"]),
        case(g, "count:5").shape("ALL").yes(&["notes.txt"]),
        case(g, "attrib:h").yes(&["a attrib=H"]).no(&["a"]),
        case(g, "attrib:HR")
            .yes(&["a attrib=HR"])
            .no(&["a attrib=H"]),
        case(g, "attrib:d").yes(&["Projects/"]).no(&["notes.txt"]),
        case(g, "attrib:L").yes(&["link attrib=L"]).no(&["a"]),
        case(g, "attrib:A").no(&["a attrib=HLR", "a"]),
        case(g, "!attrib:h").yes(&["a"]).no(&["a attrib=H"]),
        case(g, "attrib:r ext:txt")
            .yes(&["a.txt attrib=R"])
            .no(&["a.md attrib=R"]),
    ]
}

fn date_keywords() -> Vec<Case> {
    let g = "date keywords";
    vec![
        case(g, "dm:today")
            .yes(&["a dm=2026-11-03T00:00", "a dm=2026-11-03T23:59"])
            .no(&["a dm=2026-11-02T23:59", "a dm=2026-11-04"]),
        case(g, "dm:yesterday")
            .yes(&["a dm=2026-11-02T12:00"])
            .no(&["a dm=2026-11-03"]),
        case(g, "dm:thisweek")
            .yes(&["a dm=2026-11-02", "a dm=2026-11-08T23:59"])
            .no(&["a dm=2026-11-01T23:59", "a dm=2026-11-09"]),
        case(g, "dm:lastweek")
            .yes(&["a dm=2026-10-26", "a dm=2026-11-01T23:59"])
            .no(&["a dm=2026-10-25T23:59", "a dm=2026-11-02"]),
        case(g, "dm:thismonth")
            .yes(&["a dm=2026-11-01", "a dm=2026-11-30T23:59"])
            .no(&["a dm=2026-10-31T23:59", "a dm=2026-12-01"]),
        case(g, "dm:lastmonth")
            .yes(&["a dm=2026-10-01", "a dm=2026-10-31T23:59"])
            .no(&["a dm=2026-09-30", "a dm=2026-11-01"]),
        case(g, "dm:thisyear")
            .yes(&["a dm=2026-01-01", "a dm=2026-12-31T23:59"])
            .no(&["a dm=2025-12-31T23:59", "a dm=2027-01-01"]),
        case(g, "dm:lastyear")
            .yes(&["a dm=2025-06-01"])
            .no(&["a dm=2026-01-01", "a dm=2024-12-31"]),
        case(g, "DM:TODAY").yes(&["a dm=2026-11-03T01:00"]),
        case(g, "dm:today")
            .utc_offset(-5 * 3600)
            .yes(&["a dm=2026-11-03T05:00", "a dm=2026-11-04T04:59"])
            .no(&["a dm=2026-11-03T04:59"]),
        case(g, "dm:thismonth")
            .now("2024-02-15")
            .yes(&["a dm=2024-02-29T12:00"])
            .no(&["a dm=2024-03-01"]),
        case(g, "dm:lastmonth")
            .now("2026-01-10")
            .yes(&["a dm=2025-12-15"])
            .no(&["a dm=2026-01-01"]),
        case(g, "dm:lastweek")
            .now("2026-01-01")
            .yes(&["a dm=2025-12-22"])
            .no(&["a dm=2025-12-29", "a dm=2025-12-21T23:59"]),
    ]
}

fn iso_dates() -> Vec<Case> {
    let g = "iso dates";
    vec![
        case(g, "dm:2026")
            .yes(&["a dm=2026-05-05"])
            .no(&["a dm=2025-12-31T23:59"]),
        case(g, "dm:2026-10")
            .yes(&["a dm=2026-10-15"])
            .no(&["a dm=2026-11-01"]),
        case(g, "dm:2026-10-15")
            .yes(&["a dm=2026-10-15T23:59"])
            .no(&["a dm=2026-10-16"]),
        case(g, "dm:2026-10-15T08:30")
            .yes(&["a dm=2026-10-15T08:30"])
            .no(&["a dm=2026-10-15T08:31"]),
        case(g, "dm:2026-10-15t08:30").yes(&["a dm=2026-10-15T08:30"]),
        case(g, "dm:<2026")
            .yes(&["a dm=2025-12-31T23:59"])
            .no(&["a dm=2026-01-01"]),
        case(g, "dm:>=2026-10")
            .yes(&["a dm=2026-10-01", "a dm=2026-11-03"])
            .no(&["a dm=2026-09-30T23:59"]),
        case(g, "dm:>2026-10")
            .yes(&["a dm=2026-11-01"])
            .no(&["a dm=2026-10-31T23:59"]),
        case(g, "dm:<=2026-10")
            .yes(&["a dm=2026-10-31T23:59"])
            .no(&["a dm=2026-11-01"]),
        case(g, "dm:!=2026")
            .yes(&["a dm=2025-06-01"])
            .no(&["a dm=2026-06-01", "a"]),
        case(g, "dm:==2026-10-15")
            .yes(&["a dm=2026-10-15"])
            .no(&["a dm=2026-10-15T00:01"]),
        case(g, "dm:2026-01..2026-03")
            .yes(&["a dm=2026-01-01", "a dm=2026-03-31T23:59"])
            .no(&["a dm=2025-12-31T23:59", "a dm=2026-04-01"]),
        case(g, "dm:2024-02-29").yes(&["a dm=2024-02-29T12:00"]),
        case(g, "dc:today")
            .yes(&["a dc=2026-11-03T05:00"])
            .no(&["a dm=2026-11-03T05:00"]),
        case(g, "da:2026-11")
            .yes(&["a da=2026-11-02"])
            .no(&["a da=2026-10-02"]),
        case(g, "datemodified:2026").yes(&["a dm=2026-02-02"]),
        case(g, "datecreated:2025")
            .yes(&["a dc=2025-02-02"])
            .no(&["a dc=2026-02-02"]),
        case(g, "dateaccessed:today").yes(&["a da=2026-11-03"]),
        case(g, "<dm:>=2026-11-01>")
            .yes(&["a dm=2026-11-02"])
            .no(&["a dm=2026-10-02"]),
        case(g, "dm:today ext:pdf")
            .yes(&["a.pdf dm=2026-11-03T09:00"])
            .no(&["a.txt dm=2026-11-03T09:00", "a.pdf dm=2026-11-01"]),
        case(g, "dm:thisweek|dc:thisweek")
            .yes(&["a dc=2026-11-03", "a dm=2026-11-02T08:00"])
            .no(&["a dm=2026-10-01"]),
        case(g, "!dm:thisyear")
            .yes(&["a dm=2025-01-01"])
            .no(&["a dm=2026-03-03"]),
    ]
}

fn macros() -> Vec<Case> {
    let g = "macros";
    vec![
        case(g, "pic:")
            .yes(&["photo.jpg", "IMG_1.HEIC", "shot.png"])
            .no(&["notes.txt", "photojpg"]),
        case(g, "PIC:").yes(&["photo.jpg"]).no(&["notes.txt"]),
        case(g, "pic:holiday")
            .yes(&["holiday.jpg", "holiday 2025.png"])
            .no(&["holiday.txt", "beach.jpg"]),
        case(g, "!pic:").yes(&["notes.txt"]).no(&["photo.png"]),
        case(g, "!pic:holiday")
            .yes(&["holiday.txt", "beach.jpg"])
            .no(&["holiday.jpg"]),
        case(g, "pic:|video:")
            .yes(&["a.jpg", "b.mp4", "c.mov"])
            .no(&["c.txt"]),
        case(g, "<pic: | doc:> holiday")
            .yes(&["holiday.jpg", "holiday.pdf"])
            .no(&["holiday.mp3", "trip.jpg"]),
        case(g, "pic:\"summer holiday\"")
            .yes(&["summer holiday.jpg"])
            .no(&["summer.jpg", "summer holiday.txt"]),
        case(g, "pic:IMG_*")
            .yes(&["IMG_001.jpg", "IMG_002.heic"])
            .no(&["photo.png", "IMG_003.txt"]),
        case(g, "pic: case:Holiday")
            .yes(&["Holiday.jpg"])
            .no(&["holiday.jpg"]),
        case(g, "audio:")
            .yes(&["song.mp3", "take.wav", "track.flac", "voice.m4a"])
            .no(&["song.mp4"]),
        case(g, "doc:")
            .yes(&["a.pdf", "b.docx", "c.pages", "d.numbers", "e.key"])
            .no(&["a.jpg"]),
        case(g, "exe:")
            .yes(&["setup.exe", "Installer.pkg"])
            .no(&["readme.txt"]),
        case(g, "video:")
            .yes(&["clip.mp4", "movie.mkv", "screen.mov"])
            .no(&["clip.mp3"]),
        case(g, "zip:")
            .yes(&["a.zip", "b.rar", "c.7z", "d.tar.gz"])
            .no(&["a.txt", ".zip"]),
        case(g, "zip: report")
            .yes(&["report.zip"])
            .no(&["report.pdf", "data.zip"]),
        case(g, "doc: dm:today")
            .yes(&["a.pdf dm=2026-11-03T08:00"])
            .no(&["a.pdf dm=2026-10-01", "a.jpg dm=2026-11-03T08:00"]),
        case(g, "pic: size:>1mb")
            .yes(&["big.jpg size=10485760"])
            .no(&["small.jpg size=100", "big.mov size=10485760"]),
        case(g, "pic: !ext:png")
            .yes(&["a.jpg"])
            .no(&["a.png", "a.txt"]),
        case(g, "pic: parent:/Users/me")
            .yes(&["a.jpg"])
            .no(&["/Users/me/src/a.jpg"]),
        case(g, "\"pic:\"").shape("pic:"),
        case(g, "pics:").shape("pics:"),
    ]
}

fn with_modifiers() -> Vec<Case> {
    let g = "modifiers";
    vec![
        case(g, "attrib:h folder:")
            .any_diag()
            .yes(&["Dir/ attrib=H"])
            .no(&["a attrib=H", "Dir/"]),
        case(g, "case:Report ext:pdf")
            .yes(&["Report.pdf"])
            .no(&["report.pdf", "Report.txt"]),
        case(g, "path:src ext:rs")
            .yes(&["/Users/me/src/main.rs"])
            .no(&["/Users/me/lib/main.rs"]),
        case(g, "ww:main ext:rs")
            .yes(&["main.rs"])
            .no(&["domain.rs", "main.py"]),
        case(g, "wfn:main.rs depth:4")
            .yes(&["/Users/me/src/main.rs"])
            .no(&["main.rs", "/Users/me/src/main.rs.bak"]),
        case(g, "ext:txt").opts(CASE).yes(&["A.TXT"]),
    ]
}

fn leniency() -> Vec<Case> {
    let g = "leniency";
    vec![
        case(g, "size:").lenient().shape("ALL").yes(&["notes.txt"]),
        case(g, "foo size:").lenient().shape("foo"),
        case(g, "size:abc foo")
            .lenient()
            .yes(&["foo.txt"])
            .no(&["bar.txt"]),
        case(g, "size:> foo").lenient().yes(&["foo.txt"]),
        case(g, "size:1kb..").lenient(),
        case(g, "size:>1kb..2kb").lenient(),
        case(g, "parent:").lenient().shape("ALL"),
        case(g, "depth:x").lenient(),
        case(g, "len:-1").lenient(),
        case(g, "empty:x").lenient().shape("ALL"),
        case(g, "count:").lenient(),
        case(g, "count:-1 foo").lenient().shape("foo"),
        case(g, "attrib:1").lenient(),
        case(g, "dm:").lenient().shape("ALL"),
        case(g, "dm:2026-13").lenient(),
        case(g, "dm:2026-02-30").lenient(),
        case(g, "dm:2026-02-29").lenient(),
        case(g, "dm:2026-10-15T08").lenient(),
        case(g, "dm:2026- foo")
            .lenient()
            .yes(&["foo.txt"])
            .no(&["bar.txt"]),
        case(g, "dm:sometime").lenient(),
        case(g, "ext:txt |").lenient().yes(&["a.txt"]).no(&["a.md"]),
        case(g, "<size:>1mb")
            .lenient()
            .yes(&["a size=10485760"])
            .no(&["a size=10"]),
        case(g, "pic:holiday >")
            .lenient()
            .yes(&["holiday.jpg"])
            .no(&["holiday.txt"]),
    ]
}

fn all_cases() -> Vec<Case> {
    [
        sizes(),
        size_constants(),
        ext(),
        parent(),
        depth_and_len(),
        empty_count_attrib(),
        date_keywords(),
        iso_dates(),
        macros(),
        with_modifiers(),
        leniency(),
    ]
    .concat()
}

/// Part 1 has 208 cases; part 2 must bring the total to at least 300.
#[test]
fn conformance_part2_has_at_least_92_cases() {
    let n = all_cases().len();
    assert!(
        n >= 92,
        "part 2 has {n} cases; part 1 (208) + part 2 must reach 300"
    );
}

#[test]
fn conformance_part2_all_cases_pass() {
    run_cases("conformance_part2", &all_cases());
}

#[test]
fn count_sets_the_result_limit() {
    let opts = conformance::DEFAULT;
    assert_eq!(fx_query::parse("count:10 foo", &opts).count, Some(10));
    assert_eq!(fx_query::parse("count:3 count:7", &opts).count, Some(7));
    assert_eq!(fx_query::parse("foo", &opts).count, None);
}
