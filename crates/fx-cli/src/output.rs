//! The txt, csv and json result formatters.
//!
//! Each formatter writes plain [`Row`]s to any [`Write`], so it can be
//! tested without an index:
//!
//! - txt: one row per line; with several columns, values are tab-separated.
//!   No header. A [`Style`] can end each row with NUL instead (`-0`) and
//!   show control characters in values as `?` (for a terminal).
//! - csv: a header row of [`Column::key`]s, then one record per row, with
//!   RFC 4180 quoting (fields holding a comma, quote, CR or LF are quoted and
//!   quotes doubled) and CRLF record ends. A [`Style`] can show control
//!   characters in values as `?` (for a terminal), before quoting.
//! - json: an array of objects keyed by the chosen columns' keys; `size` is
//!   an exact integer, dates are strings, and strings escape quotes,
//!   backslashes, every control character (U+0000–U+001F and
//!   U+007F–U+009F) and U+2028/U+2029, whatever the [`Style`].
//!
//! Dates are ISO 8601 UTC (`YYYY-MM-DDTHH:MM:SSZ`). Names and paths are
//! written as UTF-8, with invalid bytes replaced by U+FFFD.

use std::borrow::{Borrow, Cow};
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;

/// The output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Format {
    /// One row per line (the default).
    #[default]
    Txt,
    /// Comma-separated values with a header row.
    Csv,
    /// A JSON array of objects.
    Json,
}

/// A result column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Column {
    /// The file name.
    Name,
    /// The containing folder.
    Path,
    /// The full path including the name (the default column).
    FullPath,
    /// The size in bytes.
    Size,
    /// The last modification time.
    DateModified,
    /// The creation time.
    DateCreated,
}

impl Column {
    /// The stable key of the column: the csv header and json object key.
    pub fn key(self) -> &'static str {
        match self {
            Column::Name => "name",
            Column::Path => "path",
            Column::FullPath => "full_path",
            Column::Size => "size",
            Column::DateModified => "date_modified",
            Column::DateCreated => "date_created",
        }
    }
}

/// One result, as plain data.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Row {
    /// The file name.
    pub name: OsString,
    /// The containing folder.
    pub path: PathBuf,
    /// The full path including the name.
    pub full_path: PathBuf,
    /// The size in bytes.
    pub size: u64,
    /// Modification time, seconds since the Unix epoch.
    pub date_modified: i64,
    /// Creation time, seconds since the Unix epoch.
    pub date_created: i64,
}

/// A column's value: text (escaped per format) or a number.
enum Value<'a> {
    Text(Cow<'a, str>),
    Number(u64),
}

impl Value<'_> {
    fn as_text(&self) -> Cow<'_, str> {
        match self {
            Value::Text(s) => Cow::Borrowed(s),
            Value::Number(n) => Cow::Owned(n.to_string()),
        }
    }
}

fn value(row: &Row, column: Column) -> Value<'_> {
    match column {
        Column::Name => Value::Text(row.name.to_string_lossy()),
        Column::Path => Value::Text(row.path.to_string_lossy()),
        Column::FullPath => Value::Text(row.full_path.to_string_lossy()),
        Column::Size => Value::Number(row.size),
        Column::DateModified => Value::Text(Cow::Owned(iso8601(row.date_modified))),
        Column::DateCreated => Value::Text(Cow::Owned(iso8601(row.date_created))),
    }
}

/// How txt and csv output is written. The default writes every value
/// exactly and ends each txt row with a newline. json ignores it: it
/// escapes every control character anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Style {
    /// End each txt row with NUL (`\0`) instead of a newline (`-0`). A
    /// file name can hold a newline but never a NUL, so the rows split
    /// exactly. csv ignores it.
    pub nul_terminated: bool,
    /// Write each control character in a txt or csv value (U+0000–U+001F
    /// and U+007F–U+009F, such as a newline, tab or escape) as `?`, as `ls`
    /// does on a terminal, so a file name can't move the cursor, change
    /// colours or fake extra rows. For a terminal only: the value is no
    /// longer the exact name.
    pub escape_controls: bool,
}

/// Writes `rows` to `out` in `format` with `columns`, in the order given.
/// An empty `columns` means the default, [`Column::FullPath`]. Values are
/// written exactly, and txt has one row per line.
pub fn write_rows<W, I, R>(
    out: &mut W,
    format: Format,
    columns: &[Column],
    rows: I,
) -> io::Result<()>
where
    W: Write + ?Sized,
    I: IntoIterator<Item = R>,
    R: Borrow<Row>,
{
    write_rows_with(out, format, columns, rows, Style::default())
}

/// [`write_rows`] with txt and csv written in `style`.
pub fn write_rows_with<W, I, R>(
    out: &mut W,
    format: Format,
    columns: &[Column],
    rows: I,
    style: Style,
) -> io::Result<()>
where
    W: Write + ?Sized,
    I: IntoIterator<Item = R>,
    R: Borrow<Row>,
{
    let columns = if columns.is_empty() {
        &[Column::FullPath][..]
    } else {
        columns
    };
    match format {
        Format::Txt => write_txt(out, columns, rows, style),
        Format::Csv => write_csv(out, columns, rows, style),
        Format::Json => write_json(out, columns, rows),
    }
}

fn write_txt<W, I, R>(out: &mut W, columns: &[Column], rows: I, style: Style) -> io::Result<()>
where
    W: Write + ?Sized,
    I: IntoIterator<Item = R>,
    R: Borrow<Row>,
{
    let end: &[u8] = if style.nul_terminated { b"\0" } else { b"\n" };
    for row in rows {
        let row = row.borrow();
        for (i, &c) in columns.iter().enumerate() {
            if i > 0 {
                out.write_all(b"\t")?;
            }
            out.write_all(shown(&value(row, c).as_text(), style).as_bytes())?;
        }
        out.write_all(end)?;
    }
    Ok(())
}

/// `s` as `style` writes it: with each control character as `?` when
/// [`Style::escape_controls`] is set, otherwise unchanged.
fn shown(s: &str, style: Style) -> Cow<'_, str> {
    if style.escape_controls {
        controls_as_question_marks(s)
    } else {
        Cow::Borrowed(s)
    }
}

/// `s` with each control character ([`char::is_control`]) replaced by `?`.
fn controls_as_question_marks(s: &str) -> Cow<'_, str> {
    if s.chars().any(char::is_control) {
        Cow::Owned(
            s.chars()
                .map(|c| if c.is_control() { '?' } else { c })
                .collect(),
        )
    } else {
        Cow::Borrowed(s)
    }
}

fn write_csv<W, I, R>(out: &mut W, columns: &[Column], rows: I, style: Style) -> io::Result<()>
where
    W: Write + ?Sized,
    I: IntoIterator<Item = R>,
    R: Borrow<Row>,
{
    for (i, c) in columns.iter().enumerate() {
        if i > 0 {
            out.write_all(b",")?;
        }
        write_csv_field(out, c.key())?;
    }
    out.write_all(b"\r\n")?;
    for row in rows {
        let row = row.borrow();
        for (i, &c) in columns.iter().enumerate() {
            if i > 0 {
                out.write_all(b",")?;
            }
            // Quoting follows the shown text: a newline shown as `?` no
            // longer needs quotes.
            write_csv_field(out, &shown(&value(row, c).as_text(), style))?;
        }
        out.write_all(b"\r\n")?;
    }
    Ok(())
}

/// Writes one csv field, quoted (with quotes doubled) when it holds a
/// comma, quote, CR or LF.
pub fn write_csv_field<W: Write + ?Sized>(out: &mut W, field: &str) -> io::Result<()> {
    if !field.contains([',', '"', '\r', '\n']) {
        return out.write_all(field.as_bytes());
    }
    out.write_all(b"\"")?;
    let mut rest = field;
    while let Some(i) = rest.find('"') {
        out.write_all(&rest.as_bytes()[..=i])?;
        out.write_all(b"\"")?;
        rest = &rest[i + 1..];
    }
    out.write_all(rest.as_bytes())?;
    out.write_all(b"\"")
}

fn write_json<W, I, R>(out: &mut W, columns: &[Column], rows: I) -> io::Result<()>
where
    W: Write + ?Sized,
    I: IntoIterator<Item = R>,
    R: Borrow<Row>,
{
    out.write_all(b"[")?;
    let mut first = true;
    for row in rows {
        let row = row.borrow();
        out.write_all(if first { b"\n  {" } else { b",\n  {" })?;
        first = false;
        for (i, &c) in columns.iter().enumerate() {
            if i > 0 {
                out.write_all(b", ")?;
            }
            write_json_string(out, c.key())?;
            out.write_all(b": ")?;
            match value(row, c) {
                Value::Text(s) => write_json_string(out, &s)?,
                Value::Number(n) => write!(out, "{n}")?,
            }
        }
        out.write_all(b"}")?;
    }
    out.write_all(if first { b"]\n" } else { b"\n]\n" })
}

/// Writes `s` as a JSON string literal: quotes, backslashes and control
/// characters (U+0000–U+001F and U+007F–U+009F) are escaped, as are U+2028
/// and U+2029; everything else is raw UTF-8. JSON itself needs only the
/// control characters below U+0020 escaped; U+007F–U+009F are escaped too,
/// so that no control character in a name reaches a terminal raw.
pub fn write_json_string<W: Write + ?Sized>(out: &mut W, s: &str) -> io::Result<()> {
    out.write_all(b"\"")?;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        let escape: Option<Cow<'static, str>> = match c {
            '"' => Some("\\\"".into()),
            '\\' => Some("\\\\".into()),
            '\n' => Some("\\n".into()),
            '\r' => Some("\\r".into()),
            '\t' => Some("\\t".into()),
            '\u{8}' => Some("\\b".into()),
            '\u{c}' => Some("\\f".into()),
            '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}' | '\u{2028}' | '\u{2029}' => {
                Some(format!("\\u{:04x}", c as u32).into())
            }
            _ => None,
        };
        if let Some(e) = escape {
            out.write_all(&s.as_bytes()[start..i])?;
            out.write_all(e.as_bytes())?;
            start = i + c.len_utf8();
        }
    }
    out.write_all(&s.as_bytes()[start..])?;
    out.write_all(b"\"")
}

/// Formats `secs` since the Unix epoch as ISO 8601 UTC,
/// `YYYY-MM-DDTHH:MM:SSZ`. Works for times before 1970 too.
pub fn iso8601(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// The proleptic Gregorian date of `days` since 1970-01-01, as
/// (year, month 1–12, day 1–31). The well-known era-based conversion: shift
/// the epoch to 0000-03-01 so leap days fall at the end of each year, then
/// split into 400-year eras.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // day of era, 0..=146096
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // 0..=399
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // 0..=365, from March 1
    let mp = (5 * doy + 2) / 153; // 0..=11, March = 0
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}
