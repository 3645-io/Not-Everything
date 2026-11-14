//! The soak's operation planner (`fx_testkit::soak`).
//!
//! The engine's soak (`crates/fx-engine/examples/soak.rs`) and fuzz test
//! (`crates/fx-engine/tests/fuzz.rs`) trust the planner to describe only
//! operations that can succeed, and to know exactly what the tree holds
//! afterwards. These tests check that:
//!
//! - a seed always gives the same steps, and different seeds differ;
//! - every operation is valid against an independent model of the tree
//!   kept here (parents exist, new names are free, folders never move into
//!   themselves), and the planner's own listing matches that model after
//!   every step;
//! - the entry cap and the depth cap always hold, and no two entries in a
//!   folder have names that differ only in letter case (the volume may be
//!   case-insensitive);
//! - every kind of operation and step is drawn;
//! - `apply` on a real folder leaves it equal to the model, and refuses to
//!   replace anything the planner did not make;
//! - `diff` and `parse_duration` behave as documented.
//!
//! Only `std` and the crate itself are used.

mod common;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::Path;
use std::time::Duration;

use common::shell::temp_path;
use fx_testkit::soak::{
    self, Kind, Limits, Listing, NewEntry, Op, Planner, Step, StepKind, diff, parse_duration,
};

/// An independent model of the tree: relative path to kind and length.
#[derive(Default)]
struct Model {
    entries: BTreeMap<String, (Kind, u64)>,
}

fn parent_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(p, _)| p)
}

fn name_of(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, n)| n)
}

impl Model {
    fn is_dir(&self, path: &str) -> bool {
        path.is_empty() || self.entries.get(path).is_some_and(|e| e.0 == Kind::Dir)
    }

    fn is_file(&self, path: &str) -> bool {
        self.entries.get(path).is_some_and(|e| e.0 == Kind::File)
    }

    /// `path` and every path below it.
    fn subtree(&self, path: &str) -> Vec<String> {
        let below = format!("{path}/");
        self.entries
            .keys()
            .filter(|k| *k == path || k.starts_with(&below))
            .cloned()
            .collect()
    }

    fn assert_free(&self, path: &str, op: &Op) {
        assert!(!self.entries.contains_key(path), "{op:?}: {path} is taken");
        assert!(
            self.is_dir(parent_of(path)),
            "{op:?}: the parent of {path} is not a folder"
        );
    }

    fn remove(&mut self, path: &str, dir: bool, op: &Op) {
        let kind = self.entries.get(path).map(|e| e.0);
        let want = if dir { Kind::Dir } else { Kind::File };
        assert_eq!(kind, Some(want), "{op:?}: {path} is not a {want:?}");
        for p in self.subtree(path) {
            self.entries.remove(&p);
        }
    }

    fn apply(&mut self, op: &Op) {
        match op {
            Op::Mkdir { path } => {
                self.assert_free(path, op);
                self.entries.insert(path.clone(), (Kind::Dir, 0));
            }
            Op::Create { path, len } => {
                self.assert_free(path, op);
                self.entries.insert(path.clone(), (Kind::File, *len as u64));
            }
            Op::Write { path, len } => {
                assert!(self.is_file(path), "{op:?}: not a file");
                self.entries.insert(path.clone(), (Kind::File, *len as u64));
            }
            Op::Replace { tmp, path, len } => {
                assert!(self.is_file(path), "{op:?}: not a file");
                self.assert_free(tmp, op);
                assert_eq!(
                    parent_of(tmp),
                    parent_of(path),
                    "{op:?}: temp file elsewhere"
                );
                self.entries.insert(path.clone(), (Kind::File, *len as u64));
            }
            Op::Rename { from, to, dir } => {
                let kind = self.entries.get(from).map(|e| e.0);
                let want = if *dir { Kind::Dir } else { Kind::File };
                assert_eq!(kind, Some(want), "{op:?}: {from} is not a {want:?}");
                let case_only = from.to_lowercase() == to.to_lowercase();
                if !case_only {
                    self.assert_free(to, op);
                }
                assert!(
                    !to.starts_with(&format!("{from}/")),
                    "{op:?}: a folder moved into itself"
                );
                let moved: Vec<(String, (Kind, u64))> = self
                    .subtree(from)
                    .into_iter()
                    .map(|p| {
                        let v = self.entries.remove(&p).unwrap();
                        (format!("{to}{}", &p[from.len()..]), v)
                    })
                    .collect();
                self.entries.extend(moved);
            }
            Op::Delete { path, dir } => self.remove(path, *dir, op),
            Op::Import { to, entries } => {
                self.assert_free(to, op);
                self.entries.insert(to.clone(), (Kind::Dir, 0));
                for e in entries {
                    let full = format!("{to}/{}", e.path);
                    self.assert_free(&full, op);
                    let len = if e.kind == Kind::File {
                        e.len as u64
                    } else {
                        0
                    };
                    self.entries.insert(full, (e.kind, len));
                }
            }
            Op::Export { path, dir } => self.remove(path, *dir, op),
        }
    }

    fn listing(&self) -> Listing {
        self.entries.clone()
    }
}

fn limits(max_entries: usize) -> Limits {
    Limits::new(max_entries)
}

fn steps(seed: u64, max_entries: usize, n: usize) -> Vec<Step> {
    let mut p = Planner::new(seed, limits(max_entries));
    (0..n).map(|_| p.next_step()).collect()
}

#[test]
fn a_seed_always_gives_the_same_steps() {
    assert_eq!(steps(7, 500, 2_000), steps(7, 500, 2_000));
    assert_ne!(steps(7, 500, 200), steps(8, 500, 200));
}

#[test]
fn every_operation_is_valid_and_the_listing_follows() {
    for (seed, cap) in [(1, 40), (2, 300), (3, 2_000), (4, 12), (5, 300)] {
        let mut planner = Planner::new(seed, limits(cap));
        let mut model = Model::default();
        for i in 0..4_000 {
            let step = planner.next_step();
            assert!(!step.ops.is_empty(), "seed {seed} step {i}: no operations");
            for op in &step.ops {
                model.apply(op);
            }
            assert_eq!(
                planner.listing(),
                model.listing(),
                "seed {seed} step {i} ({:?}): the planner's listing differs from the model",
                step.kind
            );
            assert_eq!(planner.live_entries(), model.entries.len());
        }
    }
}

#[test]
fn caps_hold_and_names_stay_apart_in_any_letter_case() {
    for (seed, cap) in [(11, 25), (12, 400)] {
        let lim = limits(cap);
        let mut planner = Planner::new(seed, lim);
        for i in 0..5_000 {
            planner.next_step();
            let listing = planner.listing();
            assert!(
                listing.len() <= lim.max_entries,
                "seed {seed} step {i}: {} entries, cap {}",
                listing.len(),
                lim.max_entries
            );
            let mut seen: HashMap<&str, BTreeSet<String>> = HashMap::new();
            for path in listing.keys() {
                let depth = path.split('/').count();
                assert!(depth <= lim.max_depth, "{path} is {depth} deep");
                assert!(
                    seen.entry(parent_of(path))
                        .or_default()
                        .insert(name_of(path).to_lowercase()),
                    "seed {seed} step {i}: two names like {path} in one folder"
                );
                assert!(!name_of(path).starts_with('.'), "{path} is hidden");
            }
        }
    }
}

/// The soak needs a tree near its cap (compaction, long sort orders), so
/// whole-folder deletes must not keep the tree small. The first planner,
/// which deleted any folder, had at most 924 entries after 3,000 steps with
/// a cap of 10,000 (seeds 1, 51 and 54); this one has over 2,000. Then the
/// tree reaches 90% of its cap and stays above half of it on average.
#[test]
fn the_tree_grows_towards_its_cap_and_stays_near_it() {
    // (seed, cap, steps, at least this many entries after them)
    for (seed, cap, steps, least) in [
        (51, 10_000, 3_000, 1_500),
        (54, 10_000, 3_000, 1_500),
        (52, 2_000, 2_000, 1_000),
        (53, 300, 1_000, 200),
    ] {
        let mut planner = Planner::new(seed, limits(cap));
        for _ in 0..steps {
            planner.next_step();
        }
        assert!(
            planner.live_entries() >= least,
            "seed {seed}: {} entries after {steps} steps, cap {cap}",
            planner.live_entries()
        );
        let mut more = 0;
        while planner.live_entries() * 10 < cap * 9 {
            planner.next_step();
            more += 1;
            assert!(
                more < cap * 2,
                "seed {seed}: {} entries after {} steps, cap {cap}",
                planner.live_entries(),
                steps + more
            );
        }
        let after = 4_000;
        let mut sum = 0;
        for _ in 0..after {
            planner.next_step();
            sum += planner.live_entries();
        }
        assert!(
            sum / after * 2 >= cap,
            "seed {seed}: {} entries on average after reaching the cap of {cap}",
            sum / after
        );
    }
}

/// A short name for each operation kind, for the coverage check.
fn op_label(op: &Op, case_only: bool) -> &'static str {
    match op {
        Op::Mkdir { .. } => "mkdir",
        Op::Create { .. } => "create",
        Op::Write { .. } => "write",
        Op::Replace { .. } => "replace",
        Op::Rename { from, to, dir } => {
            let across = parent_of(from) != parent_of(to);
            match (dir, across, case_only) {
                (_, _, true) => "case rename",
                (false, false, _) => "file rename",
                (false, true, _) => "file move",
                (true, false, _) => "folder rename",
                (true, true, _) => "folder move",
            }
        }
        Op::Delete { dir: false, .. } => "file delete",
        Op::Delete { dir: true, .. } => "folder delete",
        Op::Import { .. } => "import",
        Op::Export { .. } => "export",
    }
}

#[test]
fn every_kind_of_operation_and_step_is_drawn() {
    let mut ops = BTreeSet::new();
    let mut kinds = BTreeSet::new();
    let mut recreated_as_other_kind = false;
    for step in steps(21, 1_000, 6_000) {
        kinds.insert(format!("{:?}", step.kind));
        for op in &step.ops {
            let case_only = matches!(op, Op::Rename { from, to, .. }
                if from != to && from.to_lowercase() == to.to_lowercase());
            ops.insert(op_label(op, case_only));
        }
        if step.kind == StepKind::Recreate {
            if let [Op::Delete { dir, .. }, second] = step.ops.as_slice() {
                let made_dir = matches!(second, Op::Mkdir { .. });
                recreated_as_other_kind |= *dir != made_dir;
            }
        }
    }
    for want in [
        "mkdir",
        "create",
        "write",
        "replace",
        "case rename",
        "file rename",
        "file move",
        "folder rename",
        "folder move",
        "file delete",
        "folder delete",
        "import",
        "export",
    ] {
        assert!(ops.contains(want), "no {want} drawn; got {ops:?}");
    }
    for want in [
        StepKind::Single,
        StepKind::Burst,
        StepKind::DeepTree,
        StepKind::Swap,
        StepKind::Recreate,
    ] {
        assert!(
            kinds.contains(&format!("{want:?}")),
            "no {want:?} step drawn; got {kinds:?}"
        );
    }
    assert!(
        recreated_as_other_kind,
        "no path re-created as the other kind"
    );
}

#[test]
fn deep_trees_reach_the_depth_cap_region() {
    let lim = limits(2_000);
    let deepest = steps(31, 2_000, 4_000)
        .iter()
        .filter(|s| s.kind == StepKind::DeepTree)
        .flat_map(|s| s.ops.iter())
        .map(|op| match op {
            Op::Mkdir { path } | Op::Create { path, .. } => path.split('/').count(),
            _ => 0,
        })
        .max()
        .unwrap_or(0);
    assert!(
        deepest + 3 >= lim.max_depth,
        "deep trees only reached depth {deepest} of {}",
        lim.max_depth
    );
}

#[test]
fn reported_paths_cover_both_sides_of_a_rename() {
    let op = Op::Rename {
        from: "a/b".into(),
        to: "c/d".into(),
        dir: true,
    };
    assert_eq!(op.reported(), vec![("a/b", true), ("c/d", true)]);
    let op = Op::Replace {
        tmp: "a/t".into(),
        path: "a/f".into(),
        len: 3,
    };
    assert_eq!(op.reported(), vec![("a/t", false), ("a/f", false)]);
    let op = Op::Import {
        to: "x".into(),
        entries: vec![NewEntry {
            path: "y".into(),
            kind: Kind::File,
            len: 1,
        }],
    };
    assert_eq!(op.reported(), vec![("x", true)]);
    let op = Op::Export {
        path: "f".into(),
        dir: false,
    };
    assert_eq!(op.reported(), vec![("f", false)]);
    let op = Op::Delete {
        path: "d".into(),
        dir: true,
    };
    assert_eq!(op.reported(), vec![("d", false)]);
}

/// Every entry below `root`, relative to it, with its kind and length.
fn walk(root: &Path) -> Listing {
    fn go(base: &Path, dir: &Path, out: &mut Listing) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let rel = path
                .strip_prefix(base)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            let meta = fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                out.insert(rel, (Kind::Dir, 0));
                go(base, &path, out);
            } else {
                assert!(meta.is_file(), "{rel} is neither a file nor a folder");
                out.insert(rel, (Kind::File, meta.len()));
            }
        }
    }
    let mut out = Listing::new();
    go(root, root, &mut out);
    out
}

/// A scratch folder holding `root` and `outside`, removed on drop.
struct Scratch {
    dir: std::path::PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = temp_path(tag);
        fs::create_dir_all(dir.join("root")).unwrap();
        fs::create_dir_all(dir.join("outside")).unwrap();
        Scratch { dir }
    }
    fn root(&self) -> std::path::PathBuf {
        self.dir.join("root")
    }
    fn outside(&self) -> std::path::PathBuf {
        self.dir.join("outside")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn apply_leaves_a_real_folder_equal_to_the_listing() {
    let scratch = Scratch::new("soak-apply");
    let mut planner = Planner::new(41, limits(400));
    for i in 0..2_500 {
        let step = planner.next_step();
        for op in &step.ops {
            soak::apply(&scratch.root(), &scratch.outside(), op)
                .unwrap_or_else(|e| panic!("step {i}: {op:?}: {e}"));
        }
        if i % 250 == 0 || i == 2_499 {
            assert_eq!(
                walk(&scratch.root()),
                planner.listing(),
                "step {i}: the folder differs from the planner's listing"
            );
            assert_eq!(
                fs::read_dir(scratch.outside()).unwrap().count(),
                0,
                "step {i}: something was left outside the watched folder"
            );
        }
    }
}

#[test]
fn apply_refuses_to_replace_what_it_did_not_make() {
    let scratch = Scratch::new("soak-refuse");
    let root = scratch.root();
    let outside = scratch.outside();
    fs::write(root.join("mine"), b"keep").unwrap();
    fs::write(root.join("other"), b"keep too").unwrap();
    fs::create_dir(root.join("folder")).unwrap();

    let refused = [
        Op::Create {
            path: "mine".into(),
            len: 1,
        },
        Op::Mkdir {
            path: "mine".into(),
        },
        Op::Rename {
            from: "other".into(),
            to: "mine".into(),
            dir: false,
        },
        Op::Import {
            to: "mine".into(),
            entries: Vec::new(),
        },
        Op::Replace {
            tmp: "tmp".into(),
            path: "folder".into(),
            len: 1,
        },
        Op::Write {
            path: "folder".into(),
            len: 1,
        },
    ];
    for op in &refused {
        assert!(
            soak::apply(&root, &outside, op).is_err(),
            "{op:?} must be refused"
        );
        assert_eq!(fs::read(root.join("mine")).unwrap(), b"keep", "{op:?}");
        assert_eq!(fs::read(root.join("other")).unwrap(), b"keep too", "{op:?}");
        assert!(root.join("folder").is_dir(), "{op:?}");
    }
}

#[test]
fn case_only_renames_work_on_any_volume() {
    let scratch = Scratch::new("soak-case");
    let root = scratch.root();
    fs::write(root.join("7-report.txt"), b"x").unwrap();
    let op = Op::Rename {
        from: "7-report.txt".into(),
        to: "7-REPORT.TXT".into(),
        dir: false,
    };
    soak::apply(&root, &scratch.outside(), &op).unwrap();
    let names: Vec<String> = fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, vec!["7-REPORT.TXT".to_string()]);
}

#[test]
fn diff_lists_missing_extra_and_changed_paths() {
    let want: BTreeMap<String, u32> = [("a", 1), ("b", 2), ("c", 3)]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    let got: BTreeMap<String, u32> = [("b", 2), ("c", 4), ("d", 5)]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    let d = diff(&want, &got);
    assert_eq!(d.missing, vec!["a".to_string()]);
    assert_eq!(d.extra, vec!["d".to_string()]);
    assert_eq!(d.changed, vec!["c".to_string()]);
    assert_eq!(d.count(), 3);
    assert!(!d.is_empty());
    assert!(diff(&want, &want).is_empty());
}

#[test]
fn durations_take_seconds_minutes_or_hours() {
    assert_eq!(parse_duration("90"), Some(Duration::from_secs(90)));
    assert_eq!(parse_duration("90s"), Some(Duration::from_secs(90)));
    assert_eq!(parse_duration("20m"), Some(Duration::from_secs(1_200)));
    assert_eq!(parse_duration("24h"), Some(Duration::from_secs(86_400)));
    for bad in ["", "m", "-5m", "1.5h", "10d", "ten"] {
        assert_eq!(parse_duration(bad), None, "{bad:?}");
    }
}
