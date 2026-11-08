//! Contract tests for `fx_ui_macos::spike_args`: parsing the `table_spike`
//! example's arguments. `SpikeArgs::parse` takes the arguments after the
//! program name (as from `std::env::args().skip(1)`); bad input is an error
//! (the example exits with code 2), never a panic.
//! Built with the `measure` feature only, as the example is.

#![cfg(feature = "measure")]

use fx_ui_macos::spike_args::SpikeArgs;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn parse_ok(list: &[&str]) -> SpikeArgs {
    match SpikeArgs::parse(args(list)) {
        Ok(a) => a,
        Err(e) => panic!("{list:?} should parse, got error: {e}"),
    }
}

fn assert_err(list: &[&str]) {
    match SpikeArgs::parse(args(list)) {
        Ok(a) => panic!("{list:?} should be rejected, parsed as {a:?}"),
        Err(e) => {
            let msg = e.to_string();
            assert!(!msg.trim().is_empty(), "{list:?}: empty error message");
        }
    }
}

#[test]
fn no_arguments_gives_the_defaults() {
    let a = parse_ok(&[]);
    assert_eq!(a.rows, 2_000_000);
    assert_eq!(a.autoscroll, 0, "0 means interactive");
    assert!(a.rows_per_frame >= 1);
    assert!(!a.launch_probe);
    assert!(!a.json);
    assert_eq!(a, SpikeArgs::default());
}

#[test]
fn every_option_is_parsed() {
    let a = parse_ok(&[
        "--rows",
        "1500",
        "--seed",
        "42",
        "--autoscroll",
        "600",
        "--rows-per-frame",
        "7",
        "--launch-probe",
        "--json",
    ]);
    assert_eq!(a.rows, 1_500);
    assert_eq!(a.seed, 42);
    assert_eq!(a.autoscroll, 600);
    assert_eq!(a.rows_per_frame, 7);
    assert!(a.launch_probe);
    assert!(a.json);
}

#[test]
fn options_are_accepted_in_any_order() {
    let a = parse_ok(&["--json", "--seed", "9", "--rows", "10"]);
    let b = parse_ok(&["--rows", "10", "--seed", "9", "--json"]);
    assert_eq!(a, b);
    assert_eq!(a.rows, 10);
    assert_eq!(a.seed, 9);
    assert!(a.json);
    assert!(!a.launch_probe);
}

#[test]
fn unspecified_options_keep_their_defaults() {
    let d = SpikeArgs::default();
    let a = parse_ok(&["--launch-probe"]);
    assert!(a.launch_probe);
    assert_eq!(a.rows, d.rows);
    assert_eq!(a.seed, d.seed);
    assert_eq!(a.autoscroll, d.autoscroll);
    assert_eq!(a.rows_per_frame, d.rows_per_frame);
    assert_eq!(a.json, d.json);
}

#[test]
fn large_seed_is_accepted() {
    let a = parse_ok(&["--seed", "18446744073709551615"]);
    assert_eq!(a.seed, u64::MAX);
}

#[test]
fn unknown_flags_and_positionals_are_rejected() {
    assert_err(&["--bogus"]);
    assert_err(&["rows"]);
    assert_err(&["--rows", "10", "extra"]);
    assert_err(&["-r", "10"]);
}

#[test]
fn missing_values_are_rejected() {
    assert_err(&["--rows"]);
    assert_err(&["--seed"]);
    assert_err(&["--autoscroll"]);
    assert_err(&["--rows-per-frame"]);
    assert_err(&["--rows", "--json"]);
}

#[test]
fn non_numeric_and_negative_values_are_rejected() {
    assert_err(&["--rows", "many"]);
    assert_err(&["--rows", "-5"]);
    assert_err(&["--rows", "1.5"]);
    assert_err(&["--rows", ""]);
    assert_err(&["--seed", "x"]);
    assert_err(&["--seed", "-1"]);
    assert_err(&["--autoscroll", "ten"]);
    assert_err(&["--autoscroll", "-1"]);
    assert_err(&["--rows-per-frame", "abc"]);
}

#[test]
fn zero_rows_per_frame_is_rejected() {
    assert_err(&["--rows-per-frame", "0"]);
}

#[test]
fn row_counts_beyond_u32_indices_are_rejected() {
    // The synthetic tree uses u32 indices; this must be an error, not a
    // panic inside the generator.
    assert_err(&["--rows", "5000000000"]);
    assert_err(&["--rows", "99999999999999999999999"]);
}

#[test]
fn flags_do_not_take_values() {
    assert_err(&["--json", "1"]);
    assert_err(&["--launch-probe", "yes"]);
}
