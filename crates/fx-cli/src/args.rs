//! The argument parser, with the option names of the `es` command-line
//! tool where it has them.
//!
//! [`parse`] takes the arguments after the program name. Every option is
//! accepted with one or two leading dashes (`-i` or `--i`). Arguments that
//! are not options are search words; they are joined with single spaces
//! into one fx-query string, so quoting and operators (`"a b"`, `a|b`, `!c`,
//! `ext:rs;md`) pass through unchanged. Options may be interleaved with the
//! words. A lone `--` ends the options: everything after it is a search
//! word, even if it starts with a dash. A lone `-` is a search word.
//!
//! There is no option that deletes, renames, moves or trashes a file: the
//! MVP is read-only.

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

use crate::exit::ExitCode;
use crate::output::{Column, Format};
use crate::store_path::DEFAULT_ROOT;

/// Usage text for the binary, naming the command ([`crate::PROGRAM`]).
pub fn usage() -> String {
    format!(
        "usage: {} [options] [search words...]\n\n{USAGE_BODY}",
        crate::PROGRAM
    )
}

/// The usage text after its first line.
const USAGE_BODY: &str = "\
Searches the index the app saved, read-only. Search words are joined with
spaces into one query. Every option also works with two dashes.

Search:
  -i, -case                 match case
  -w, -ww, -whole-word      match whole words
  -p, -match-path           match against the full path
  -a, -diacritics           match diacritics
  -r, -regex <pattern>      search with a regular expression

Results:
  -n, -max-results <num>    print at most <num> results
  -o, -offset <num>         skip the first <num> results
  -s                        sort by path
  -sort <field>             sort by name, path, size, date-modified or date-created
  -sort-ascending           sort ascending (the default)
  -sort-descending          sort descending
  -get-result-count         print only the number of results
  -no-result-error          exit with code 9 when nothing is found

Output:
  -txt                      one result per line (the default)
  -0                        end each txt result with NUL, not a newline
  -csv                      comma-separated values with a header row
  -json                     a JSON array of objects
  -name                     show the name column
  -path-column              show the containing folder column
  -full-path-and-name       show the full path column (the default)
  -size                     show the size column
  -dm, -date-modified       show the date modified column
  -dc, -date-created        show the date created column
  -export-txt <file>        write txt to <file> instead of standard output
  -export-csv <file>        write csv to <file> instead of standard output
  -export-json <file>       write json to <file> instead of standard output

Index:
  -store <file>             read this store file instead of the app's
  -root <dir>               use the app's store for this root (default /)

Other:
  -h, -help                 show this help
  -version                  show the version
  -licenses                 show the third-party licences";

/// The field results are sorted by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SortField {
    /// By name (the default).
    #[default]
    Name,
    /// By full path.
    Path,
    /// By size.
    Size,
    /// By modification time.
    DateModified,
    /// By creation time.
    DateCreated,
}

/// The sort direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SortOrder {
    /// Smallest first (the default).
    #[default]
    Ascending,
    /// Largest first.
    Descending,
}

/// Write the results to a file instead of standard output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    /// The format written to the file.
    pub format: Format,
    /// The file to create.
    pub path: PathBuf,
}

/// The parsed command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliArgs {
    /// The fx-query search string (the search words joined with spaces).
    pub query: String,
    /// Match case (`-i`).
    pub match_case: bool,
    /// Match whole words (`-w`).
    pub whole_word: bool,
    /// Match against the full path (`-p`).
    pub match_path: bool,
    /// Match diacritics (`-a`).
    pub diacritics: bool,
    /// The query is a regular expression (`-r`).
    pub regex: bool,
    /// Print at most this many results (`-n`).
    pub max_results: Option<usize>,
    /// Skip this many results (`-o`).
    pub offset: usize,
    /// The sort field.
    pub sort: SortField,
    /// The sort direction.
    pub order: SortOrder,
    /// The format written to standard output.
    pub format: Format,
    /// The columns, in output order. Defaults to the full path only.
    pub columns: Vec<Column>,
    /// Print only the result count (`-get-result-count`).
    pub get_result_count: bool,
    /// Exit with [`ExitCode::NoResults`] when nothing matches.
    pub no_result_error: bool,
    /// Write to a file instead of standard output.
    pub export: Option<Export>,
    /// An explicit store file (`-store`).
    pub store: Option<PathBuf>,
    /// The root whose app store is read when `store` is `None` (`-root`).
    pub root: PathBuf,
    /// Show the help (`-h`).
    pub help: bool,
    /// Show the version (`-version`).
    pub version: bool,
    /// Show the third-party notices (`-licenses`).
    pub licenses: bool,
    /// End each txt result with NUL instead of a newline (`-0`). Txt
    /// output only.
    pub nul_terminated: bool,
}

impl Default for CliArgs {
    fn default() -> Self {
        CliArgs {
            query: String::new(),
            match_case: false,
            whole_word: false,
            match_path: false,
            diacritics: false,
            regex: false,
            max_results: None,
            offset: 0,
            sort: SortField::default(),
            order: SortOrder::default(),
            format: Format::default(),
            columns: vec![Column::FullPath],
            get_result_count: false,
            no_result_error: false,
            export: None,
            store: None,
            root: PathBuf::from(DEFAULT_ROOT),
            help: false,
            version: false,
            licenses: false,
            nul_terminated: false,
        }
    }
}

/// A bad command line, with the exit code it maps to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgError {
    code: ExitCode,
    message: String,
}

impl ArgError {
    fn bad(message: impl Into<String>) -> ArgError {
        ArgError {
            code: ExitCode::BadArgument,
            message: message.into(),
        }
    }

    fn unknown(option: &str) -> ArgError {
        ArgError {
            code: ExitCode::UnknownOption,
            message: format!("unknown option {option:?}"),
        }
    }

    /// The exit code: [`ExitCode::BadArgument`] for a missing or invalid
    /// option argument, [`ExitCode::UnknownOption`] for an unknown option.
    pub fn exit_code(&self) -> ExitCode {
        self.code
    }
}

impl fmt::Display for ArgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ArgError {}

/// Parses the arguments after the program name. A repeated option keeps its
/// last value; column options add columns in the order given. `-0` with csv
/// or json output (an export's format, else `-csv` or `-json`) is a bad
/// argument, whatever the order of the options.
pub fn parse(args: Vec<OsString>) -> Result<CliArgs, ArgError> {
    let mut out = CliArgs::default();
    let mut words: Vec<String> = Vec::new();
    let mut columns: Vec<Column> = Vec::new();
    let mut it = args.into_iter();
    let mut options_done = false;
    // `-0` as the user spelt it, for the message if the format rejects it.
    let mut nul_option: Option<String> = None;

    while let Some(arg) = it.next() {
        if options_done {
            words.push(arg.to_string_lossy().into_owned());
            continue;
        }
        let text = arg.to_string_lossy();
        if text == "--" {
            options_done = true;
            continue;
        }
        let name = match text.strip_prefix("--").or_else(|| text.strip_prefix('-')) {
            Some(name) if !name.is_empty() => name.to_owned(),
            _ => {
                words.push(text.into_owned());
                continue;
            }
        };
        let shown = text.into_owned();
        let mut column = |c: Column| {
            if !columns.contains(&c) {
                columns.push(c);
            }
        };
        match name.as_str() {
            "i" | "case" => out.match_case = true,
            "w" | "ww" | "whole-word" => out.whole_word = true,
            "p" | "match-path" => out.match_path = true,
            "a" | "diacritics" => out.diacritics = true,
            "r" | "regex" => {
                let pattern = value(&shown, it.next(), "a pattern")?;
                out.regex = true;
                words.push(pattern.to_string_lossy().into_owned());
            }
            "n" | "max-results" => out.max_results = Some(number(&shown, it.next())?),
            "o" | "offset" => out.offset = number(&shown, it.next())?,
            "s" => out.sort = SortField::Path,
            "sort" => out.sort = sort_field(&shown, it.next())?,
            "sort-ascending" => out.order = SortOrder::Ascending,
            "sort-descending" => out.order = SortOrder::Descending,
            "0" => {
                out.nul_terminated = true;
                nul_option = Some(shown);
            }
            "txt" => out.format = Format::Txt,
            "csv" => out.format = Format::Csv,
            "json" => out.format = Format::Json,
            "name" => column(Column::Name),
            "path-column" => column(Column::Path),
            "full-path-and-name" => column(Column::FullPath),
            "size" => column(Column::Size),
            "dm" | "date-modified" => column(Column::DateModified),
            "dc" | "date-created" => column(Column::DateCreated),
            "get-result-count" => out.get_result_count = true,
            "no-result-error" => out.no_result_error = true,
            "export-txt" | "export-csv" | "export-json" => {
                let format = match name.as_str() {
                    "export-txt" => Format::Txt,
                    "export-csv" => Format::Csv,
                    _ => Format::Json,
                };
                let path = PathBuf::from(value(&shown, it.next(), "a file")?);
                out.export = Some(Export { format, path });
            }
            "store" => out.store = Some(PathBuf::from(value(&shown, it.next(), "a file")?)),
            "root" => out.root = PathBuf::from(value(&shown, it.next(), "a folder")?),
            "h" | "help" => out.help = true,
            "version" => out.version = true,
            "licenses" => out.licenses = true,
            _ => return Err(ArgError::unknown(&shown)),
        }
    }

    out.query = words.join(" ");
    if !columns.is_empty() {
        out.columns = columns;
    }
    if let Some(option) = nul_option {
        // The export replaces standard output, so its format is the output's.
        let format = out.export.as_ref().map_or(out.format, |e| e.format);
        let name = match format {
            Format::Txt => None,
            Format::Csv => Some("csv"),
            Format::Json => Some("json"),
        };
        if let Some(name) = name {
            return Err(ArgError::bad(format!(
                "{option} ends txt results with NUL and can't be used with {name} output"
            )));
        }
    }
    Ok(out)
}

/// The non-empty argument of `option`, or a [`ExitCode::BadArgument`] error
/// naming what was expected.
fn value(option: &str, value: Option<OsString>, what: &str) -> Result<OsString, ArgError> {
    match value {
        Some(v) if !v.is_empty() => Ok(v),
        Some(_) => Err(ArgError::bad(format!(
            "{option} needs {what}, got an empty argument"
        ))),
        None => Err(ArgError::bad(format!("{option} needs {what}"))),
    }
}

/// The argument of `option` as a non-negative decimal integer.
fn number(option: &str, arg: Option<OsString>) -> Result<usize, ArgError> {
    let arg = value(option, arg, "a number")?;
    let text = arg.to_string_lossy();
    if !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ArgError::bad(format!(
            "{option} needs a non-negative whole number, got {text:?}"
        )));
    }
    text.parse()
        .map_err(|_| ArgError::bad(format!("{option} value {text:?} is too large")))
}

/// The argument of `option` as a sort field name.
fn sort_field(option: &str, arg: Option<OsString>) -> Result<SortField, ArgError> {
    let arg = value(option, arg, "a sort field")?;
    match arg.to_string_lossy().as_ref() {
        "name" => Ok(SortField::Name),
        "path" => Ok(SortField::Path),
        "size" => Ok(SortField::Size),
        "date-modified" => Ok(SortField::DateModified),
        "date-created" => Ok(SortField::DateCreated),
        other => Err(ArgError::bad(format!(
            "{option} needs one of name, path, size, date-modified or date-created, got {other:?}"
        ))),
    }
}
