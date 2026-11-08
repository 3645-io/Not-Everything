//! Command-line interface (CLI) over the index the app saves.
//!
//! The `not-everything` binary opens the engine's checkpoint store
//! read-only with [`fx_engine::StoreReader`], runs one fx-query search with
//! the toggles, sort and paging given on the command line, and prints the
//! results as txt (one full path per line, the default), csv or json, to
//! standard output or an export file. On a terminal, txt and csv show
//! control characters in names as `?`; a pipe or a file gets the exact
//! names. json escapes every control character wherever it is written. It
//! exits with the documented codes of [`fx_cli::exit::ExitCode`];
//! `docs/cli.md` describes the options.
//!
//! The store is never written, and there is no option that deletes,
//! renames, moves or trashes a file: the MVP is read-only.

#![forbid(unsafe_code)]

use std::fs::File;
use std::io::{self, BufWriter, IsTerminal, Write};
use std::path::{Path, PathBuf};

use fx_channel::{CURRENT_NUMBER, Channel, version_label};
use fx_cli::PROGRAM;
use fx_cli::args::{self, CliArgs, SortField, SortOrder, usage};
use fx_cli::exit::ExitCode;
use fx_cli::output::{Row, Style, write_rows_with};
use fx_cli::store_path::{StoreScope, home_dir, store_candidates};
use fx_engine::{ReadError, ReadOptions, StoreReader};
use fx_sort::{Direction, SortKey};

/// The third-party notices (`THIRD-PARTY-NOTICES.md`, written by
/// `scripts/third-party-notices.sh`), printed by `-licenses`.
const THIRD_PARTY_NOTICES: &str = include_str!("../../../THIRD-PARTY-NOTICES.md");

fn main() -> std::process::ExitCode {
    run().into()
}

/// Prints `message` on standard error, prefixed with the program name.
fn complain(message: impl std::fmt::Display) {
    eprintln!("{PROGRAM}: {message}");
}

/// Runs the command line and returns the exit code.
fn run() -> ExitCode {
    let args = match args::parse(std::env::args_os().skip(1).collect()) {
        Ok(args) => args,
        Err(e) => {
            complain(&e);
            complain(format_args!("run {PROGRAM} -h for help"));
            return e.exit_code();
        }
    };
    if args.help {
        return print_stdout(|out| writeln!(out, "{}", usage()));
    }
    if args.licenses {
        // The licences of the open-source crates in the command ask for
        // their notices to travel with every copy.
        return print_stdout(|out| out.write_all(THIRD_PARTY_NOTICES.as_bytes()));
    }
    if args.version {
        // The release's version, or a Preview build's label (0.2.0-RC-1).
        let label = version_label(env!("CARGO_PKG_VERSION"), Channel::CURRENT, CURRENT_NUMBER);
        return print_stdout(|out| writeln!(out, "{PROGRAM} {label}"));
    }

    // The app keys its store by the canonical root, so resolve trailing
    // slashes, `..`, relative paths and symlinks the same way. A root that
    // can't be resolved (it doesn't exist) is used as given.
    let root = std::fs::canonicalize(&args.root).unwrap_or_else(|_| args.root.clone());
    let reader = match open_store(&args, &root) {
        Ok(reader) => reader,
        Err(code) => return code,
    };

    let opts = ReadOptions {
        match_case: args.match_case,
        whole_word: args.whole_word,
        match_path: args.match_path,
        match_diacritics: args.diacritics,
        regex: args.regex,
    };
    let (sort, direction) = sort_of(&args);
    let query = search_text(&args);
    let limit = args.max_results.unwrap_or(usize::MAX);
    // An invalid regular expression (the `-r` pattern or a `regex:` term)
    // is a search error, so it exits 7 here.
    let mut page = match reader.search(&query, &opts, sort, direction, args.offset, limit) {
        Ok(page) => page,
        Err(e) => {
            complain(&e);
            return ExitCode::QueryFailed;
        }
    };
    for d in &page.diagnostics {
        complain(format_args!("warning: {d}"));
    }
    // A `count:N` in the query caps the total result count: `-o` and `-n`
    // page within the first N results.
    let total = match page.count {
        Some(cap) => {
            page.ids.truncate(cap.saturating_sub(args.offset));
            page.total.min(cap)
        }
        None => page.total,
    };

    let done = if args.no_result_error && total == 0 {
        ExitCode::NoResults
    } else {
        ExitCode::Success
    };
    if args.get_result_count {
        let code = print_stdout(|out| writeln!(out, "{total}"));
        return if code == ExitCode::Success {
            done
        } else {
            code
        };
    }
    if done == ExitCode::NoResults {
        return done;
    }

    let root_parent = root_parent(&root);
    let rows = page
        .ids
        .iter()
        .filter_map(|&id| row(&reader, id, root_parent.as_deref()));

    match &args.export {
        Some(export) => {
            let file = match File::create(&export.path) {
                Ok(file) => file,
                Err(e) => {
                    complain(format_args!(
                        "cannot create the export file {}: {e}",
                        export.path.display()
                    ));
                    return ExitCode::ExportFailed;
                }
            };
            let mut out = BufWriter::new(file);
            // A file gets the exact names, whatever standard output is.
            let style = Style {
                nul_terminated: args.nul_terminated,
                escape_controls: false,
            };
            let written = write_rows_with(&mut out, export.format, &args.columns, rows, style)
                .and_then(|()| out.flush());
            if let Err(e) = written {
                complain(format_args!(
                    "cannot write the export file {}: {e}",
                    export.path.display()
                ));
                return ExitCode::ExportFailed;
            }
            done
        }
        None => {
            // A control character in a name (an escape sequence, a newline)
            // would act on a terminal, so in txt and csv a terminal sees `?`
            // in its place; a pipe or a file gets the exact name. json
            // escapes it either way.
            let style = Style {
                nul_terminated: args.nul_terminated,
                escape_controls: io::stdout().is_terminal(),
            };
            let code =
                print_stdout(|out| write_rows_with(out, args.format, &args.columns, rows, style));
            if code == ExitCode::Success {
                done
            } else {
                code
            }
        }
    }
}

/// Opens the store to read: `-store` if given; otherwise the app's
/// full-scope store for the canonical `root` under `$HOME`, or, when that
/// file does not exist, its limited-scope store (with a note on stderr),
/// both in the app's data folder ([`store_candidates`]). A store that
/// exists but is rejected is never skipped for another one.
///
/// Errors (already reported on stderr): [`ExitCode::NoIndex`] when there is
/// no store, [`ExitCode::QueryFailed`] when the store is rejected.
fn open_store(args: &CliArgs, root: &Path) -> Result<StoreReader, ExitCode> {
    let rejected = |e: ReadError| {
        complain(&e);
        ExitCode::QueryFailed
    };
    let missing = |e: ReadError| {
        complain(format_args!(
            "{e}; open the app to index {} first, or pass -store <file>",
            root.display()
        ));
        ExitCode::NoIndex
    };
    if let Some(store) = &args.store {
        return StoreReader::open(store).map_err(|e| match e {
            e @ ReadError::NotFound { .. } => missing(e),
            e => rejected(e),
        });
    }
    let Some(home) = home_dir() else {
        complain("no index store: $HOME is not set; pass -store <file>");
        return Err(ExitCode::NoIndex);
    };
    let mut full_missing = None;
    for (scope, path) in store_candidates(&home, root) {
        match StoreReader::open(&path) {
            Ok(reader) => {
                if scope == StoreScope::Limited {
                    complain(format_args!(
                        "note: reading the limited store {} (indexed without Full Disk \
                         Access), so protected folders are left out",
                        path.display()
                    ));
                }
                return Ok(reader);
            }
            Err(e @ ReadError::NotFound { .. }) => {
                full_missing.get_or_insert(e);
            }
            Err(e) => return Err(rejected(e)),
        }
    }
    Err(missing(
        full_missing.expect("every lookup ran and found no store"),
    ))
}

/// The fx-query text to run. With `-r` the whole search is one regular
/// expression, made one term by [`fx_query::whole_regex_text`] (as the
/// app's regular-expression option does): wrapped in double quotes so that
/// `|`, `<`, `>`, `!` and spaces stay part of the pattern instead of being
/// query operators, with a `"` in the pattern passed as the regex escape
/// `\x22`.
fn search_text(args: &CliArgs) -> String {
    if args.regex {
        fx_query::whole_regex_text(&args.query)
    } else {
        args.query.clone()
    }
}

/// The engine sort key and direction for the parsed sort options.
fn sort_of(args: &CliArgs) -> (SortKey, Direction) {
    let key = match args.sort {
        SortField::Name => SortKey::Name,
        SortField::Path => SortKey::Path,
        SortField::Size => SortKey::Size,
        SortField::DateModified => SortKey::Modified,
        SortField::DateCreated => SortKey::Created,
    };
    let direction = match args.order {
        SortOrder::Ascending => Direction::Ascending,
        SortOrder::Descending => Direction::Descending,
    };
    (key, direction)
}

/// The folder holding the indexed root (`/Users/me` for `/Users/me/proj`),
/// prefixed to stored paths that are relative to it. `None` for `/`.
fn root_parent(root: &Path) -> Option<String> {
    root.parent().map(|p| p.to_string_lossy().into_owned())
}

/// The output row for entry `id`, or `None` if it is not a live entry.
fn row(reader: &StoreReader, id: u32, root_parent: Option<&str>) -> Option<Row> {
    let full_path = PathBuf::from(reader.full_path_under(id, root_parent)?);
    let name = reader.name(id)?;
    let path = full_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    Some(Row {
        name: name.into(),
        path,
        full_path,
        size: reader.size(id).unwrap_or(0),
        date_modified: reader.modified(id).unwrap_or(0),
        date_created: reader.created(id).unwrap_or(0),
    })
}

/// Writes to standard output through a buffer. A closed pipe (such as
/// `not-everything foo | head`) is not an error; any other write failure is
/// reported and gives [`ExitCode::ExportFailed`].
fn print_stdout(write: impl FnOnce(&mut dyn Write) -> io::Result<()>) -> ExitCode {
    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    match write(&mut out).and_then(|()| out.flush()) {
        Ok(()) => ExitCode::Success,
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => ExitCode::Success,
        Err(e) => {
            complain(format_args!("cannot write the results: {e}"));
            ExitCode::ExportFailed
        }
    }
}
