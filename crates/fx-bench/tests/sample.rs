//! `fx-bench sample`: arguments, the output folder, and short real runs on
//! macOS (the test process itself, and a child that exits early). The
//! interval and summary arithmetic is unit-tested in `src/sample.rs`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

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
            "fx-bench-sample-{tag}-{}-{}-{nanos}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).expect("create scratch dir");
        Scratch(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn sample_bad_arguments_exit_2_with_usage() {
    let s = Scratch::new("args");
    let out_dir = s.path().join("run");
    let out_dir = out_dir.to_str().unwrap();
    let pid = std::process::id().to_string();
    for args in [
        vec!["sample", "--duration", "2", "--out", out_dir],
        vec!["sample", "--pid", &pid, "--out", out_dir],
        vec!["sample", "--pid", &pid, "--duration", "2"],
        vec!["sample", "--pid", "0", "--duration", "2", "--out", out_dir],
        vec![
            "sample",
            "--pid",
            "abc",
            "--duration",
            "2",
            "--out",
            out_dir,
        ],
        vec!["sample", "--pid", &pid, "--duration", "0", "--out", out_dir],
        vec![
            "sample",
            "--pid",
            &pid,
            "--duration",
            "2",
            "--interval",
            "0",
            "--out",
            out_dir,
        ],
        vec![
            "sample",
            "--pid",
            &pid,
            "--duration",
            "2",
            "--label",
            "App!",
            "--out",
            out_dir,
        ],
        vec![
            "sample",
            "--pid",
            &pid,
            "--duration",
            "2",
            "--label",
            "",
            "--out",
            out_dir,
        ],
        vec![
            "sample",
            "--pid",
            &pid,
            "--duration",
            "2",
            "--burst-pct",
            "lots",
            "--out",
            out_dir,
        ],
        vec![
            "sample",
            "--pid",
            &pid,
            "--duration",
            "2",
            "--out",
            out_dir,
            "extra",
        ],
    ] {
        let out = fx_bench(&args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", describe(&out));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("usage"),
            "{args:?}: {}",
            describe(&out)
        );
    }
    assert!(
        !s.path().join("run").exists(),
        "no output folder is created"
    );
}

#[test]
fn sample_refuses_an_existing_output_folder() {
    let s = Scratch::new("exists");
    let out_dir = s.path().join("run");
    fs::create_dir(&out_dir).unwrap();
    fs::write(out_dir.join("keep.txt"), "keep").unwrap();
    let pid = std::process::id().to_string();
    let out = fx_bench(&[
        "sample",
        "--pid",
        &pid,
        "--duration",
        "1",
        "--interval",
        "1",
        "--out",
        out_dir.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(2), "{}", describe(&out));
    let names: Vec<_> = fs::read_dir(&out_dir).unwrap().collect();
    assert_eq!(names.len(), 1);
    assert_eq!(
        fs::read_to_string(out_dir.join("keep.txt")).unwrap(),
        "keep"
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn sample_is_macos_only() {
    let s = Scratch::new("other-os");
    let out_dir = s.path().join("run");
    let pid = std::process::id().to_string();
    let out = fx_bench(&[
        "sample",
        "--pid",
        &pid,
        "--duration",
        "1",
        "--out",
        out_dir.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(2), "{}", describe(&out));
    assert!(String::from_utf8_lossy(&out.stderr).contains("macOS"));
    assert!(!out_dir.exists());
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;

    /// The value of the `key=value` line `key` in `text`.
    fn value<'a>(text: &'a str, key: &str) -> &'a str {
        let prefix = format!("{key}=");
        text.lines()
            .find_map(|l| l.strip_prefix(&prefix))
            .unwrap_or_else(|| panic!("no {key}= line:\n{text}"))
    }

    fn number(text: &str, key: &str) -> f64 {
        value(text, key)
            .parse()
            .unwrap_or_else(|_| panic!("{key} is not a number:\n{text}"))
    }

    /// Samples this test process for about 2 s at a 1 s interval.
    #[test]
    fn sampling_the_test_process_writes_csvs_and_metrics() {
        let s = Scratch::new("self");
        let out_dir = s.path().join("run");
        let pid = std::process::id().to_string();
        let out = fx_bench(&[
            "sample",
            "--pid",
            &pid,
            "--duration",
            "2",
            "--interval",
            "1",
            "--label",
            "test",
            "--out",
            out_dir.to_str().unwrap(),
        ]);
        assert!(out.status.success(), "{}", describe(&out));
        let text = String::from_utf8_lossy(&out.stdout).into_owned();

        for key in [
            "cpu_pct",
            "cpu_max_interval_pct",
            "energy_mw",
            "wakeups_per_s",
            "disk_written_bytes",
            "disk_written_per_day_bytes",
            "footprint_bytes",
            "footprint_max_bytes",
            "duration_s",
            "samples",
        ] {
            let v = number(&text, &format!("test_{key}"));
            assert!(v >= 0.0, "test_{key}={v}");
        }
        assert!(!text.contains("process_exited"), "{text}");
        assert_eq!(number(&text, "test_samples"), 3.0, "{text}");
        let duration = number(&text, "test_duration_s");
        assert!((1.5..=4.0).contains(&duration), "{text}");
        assert!(number(&text, "test_footprint_bytes") > 0.0, "{text}");
        assert!(
            number(&text, "test_footprint_max_bytes") >= number(&text, "test_footprint_bytes"),
            "{text}"
        );
        // Every stdout line is a metric line.
        assert!(
            text.lines()
                .all(|l| l.starts_with("test_") && l.contains('=')),
            "{text}"
        );

        let metrics = fs::read_to_string(out_dir.join("metrics.txt")).expect("metrics.txt");
        assert_eq!(metrics, text);

        let rusage = fs::read_to_string(out_dir.join("rusage.csv")).expect("rusage.csv");
        let lines: Vec<&str> = rusage.lines().collect();
        assert_eq!(lines.len(), 4, "a header and 3 samples:\n{rusage}");
        assert!(lines[0].starts_with("epoch_s,elapsed_s,"), "{rusage}");
        let columns = lines[0].split(',').count();
        assert!(lines.iter().all(|l| l.split(',').count() == columns));

        let intervals = fs::read_to_string(out_dir.join("intervals.csv")).expect("intervals.csv");
        let lines: Vec<&str> = intervals.lines().collect();
        assert_eq!(lines.len(), 3, "a header and 2 intervals:\n{intervals}");
        assert_eq!(
            lines[0],
            "start_s,seconds,cpu_pct,energy_mw,wakeups_per_s,disk_read_bytes,\
             disk_written_bytes,footprint_bytes"
        );
    }

    #[test]
    fn a_process_that_exits_ends_the_run_early() {
        let s = Scratch::new("exits");
        let out_dir = s.path().join("run");
        let mut child = Command::new("/bin/sleep")
            .arg("1")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id().to_string();
        let out = fx_bench(&[
            "sample",
            "--pid",
            &pid,
            "--duration",
            "20",
            "--interval",
            "1",
            "--out",
            out_dir.to_str().unwrap(),
        ]);
        let _ = child.wait();
        assert!(out.status.success(), "{}", describe(&out));
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert_eq!(value(&text, "app_process_exited"), "true", "{text}");
        assert!(number(&text, "app_duration_s") < 5.0, "{text}");
        let metrics = fs::read_to_string(out_dir.join("metrics.txt")).expect("metrics.txt");
        assert!(metrics.contains("app_process_exited=true"));
    }

    #[test]
    fn a_missing_process_exits_1_without_creating_the_folder() {
        let mut child = Command::new("/usr/bin/true").spawn().expect("spawn true");
        let pid = child.id().to_string();
        child.wait().expect("reap true");
        let s = Scratch::new("missing");
        let out_dir = s.path().join("run");
        let out = fx_bench(&[
            "sample",
            "--pid",
            &pid,
            "--duration",
            "1",
            "--out",
            out_dir.to_str().unwrap(),
        ]);
        assert_eq!(out.status.code(), Some(1), "{}", describe(&out));
        assert!(!out_dir.exists());
    }
}
