//! The `fx-bench` binary: `gen`, `materialize`, `self-bench`, the
//! subcommand list in `help`, and argument errors (usage on stderr, exit
//! code 2). `gate-summary` and `phase1-summary` behaviour is tested in
//! `gate_summary.rs` and `phase1_gate_summary.rs`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fx_bench::{TreeShape, generate_names, generate_tree};

fn fx_bench(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fx-bench"))
        .args(args)
        .output()
        .expect("run fx-bench binary")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn assert_success(out: &Output, what: &str) {
    assert!(
        out.status.success(),
        "{what} failed with {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        stdout(out),
        stderr(out)
    );
}

fn assert_usage_error(args: &[&str]) {
    let out = fx_bench(args);
    assert_eq!(
        out.status.code(),
        Some(2),
        "args {args:?} must exit with code 2\nstdout:\n{}\nstderr:\n{}",
        stdout(&out),
        stderr(&out)
    );
    assert!(
        stderr(&out).to_lowercase().contains("usage"),
        "args {args:?} must print usage to stderr, got:\n{}",
        stderr(&out)
    );
}

/// The lines mentioning the checksum value (deterministic output). Timing
/// lines such as `checksum time:` are excluded because they vary per run.
fn checksum_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::to_lowercase)
        .filter(|l| l.contains("checksum") && !l.contains("time"))
        .collect()
}

/// Asserts that `text` has a line `<label>: <number> ms`, with the number
/// printed to three decimals (the format `gen` and `materialize` share).
fn assert_ms_line(text: &str, label: &str) {
    let prefix = format!("{label}: ");
    let line = text
        .lines()
        .find(|l| l.starts_with(&prefix))
        .unwrap_or_else(|| panic!("output must have a '{label}:' line:\n{text}"));
    let value = line[prefix.len()..]
        .strip_suffix(" ms")
        .unwrap_or_else(|| panic!("'{label}' must be reported in ms: {line}"));
    let (whole, frac) = value
        .split_once('.')
        .unwrap_or_else(|| panic!("'{label}' must have three decimals: {line}"));
    assert!(
        !whole.is_empty() && whole.bytes().all(|b| b.is_ascii_digit()),
        "'{label}' must be a number: {line}"
    );
    assert!(
        frac.len() == 3 && frac.bytes().all(|b| b.is_ascii_digit()),
        "'{label}' must have three decimals: {line}"
    );
}

#[test]
fn gen_reports_generation_and_checksum_times_separately() {
    for args in [
        &["gen", "--count", "2000", "--seed", "9"][..],
        &["gen", "--count", "2000", "--seed", "9", "--tree"][..],
    ] {
        let out = fx_bench(args);
        assert_success(&out, "gen");
        let text = stdout(&out);
        assert_ms_line(&text, "generation time");
        assert_ms_line(&text, "checksum time");
    }
}

#[test]
fn materialize_reports_generation_time_with_the_same_label() {
    let scratch = Scratch::new("materialize-label");
    let out_dir = scratch.path.to_str().expect("utf-8 temp path");
    let out = fx_bench(&[
        "materialize",
        "--count",
        "20",
        "--seed",
        "1",
        "--out",
        out_dir,
    ]);
    assert_success(&out, "materialize");
    let text = stdout(&out);
    assert_ms_line(&text, "generation time");
    assert_ms_line(&text, "materialize time");
}

#[test]
fn gen_prints_the_names_checksum_and_is_reproducible() {
    let count = 20_000;
    let seed = 5;
    let args = ["gen", "--count", "20000", "--seed", "5"];

    let a = fx_bench(&args);
    assert_success(&a, "gen");
    let b = fx_bench(&args);
    assert_success(&b, "gen (second run)");
    let (a, b) = (stdout(&a), stdout(&b));

    let lines_a = checksum_lines(&a);
    assert!(
        !lines_a.is_empty(),
        "gen output must mention a checksum:\n{a}"
    );
    assert_eq!(lines_a, checksum_lines(&b), "two gen runs must agree");

    let names = generate_names(count, seed);
    let sum = names.fnv1a64();
    let forms = [
        format!("{sum:016x}"),
        format!("{sum:016X}"),
        format!("{sum:x}"),
        format!("{sum:X}"),
        sum.to_string(),
    ];
    assert!(
        forms.iter().any(|f| a.contains(f.as_str())),
        "gen output must contain the FNV-1a checksum {sum:016x} of generate_names({count}, {seed}):\n{a}"
    );
    assert!(a.contains("20000") || a.contains("20,000") || a.contains("20_000"));
    let total = names.total_bytes().to_string();
    assert!(
        a.contains(&total),
        "gen output must contain the total bytes {total}:\n{a}"
    );
}

#[test]
fn gen_with_another_seed_prints_another_checksum() {
    let a = fx_bench(&["gen", "--count", "5000", "--seed", "1"]);
    let b = fx_bench(&["gen", "--count", "5000", "--seed", "2"]);
    assert_success(&a, "gen seed 1");
    assert_success(&b, "gen seed 2");
    let (la, lb) = (checksum_lines(&stdout(&a)), checksum_lines(&stdout(&b)));
    assert!(!la.is_empty());
    assert_ne!(la, lb, "different seeds must give different checksums");
}

#[test]
fn gen_tree_prints_dir_count_and_is_reproducible() {
    let args = ["gen", "--count", "10000", "--seed", "3", "--tree"];
    let a = fx_bench(&args);
    assert_success(&a, "gen --tree");
    let b = fx_bench(&args);
    assert_success(&b, "gen --tree (second run)");
    let (a, b) = (stdout(&a), stdout(&b));

    let lines_a = checksum_lines(&a);
    assert!(
        !lines_a.is_empty(),
        "gen --tree must mention a checksum:\n{a}"
    );
    assert_eq!(
        lines_a,
        checksum_lines(&b),
        "two gen --tree runs must agree"
    );

    assert!(
        a.to_lowercase().contains("dir"),
        "gen --tree must print a directory count:\n{a}"
    );
    let dirs = generate_tree(10_000, 3, TreeShape::default())
        .dir_count()
        .to_string();
    assert!(
        a.contains(&dirs),
        "gen --tree output must contain the dir count {dirs}:\n{a}"
    );
}

/// `gen --tree` prints the golden tree checksum pinned in
/// `tests/generator.rs` (`GOLDEN_TREE_FNV1A64`): FNV-1a 64 over the names
/// buffer, then each parent (little-endian `u32`), then one byte per
/// directory flag. Changing it is a breaking change to seed stability.
#[test]
fn gen_tree_prints_the_golden_tree_checksum() {
    let out = fx_bench(&["gen", "--count", "1000", "--seed", "20240601", "--tree"]);
    assert_success(&out, "gen --tree (golden)");
    let text = stdout(&out);
    assert!(
        checksum_lines(&text)
            .iter()
            .any(|l| l.contains("f3a1273bdff6b0e4")),
        "gen --tree must print the golden checksum f3a1273bdff6b0e4:\n{text}"
    );
}

#[test]
fn self_bench_prints_p50_and_p95() {
    let out = fx_bench(&["self-bench", "--count", "10000", "--iterations", "3"]);
    assert_success(&out, "self-bench");
    let text = stdout(&out);
    assert!(text.contains("p50"), "self-bench must print p50:\n{text}");
    assert!(text.contains("p95"), "self-bench must print p95:\n{text}");
    // One report for generation and one for the contains scan.
    assert!(
        text.matches("p50").count() >= 2,
        "self-bench must report at least two benchmarks:\n{text}"
    );
}

#[test]
fn self_bench_json_and_cache_state() {
    let out = fx_bench(&[
        "self-bench",
        "--count",
        "2000",
        "--iterations",
        "2",
        "--json",
        "--cache",
        "warm",
    ]);
    assert_success(&out, "self-bench --json");
    let text = stdout(&out);
    assert!(
        text.contains("\"p50_ns\""),
        "json must have p50_ns:\n{text}"
    );
    assert!(
        text.contains("\"p95_ns\""),
        "json must have p95_ns:\n{text}"
    );
    assert!(
        text.contains("\"warm\""),
        "json must record the cache state:\n{text}"
    );
}

#[test]
fn bad_arguments_print_usage_and_exit_2() {
    assert_usage_error(&["self-bench", "--no-such-flag"]);
    assert_usage_error(&["gen", "--count", "100", "--seed", "1", "--bogus"]);
    assert_usage_error(&[]);
    assert_usage_error(&["no-such-subcommand"]);
    assert_usage_error(&["gen", "--count", "not-a-number", "--seed", "1"]);
    assert_usage_error(&["gen", "--count"]);
    assert_usage_error(&["self-bench", "--cache", "lukewarm"]);
    assert_usage_error(&["materialize", "--count", "10", "--seed", "1"]);
}

#[test]
fn help_lists_every_subcommand() {
    let out = fx_bench(&["help"]);
    assert_success(&out, "help");
    let text = stdout(&out);
    for sub in [
        "gen",
        "materialize",
        "self-bench",
        "gate-summary",
        "phase1-summary",
        "help",
    ] {
        assert!(
            text.contains(&format!("fx-bench {sub}")),
            "help lacks {sub}:\n{text}"
        );
    }
}

#[test]
fn phase1_summary_bad_arguments_print_usage_and_exit_2() {
    assert_usage_error(&["phase1-summary"]);
    assert_usage_error(&["phase1-summary", "--machine", "m", "--cache", "warm"]);
    assert_usage_error(&[
        "phase1-summary",
        "--machine",
        "m",
        "--cache",
        "warm",
        "a",
        "b",
    ]);
    assert_usage_error(&[
        "phase1-summary",
        "--machine",
        "m",
        "--cache",
        "warm",
        "--format",
        "json",
        "a",
    ]);
}

/// A fresh, unique scratch directory under the temp dir, removed on drop.
/// Only this directory is ever deleted.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Scratch {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "fx-bench-cli-test-{tag}-{}-{}-{nanos}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&path).expect("create unique scratch dir");
        Scratch { path }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn walk(dir: &Path, prefix: &str, out: &mut BTreeSet<String>) {
    for entry in fs::read_dir(dir).expect("read_dir") {
        let entry = entry.expect("entry");
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let is_dir = entry.file_type().expect("file type").is_dir();
        out.insert(rel.clone());
        if is_dir {
            walk(&entry.path(), &rel, out);
        }
    }
}

#[test]
fn materialize_subcommand_writes_the_seeded_tree() {
    let scratch = Scratch::new("materialize");
    let out_dir = scratch.path.to_str().expect("utf-8 temp path");
    let out = fx_bench(&[
        "materialize",
        "--count",
        "300",
        "--seed",
        "7",
        "--out",
        out_dir,
    ]);
    assert_success(&out, "materialize");

    let tree = generate_tree(300, 7, TreeShape::default());
    let expected: BTreeSet<String> = (0..tree.len()).map(|i| tree.path(i)).collect();
    let mut found = BTreeSet::new();
    walk(&scratch.path, "", &mut found);
    assert_eq!(found, expected, "materialized tree must match Tree::path");
}

#[test]
fn materialize_subcommand_refuses_a_non_empty_dir() {
    let scratch = Scratch::new("materialize-nonempty");
    let keep = scratch.path.join("keep.txt");
    fs::write(&keep, b"keep").expect("write marker");
    let out_dir = scratch.path.to_str().expect("utf-8 temp path");

    let out = fx_bench(&[
        "materialize",
        "--count",
        "50",
        "--seed",
        "1",
        "--out",
        out_dir,
    ]);
    assert!(
        !out.status.success(),
        "materialize into a non-empty dir must fail"
    );
    let mut found = BTreeSet::new();
    walk(&scratch.path, "", &mut found);
    assert_eq!(found, BTreeSet::from(["keep.txt".to_owned()]));
    assert_eq!(fs::read(&keep).expect("read marker"), b"keep");
}
