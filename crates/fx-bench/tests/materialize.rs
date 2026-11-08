//! On-disk materialization of synthetic trees.
//!
//! `materialize(tree, root, limit)` creates entry `i` of the tree at
//! `root.join(tree.path(i))` (so entry 0 becomes `root/root`), directories
//! as directories and files as empty files, for the first `limit` entries.
//! These tests only ever delete the unique scratch directories they create
//! themselves under `std::env::temp_dir()`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fx_bench::{Tree, TreeShape, generate_tree, materialize};

/// A fresh, unique, empty scratch directory removed on drop (also when the
/// test panics). Only this directory is ever deleted.
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
            "fx-bench-materialize-test-{tag}-{}-{}-{nanos}",
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

#[derive(Debug, Default)]
struct Walk {
    /// Relative paths (components joined by '/') of every entry found.
    paths: BTreeSet<String>,
    dirs: BTreeSet<String>,
    files: BTreeSet<String>,
    /// Files with a non-zero length.
    non_empty_files: Vec<String>,
    /// Entries that are neither plain files nor directories (e.g. symlinks).
    other: Vec<String>,
}

fn walk(root: &Path) -> Walk {
    let mut out = Walk::default();
    walk_into(root, "", &mut out);
    out
}

fn walk_into(dir: &Path, prefix: &str, out: &mut Walk) {
    for entry in fs::read_dir(dir).expect("read_dir") {
        let entry = entry.expect("dir entry");
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let ft = entry.file_type().expect("file type");
        assert!(out.paths.insert(rel.clone()), "duplicate path {rel}");
        if ft.is_dir() {
            out.dirs.insert(rel.clone());
            walk_into(&entry.path(), &rel, out);
        } else if ft.is_file() {
            let len = entry.metadata().expect("metadata").len();
            if len != 0 {
                out.non_empty_files.push(rel.clone());
            }
            out.files.insert(rel);
        } else {
            out.other.push(rel);
        }
    }
}

/// Checks that `root` holds exactly the first `n` entries of `tree`.
fn assert_matches_prefix(tree: &Tree, root: &Path, n: usize) {
    let w = walk(root);
    let expected_dirs = (0..n).filter(|&i| tree.is_dir[i]).count();

    assert!(
        w.other.is_empty(),
        "unexpected special entries: {:?}",
        w.other
    );
    assert!(
        w.non_empty_files.is_empty(),
        "files must be empty: {:?}",
        w.non_empty_files
    );
    assert_eq!(w.paths.len(), n, "entry count on disk");
    assert_eq!(w.dirs.len(), expected_dirs, "directory count on disk");
    assert_eq!(w.files.len(), n - expected_dirs, "file count on disk");

    for i in 0..n {
        let p = tree.path(i);
        if tree.is_dir[i] {
            assert!(w.dirs.contains(&p), "missing directory {i}: {p}");
        } else {
            assert!(w.files.contains(&p), "missing file {i}: {p}");
        }
    }
    let expected: BTreeSet<String> = (0..n).map(|i| tree.path(i)).collect();
    assert_eq!(w.paths, expected, "on-disk paths must equal Tree::path");
}

#[test]
fn materializes_2000_entries_matching_tree_paths() {
    let tree = generate_tree(2_000, 42, TreeShape::default());
    let scratch = Scratch::new("full");

    materialize(&tree, &scratch.path, None).expect("materialize 2,000 entries");

    assert_matches_prefix(&tree, &scratch.path, 2_000);
}

#[test]
fn limit_materializes_a_valid_prefix() {
    let tree = generate_tree(5_000, 7, TreeShape::default());
    let scratch = Scratch::new("prefix");

    materialize(&tree, &scratch.path, Some(2_000)).expect("materialize first 2,000 entries");

    assert_matches_prefix(&tree, &scratch.path, 2_000);
}

#[test]
fn tiny_limits_work() {
    let tree = generate_tree(100, 3, TreeShape::default());

    let one = Scratch::new("one");
    materialize(&tree, &one.path, Some(1)).expect("materialize root only");
    assert_matches_prefix(&tree, &one.path, 1);

    let zero = Scratch::new("zero");
    materialize(&tree, &zero.path, Some(0)).expect("materialize nothing");
    assert_matches_prefix(&tree, &zero.path, 0);
}

#[test]
fn refuses_a_non_empty_directory_and_leaves_it_untouched() {
    let tree = generate_tree(200, 1, TreeShape::default());
    let scratch = Scratch::new("nonempty");
    let keep = scratch.path.join("keep.txt");
    fs::write(&keep, b"do not touch").expect("write marker file");

    let result = materialize(&tree, &scratch.path, None);
    assert!(result.is_err(), "materialize must refuse a non-empty dir");

    let names: Vec<_> = fs::read_dir(&scratch.path)
        .expect("read_dir")
        .map(|e| e.expect("entry").file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from("keep.txt")]);
    assert_eq!(fs::read(&keep).expect("read marker"), b"do not touch");
}

#[test]
fn refuses_a_missing_directory_without_creating_it() {
    let tree = generate_tree(50, 1, TreeShape::default());
    let scratch = Scratch::new("missing");
    let missing = scratch.path.join("does-not-exist");

    let result = materialize(&tree, &missing, None);
    assert!(result.is_err(), "materialize must refuse a missing root");
    assert!(!missing.exists(), "missing root must not be created");
}

#[test]
fn refuses_a_file_as_root() {
    let tree = generate_tree(50, 1, TreeShape::default());
    let scratch = Scratch::new("fileroot");
    let file = scratch.path.join("plain-file");
    fs::write(&file, b"x").expect("write file");

    let result = materialize(&tree, &file, None);
    assert!(result.is_err(), "materialize must refuse a file as root");
    assert_eq!(fs::read(&file).expect("read file"), b"x");
}
