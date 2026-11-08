//! The txt, csv and json formatters.
//!
//! API these tests expect from `fx_cli::output`:
//!
//! - `Row { name: OsString, path: PathBuf, full_path: PathBuf, size: u64,
//!   date_modified: i64, date_created: i64 }`: a plain result row; `path` is
//!   the containing folder, dates are seconds since the Unix epoch.
//! - `Format { Txt, Csv, Json }`, `Column { Name, Path, FullPath, Size,
//!   DateModified, DateCreated }`, and `Column::key(self) -> &'static str`
//!   giving `name`, `path`, `full_path`, `size`, `date_modified`,
//!   `date_created`. `Format` and `Column` derive `Debug, Clone, Copy,
//!   PartialEq, Eq`.
//! - `write_rows(out: &mut impl Write, format: Format, columns: &[Column],
//!   rows: I) -> io::Result<()>` where `I: IntoIterator<Item = R>` and
//!   `R: Borrow<Row>` (so a slice iterator of `&Row` works).
//! - `write_rows_with(out, format, columns, rows, style: Style)`, the
//!   same with a `Style { nul_terminated, escape_controls }` (both
//!   `bool`, `Default` all off, `Copy`): `nul_terminated` ends each txt
//!   row with NUL instead of a newline, and `escape_controls` writes each
//!   control character in a txt or csv value as `?`, before csv quoting.
//!   json ignores the style.
//! - `write_json_string(out, s)`, which writes one JSON string literal.
//!
//! Output contract: txt is one row per line, values tab-separated; csv has
//! a header row of column keys and RFC 4180 quoting; json is an array of
//! objects whose keys are the chosen columns' keys, `size` a number and
//! dates ISO 8601 UTC strings (`YYYY-MM-DDTHH:MM:SSZ`). Paths are UTF-8,
//! lossily converted when needed. json escapes every control character,
//! U+007F to U+009F as `\u007f` to `\u009f`. The tests mostly parse csv
//! and json rather than compare bytes, so whitespace and optional quoting
//! are free, except where a terminal's `?` removes the need for quotes.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use fx_cli::output::{Column, Format, Row, Style, write_json_string, write_rows, write_rows_with};

const ALL: [Column; 6] = [
    Column::Name,
    Column::Path,
    Column::FullPath,
    Column::Size,
    Column::DateModified,
    Column::DateCreated,
];

fn row(dir: &str, name: &str, size: u64, modified: i64, created: i64) -> Row {
    let full = if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    };
    Row {
        name: OsString::from(name),
        path: PathBuf::from(dir),
        full_path: PathBuf::from(full),
        size,
        date_modified: modified,
        date_created: created,
    }
}

fn render(format: Format, columns: &[Column], rows: &[Row]) -> String {
    let mut out = Vec::new();
    write_rows(&mut out, format, columns, rows.iter()).expect("write to a Vec");
    String::from_utf8(out).expect("output is UTF-8")
}

// ---------------------------------------------------------------- keys

#[test]
fn column_keys_are_stable() {
    let keys: Vec<&str> = ALL.iter().map(|c| c.key()).collect();
    assert_eq!(
        keys,
        [
            "name",
            "path",
            "full_path",
            "size",
            "date_modified",
            "date_created"
        ]
    );
}

// ----------------------------------------------------------------- txt

#[test]
fn txt_default_column_is_one_full_path_per_line() {
    let rows = [
        row("/Users/me", "a.txt", 1, 0, 0),
        row("/Users/me/docs", "b c.md", 2, 0, 0),
        row("/", "etc", 0, 0, 0),
    ];
    let out = render(Format::Txt, &[Column::FullPath], &rows);
    assert_eq!(out, "/Users/me/a.txt\n/Users/me/docs/b c.md\n/etc\n");
}

#[test]
fn txt_with_no_rows_is_empty() {
    assert_eq!(render(Format::Txt, &[Column::FullPath], &[]), "");
}

#[test]
fn txt_several_columns_are_tab_separated() {
    let rows = [row("/Users/me", "a.txt", 1234, 1_700_000_000, 0)];
    let out = render(
        Format::Txt,
        &[
            Column::Name,
            Column::Size,
            Column::DateModified,
            Column::Path,
        ],
        &rows,
    );
    assert_eq!(out, "a.txt\t1234\t2023-11-14T22:13:20Z\t/Users/me\n");
}

#[test]
fn txt_columns_follow_the_order_given() {
    let rows = [row("/d", "n", 5, 0, 951_782_400)];
    let out = render(Format::Txt, &[Column::DateCreated, Column::FullPath], &rows);
    assert_eq!(out, "2000-02-29T00:00:00Z\t/d/n\n");
}

#[test]
fn dates_are_iso_8601_utc() {
    for (secs, iso) in [
        (0, "1970-01-01T00:00:00Z"),
        (1_700_000_000, "2023-11-14T22:13:20Z"),
        (951_782_400, "2000-02-29T00:00:00Z"),
        (1_709_251_199, "2024-02-29T23:59:59Z"),
        (-86_400, "1969-12-31T00:00:00Z"),
        (-1, "1969-12-31T23:59:59Z"),
        (4_102_444_800, "2100-01-01T00:00:00Z"),
    ] {
        let rows = [row("/d", "n", 0, secs, secs)];
        let out = render(
            Format::Txt,
            &[Column::DateModified, Column::DateCreated],
            &rows,
        );
        assert_eq!(out, format!("{iso}\t{iso}\n"), "{secs}");
    }
}

#[cfg(unix)]
#[test]
fn non_utf8_paths_are_written_lossily() {
    use std::os::unix::ffi::OsStringExt;
    let name = OsString::from_vec(b"bad\xffname".to_vec());
    let full = OsString::from_vec(b"/d/bad\xffname".to_vec());
    let r = Row {
        name,
        path: PathBuf::from("/d"),
        full_path: PathBuf::from(full),
        size: 0,
        date_modified: 0,
        date_created: 0,
    };
    let rows = [r];
    let txt = render(Format::Txt, &[Column::Name, Column::FullPath], &rows);
    assert_eq!(txt, "bad\u{FFFD}name\t/d/bad\u{FFFD}name\n");
    let csv = parse_csv(&render(Format::Csv, &[Column::FullPath], &rows));
    assert_eq!(csv[1], ["/d/bad\u{FFFD}name"]);
    let json = parse_json(&render(Format::Json, &[Column::Name], &rows));
    assert_eq!(
        objects(&json)[0]["name"],
        Json::Str("bad\u{FFFD}name".into())
    );
}

// ------------------------------------------------- style: terminals and -0

fn render_styled(format: Format, columns: &[Column], rows: &[Row], style: Style) -> Vec<u8> {
    let mut out = Vec::new();
    write_rows_with(&mut out, format, columns, rows.iter(), style).expect("write to a Vec");
    out
}

/// Names holding control characters: C0 (newline, tab, escape, bell), DEL
/// and C1 (U+009B, which some terminals read as an escape sequence).
const CONTROL_NAMES: [&str; 6] = [
    "line\nbreak",
    "tab\there",
    "esc\u{1b}[31mred",
    "bell\u{7}del\u{7f}",
    "c1\u{9b}2J",
    "café ü 日本 ?",
];

#[test]
fn txt_is_exact_by_default() {
    let rows: Vec<Row> = CONTROL_NAMES
        .iter()
        .map(|n| row("/d", n, 0, 0, 0))
        .collect();
    let expected: String = CONTROL_NAMES.iter().map(|n| format!("{n}\n")).collect();
    assert_eq!(render(Format::Txt, &[Column::Name], &rows), expected);
    let plain = render_styled(Format::Txt, &[Column::Name], &rows, Style::default());
    assert_eq!(plain, expected.as_bytes());
}

#[test]
fn txt_shows_control_characters_as_question_marks_when_asked() {
    let rows: Vec<Row> = CONTROL_NAMES
        .iter()
        .map(|n| row("/d", n, 0, 0, 0))
        .collect();
    let style = Style {
        escape_controls: true,
        ..Style::default()
    };
    let out = render_styled(Format::Txt, &[Column::Name, Column::FullPath], &rows, style);
    let out = String::from_utf8(out).expect("UTF-8");
    let shown = [
        "line?break",
        "tab?here",
        "esc?[31mred",
        "bell?del?",
        "c1?2J",
        "café ü 日本 ?",
    ];
    let expected: String = shown.iter().map(|n| format!("{n}\t/d/{n}\n")).collect();
    // The tab between columns and the newline after each row stay as they
    // are; only the values change.
    assert_eq!(out, expected);
    assert!(
        !out.chars()
            .any(|c| c.is_control() && c != '\t' && c != '\n')
    );
}

#[test]
fn txt_with_nul_ends_each_row_with_nul_and_keeps_names_exact() {
    let rows: Vec<Row> = CONTROL_NAMES
        .iter()
        .map(|n| row("/d", n, 7, 0, 0))
        .collect();
    let style = Style {
        nul_terminated: true,
        ..Style::default()
    };
    let out = render_styled(Format::Txt, &[Column::FullPath], &rows, style);
    let expected: String = CONTROL_NAMES.iter().map(|n| format!("/d/{n}\0")).collect();
    assert_eq!(out, expected.as_bytes());

    // Several columns are still tab-separated.
    let out = render_styled(
        Format::Txt,
        &[Column::Name, Column::Size],
        &rows[..1],
        style,
    );
    assert_eq!(out, b"line\nbreak\t7\0");

    // No rows print nothing.
    assert!(render_styled(Format::Txt, &[Column::FullPath], &[], style).is_empty());

    // On a terminal both apply: NUL after each row, `?` inside the values.
    let both = Style {
        nul_terminated: true,
        escape_controls: true,
    };
    let out = render_styled(Format::Txt, &[Column::Name], &rows[..2], both);
    assert_eq!(out, b"line?break\0tab?here\0");
}

#[test]
fn csv_is_exact_by_default_and_ignores_nul() {
    let rows: Vec<Row> = CONTROL_NAMES
        .iter()
        .map(|n| row("/d", n, 1, 0, 0))
        .collect();
    let exact = render(Format::Csv, &[Column::Name], &rows);
    let nul = Style {
        nul_terminated: true,
        ..Style::default()
    };
    for style in [Style::default(), nul] {
        assert_eq!(
            render_styled(Format::Csv, &[Column::Name], &rows, style),
            exact.as_bytes(),
            "{style:?}"
        );
    }
    let table = parse_csv(&exact);
    assert_eq!(table.len(), CONTROL_NAMES.len() + 1);
    for (record, name) in table[1..].iter().zip(CONTROL_NAMES) {
        assert_eq!(record, &[name]);
    }
}

#[test]
fn csv_shows_control_characters_as_question_marks_when_asked() {
    let mut names = CONTROL_NAMES.to_vec();
    names.push("quote\",comma\u{1b}");
    let rows: Vec<Row> = names.iter().map(|n| row("/d", n, 0, 0, 0)).collect();
    let style = Style {
        escape_controls: true,
        ..Style::default()
    };
    let out = render_styled(Format::Csv, &[Column::Name, Column::FullPath], &rows, style);
    let out = String::from_utf8(out).expect("UTF-8");
    // The CRLF after each record is the only control character left.
    assert!(
        !out.replace("\r\n", "").chars().any(char::is_control),
        "{out:?}"
    );
    let shown = [
        "line?break",
        "tab?here",
        "esc?[31mred",
        "bell?del?",
        "c1?2J",
        "café ü 日本 ?",
        "quote\",comma?",
    ];
    let table = parse_csv(&out);
    assert_eq!(table[0], ["name", "full_path"]);
    let expected: Vec<Vec<String>> = shown
        .iter()
        .map(|n| vec![n.to_string(), format!("/d/{n}")])
        .collect();
    assert_eq!(table[1..], expected[..]);
    // Quoting follows the shown text: a comma or a quote is still quoted,
    // a line break shown as `?` no longer needs quotes.
    assert!(out.contains("\r\nline?break,/d/line?break\r\n"), "{out:?}");
    assert!(out.contains("\"quote\"\",comma?\""), "{out:?}");
}

#[test]
fn json_ignores_the_style() {
    let rows: Vec<Row> = CONTROL_NAMES
        .iter()
        .map(|n| row("/d", n, 1, 0, 0))
        .collect();
    let all_on = Style {
        nul_terminated: true,
        escape_controls: true,
    };
    assert_eq!(
        render_styled(Format::Json, &ALL, &rows, all_on),
        render(Format::Json, &ALL, &rows).into_bytes()
    );
}

// ----------------------------------------------------------------- csv

#[test]
fn csv_has_a_header_row_of_column_keys() {
    let rows = [row("/a", "b", 3, 0, 0)];
    let table = parse_csv(&render(Format::Csv, &ALL, &rows));
    assert_eq!(
        table[0],
        [
            "name",
            "path",
            "full_path",
            "size",
            "date_modified",
            "date_created"
        ]
    );
    assert_eq!(
        table[1],
        [
            "b",
            "/a",
            "/a/b",
            "3",
            "1970-01-01T00:00:00Z",
            "1970-01-01T00:00:00Z"
        ]
    );
    assert_eq!(table.len(), 2);
}

#[test]
fn csv_with_no_rows_is_just_the_header() {
    let table = parse_csv(&render(Format::Csv, &[Column::FullPath, Column::Size], &[]));
    assert_eq!(
        table,
        vec![vec!["full_path".to_string(), "size".to_string()]]
    );
}

#[test]
fn csv_quotes_commas_quotes_and_newlines() {
    let tricky = [
        "comma,name",
        "quote\"name",
        "\"leading quote",
        "new\nline",
        "carriage\rreturn",
        "crlf\r\nname",
        "both, \"and\"\n",
        "plain",
        " spaces ",
        "café ü 日本",
    ];
    let rows: Vec<Row> = tricky.iter().map(|n| row("/dir", n, 1, 0, 0)).collect();
    let out = render(Format::Csv, &[Column::Name, Column::FullPath], &rows);
    let table = parse_csv(&out);
    assert_eq!(table.len(), tricky.len() + 1, "{out:?}");
    for (i, name) in tricky.iter().enumerate() {
        assert_eq!(table[i + 1], [name.to_string(), format!("/dir/{name}")]);
    }
}

#[test]
fn csv_fields_with_special_characters_are_quoted() {
    let rows = [row("/d", "a,b", 0, 0, 0), row("/d", "q\"q", 0, 0, 0)];
    let out = render(Format::Csv, &[Column::Name], &rows);
    assert!(out.contains("\"a,b\""), "{out:?}");
    assert!(out.contains("\"q\"\"q\""), "{out:?}");
}

// ---------------------------------------------------------------- json

#[test]
fn json_is_an_array_of_objects_keyed_by_column() {
    let rows = [
        row("/Users/me", "a.txt", 1234, 1_700_000_000, 951_782_400),
        row("/", "etc", 0, 0, -1),
    ];
    let json = parse_json(&render(Format::Json, &ALL, &rows));
    let objs = objects(&json);
    assert_eq!(objs.len(), 2);
    let a = &objs[0];
    assert_eq!(a.len(), 6);
    assert_eq!(a["name"], Json::Str("a.txt".into()));
    assert_eq!(a["path"], Json::Str("/Users/me".into()));
    assert_eq!(a["full_path"], Json::Str("/Users/me/a.txt".into()));
    assert_eq!(a["size"], Json::Num(1234.0));
    assert_eq!(a["date_modified"], Json::Str("2023-11-14T22:13:20Z".into()));
    assert_eq!(a["date_created"], Json::Str("2000-02-29T00:00:00Z".into()));
    let b = &objs[1];
    assert_eq!(b["full_path"], Json::Str("/etc".into()));
    assert_eq!(b["size"], Json::Num(0.0));
    assert_eq!(b["date_created"], Json::Str("1969-12-31T23:59:59Z".into()));
}

#[test]
fn json_objects_hold_only_the_chosen_columns() {
    let rows = [row("/d", "n", 9, 0, 0)];
    let json = parse_json(&render(Format::Json, &[Column::FullPath], &rows));
    let objs = objects(&json);
    assert_eq!(objs.len(), 1);
    assert_eq!(objs[0].keys().collect::<Vec<_>>(), ["full_path"]);
    assert_eq!(objs[0]["full_path"], Json::Str("/d/n".into()));

    let json = parse_json(&render(Format::Json, &[Column::Size, Column::Name], &rows));
    let objs = objects(&json);
    assert_eq!(objs[0].len(), 2);
    assert_eq!(objs[0]["size"], Json::Num(9.0));
    assert_eq!(objs[0]["name"], Json::Str("n".into()));
}

#[test]
fn json_with_no_rows_is_an_empty_array() {
    let json = parse_json(&render(Format::Json, &[Column::FullPath], &[]));
    assert_eq!(json, Json::Arr(vec![]));
}

#[test]
fn json_large_sizes_are_exact() {
    // Above 2^53 a float would round; the writer must print the integer.
    let big: u64 = 9_007_199_254_740_993;
    let rows = [row("/d", "huge", big, 0, 0)];
    let out = render(Format::Json, &[Column::Size], &rows);
    assert!(out.contains("9007199254740993"), "{out}");
}

#[test]
fn json_escapes_quotes_backslashes_and_control_characters() {
    let mut tricky: Vec<String> = vec![
        "quote\"name".into(),
        "back\\slash".into(),
        "slash/name".into(),
        "new\nline".into(),
        "tab\tname".into(),
        "cr\rname".into(),
        "nul\u{0}name".into(),
        "bell\u{7}name".into(),
        "esc\u{1b}name".into(),
        "unit\u{1f}sep".into(),
        "del\u{7f}name".into(),
        "nel\u{85}name".into(),
        "csi\u{9b}2J".into(),
        "café ü 日本 🎉".into(),
        "line\u{2028}sep\u{2029}".into(),
    ];
    // Every C0 control character in one name, and every C1 in another.
    tricky.push((0u8..0x20).map(char::from).collect());
    tricky.push(('\u{80}'..='\u{9f}').collect());
    let rows: Vec<Row> = tricky.iter().map(|n| row("/dir", n, 0, 0, 0)).collect();
    let out = render(Format::Json, &[Column::Name, Column::FullPath], &rows);
    // Only the newlines between objects are raw control characters.
    assert!(
        !out.chars().any(|c| c.is_control() && c != '\n'),
        "raw control characters in {out:?}"
    );
    let json = parse_json(&out);
    let objs = objects(&json);
    assert_eq!(objs.len(), tricky.len());
    for (obj, name) in objs.iter().zip(&tricky) {
        assert_eq!(obj["name"], Json::Str(name.clone()));
        assert_eq!(obj["full_path"], Json::Str(format!("/dir/{name}")));
    }
}

fn json_string_of(s: &str) -> String {
    let mut out = Vec::new();
    write_json_string(&mut out, s).expect("write to a Vec");
    String::from_utf8(out).expect("UTF-8")
}

#[test]
fn json_strings_escape_c1_control_characters() {
    // DEL and every C1 control character (U+007F to U+009F) as `\u00xx`.
    for c in '\u{7f}'..='\u{9f}' {
        assert_eq!(
            json_string_of(&c.to_string()),
            format!("\"\\u{:04x}\"", c as u32),
            "U+{:04X}",
            c as u32
        );
    }
    // The characters on either side are not control characters and stay
    // raw: `~` (U+007E), a no-break space (U+00A0) and `é`.
    assert_eq!(
        json_string_of("~\u{7f}\u{80}a\u{9b}2J\u{9f}\u{a0}é"),
        "\"~\\u007f\\u0080a\\u009b2J\\u009f\u{a0}é\""
    );
}

// ------------------------------------------------------------------------
// Minimal RFC 4180 reader (accepts CRLF or LF record ends).

fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let mut records = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut chars = text.chars().peekable();
    let mut in_quotes = false;
    let mut field_started = false;
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => {
                in_quotes = true;
                field_started = true;
            }
            '"' => panic!("bare quote inside an unquoted field in {text:?}"),
            ',' => {
                record.push(std::mem::take(&mut field));
                field_started = true;
            }
            '\r' if chars.peek() == Some(&'\n') => {}
            '\r' => panic!("bare CR outside quotes in {text:?}"),
            '\n' => {
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
                field_started = false;
            }
            _ => {
                field.push(c);
                field_started = true;
            }
        }
    }
    assert!(!in_quotes, "unterminated quote in {text:?}");
    if field_started || !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    }
    records
}

// ------------------------------------------------------------------------
// Minimal JSON reader: enough to check structure and decode escapes.

#[derive(Debug, Clone, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(BTreeMap<String, Json>),
}

fn objects(json: &Json) -> Vec<BTreeMap<String, Json>> {
    match json {
        Json::Arr(items) => items
            .iter()
            .map(|i| match i {
                Json::Obj(m) => m.clone(),
                other => panic!("expected an object, got {other:?}"),
            })
            .collect(),
        other => panic!("expected an array, got {other:?}"),
    }
}

fn parse_json(text: &str) -> Json {
    let chars: Vec<char> = text.chars().collect();
    let mut pos = 0;
    let value = json_value(&chars, &mut pos);
    skip_ws(&chars, &mut pos);
    assert_eq!(pos, chars.len(), "trailing data in {text:?}");
    value
}

fn skip_ws(c: &[char], pos: &mut usize) {
    while *pos < c.len() && matches!(c[*pos], ' ' | '\n' | '\r' | '\t') {
        *pos += 1;
    }
}

fn expect(c: &[char], pos: &mut usize, want: char) {
    skip_ws(c, pos);
    assert_eq!(c.get(*pos), Some(&want), "at {pos}");
    *pos += 1;
}

fn json_value(c: &[char], pos: &mut usize) -> Json {
    skip_ws(c, pos);
    match c.get(*pos).copied() {
        Some('[') => {
            *pos += 1;
            let mut items = Vec::new();
            skip_ws(c, pos);
            if c.get(*pos) == Some(&']') {
                *pos += 1;
                return Json::Arr(items);
            }
            loop {
                items.push(json_value(c, pos));
                skip_ws(c, pos);
                match c.get(*pos) {
                    Some(',') => *pos += 1,
                    Some(']') => {
                        *pos += 1;
                        return Json::Arr(items);
                    }
                    other => panic!("bad array at {pos}: {other:?}"),
                }
            }
        }
        Some('{') => {
            *pos += 1;
            let mut map = BTreeMap::new();
            skip_ws(c, pos);
            if c.get(*pos) == Some(&'}') {
                *pos += 1;
                return Json::Obj(map);
            }
            loop {
                skip_ws(c, pos);
                let key = match json_value(c, pos) {
                    Json::Str(s) => s,
                    other => panic!("object key must be a string, got {other:?}"),
                };
                expect(c, pos, ':');
                let value = json_value(c, pos);
                assert!(map.insert(key, value).is_none(), "duplicate key");
                skip_ws(c, pos);
                match c.get(*pos) {
                    Some(',') => *pos += 1,
                    Some('}') => {
                        *pos += 1;
                        return Json::Obj(map);
                    }
                    other => panic!("bad object at {pos}: {other:?}"),
                }
            }
        }
        Some('"') => {
            *pos += 1;
            Json::Str(json_string(c, pos))
        }
        Some('n') => literal(c, pos, "null", Json::Null),
        Some('t') => literal(c, pos, "true", Json::Bool(true)),
        Some('f') => literal(c, pos, "false", Json::Bool(false)),
        Some(ch) if ch == '-' || ch.is_ascii_digit() => {
            let start = *pos;
            while *pos < c.len() && (c[*pos].is_ascii_digit() || "+-.eE".contains(c[*pos])) {
                *pos += 1;
            }
            let s: String = c[start..*pos].iter().collect();
            Json::Num(s.parse().unwrap_or_else(|_| panic!("bad number {s}")))
        }
        other => panic!("unexpected {other:?} at {pos}"),
    }
}

fn literal(c: &[char], pos: &mut usize, word: &str, value: Json) -> Json {
    for w in word.chars() {
        assert_eq!(c.get(*pos), Some(&w));
        *pos += 1;
    }
    value
}

fn hex4(c: &[char], pos: &mut usize) -> u32 {
    let s: String = c[*pos..*pos + 4].iter().collect();
    *pos += 4;
    u32::from_str_radix(&s, 16).unwrap_or_else(|_| panic!("bad \\u escape {s}"))
}

fn json_string(c: &[char], pos: &mut usize) -> String {
    let mut out = String::new();
    loop {
        let ch = *c.get(*pos).expect("unterminated string");
        *pos += 1;
        match ch {
            '"' => return out,
            '\\' => {
                let e = c[*pos];
                *pos += 1;
                match e {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => {
                        let hi = hex4(c, pos);
                        let code = if (0xD800..0xDC00).contains(&hi) {
                            assert_eq!(&c[*pos..*pos + 2], ['\\', 'u'], "lone surrogate");
                            *pos += 2;
                            let lo = hex4(c, pos);
                            assert!((0xDC00..0xE000).contains(&lo), "bad low surrogate");
                            0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                        } else {
                            hi
                        };
                        out.push(char::from_u32(code).expect("valid scalar"));
                    }
                    other => panic!("bad escape \\{other}"),
                }
            }
            ch if (ch as u32) < 0x20 => panic!("raw control character {ch:?} in string"),
            ch => out.push(ch),
        }
    }
}
