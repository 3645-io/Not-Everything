//! Contract tests for `fx_ui_macos::app_args`: the command line of the
//! `fx-ui-macos` app binary, the per-root store location, and the binary's
//! behaviour on bad input.
//!
//! `AppArgs::parse` takes the arguments after the program name (as from
//! `std::env::args().skip(1)`). Bad input is an `AppArgsError` (the binary
//! prints it with `USAGE`, which contains the word "usage", to stderr and
//! exits with code 2 before opening any window), never a panic.
//!
//! Options:
//! - `--root <path>`: the root to index (default `/`, `DEFAULT_ROOT`).
//!
//! Only with the `measure` feature (the gate scripts turn it on; release
//! builds leave it off, so they reject these as unknown options):
//! - `--synthetic <N>`: run the window on `SyntheticSource` with N rows
//!   instead of an engine (N at least 1 and below `u32::MAX`, the table's
//!   row-index limit, as in the spike).
//! - `--autoscroll`: a flag (no value), so that
//!   `--synthetic 2000000 --autoscroll` works; scroll, print frame stats
//!   and exit.
//! - `--launch-probe`: a flag; print the time to first paint, then exit.
//! - `--page-cost-ms <MS>` and `--gate-probe <OUT>`: see
//!   `tests/engine_source.rs` and `tests/gate_probe.rs`.
//! - `--summon-probe <OUT>` and `--sort-probe <OUT>`: the shortcut and
//!   re-sort probes (`tests/summon_probe.rs`, `tests/sort_probe.rs`). Each
//!   is a measurement of its own: never combined with another probe,
//!   `--autoscroll` or `--launch-probe`.
//! - `--data-dir <DIR>`: keep the stores and settings in `DIR`, an existing
//!   folder, instead of the app's folder under Application Support.
//! - `--window hidden|search` and `--query <TEXT>`: start the resident app
//!   hidden, or showing a search for TEXT (`search` needs `--query`, and
//!   `--query` needs `search`). Not a measurement run, so never combined
//!   with one.
//!
//! `store_path(home, root)` is where the engine's store for `root` lives:
//! under `<home>/Library/Application Support/`, distinct per root.

use std::path::{Path, PathBuf};

#[cfg(feature = "measure")]
use fx_ui_macos::app_args::WindowStart;
use fx_ui_macos::app_args::{
    AppArgs, AppArgsError, DEFAULT_ROOT, USAGE, app_data_folder, store_path,
};

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn parse_ok(list: &[&str]) -> AppArgs {
    match AppArgs::parse(args(list)) {
        Ok(a) => a,
        Err(e) => panic!("{list:?} should parse, got error: {e}"),
    }
}

fn parse_err(list: &[&str]) -> AppArgsError {
    match AppArgs::parse(args(list)) {
        Ok(a) => panic!("{list:?} should be rejected, parsed as {a:?}"),
        Err(e) => {
            let msg = e.to_string();
            assert!(!msg.trim().is_empty(), "{list:?}: empty error message");
            e
        }
    }
}

#[test]
fn no_arguments_gives_the_defaults() {
    let a = parse_ok(&[]);
    assert_eq!(DEFAULT_ROOT, "/");
    assert_eq!(a.root, PathBuf::from("/"));
    assert!(!a.measuring(), "interactive by default");
    assert_eq!(a, AppArgs::default());
}

#[cfg(feature = "measure")]
#[test]
fn no_arguments_measure_nothing() {
    let a = parse_ok(&[]);
    assert_eq!(a.synthetic, None, "an engine over the root by default");
    assert!(!a.autoscroll, "interactive by default");
    assert!(!a.launch_probe);
    assert_eq!(a.gate_probe, None);
}

#[test]
fn root_option_sets_the_root() {
    let a = parse_ok(&["--root", "/Users/someone/Projects"]);
    assert_eq!(a.root, PathBuf::from("/Users/someone/Projects"));
    assert!(!a.measuring());
}

#[test]
fn root_with_spaces_is_kept_verbatim() {
    let a = parse_ok(&["--root", "/Volumes/My Disk/some dir"]);
    assert_eq!(a.root, PathBuf::from("/Volumes/My Disk/some dir"));
}

#[cfg(feature = "measure")]
#[test]
fn synthetic_autoscroll_measurement_command_parses() {
    // The run verification command: `-- --synthetic 2000000 --autoscroll`.
    let a = parse_ok(&["--synthetic", "2000000", "--autoscroll"]);
    assert_eq!(a.synthetic, Some(2_000_000));
    assert!(a.autoscroll);
    assert!(!a.launch_probe);
}

#[cfg(feature = "measure")]
#[test]
fn launch_probe_is_a_flag() {
    let a = parse_ok(&["--launch-probe"]);
    assert!(a.launch_probe);
    assert!(!a.autoscroll);
    assert_eq!(a.synthetic, None);

    let b = parse_ok(&["--synthetic", "2000000", "--launch-probe"]);
    assert!(b.launch_probe);
    assert_eq!(b.synthetic, Some(2_000_000));
}

#[cfg(feature = "measure")]
#[test]
fn options_may_come_in_any_order() {
    let a = parse_ok(&["--autoscroll", "--synthetic", "10", "--launch-probe"]);
    let b = parse_ok(&["--launch-probe", "--synthetic", "10", "--autoscroll"]);
    assert_eq!(a, b);
    assert_eq!(a.synthetic, Some(10));
    assert!(a.autoscroll && a.launch_probe);

    let c = parse_ok(&["--root", "/tmp", "--launch-probe"]);
    let d = parse_ok(&["--launch-probe", "--root", "/tmp"]);
    assert_eq!(c, d);
}

#[test]
fn repeated_option_keeps_the_last_value() {
    let b = parse_ok(&["--root", "/a", "--root", "/b"]);
    assert_eq!(b.root, PathBuf::from("/b"));
}

#[cfg(feature = "measure")]
#[test]
fn repeated_measure_option_keeps_the_last_value() {
    let a = parse_ok(&["--synthetic", "5", "--synthetic", "7"]);
    assert_eq!(a.synthetic, Some(7));
}

#[cfg(feature = "measure")]
#[test]
fn synthetic_accepts_the_smallest_and_largest_row_counts() {
    assert_eq!(parse_ok(&["--synthetic", "1"]).synthetic, Some(1));
    let max = (u32::MAX - 1).to_string();
    assert_eq!(
        parse_ok(&["--synthetic", &max]).synthetic,
        Some((u32::MAX - 1) as usize)
    );
}

#[test]
fn missing_values_are_errors() {
    let e = parse_err(&["--root"]);
    assert!(
        e.to_string().contains("--root"),
        "message names the option: {e}"
    );
}

#[cfg(feature = "measure")]
#[test]
fn missing_measure_values_are_errors() {
    let e = parse_err(&["--synthetic"]);
    assert!(
        e.to_string().contains("--synthetic"),
        "message names the option: {e}"
    );
}

#[cfg(feature = "measure")]
#[test]
fn bad_synthetic_counts_are_errors() {
    for bad in ["abc", "-5", "1.5", "", " 3", "0x10", "+4"] {
        let e = parse_err(&["--synthetic", bad]);
        assert!(
            e.to_string().contains("--synthetic"),
            "{bad:?}: message names the option: {e}"
        );
    }
    // Zero rows is not a measurement; the table's row index is 32-bit.
    parse_err(&["--synthetic", "0"]);
    parse_err(&["--synthetic", &u32::MAX.to_string()]);
    parse_err(&["--synthetic", "99999999999999999999999999"]);
}

#[test]
fn empty_root_is_an_error() {
    parse_err(&["--root", ""]);
}

#[test]
fn unknown_options_and_stray_arguments_are_errors() {
    parse_err(&["--bogus"]);
    parse_err(&["stray"]);
    parse_err(&["--root", "/tmp", "extra"]);
}

#[cfg(feature = "measure")]
#[test]
fn measure_flags_take_no_value() {
    parse_err(&["--synthetic", "10", "extra"]);
    // `--autoscroll` and `--launch-probe` are flags: a following number is
    // a stray argument, not their value.
    parse_err(&["--autoscroll", "100"]);
    parse_err(&["--launch-probe", "1"]);
}

/// The measurement options the gate scripts use.
const MEASURE_OPTIONS: [&str; 10] = [
    "--synthetic",
    "--page-cost-ms",
    "--autoscroll",
    "--launch-probe",
    "--gate-probe",
    "--summon-probe",
    "--sort-probe",
    "--data-dir",
    "--window",
    "--query",
];

/// A build without the `measure` feature (every release) has no
/// measurement modes: their options are unknown, as any other would be, and
/// the usage text does not offer them.
#[cfg(not(feature = "measure"))]
#[test]
fn measure_options_are_unknown_without_the_feature() {
    for opt in MEASURE_OPTIONS {
        for list in [&[opt][..], &[opt, "10"][..], &["--root", "/tmp", opt][..]] {
            let e = parse_err(list);
            assert!(
                e.to_string().contains("unknown option"),
                "{list:?}: {opt} is an unknown option without `measure`: {e}"
            );
        }
        assert!(!USAGE.contains(opt), "USAGE must not offer {opt}:\n{USAGE}");
    }
}

#[test]
fn there_are_no_destructive_options() {
    // Read-only MVP: no option deletes, moves, renames or trashes files.
    for opt in ["--delete", "--rm", "--move", "--rename", "--trash"] {
        parse_err(&[opt]);
        assert!(!USAGE.contains(opt), "USAGE must not offer {opt}");
    }
}

#[test]
fn usage_documents_every_option() {
    assert!(
        USAGE.contains("--root"),
        "USAGE should mention --root:\n{USAGE}"
    );
    if cfg!(feature = "measure") {
        for opt in MEASURE_OPTIONS {
            assert!(USAGE.contains(opt), "USAGE should mention {opt}:\n{USAGE}");
        }
    }
}

#[test]
fn errors_are_std_errors() {
    fn is_error<E: std::error::Error + Clone + PartialEq>(_: &E) {}
    let e = parse_err(&["--bogus"]);
    is_error(&e);
}

#[test]
fn store_path_lives_under_application_support() {
    let home = Path::new("/Users/tester");
    let p = store_path(home, Path::new("/"));
    let support = home.join("Library").join("Application Support");
    assert!(
        p.starts_with(&support),
        "{} should be under {}",
        p.display(),
        support.display()
    );
    assert_ne!(
        p, support,
        "the store is inside an app folder, not the folder itself"
    );
}

#[test]
fn store_path_is_stable_and_distinct_per_root() {
    let home = Path::new("/Users/tester");
    let a1 = store_path(home, Path::new("/"));
    let a2 = store_path(home, Path::new("/"));
    assert_eq!(a1, a2, "same root, same store across launches");

    let b = store_path(home, Path::new("/Users/tester/Projects"));
    let c = store_path(home, Path::new("/Users/tester/Projects/sub"));
    assert_ne!(a1, b, "different roots must not share a store");
    assert_ne!(b, c, "different roots must not share a store");

    let other_home = Path::new("/Users/other");
    let d = store_path(other_home, Path::new("/"));
    assert!(d.starts_with(other_home));
}

/// The app binary exists. On non-macOS hosts it says it is macOS-only and
/// exits 0.
#[cfg(not(target_os = "macos"))]
#[test]
fn binary_is_macos_only_elsewhere() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_fx-ui-macos"))
        .output()
        .expect("run fx-ui-macos");
    assert!(out.status.success(), "exit status {:?}", out.status);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("macOS"),
        "should say it is macOS-only: {text:?}"
    );
}

/// On macOS, a bad command line is rejected with exit code 2 before any
/// window opens (so this test cannot hang on a GUI).
#[cfg(target_os = "macos")]
#[test]
fn binary_rejects_bad_arguments_with_exit_code_2() {
    for bad in [
        &["--bogus"][..],
        &["--root"][..],
        &["--synthetic"][..],
        &["--synthetic", "0"][..],
        // Without `measure` an unknown option; with it, a gate probe of
        // synthetic rows, which it refuses.
        &["--gate-probe", "out.txt", "--synthetic", "10"][..],
        // Without `measure` unknown options; with them, a data folder that
        // does not exist, a search without its text and two probes at once.
        &["--data-dir", "/nonexistent/fx-ui-macos-test-data"][..],
        &["--window", "search"][..],
        &["--summon-probe", "a.txt", "--sort-probe", "b.txt"][..],
    ] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_fx-ui-macos"))
            .args(bad)
            .output()
            .expect("run fx-ui-macos");
        assert_eq!(
            out.status.code(),
            Some(2),
            "{bad:?}: status {:?}",
            out.status
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("usage"),
            "{bad:?}: stderr should show the usage: {stderr:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The data folder
// ---------------------------------------------------------------------------

/// A fresh, empty folder under the system temp directory, removed when
/// dropped.
#[cfg(feature = "measure")]
struct TempDir(PathBuf);

#[cfg(feature = "measure")]
impl TempDir {
    fn new(tag: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!(
            "fx-ui-app-args-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        TempDir(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn arg(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
}

#[cfg(feature = "measure")]
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn the_default_data_folder_is_the_app_folder_in_application_support() {
    let home = Path::new("/Users/me");
    let expected = home
        .join("Library")
        .join("Application Support")
        .join(fx_ui_macos::app_args::APP_FOLDER);
    assert_eq!(app_data_folder(home), expected);
    let a = parse_ok(&[]);
    assert_eq!(a.data_dir(), None);
    assert!(!a.has_data_dir());
    assert_eq!(a.data_folder(home), expected);
}

#[cfg(feature = "measure")]
#[test]
fn data_dir_takes_an_existing_folder() {
    let dir = TempDir::new("data");
    let a = parse_ok(&["--data-dir", &dir.arg()]);
    assert_eq!(a.data_dir(), Some(dir.path()));
    assert!(a.has_data_dir());
    assert!(!a.measuring(), "a data folder alone measures nothing");
    assert_eq!(a.data_folder(Path::new("/Users/me")), dir.path());
}

#[cfg(feature = "measure")]
#[test]
fn data_dir_must_be_an_existing_folder() {
    let dir = TempDir::new("data-bad");
    let missing = dir.path().join("missing");
    let file = dir.path().join("a-file");
    std::fs::write(&file, "x").expect("write a file");
    for bad in [
        vec!["--data-dir".to_string()],
        vec!["--data-dir".to_string(), String::new()],
        vec![
            "--data-dir".to_string(),
            missing.to_string_lossy().into_owned(),
        ],
        vec![
            "--data-dir".to_string(),
            file.to_string_lossy().into_owned(),
        ],
    ] {
        let list: Vec<&str> = bad.iter().map(String::as_str).collect();
        let e = parse_err(&list);
        assert!(e.to_string().contains("--data-dir"), "{list:?}: {e}");
    }
    assert!(!missing.exists(), "parsing never creates the folder");
}

// ---------------------------------------------------------------------------
// --window and --query
// ---------------------------------------------------------------------------

#[cfg(feature = "measure")]
#[test]
fn window_hidden_starts_the_resident_app_hidden() {
    let a = parse_ok(&["--window", "hidden"]);
    assert_eq!(a.window, Some(WindowStart::Hidden));
    assert!(!a.measuring(), "the app then runs until it is quit");
    assert_eq!(parse_ok(&[]).window, None);
}

#[cfg(feature = "measure")]
#[test]
fn window_search_takes_the_query_in_any_order() {
    let a = parse_ok(&["--window", "search", "--query", "main"]);
    let b = parse_ok(&["--query", "main", "--window", "search"]);
    assert_eq!(a, b);
    assert_eq!(a.window, Some(WindowStart::Search("main".to_string())));
    assert!(!a.measuring());
    let spaced = parse_ok(&["--window", "search", "--query", "read me"]);
    assert_eq!(
        spaced.window,
        Some(WindowStart::Search("read me".to_string()))
    );
}

#[cfg(feature = "measure")]
#[test]
fn window_search_needs_a_query() {
    let e = parse_err(&["--window", "search"]);
    assert!(e.to_string().contains("--query"), "{e}");
    for bad in [
        &["--window", "search", "--query"][..],
        &["--window", "search", "--query", ""][..],
    ] {
        let e = parse_err(bad);
        assert!(e.to_string().contains("--query"), "{bad:?}: {e}");
    }
}

#[cfg(feature = "measure")]
#[test]
fn query_needs_window_search() {
    for bad in [
        &["--query", "main"][..],
        &["--window", "hidden", "--query", "main"][..],
    ] {
        let e = parse_err(bad);
        assert!(e.to_string().contains("--window search"), "{bad:?}: {e}");
    }
}

#[cfg(feature = "measure")]
#[test]
fn unknown_window_values_are_errors() {
    for bad in [
        &["--window"][..],
        &["--window", ""][..],
        &["--window", "shown"][..],
        &["--window", "Hidden"][..],
    ] {
        let e = parse_err(bad);
        assert!(e.to_string().contains("--window"), "{bad:?}: {e}");
    }
}

#[cfg(feature = "measure")]
#[test]
fn window_combines_with_a_data_dir_and_synthetic_rows() {
    let dir = TempDir::new("window");
    let a = parse_ok(&[
        "--window",
        "hidden",
        "--data-dir",
        &dir.arg(),
        "--synthetic",
        "1000",
    ]);
    assert_eq!(a.window, Some(WindowStart::Hidden));
    assert_eq!(a.data_dir(), Some(dir.path()));
    assert_eq!(a.synthetic, Some(1000));
}

// ---------------------------------------------------------------------------
// --summon-probe and --sort-probe
// ---------------------------------------------------------------------------

#[cfg(feature = "measure")]
#[test]
fn probes_take_an_output_path() {
    let a = parse_ok(&["--summon-probe", "/tmp/probe out/summon.txt"]);
    assert_eq!(
        a.summon_probe,
        Some(PathBuf::from("/tmp/probe out/summon.txt"))
    );
    assert!(a.measuring(), "a probe measures and exits");
    let b = parse_ok(&["--sort-probe", "sort.txt"]);
    assert_eq!(b.sort_probe, Some(PathBuf::from("sort.txt")));
    assert!(b.measuring());
    let none = parse_ok(&[]);
    assert_eq!((none.summon_probe, none.sort_probe), (None, None));
}

#[cfg(feature = "measure")]
#[test]
fn probes_run_on_synthetic_rows_or_a_root() {
    for probe in ["--summon-probe", "--sort-probe"] {
        let a = parse_ok(&[probe, "out.txt", "--synthetic", "200000"]);
        assert_eq!(a.synthetic, Some(200_000), "{probe}");
        let b = parse_ok(&["--root", "/Users/me/Projects", probe, "out.txt"]);
        assert_eq!(b.root, PathBuf::from("/Users/me/Projects"), "{probe}");
    }
}

#[cfg(feature = "measure")]
#[test]
fn probes_need_a_non_empty_path() {
    for probe in ["--summon-probe", "--sort-probe"] {
        for bad in [&[probe][..], &[probe, ""][..]] {
            let e = parse_err(bad);
            assert!(e.to_string().contains(probe), "{bad:?}: {e}");
        }
    }
}

#[cfg(feature = "measure")]
#[test]
fn a_probe_runs_alone() {
    for bad in [
        &["--summon-probe", "a", "--gate-probe", "b"][..],
        &["--gate-probe", "b", "--sort-probe", "a"][..],
        &["--summon-probe", "a", "--sort-probe", "b"][..],
        &["--summon-probe", "a", "--autoscroll"][..],
        &["--sort-probe", "a", "--launch-probe"][..],
    ] {
        parse_err(bad);
    }
}

#[cfg(feature = "measure")]
#[test]
fn window_is_never_combined_with_a_measurement() {
    for other in [
        &["--gate-probe", "out.txt"][..],
        &["--summon-probe", "out.txt"][..],
        &["--sort-probe", "out.txt"][..],
        &["--autoscroll"][..],
        &["--launch-probe"][..],
    ] {
        for window in [
            &["--window", "hidden"][..],
            &["--window", "search", "--query", "x"][..],
        ] {
            let list: Vec<&str> = window.iter().chain(other).copied().collect();
            let e = parse_err(&list);
            assert!(e.to_string().contains("--window"), "{list:?}: {e}");
        }
    }
}

#[cfg(feature = "measure")]
#[test]
fn usage_names_the_new_measurement_values() {
    for text in ["--window hidden|search", "--query TEXT", "--data-dir DIR"] {
        assert!(USAGE.contains(text), "USAGE should show {text:?}:\n{USAGE}");
    }
}
