//! `cleanup_stale_temps(target)` is scoped to one checkpoint: it removes
//! only `temp_name_prefix(target)` + `<pid>.<counter>` files in the
//! target's directory, older than `STALE_TEMP_MIN_AGE`, and never temps of
//! this process. `cleanup_stale_temps_older_than` (hidden, for tests) takes
//! the age threshold as a parameter.
//!
//! Clean-room: written from docs/PLAN.md and the in-repo code only.

#![allow(clippy::needless_borrows_for_generic_args)]

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fx_store::{
    STALE_TEMP_MIN_AGE, cleanup_stale_temps, cleanup_stale_temps_older_than, temp_name_prefix,
};

/// A unique directory under the system temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "fx-store-cleanup-{tag}-{}-{n}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        TempDir(dir)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn entries(&self) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(&self.0)
            .expect("read temp dir")
            .map(|e| {
                e.expect("dir entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        v.sort();
        v
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn temp_name(target: &Path, pid: u32, n: u64) -> String {
    let mut name: OsString = temp_name_prefix(target).expect("temp prefix");
    name.push(format!("{pid}.{n}"));
    name.to_string_lossy().into_owned()
}

/// A process ID that is not this process.
fn other_pid() -> u32 {
    let me = std::process::id();
    if me == 999_983 { 999_979 } else { 999_983 }
}

/// Writes a file whose modification time is `age` in the past.
fn write_aged(path: &Path, age: Duration) {
    fs::write(path, b"partial checkpoint").expect("write");
    fs::File::options()
        .write(true)
        .open(path)
        .and_then(|f| f.set_modified(SystemTime::now() - age))
        .expect("set mtime");
}

fn old() -> Duration {
    STALE_TEMP_MIN_AGE + Duration::from_secs(3600)
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[test]
fn threshold_is_ten_minutes() {
    assert_eq!(STALE_TEMP_MIN_AGE, Duration::from_secs(600));
}

#[test]
fn other_targets_temps_are_kept() {
    let dir = TempDir::new("scope");
    let target = dir.file("index.fxs");
    let pid = other_pid();
    let mine = temp_name(&target, pid, 1);
    let other_target = temp_name(&dir.file("other.fxs"), pid, 2);
    // A target whose name extends ours ("index.fxs.tmp") and one ours
    // extends ("index").
    let longer = temp_name(&dir.file("index.fxs.tmp"), pid, 3);
    let shorter = temp_name(&dir.file("index"), pid, 4);
    for name in [&mine, &other_target, &longer, &shorter] {
        write_aged(&dir.file(name), old());
    }

    let removed = cleanup_stale_temps(&target).expect("cleanup");
    assert_eq!(removed, 1);
    assert_eq!(
        dir.entries(),
        sorted(vec![other_target.clone(), longer.clone(), shorter.clone()])
    );

    // Cleaning for the other target removes only its own temp.
    let removed = cleanup_stale_temps(dir.file("other.fxs")).expect("cleanup other");
    assert_eq!(removed, 1);
    assert_eq!(dir.entries(), sorted(vec![longer, shorter]));
}

#[test]
fn fresh_temps_are_kept_by_the_age_threshold() {
    let dir = TempDir::new("age");
    let target = dir.file("index.fxs");
    let pid = other_pid();
    let fresh = temp_name(&target, pid, 1);
    let recent = temp_name(&target, pid, 2);
    let stale = temp_name(&target, pid, 3);
    write_aged(&dir.file(&fresh), Duration::ZERO);
    write_aged(&dir.file(&recent), Duration::from_secs(60));
    write_aged(&dir.file(&stale), old());

    assert_eq!(cleanup_stale_temps(&target).expect("cleanup"), 1);
    assert_eq!(dir.entries(), sorted(vec![fresh.clone(), recent.clone()]));

    // A custom threshold: 30 s removes the one-minute-old temp only.
    let removed =
        cleanup_stale_temps_older_than(&target, Duration::from_secs(30)).expect("cleanup 30s");
    assert_eq!(removed, 1);
    assert_eq!(dir.entries(), vec![fresh.clone()]);

    // Zero removes regardless of age.
    let removed = cleanup_stale_temps_older_than(&target, Duration::ZERO).expect("cleanup 0");
    assert_eq!(removed, 1);
    assert!(dir.entries().is_empty());
}

#[test]
fn future_mtime_counts_as_fresh() {
    let dir = TempDir::new("future");
    let target = dir.file("index.fxs");
    let name = temp_name(&target, other_pid(), 1);
    let path = dir.file(&name);
    fs::write(&path, b"x").expect("write");
    fs::File::options()
        .write(true)
        .open(&path)
        .and_then(|f| f.set_modified(SystemTime::now() + Duration::from_secs(3600)))
        .expect("set mtime");
    assert_eq!(cleanup_stale_temps(&target).expect("cleanup"), 0);
    assert_eq!(dir.entries(), vec![name]);
}

#[test]
fn own_process_temps_and_non_files_are_kept() {
    let dir = TempDir::new("own");
    let target = dir.file("index.fxs");
    let own = temp_name(&target, std::process::id(), 1);
    write_aged(&dir.file(&own), old());
    let as_dir = temp_name(&target, other_pid(), 2);
    fs::create_dir(dir.file(&as_dir)).expect("mkdir");

    let removed = cleanup_stale_temps_older_than(&target, Duration::ZERO).expect("cleanup");
    assert_eq!(removed, 0);
    assert_eq!(dir.entries(), sorted(vec![own, as_dir]));
}

#[test]
fn malformed_names_are_kept() {
    let dir = TempDir::new("names");
    let target = dir.file("index.fxs");
    let pid = other_pid();
    let names = [
        format!(".index.fxs.tmp.{pid}"),
        format!(".index.fxs.tmp.{pid}.1.2"),
        format!(".index.fxs.tmp.{pid}.x"),
        format!("index.fxs.tmp.{pid}.1"),
        "index.fxs".to_string(),
    ];
    for name in &names {
        write_aged(&dir.file(name), old());
    }
    let removed = cleanup_stale_temps_older_than(&target, Duration::ZERO).expect("cleanup");
    assert_eq!(removed, 0);
    assert_eq!(dir.entries(), sorted(names.to_vec()));
}

#[test]
fn target_without_file_name_is_invalid_input() {
    let err = cleanup_stale_temps(Path::new("/")).expect_err("no file name");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}
