//! `fx_bench::saves` and `fx-bench saves`: the index saves a gate phase is
//! too short to see, projected to a day for B7 ("Disk writes a day").
//!
//! fx-engine's `tests/compaction_saves.rs` checks the model against the
//! real writer; these tests check its arithmetic and the command.

use std::process::{Command, Output};

use fx_bench::saves::{
    Activity, COMPACT_FRACTION, MIN_COMPACT_ROWS, Projection, compaction_due, compaction_limit,
    metric_lines, project,
};

const MIB: u64 = 1_048_576;

fn fx_bench(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fx-bench"))
        .args(args)
        .output()
        .expect("run fx-bench binary")
}

fn describe(out: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn value<'a>(text: &'a str, key: &str) -> &'a str {
    text.lines()
        .find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
        .unwrap_or_else(|| panic!("no {key}= line in:\n{text}"))
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
}

// ---------------------------------------------------------------------------
// The writer's rule
// ---------------------------------------------------------------------------

#[test]
fn the_limit_is_a_tenth_of_the_entries_and_at_least_the_floor() {
    assert_eq!(COMPACT_FRACTION, 0.10);
    assert_eq!(MIN_COMPACT_ROWS, 1024);
    assert_eq!(compaction_limit(2_000_000), 200_000);
    assert_eq!(compaction_limit(12_019), 1_201);
    assert_eq!(compaction_limit(10_250), 1_025);
    assert_eq!(compaction_limit(10_240), 1_024);
    assert_eq!(compaction_limit(3_000), 1_024);
    assert_eq!(compaction_limit(0), 1_024);
}

#[test]
fn a_compaction_needs_more_rows_than_the_limit_of_either_kind() {
    assert!(!compaction_due(12_000, 1_200, 1_200));
    assert!(compaction_due(12_000, 1_201, 0));
    assert!(compaction_due(12_000, 0, 1_201));
    assert!(!compaction_due(3_000, 1_024, 1_024));
    assert!(compaction_due(3_000, 1_025, 0));
}

// ---------------------------------------------------------------------------
// The projection
// ---------------------------------------------------------------------------

fn gate_phase() -> Activity {
    // A 10-minute phase at 20 operations a second, half of them creates and
    // a quarter deletes.
    Activity {
        creates: 6_000,
        deletes: 3_000,
        seconds: 600.0,
    }
}

#[test]
fn a_day_at_the_gate_rate_compacts_a_whole_disk_index_about_four_times() {
    let p = project(2_000_000, 150 * MIB, &gate_phase()).expect("a projection");
    // 10 new rows a second, 864,000 a day, a compaction every 200,001.
    assert_eq!(p.entries, 2_000_000);
    assert_eq!(p.rows_per_compaction, 200_001);
    assert!(close(p.compactions_per_day, 864_000.0 / 200_001.0), "{p:?}");
    assert_eq!(p.save_bytes, 150 * MIB);
    assert!(
        close(
            p.saves_per_day_bytes,
            p.compactions_per_day * (150 * MIB) as f64
        ),
        "{p:?}"
    );
    // About 648 MB a day.
    let mb = p.saves_per_day_bytes / MIB as f64;
    assert!((647.0..649.0).contains(&mb), "{mb} MB");
}

#[test]
fn the_faster_kind_of_row_sets_the_pace() {
    let creates = project(
        100_000,
        MIB,
        &Activity {
            creates: 3_000,
            deletes: 1_000,
            seconds: 300.0,
        },
    )
    .unwrap();
    let deletes = project(
        100_000,
        MIB,
        &Activity {
            creates: 1_000,
            deletes: 3_000,
            seconds: 300.0,
        },
    )
    .unwrap();
    // 10 rows a second either way: 864,000 a day over 10,001 a compaction.
    assert!(close(creates.compactions_per_day, 864_000.0 / 10_001.0));
    assert_eq!(creates, deletes);
}

#[test]
fn renames_alone_and_no_activity_project_no_saves() {
    let p = project(
        2_000_000,
        150 * MIB,
        &Activity {
            creates: 0,
            deletes: 0,
            seconds: 600.0,
        },
    )
    .unwrap();
    assert_eq!(p.compactions_per_day, 0.0);
    assert_eq!(p.saves_per_day_bytes, 0.0);
}

#[test]
fn a_small_index_compacts_at_the_floor() {
    let p = project(
        3_000,
        MIB,
        &Activity {
            creates: 1_025,
            deletes: 0,
            seconds: 86_400.0,
        },
    )
    .unwrap();
    assert_eq!(p.rows_per_compaction, 1_025);
    assert!(close(p.compactions_per_day, 1.0));
}

#[test]
fn no_projection_without_a_duration() {
    for seconds in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let a = Activity {
            creates: 1,
            deletes: 1,
            seconds,
        };
        assert_eq!(project(2_000_000, MIB, &a), None, "{seconds}");
    }
}

#[test]
fn metric_lines_carry_the_label_and_add_the_measured_writes() {
    let p = Projection {
        entries: 2_000_000,
        rows_per_compaction: 200_001,
        compactions_per_day: 4.25,
        save_bytes: 1_000_000,
        saves_per_day_bytes: 4_250_000.0,
    };
    assert_eq!(
        metric_lines("hidden", &p, None),
        [
            "hidden_entries=2000000",
            "hidden_rows_per_compaction=200001",
            "hidden_compactions_per_day=4.250",
            "hidden_save_bytes=1000000",
            "hidden_compaction_saves_per_day_bytes=4250000",
        ]
    );
    let with = metric_lines("search", &p, Some(17_280_000.0));
    assert_eq!(with.len(), 6);
    assert_eq!(
        with[5],
        "search_disk_written_with_saves_per_day_bytes=21530000"
    );
}

// ---------------------------------------------------------------------------
// The command
// ---------------------------------------------------------------------------

#[test]
fn saves_prints_the_projection_and_the_total() {
    let out = fx_bench(&[
        "saves",
        "--label",
        "hidden",
        "--entries",
        "2000000",
        "--save-bytes",
        "157286400",
        "--creates",
        "6000",
        "--deletes",
        "3000",
        "--duration",
        "600",
        "--measured-per-day",
        "1000",
    ]);
    assert!(out.status.success(), "{}", describe(&out));
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(value(&text, "hidden_entries"), "2000000");
    assert_eq!(value(&text, "hidden_rows_per_compaction"), "200001");
    assert_eq!(value(&text, "hidden_compactions_per_day"), "4.320");
    assert_eq!(value(&text, "hidden_save_bytes"), "157286400");
    let saves: f64 = value(&text, "hidden_compaction_saves_per_day_bytes")
        .parse()
        .unwrap();
    let total: f64 = value(&text, "hidden_disk_written_with_saves_per_day_bytes")
        .parse()
        .unwrap();
    assert!((total - saves - 1000.0).abs() <= 1.0, "{text}");
    assert!(
        (saves - 864_000.0 / 200_001.0 * 157_286_400.0).abs() <= 1.0,
        "{text}"
    );
}

#[test]
fn saves_without_measured_writes_prints_no_total() {
    let out = fx_bench(&[
        "saves",
        "--label",
        "search",
        "--entries",
        "1000",
        "--save-bytes",
        "0",
        "--creates",
        "0",
        "--deletes",
        "0",
        "--duration",
        "1800",
    ]);
    assert!(out.status.success(), "{}", describe(&out));
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(value(&text, "search_compactions_per_day"), "0.000");
    assert!(!text.contains("disk_written_with_saves"), "{text}");
}

#[test]
fn saves_bad_arguments_exit_2_with_usage() {
    let full = [
        "saves",
        "--label",
        "hidden",
        "--entries",
        "10",
        "--save-bytes",
        "10",
        "--creates",
        "1",
        "--deletes",
        "1",
        "--duration",
        "60",
    ];
    // Each required flag left out in turn.
    for skip in (1..full.len()).step_by(2) {
        let args: Vec<&str> = full
            .iter()
            .enumerate()
            .filter(|&(i, _)| i != skip && i != skip + 1)
            .map(|(_, a)| *a)
            .collect();
        let out = fx_bench(&args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", describe(&out));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("usage"),
            "{args:?}: {}",
            describe(&out)
        );
    }
    for (flag, bad) in [
        ("--label", "Hidden"),
        ("--entries", "-5"),
        ("--save-bytes", "1.5"),
        ("--creates", "many"),
        ("--deletes", ""),
        ("--duration", "0"),
    ] {
        let mut args: Vec<&str> = full.to_vec();
        let i = args.iter().position(|a| *a == flag).unwrap();
        args[i + 1] = bad;
        let out = fx_bench(&args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", describe(&out));
    }
    let mut extra: Vec<&str> = full.to_vec();
    extra.extend(["--measured-per-day", "1.5"]);
    assert_eq!(fx_bench(&extra).status.code(), Some(2));
    extra = full.to_vec();
    extra.push("stray");
    assert_eq!(fx_bench(&extra).status.code(), Some(2));
}

#[test]
fn help_lists_saves() {
    let out = fx_bench(&["help"]);
    assert!(out.status.success(), "{}", describe(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("fx-bench saves"));
}
