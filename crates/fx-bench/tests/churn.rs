//! `fx-bench churn`: the seeded operation plan (pure, no disk) and a short
//! real run inside a temporary parent folder.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fx_bench::churn::{self, Op};

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
            "fx-bench-churn-{tag}-{}-{}-{nanos}",
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

fn names_in(dir: &Path) -> BTreeSet<String> {
    fs::read_dir(dir)
        .expect("read_dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect()
}

/// Every folder and every file (with its size) under `dir`, as paths
/// relative to it with `/` separators.
fn walk(dir: &Path, prefix: &str, dirs: &mut BTreeSet<String>, files: &mut BTreeMap<String, u64>) {
    for entry in fs::read_dir(dir).expect("read_dir") {
        let entry = entry.expect("entry");
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let meta = fs::symlink_metadata(entry.path()).expect("metadata");
        if meta.is_dir() {
            dirs.insert(rel.clone());
            walk(&entry.path(), &rel, dirs, files);
        } else {
            assert!(meta.is_file(), "{rel} must be a regular file");
            files.insert(rel, meta.len());
        }
    }
}

/// Replays `ops` on an in-memory model, checking each step, and returns
/// the files left with their sizes.
fn replay(ops: &[Op], max_files: usize) -> BTreeMap<String, u64> {
    let mut files = BTreeMap::new();
    for (i, op) in ops.iter().enumerate() {
        match op {
            Op::Create { path, len } => {
                assert!(
                    files.insert(path.clone(), *len as u64).is_none(),
                    "op {i} creates {path}, which exists"
                );
            }
            Op::Rename { from, to } => {
                let len = files
                    .remove(from)
                    .unwrap_or_else(|| panic!("op {i} renames missing {from}"));
                assert!(
                    files.insert(to.clone(), len).is_none(),
                    "op {i} renames onto existing {to}"
                );
            }
            Op::Delete { path } => {
                assert!(
                    files.remove(path).is_some(),
                    "op {i} deletes missing {path}"
                );
            }
        }
        assert!(files.len() <= max_files, "op {i} exceeds {max_files} files");
    }
    files
}

fn value<'a>(text: &'a str, key: &str) -> &'a str {
    let prefix = format!("{key}=");
    text.lines()
        .find_map(|l| l.strip_prefix(&prefix))
        .unwrap_or_else(|| panic!("no {key}= line:\n{text}"))
}

// ---------------------------------------------------------------------------
// The plan
// ---------------------------------------------------------------------------

#[test]
fn the_same_seed_gives_the_same_operations() {
    assert_eq!(churn::plan(1, 2_000, 5_000), churn::plan(1, 2_000, 5_000));
    assert_eq!(churn::folders(1), churn::folders(1));
}

#[test]
fn different_seeds_give_different_operations() {
    assert_ne!(churn::plan(1, 200, 5_000), churn::plan(2, 200, 5_000));
    assert_ne!(churn::folders(1), churn::folders(2));
}

#[test]
fn a_shorter_plan_is_a_prefix_of_a_longer_one() {
    let long = churn::plan(4, 1_000, 50);
    assert_eq!(churn::plan(4, 100, 50), long[..100]);
}

#[test]
fn sixteen_folders_nested_up_to_depth_three() {
    let folders = churn::folders(7);
    assert_eq!(folders.len(), 16);
    let depth = |f: &String| f.split('/').count();
    assert!(folders.iter().all(|f| depth(f) <= 3), "{folders:?}");
    for d in 1..=3 {
        assert!(
            folders.iter().any(|f| depth(f) == d),
            "no folder at depth {d}: {folders:?}"
        );
    }
    for (i, f) in folders.iter().enumerate() {
        if let Some((parent, _)) = f.rsplit_once('/') {
            assert!(
                folders[..i].iter().any(|p| p == parent),
                "{f} must come after its parent"
            );
        }
    }
    let unique: BTreeSet<String> = folders.iter().map(|f| f.to_lowercase()).collect();
    assert_eq!(unique.len(), 16, "folder paths must differ: {folders:?}");
}

#[test]
fn paths_stay_inside_the_folders() {
    let folders = churn::folders(5);
    let ok = |path: &str| {
        let (dir, name) = path.rsplit_once('/').expect("a file is in a folder");
        assert!(
            folders.iter().any(|f| f == dir),
            "{path} is not in a folder"
        );
        assert!(
            !name.is_empty() && name != "." && name != ".." && !name.contains('\0'),
            "bad name in {path}"
        );
        assert!(!name.starts_with('.'), "{path} must not be hidden");
    };
    for op in churn::plan(5, 3_000, 200) {
        match op {
            Op::Create { path, .. } | Op::Delete { path } => ok(&path),
            Op::Rename { from, to } => {
                ok(&from);
                ok(&to);
            }
        }
    }
}

#[test]
fn the_mix_is_half_creates_and_a_quarter_each_renames_and_deletes() {
    let ops = churn::plan(3, 20_000, 1_000_000);
    let share = |f: fn(&Op) -> bool| ops.iter().filter(|op| f(op)).count() as f64 / 20_000.0;
    let creates = share(|op| matches!(op, Op::Create { .. }));
    let renames = share(|op| matches!(op, Op::Rename { .. }));
    let deletes = share(|op| matches!(op, Op::Delete { .. }));
    assert!((0.47..=0.53).contains(&creates), "creates {creates}");
    assert!((0.22..=0.28).contains(&renames), "renames {renames}");
    assert!((0.22..=0.28).contains(&deletes), "deletes {deletes}");
    assert!(
        ops.iter()
            .any(|op| matches!(op, Op::Create { len, .. } if *len == 0)),
        "some files are empty"
    );
    assert!(
        ops.iter()
            .any(|op| matches!(op, Op::Create { len, .. } if *len > 0)),
        "some files hold a few bytes"
    );
}

#[test]
fn the_plan_replays_and_respects_max_files() {
    let ops = churn::plan(11, 3_000, 10);
    let left = replay(&ops, 10);
    assert!(left.len() <= 10);
    // With a cap of 10 the stream must have turned creates into deletes.
    let deletes = ops
        .iter()
        .filter(|op| matches!(op, Op::Delete { .. }))
        .count();
    assert!(deletes > 600, "deletes {deletes}");
}

#[test]
fn renames_move_files_within_and_between_folders() {
    let ops = churn::plan(6, 2_000, 5_000);
    let folder = |p: &str| p.rsplit_once('/').unwrap().0.to_owned();
    let (mut within, mut between) = (0, 0);
    for op in &ops {
        if let Op::Rename { from, to } = op {
            if folder(from) == folder(to) {
                within += 1;
            } else {
                between += 1;
            }
        }
    }
    assert!(
        within > 0 && between > 0,
        "within {within}, between {between}"
    );
}

// ---------------------------------------------------------------------------
// The command
// ---------------------------------------------------------------------------

#[test]
fn a_short_run_leaves_only_new_dir_and_follows_the_plan() {
    let parent = Scratch::new("run");
    let dir = parent.path().join("churn");
    let out = fx_bench(&[
        "churn",
        "--dir",
        dir.to_str().unwrap(),
        "--duration",
        "1",
        "--rate",
        "50",
        "--seed",
        "9",
    ]);
    assert!(out.status.success(), "{}", describe(&out));
    let text = String::from_utf8_lossy(&out.stdout).into_owned();

    let ops: usize = value(&text, "churn_ops").parse().expect("churn_ops");
    let seconds: f64 = value(&text, "churn_seconds")
        .parse()
        .expect("churn_seconds");
    let rate: f64 = value(&text, "churn_rate").parse().expect("churn_rate");
    assert!((1..=51).contains(&ops), "ops {ops}");
    assert!((0.9..=5.0).contains(&seconds), "seconds {seconds}");
    // Both are printed to 3 decimals, so allow for that rounding.
    assert!(
        (rate - ops as f64 / seconds).abs() <= 0.005 * rate.max(1.0),
        "rate {rate} for {ops} ops in {seconds} s"
    );

    assert_eq!(
        names_in(parent.path()),
        BTreeSet::from(["churn".to_owned()])
    );

    let mut dirs = BTreeSet::new();
    let mut files = BTreeMap::new();
    walk(&dir, "", &mut dirs, &mut files);
    let want_dirs: BTreeSet<String> = churn::folders(9).into_iter().collect();
    assert_eq!(dirs, want_dirs);
    let planned = churn::plan(9, ops, 5_000);
    let want_files = replay(&planned, 5_000);
    assert_eq!(
        files, want_files,
        "the disk must hold what {ops} planned ops leave"
    );

    // The counts by kind, which `fx-bench saves` projects index saves from.
    let count = |kind: fn(&Op) -> bool| planned.iter().filter(|op| kind(op)).count();
    let creates = count(|op| matches!(op, Op::Create { .. }));
    let renames = count(|op| matches!(op, Op::Rename { .. }));
    let deletes = count(|op| matches!(op, Op::Delete { .. }));
    assert_eq!(value(&text, "churn_creates"), creates.to_string());
    assert_eq!(value(&text, "churn_renames"), renames.to_string());
    assert_eq!(value(&text, "churn_deletes"), deletes.to_string());
    assert_eq!(creates + renames + deletes, ops);
}

#[test]
fn stats_count_each_kind_of_operation() {
    let mut stats = churn::ChurnStats::default();
    for op in churn::plan(3, 400, 50) {
        stats.record(&op);
    }
    assert_eq!(stats.ops, 400);
    assert_eq!(stats.creates + stats.renames + stats.deletes, 400);
    let planned = churn::plan(3, 400, 50);
    let creates = planned
        .iter()
        .filter(|op| matches!(op, Op::Create { .. }))
        .count() as u64;
    let deletes = planned
        .iter()
        .filter(|op| matches!(op, Op::Delete { .. }))
        .count() as u64;
    assert_eq!((stats.creates, stats.deletes), (creates, deletes));
}

#[test]
fn churn_refuses_an_existing_folder() {
    let parent = Scratch::new("exists");
    let dir = parent.path().join("churn");
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("keep.txt"), "keep").unwrap();
    let out = fx_bench(&["churn", "--dir", dir.to_str().unwrap(), "--duration", "1"]);
    assert_eq!(out.status.code(), Some(2), "{}", describe(&out));
    assert_eq!(names_in(&dir), BTreeSet::from(["keep.txt".to_owned()]));
    assert_eq!(fs::read_to_string(dir.join("keep.txt")).unwrap(), "keep");
}

#[test]
fn churn_refuses_a_missing_parent() {
    let parent = Scratch::new("no-parent");
    let dir = parent.path().join("missing").join("churn");
    let out = fx_bench(&["churn", "--dir", dir.to_str().unwrap(), "--duration", "1"]);
    assert_eq!(out.status.code(), Some(2), "{}", describe(&out));
    assert!(names_in(parent.path()).is_empty());
}

#[test]
fn churn_bad_arguments_exit_2_with_usage() {
    let parent = Scratch::new("args");
    let dir = parent.path().join("churn");
    let dir = dir.to_str().unwrap();
    for args in [
        vec!["churn", "--duration", "1"],
        vec!["churn", "--dir", dir],
        vec!["churn", "--dir", dir, "--duration", "0"],
        vec!["churn", "--dir", dir, "--duration", "1", "--rate", "0"],
        vec!["churn", "--dir", dir, "--duration", "1", "--rate", "fast"],
        vec!["churn", "--dir", dir, "--duration", "1", "--max-files", "0"],
        vec!["churn", "--dir", dir, "--duration", "1", "--seed", "x"],
        vec!["churn", "--dir", dir, "--duration", "1", "extra"],
    ] {
        let out = fx_bench(&args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", describe(&out));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("usage"),
            "{args:?}: {}",
            describe(&out)
        );
    }
    assert!(names_in(parent.path()).is_empty());
}
