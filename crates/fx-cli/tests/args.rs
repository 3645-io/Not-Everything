//! The argument parser and the exit codes.
//!
//! API these tests expect from the `fx-cli` library crate:
//!
//! - `fx_cli::args::parse(args: Vec<OsString>) -> Result<CliArgs, ArgError>`
//!   parses the arguments *after* the program name.
//! - `CliArgs` has public fields: `query: String`, `match_case`,
//!   `whole_word`, `match_path`, `diacritics`, `regex` (all `bool`),
//!   `max_results: Option<usize>`, `offset: usize`, `sort: SortField`,
//!   `order: SortOrder`, `format: Format`, `columns: Vec<Column>`,
//!   `get_result_count`, `no_result_error` (`bool`), `export: Option<Export>`,
//!   `store: Option<PathBuf>`, `root: PathBuf`, `help`, `version`,
//!   `licenses` and `nul_terminated` (`bool`).
//! - `-0` (`nul_terminated`) works only with txt output: combined with csv
//!   or json output (`-csv`, `-json`, `-export-csv`, `-export-json`) it is
//!   a bad argument. An export's format is the one that counts.
//! - `SortField { Name, Path, Size, DateModified, DateCreated }` (default
//!   `Name`), `SortOrder { Ascending, Descending }` (default `Ascending`),
//!   `Export { format: Format, path: PathBuf }`, all in `fx_cli::args`.
//! - `Format { Txt, Csv, Json }` and `Column { Name, Path, FullPath, Size,
//!   DateModified, DateCreated }` live in `fx_cli::output`.
//! - `ArgError::exit_code() -> fx_cli::exit::ExitCode`, and `ArgError`
//!   implements `Display` with a non-empty message.
//! - `fx_cli::exit::ExitCode::code(self) -> u8`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use fx_cli::args::{ArgError, CliArgs, SortField, SortOrder, parse};
use fx_cli::exit::ExitCode;
use fx_cli::output::{Column, Format};

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

fn ok(args: &[&str]) -> CliArgs {
    match parse(os(args)) {
        Ok(a) => a,
        Err(e) => panic!("{args:?} should parse, got error: {e}"),
    }
}

fn err(args: &[&str]) -> ArgError {
    match parse(os(args)) {
        Ok(_) => panic!("{args:?} should be rejected"),
        Err(e) => e,
    }
}

fn err_code(args: &[&str]) -> ExitCode {
    err(args).exit_code()
}

// ---------------------------------------------------------------- exit codes

#[test]
fn exit_codes_have_the_documented_values() {
    assert_eq!(ExitCode::Success.code(), 0);
    assert_eq!(ExitCode::OutOfMemory.code(), 3);
    assert_eq!(ExitCode::BadArgument.code(), 4);
    assert_eq!(ExitCode::ExportFailed.code(), 5);
    assert_eq!(ExitCode::UnknownOption.code(), 6);
    assert_eq!(ExitCode::QueryFailed.code(), 7);
    assert_eq!(ExitCode::NoIndex.code(), 8);
    assert_eq!(ExitCode::NoResults.code(), 9);
}

#[test]
fn exit_codes_are_distinct_and_never_use_the_reserved_1_and_2() {
    let all = [
        ExitCode::Success,
        ExitCode::OutOfMemory,
        ExitCode::BadArgument,
        ExitCode::ExportFailed,
        ExitCode::UnknownOption,
        ExitCode::QueryFailed,
        ExitCode::NoIndex,
        ExitCode::NoResults,
    ];
    let mut codes: Vec<u8> = all.iter().map(|c| c.code()).collect();
    assert!(!codes.contains(&1) && !codes.contains(&2), "{codes:?}");
    assert!(codes.iter().all(|&c| c <= 9), "{codes:?}");
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), all.len());
}

// ------------------------------------------------------------------ defaults

#[test]
fn no_arguments_gives_the_defaults() {
    let a = ok(&[]);
    assert_eq!(a.query, "");
    assert!(!a.match_case && !a.whole_word && !a.match_path && !a.diacritics && !a.regex);
    assert_eq!(a.max_results, None);
    assert_eq!(a.offset, 0);
    assert_eq!(a.sort, SortField::Name);
    assert_eq!(a.order, SortOrder::Ascending);
    assert_eq!(a.format, Format::Txt);
    assert_eq!(a.columns, vec![Column::FullPath]);
    assert!(!a.get_result_count);
    assert!(!a.no_result_error);
    assert!(a.export.is_none());
    assert_eq!(a.store, None);
    assert_eq!(a.root, Path::new("/"));
    assert!(!a.help);
    assert!(!a.version);
    assert!(!a.licenses);
    assert!(!a.nul_terminated);
}

// -------------------------------------------------------------- search terms

#[test]
fn search_words_are_joined_with_single_spaces() {
    assert_eq!(ok(&["foo"]).query, "foo");
    assert_eq!(ok(&["foo", "bar", "baz"]).query, "foo bar baz");
}

#[test]
fn quoting_and_operators_pass_through_unchanged() {
    assert_eq!(
        ok(&["\"foo bar\"", "a|b", "!c", "ext:rs;md", "<x", "y>"]).query,
        "\"foo bar\" a|b !c ext:rs;md <x y>"
    );
    // One argument holding a space (a shell-quoted phrase) stays as given.
    assert_eq!(ok(&["foo bar"]).query, "foo bar");
}

#[test]
fn options_may_be_interleaved_with_search_words() {
    let a = ok(&["foo", "-i", "bar", "-n", "5", "baz"]);
    assert_eq!(a.query, "foo bar baz");
    assert!(a.match_case);
    assert_eq!(a.max_results, Some(5));
}

// ------------------------------------------------------------------- toggles

#[test]
fn toggle_options_and_their_aliases() {
    for opt in ["-i", "-case", "--i", "--case"] {
        assert!(ok(&[opt, "x"]).match_case, "{opt}");
    }
    for opt in ["-w", "-ww", "-whole-word", "--w", "--ww", "--whole-word"] {
        assert!(ok(&[opt, "x"]).whole_word, "{opt}");
    }
    for opt in ["-p", "-match-path", "--p", "--match-path"] {
        assert!(ok(&[opt, "x"]).match_path, "{opt}");
    }
    for opt in ["-a", "-diacritics", "--a", "--diacritics"] {
        assert!(ok(&[opt, "x"]).diacritics, "{opt}");
    }
}

#[test]
fn each_toggle_sets_only_its_own_flag() {
    let a = ok(&["-i", "x"]);
    assert!(a.match_case && !a.whole_word && !a.match_path && !a.diacritics && !a.regex);
    let a = ok(&["-p", "x"]);
    assert!(!a.match_case && !a.whole_word && a.match_path && !a.diacritics && !a.regex);
}

#[test]
fn regex_option_takes_the_pattern_as_the_search() {
    for opt in ["-r", "-regex", "--r", "--regex"] {
        let a = ok(&[opt, "^a.*b$"]);
        assert!(a.regex, "{opt}");
        assert_eq!(a.query, "^a.*b$", "{opt}");
    }
}

#[test]
fn regex_option_without_a_pattern_is_a_bad_argument() {
    assert_eq!(err_code(&["-r"]), ExitCode::BadArgument);
    assert_eq!(err_code(&["-regex"]), ExitCode::BadArgument);
}

// -------------------------------------------------------------------- paging

#[test]
fn paging_options() {
    for opt in ["-n", "-max-results", "--n", "--max-results"] {
        assert_eq!(ok(&[opt, "25"]).max_results, Some(25), "{opt}");
    }
    for opt in ["-o", "-offset", "--o", "--offset"] {
        assert_eq!(ok(&[opt, "7"]).offset, 7, "{opt}");
    }
    let a = ok(&["-n", "0", "-o", "0"]);
    assert_eq!(a.max_results, Some(0));
    assert_eq!(a.offset, 0);
}

#[test]
fn paging_value_missing_or_invalid_is_a_bad_argument() {
    for args in [
        &["-n"][..],
        &["-max-results"],
        &["-o"],
        &["-offset"],
        &["-n", "abc"],
        &["-n", "-5"],
        &["-n", "1.5"],
        &["-n", ""],
        &["-o", "x"],
        &["-o", "99999999999999999999999999999"],
    ] {
        assert_eq!(err_code(args), ExitCode::BadArgument, "{args:?}");
    }
}

// ---------------------------------------------------------------------- sort

#[test]
fn short_s_sorts_by_path() {
    assert_eq!(ok(&["-s"]).sort, SortField::Path);
    assert_eq!(ok(&["--s"]).sort, SortField::Path);
}

#[test]
fn sort_option_accepts_every_field_name() {
    for (value, field) in [
        ("name", SortField::Name),
        ("path", SortField::Path),
        ("size", SortField::Size),
        ("date-modified", SortField::DateModified),
        ("date-created", SortField::DateCreated),
    ] {
        assert_eq!(ok(&["-sort", value]).sort, field, "{value}");
        assert_eq!(ok(&["--sort", value]).sort, field, "{value}");
    }
}

#[test]
fn sort_direction_options() {
    assert_eq!(ok(&["-sort-descending"]).order, SortOrder::Descending);
    assert_eq!(ok(&["--sort-descending"]).order, SortOrder::Descending);
    assert_eq!(
        ok(&["-sort-descending", "-sort-ascending"]).order,
        SortOrder::Ascending
    );
    let a = ok(&["-sort", "size", "-sort-descending", "foo"]);
    assert_eq!(a.sort, SortField::Size);
    assert_eq!(a.order, SortOrder::Descending);
    assert_eq!(a.query, "foo");
}

#[test]
fn sort_value_missing_or_unknown_is_a_bad_argument() {
    assert_eq!(err_code(&["-sort"]), ExitCode::BadArgument);
    assert_eq!(err_code(&["-sort", "colour"]), ExitCode::BadArgument);
}

// -------------------------------------------------------- format and columns

#[test]
fn format_options() {
    assert_eq!(ok(&["-txt"]).format, Format::Txt);
    assert_eq!(ok(&["-csv"]).format, Format::Csv);
    assert_eq!(ok(&["--csv"]).format, Format::Csv);
    assert_eq!(ok(&["-json"]).format, Format::Json);
    assert_eq!(ok(&["--json"]).format, Format::Json);
}

#[test]
fn column_options_select_columns_in_the_order_given() {
    assert_eq!(ok(&["-name"]).columns, vec![Column::Name]);
    assert_eq!(ok(&["-path-column"]).columns, vec![Column::Path]);
    assert_eq!(ok(&["-full-path-and-name"]).columns, vec![Column::FullPath]);
    assert_eq!(ok(&["-size"]).columns, vec![Column::Size]);
    assert_eq!(ok(&["-dm"]).columns, vec![Column::DateModified]);
    assert_eq!(ok(&["-date-modified"]).columns, vec![Column::DateModified]);
    assert_eq!(ok(&["-dc"]).columns, vec![Column::DateCreated]);
    assert_eq!(ok(&["--date-created"]).columns, vec![Column::DateCreated]);
    assert_eq!(
        ok(&["-size", "-full-path-and-name", "-dm"]).columns,
        vec![Column::Size, Column::FullPath, Column::DateModified]
    );
    assert_eq!(
        ok(&["-name", "-path-column", "-dc"]).columns,
        vec![Column::Name, Column::Path, Column::DateCreated]
    );
}

// ------------------------------------------------------------------------ -0

#[test]
fn nul_option_ends_txt_results_with_nul() {
    for opt in ["-0", "--0"] {
        let a = ok(&[opt, "foo"]);
        assert!(a.nul_terminated, "{opt}");
        assert_eq!(a.query, "foo", "{opt}");
    }
    assert!(ok(&["-txt", "-0"]).nul_terminated);
    assert!(ok(&["foo", "-0", "-name", "-size"]).nul_terminated);
    let a = ok(&["-0", "-export-txt", "/tmp/out.txt", "foo"]);
    assert!(a.nul_terminated);
    assert_eq!(a.export.map(|e| e.format), Some(Format::Txt));
    // The export replaces standard output, so its format is the one that
    // counts: txt into the file, whatever -csv says about the screen.
    assert!(ok(&["-csv", "-export-txt", "/tmp/out.txt", "-0"]).nul_terminated);
    // After `--`, `-0` is a search word.
    let a = ok(&["--", "-0"]);
    assert!(!a.nul_terminated);
    assert_eq!(a.query, "-0");
}

#[test]
fn nul_option_with_csv_or_json_output_is_a_bad_argument() {
    for args in [
        &["-0", "-csv", "foo"][..],
        &["-csv", "-0", "foo"],
        &["-json", "--0"],
        &["-0", "-export-csv", "/tmp/out.csv"],
        &["-export-json", "/tmp/out.json", "-0"],
        &["-txt", "-0", "-export-csv", "/tmp/out.csv"],
        &["-0", "-get-result-count", "-json", "foo"],
    ] {
        let e = err(args);
        assert_eq!(e.exit_code(), ExitCode::BadArgument, "{args:?}");
        assert!(e.to_string().contains("-0"), "{args:?}: {e}");
    }
}

// --------------------------------------------------------------------- other

#[test]
fn result_count_and_no_result_error_flags() {
    assert!(ok(&["-get-result-count"]).get_result_count);
    assert!(ok(&["--get-result-count"]).get_result_count);
    assert!(ok(&["-no-result-error"]).no_result_error);
    assert!(ok(&["--no-result-error"]).no_result_error);
}

#[test]
fn export_options_take_a_file() {
    for (opt, format) in [
        ("-export-txt", Format::Txt),
        ("-export-csv", Format::Csv),
        ("-export-json", Format::Json),
        ("--export-json", Format::Json),
    ] {
        let a = ok(&[opt, "/tmp/out.file", "foo"]);
        let export = a.export.unwrap_or_else(|| panic!("{opt} sets export"));
        assert_eq!(export.format, format, "{opt}");
        assert_eq!(export.path, PathBuf::from("/tmp/out.file"), "{opt}");
        assert_eq!(a.query, "foo", "{opt}");
    }
}

#[test]
fn export_without_a_file_is_a_bad_argument() {
    for opt in ["-export-txt", "-export-csv", "-export-json"] {
        assert_eq!(err_code(&[opt]), ExitCode::BadArgument, "{opt}");
    }
}

#[test]
fn store_and_root_options_take_paths() {
    let a = ok(&["-store", "/tmp/x.fxstore", "-root", "/Users/me", "q"]);
    assert_eq!(a.store, Some(PathBuf::from("/tmp/x.fxstore")));
    assert_eq!(a.root, PathBuf::from("/Users/me"));
    assert_eq!(a.query, "q");
    let a = ok(&["--store", "s.fxstore", "--root", "/Volumes/Data"]);
    assert_eq!(a.store, Some(PathBuf::from("s.fxstore")));
    assert_eq!(a.root, PathBuf::from("/Volumes/Data"));
}

#[test]
fn store_or_root_without_a_path_is_a_bad_argument() {
    assert_eq!(err_code(&["-store"]), ExitCode::BadArgument);
    assert_eq!(err_code(&["-root"]), ExitCode::BadArgument);
}

#[cfg(unix)]
#[test]
fn store_path_may_be_non_utf8() {
    use std::os::unix::ffi::OsStringExt;
    let raw = OsString::from_vec(b"/tmp/bad\xff.fxstore".to_vec());
    let a = parse(vec![OsString::from("-store"), raw.clone()]).expect("parses");
    assert_eq!(a.store, Some(PathBuf::from(raw)));
}

#[test]
fn help_and_version_flags() {
    for opt in ["-h", "-help", "--h", "--help"] {
        assert!(ok(&[opt]).help, "{opt}");
    }
    assert!(ok(&["-version"]).version);
    assert!(ok(&["--version"]).version);
    // The third-party notices.
    assert!(ok(&["-licenses"]).licenses);
    assert!(ok(&["--licenses"]).licenses);
    assert!(!ok(&["foo"]).licenses);
}

// ----------------------------------------------------------- unknown options

#[test]
fn unknown_options_are_rejected_with_exit_code_6() {
    for opt in ["-bogus", "--bogus", "-x", "-sortt"] {
        let e = err(&[opt, "foo"]);
        assert_eq!(e.exit_code(), ExitCode::UnknownOption, "{opt}");
        assert!(!e.to_string().is_empty());
    }
}

#[test]
fn there_are_no_destructive_options() {
    // Read-only MVP: nothing that could delete, rename, move or trash a file.
    for opt in [
        "-delete", "--delete", "-del", "-rm", "-remove", "-rename", "--rename", "-move", "--move",
        "-mv", "-trash", "--trash",
    ] {
        assert_eq!(
            err_code(&[opt, "foo"]),
            ExitCode::UnknownOption,
            "{opt} must not be an option"
        );
    }
}

#[test]
fn errors_have_a_message() {
    assert!(!err(&["-n"]).to_string().is_empty());
    assert!(!err(&["-n", "abc"]).to_string().is_empty());
    assert!(!err(&["-sort", "colour"]).to_string().is_empty());
}
