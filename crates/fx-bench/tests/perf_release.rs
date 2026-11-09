//! `fx-bench perf-release`, driven through the built binary: the
//! performance section of a release's notes. It compares this release's
//! metrics with the previous release's and with the targets in the baseline
//! file, as a summary table (the `[meta]` table's `summary` IDs) followed by
//! every metric inside a `<details>` block, each row marked with a coloured
//! dot.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

fn fx_bench(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fx-bench"))
        .args(args)
        .output()
        .expect("run fx-bench binary")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn describe(out: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        stdout(out),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// A unique scratch directory under the system temp dir, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "fx-bench-perf-release-{tag}-{}-{}-{nanos}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).expect("create scratch dir");
        Scratch(dir)
    }

    fn write(&self, name: &str, text: &str) -> String {
        let p = self.0.join(name);
        fs::write(&p, text).expect("write fixture");
        p.to_str().expect("utf-8 temp path").to_owned()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A baseline with one metric per kind the section has to show.
const BASELINE: &str = "\
[meta]
machine = \"Apple M4 Pro, 24 GB, macOS 27\"
cache = \"warm\"
build = \"0.3.2\"
date = \"2026-11-09\"
summary = \"B1 F1 B7\"

[B1]
name = \"CPU, window hidden\"
key = \"hidden_cpu_pct\"
unit = \"% of a core\"
better = \"lower\"
target = 1
allow_pct = 10
allow_floor = 0.05
aggregate = \"mean\"
tiers = \"pr rc\"

[B7]
name = \"Disk writes a day\"
key = \"disk_mb\"
unit = \"MB\"
better = \"lower\"
target = 512
allow_pct = 5
allow_floor = 10
aggregate = \"mean\"
tiers = \"pr rc\"

[M1]
name = \"Memory, window hidden\"
key = \"footprint_mb\"
unit = \"MB\"
better = \"lower\"
target = 200
allow_pct = 5
aggregate = \"mean\"
tiers = \"pr rc\"

[S5]
name = \"Keystroke to results (p95)\"
key = \"keystroke_ms\"
unit = \"ms\"
better = \"lower\"
target = 50
allow_pct = 10
allow_floor = 1
aggregate = \"median\"
tiers = \"quick pr rc\"

[F1]
name = \"New file shown (p99)\"
key = \"freshness_ms\"
unit = \"ms\"
better = \"lower\"
target = 1000
allow_pct = 10
allow_floor = 10
aggregate = \"median\"
tiers = \"quick pr rc\"

[F4]
name = \"First scan of the disk\"
key = \"first_run_ms\"
unit = \"ms\"
better = \"lower\"
target = 15000
allow_pct = 10
aggregate = \"median\"
tiers = \"pr rc\"

[F6]
name = \"Catch-up after a relaunch\"
key = \"catchup_ms\"
unit = \"ms\"
better = \"lower\"
target = 10000
allow_pct = 10
allow_floor = 10
aggregate = \"median\"
tiers = \"pr rc\"

[C2]
name = \"Catch-up equals a fresh scan\"
key = \"catchup_equal\"
unit = \"bool\"
better = \"higher\"
target = 1
aggregate = \"min\"
tiers = \"pr rc\"

[C3]
name = \"Differences after a 24 h soak\"
key = \"soak_differences\"
unit = \"count\"
better = \"lower\"
target = 0
aggregate = \"max\"
tiers = \"rc\"
gated = false

[O1]
name = \"DMG size\"
key = \"dmg_bytes\"
unit = \"bytes\"
better = \"lower\"
allow_pct = 5
aggregate = \"last\"
tiers = \"rc\"

[O2]
name = \"Command-line search, 2M files\"
key = \"cli_ms\"
unit = \"ms\"
better = \"lower\"
target = 100
aggregate = \"median\"
tiers = \"quick pr\"
gated = false
";

const PREVIOUS: &str = "\
# 0.3.1
hidden_cpu_pct=40.852
disk_mb=44057.1
footprint_mb=1318.4
keystroke_ms=42.45
freshness_ms=117.429
first_run_ms=19702
catchup_ms=900
catchup_equal=true
dmg_bytes=4200412
cli_ms=586.91
";

const CURRENT: &str = "\
hidden_cpu_pct=10.389
disk_mb=0
footprint_mb=1350
keystroke_ms=42.56
freshness_ms=187.125
first_run_ms=18959
catchup_ms=12000
catchup_equal=true
dmg_bytes=4200468
cli_ms=581.87
";

fn release(baseline: &str, previous: &str, current: &str, extra: &[&str]) -> Output {
    let s = Scratch::new("run");
    let b = s.write("baseline.toml", baseline);
    let p = s.write("previous.txt", previous);
    let c = s.write("current.txt", current);
    let mut args = vec![
        "perf-release",
        "--baseline",
        &b,
        "--previous",
        &p,
        "--previous-label",
        "0.3.1",
        "--label",
        "0.3.2",
    ];
    args.extend_from_slice(extra);
    args.push(&c);
    fx_bench(&args)
}

fn section() -> String {
    let out = release(BASELINE, PREVIOUS, CURRENT, &[]);
    assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    stdout(&out)
}

/// The row of the summary table (before `<details>`) whose Metric cell is
/// `name`.
fn summary_row(text: &str, name: &str) -> Vec<String> {
    let summary = text.split("<details>").next().unwrap();
    cells(summary, &format!("| {name} |"))
}

/// The row of the detailed table (inside `<details>`) whose ID cell is `id`.
fn detail_row(text: &str, id: &str) -> Vec<String> {
    let details = text.split("<details>").nth(1).expect("a <details> block");
    cells(details, &format!("| {id} |"))
}

fn cells(text: &str, prefix: &str) -> Vec<String> {
    let line = text
        .lines()
        .find(|l| l.starts_with(prefix))
        .unwrap_or_else(|| panic!("no row starting {prefix:?}:\n{text}"));
    line.trim()
        .trim_matches('|')
        .split(" | ")
        .map(|c| c.trim().to_owned())
        .collect()
}

#[test]
fn has_a_heading_a_legend_a_summary_and_a_folded_detail_table() {
    let text = section();
    assert!(text.starts_with("### Performance\n"), "{text}");
    assert!(
        text.contains("0.3.2 compared with 0.3.1 and the targets"),
        "{text}"
    );
    assert!(text.contains("Apple M4 Pro, 24 GB, macOS 27"), "{text}");
    for dot in ["🟢", "⚪", "🟡", "🔴"] {
        assert!(text.contains(dot), "the legend explains {dot}:\n{text}");
    }
    assert!(
        text.contains("| Metric | Target | 0.3.1 | 0.3.2 | Change | Target met |"),
        "{text}"
    );
    // GitHub renders markdown inside <details> only after a blank line.
    assert!(
        text.contains("<details>\n<summary>All 11 metrics</summary>\n\n| ID | Metric | Target | 0.3.1 | 0.3.2 | Change | Target met |"),
        "{text}"
    );
    assert!(text.trim_end().ends_with("</details>"), "{text}");
}

#[test]
fn the_summary_lists_the_meta_summary_ids_in_order_without_ids() {
    let text = section();
    let summary = text.split("<details>").next().unwrap();
    let names: Vec<&str> = summary
        .lines()
        .filter(|l| l.starts_with("| ") && !l.starts_with("| Metric") && !l.starts_with("| ---"))
        .map(|l| l.trim_matches('|').split(" | ").next().unwrap().trim())
        .collect();
    assert_eq!(
        names,
        [
            "CPU, window hidden",
            "New file shown (p99)",
            "Disk writes a day"
        ]
    );
}

#[test]
fn the_detail_table_has_every_metric_under_its_group() {
    let text = section();
    let details = text.split("<details>").nth(1).unwrap();
    let groups: Vec<&str> = details
        .lines()
        .filter(|l| l.starts_with("| **"))
        .map(|l| l.trim_matches('|').split(" | ").next().unwrap().trim())
        .collect();
    assert_eq!(
        groups,
        [
            "**Background**",
            "**Memory**",
            "**Search**",
            "**Freshness and start-up**",
            "**Correctness**",
            "**Other**"
        ]
    );
    for id in [
        "B1", "B7", "M1", "S5", "F1", "F4", "F6", "C2", "C3", "O1", "O2",
    ] {
        detail_row(&text, id);
    }
}

#[test]
fn changes_carry_a_dot_by_direction_and_allowance() {
    let text = section();
    // 40.852 -> 10.389, lower is better.
    assert_eq!(detail_row(&text, "B1")[5], "🟢 75% better");
    // 42.45 -> 42.55: within 1%.
    assert_eq!(detail_row(&text, "S5")[5], "⚪ same");
    // 1318.4 -> 1350: 2% worse, within 5%.
    assert_eq!(detail_row(&text, "M1")[5], "🟡 2% worse");
    // 117.429 -> 187.125: 59% worse, past 10% (and the 10 ms floor).
    assert_eq!(detail_row(&text, "F1")[5], "🔴 59% worse");
    // 44057.1 -> 0.
    assert_eq!(detail_row(&text, "B7")[5], "🟢 100% better");
    // true -> true.
    assert_eq!(detail_row(&text, "C2")[5], "⚪ same");
}

#[test]
fn target_met_is_green_yellow_or_red() {
    let text = section();
    assert_eq!(detail_row(&text, "B7")[6], "🟢 Met");
    assert_eq!(detail_row(&text, "B1")[6], "🟡 Not yet");
    // The previous release met the 10 s target (900 ms); this one misses it.
    assert_eq!(detail_row(&text, "F6")[6], "🔴 Missed");
    assert_eq!(detail_row(&text, "O2")[6], "🟡 Not yet (tracked)");
    // No target.
    assert_eq!(detail_row(&text, "O1")[6], "–");
}

#[test]
fn a_metric_without_a_value_says_not_measured() {
    let text = section();
    let row = detail_row(&text, "C3");
    assert_eq!(row[3], "not measured");
    assert_eq!(row[4], "not measured");
    assert_eq!(row[5], "–");
    assert_eq!(row[6], "–");
}

#[test]
fn values_and_targets_read_naturally() {
    let text = section();
    let b1 = detail_row(&text, "B1");
    assert_eq!(&b1[2..5], ["≤1% of a core", "40.9%", "10.4%"]);
    let b7 = detail_row(&text, "B7");
    assert_eq!(&b7[2..5], ["≤512 MB", "44,057 MB", "0 MB"]);
    let s5 = detail_row(&text, "S5");
    assert_eq!(&s5[2..5], ["≤50 ms", "42.5 ms", "42.6 ms"]);
    let f1 = detail_row(&text, "F1");
    assert_eq!(&f1[2..5], ["≤1,000 ms", "117 ms", "187 ms"]);
    // Ten seconds and more read as seconds.
    let f4 = detail_row(&text, "F4");
    assert_eq!(&f4[2..5], ["≤15 s", "19.7 s", "19.0 s"]);
    let c2 = detail_row(&text, "C2");
    assert_eq!(&c2[2..5], ["yes", "yes", "yes"]);
    let o1 = detail_row(&text, "O1");
    assert_eq!(&o1[2..5], ["–", "4,200,412 B", "4,200,468 B"]);
    // The summary row is the same row without its ID.
    assert_eq!(summary_row(&text, "CPU, window hidden"), b1[1..].to_vec());
}

#[test]
fn notes_follow_the_summary_table() {
    let s = Scratch::new("notes");
    let notes = s.write("notes.md", "New files show about 65 ms later.\n");
    let out = release(BASELINE, PREVIOUS, CURRENT, &["--notes", &notes]);
    assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    let text = stdout(&out);
    let summary = text.split("<details>").next().unwrap();
    assert!(
        summary.contains("| Disk writes a day |")
            && summary.contains("New files show about 65 ms later."),
        "{text}"
    );
    let notes_at = text.find("New files show").unwrap();
    let last_summary_row = text.find("| Disk writes a day |").unwrap();
    assert!(notes_at > last_summary_row, "{text}");
}

#[test]
fn a_summary_id_that_is_not_a_metric_is_an_error() {
    let bad = BASELINE.replace("summary = \"B1 F1 B7\"", "summary = \"B1 X9\"");
    let out = release(&bad, PREVIOUS, CURRENT, &[]);
    assert_eq!(out.status.code(), Some(2), "{}", describe(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("X9"),
        "{}",
        describe(&out)
    );
}

#[test]
fn a_baseline_without_a_summary_is_an_error() {
    let bad = BASELINE.replace("summary = \"B1 F1 B7\"\n", "");
    let out = release(&bad, PREVIOUS, CURRENT, &[]);
    assert_eq!(out.status.code(), Some(2), "{}", describe(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("summary"),
        "{}",
        describe(&out)
    );
}

#[test]
fn missing_arguments_are_usage_errors() {
    let s = Scratch::new("args");
    let b = s.write("b.toml", BASELINE);
    let c = s.write("c.txt", CURRENT);
    for args in [
        vec!["perf-release", "--baseline", &b, "--label", "0.3.2", &c],
        vec![
            "perf-release",
            "--baseline",
            &b,
            "--previous",
            &c,
            "--previous-label",
            "0.3.1",
            &c,
        ],
        vec![
            "perf-release",
            "--baseline",
            &b,
            "--previous",
            &c,
            "--previous-label",
            "0.3.1",
            "--label",
            "0.3.2",
        ],
    ] {
        let out = fx_bench(&args);
        assert_eq!(out.status.code(), Some(2), "{args:?}\n{}", describe(&out));
    }
}
