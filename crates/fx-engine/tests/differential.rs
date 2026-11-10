//! Differential test of the writer (run 2.17): random file operations on
//! the fake file system, reported in batches the way FSEvents reports them
//! (including stale paths inside moved folders, files replaced in place,
//! sub-tree re-scans and a full re-scan after a gap). After every batch the
//! published index must equal a fresh scan, and every sort order a fresh
//! build. The writer navigates the index itself since run 2.17, so this
//! guards the paths, subtrees and comparisons the old path tree answered.

mod common;

use common::*;
use fx_engine::Engine;
use fx_model::{Change, StartOutcome};

/// A small deterministic generator.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// The paths the test knows exist, besides the root.
struct Paths {
    dirs: Vec<String>,
    files: Vec<String>,
    next: u64,
}

impl Paths {
    fn fresh_name(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("{prefix}{}", self.next)
    }
    /// Every known path at or below `top`.
    fn under(&self, top: &str) -> Vec<String> {
        let prefix = format!("{top}/");
        self.dirs
            .iter()
            .chain(&self.files)
            .filter(|p| *p == top || p.starts_with(&prefix))
            .cloned()
            .collect()
    }
    fn forget_under(&mut self, top: &str) {
        let prefix = format!("{top}/");
        self.dirs.retain(|p| p != top && !p.starts_with(&prefix));
        self.files.retain(|p| p != top && !p.starts_with(&prefix));
    }
    fn rename_under(&mut self, from: &str, to: &str) {
        let prefix = format!("{from}/");
        for p in self.dirs.iter_mut().chain(self.files.iter_mut()) {
            if p == from {
                *p = to.to_string();
            } else if let Some(rest) = p.strip_prefix(&prefix) {
                *p = format!("{to}/{rest}");
            }
        }
    }
    fn any_dir(&self, rng: &mut Rng) -> String {
        if self.dirs.is_empty() || rng.below(4) == 0 {
            ROOT.to_string()
        } else {
            self.dirs[rng.below(self.dirs.len())].clone()
        }
    }
}

/// One random operation on `tree`; returns the changes FSEvents would
/// report for it.
fn random_op(rng: &mut Rng, tree: &FakeTree, paths: &mut Paths) -> Vec<Change> {
    match rng.below(9) {
        0 | 1 => {
            let dir = paths.any_dir(rng);
            let name = paths.fresh_name("f");
            let path = format!("{dir}/{name}.txt");
            tree.touch(&path, rng.below(5000) as u64);
            paths.files.push(path.clone());
            vec![check(&path)]
        }
        2 => {
            let dir = paths.any_dir(rng);
            let path = format!("{dir}/{}", paths.fresh_name("d"));
            tree.mkdir(&path);
            paths.dirs.push(path.clone());
            vec![check(&path)]
        }
        3 if !paths.files.is_empty() => {
            // Rename a file within its folder.
            let i = rng.below(paths.files.len());
            let from = paths.files[i].clone();
            let dir = from.rsplit_once('/').unwrap().0.to_string();
            let to = format!("{dir}/{}.txt", paths.fresh_name("r"));
            tree.rename(&from, &to);
            paths.files[i] = to.clone();
            vec![check(&from), check(&to)]
        }
        4 if !paths.dirs.is_empty() => {
            // Move a folder elsewhere; a file made inside it just before
            // the move is reported under the old path, as history replays it.
            let from = paths.dirs[rng.below(paths.dirs.len())].clone();
            let target = paths.any_dir(rng);
            if target == from || target.starts_with(&format!("{from}/")) {
                return Vec::new();
            }
            let inner = format!("{from}/{}.txt", paths.fresh_name("i"));
            tree.touch(&inner, 3);
            paths.files.push(inner.clone());
            let to = format!("{target}/{}", paths.fresh_name("m"));
            tree.rename(&from, &to);
            paths.rename_under(&from, &to);
            vec![check(&inner), check(&from), check(&to)]
        }
        5 if !paths.files.is_empty() || !paths.dirs.is_empty() => {
            // Remove a file or a whole folder.
            let all: Vec<String> = paths.dirs.iter().chain(&paths.files).cloned().collect();
            let path = all[rng.below(all.len())].clone();
            tree.remove(&path);
            paths.forget_under(&path);
            vec![check(&path)]
        }
        6 if !paths.files.is_empty() => {
            let path = paths.files[rng.below(paths.files.len())].clone();
            tree.set_size(&path, rng.below(100_000) as u64);
            vec![check(&path)]
        }
        7 if !paths.files.is_empty() => {
            // A file replaced in place: a new file ID at the same path.
            let path = paths.files[rng.below(paths.files.len())].clone();
            tree.remove(&path);
            tree.touch(&path, 77);
            vec![check(&path)]
        }
        8 => {
            // Several unreported changes in one folder, then a re-scan of it.
            let dir = paths.any_dir(rng);
            for _ in 0..3 {
                let path = format!("{dir}/{}.txt", paths.fresh_name("s"));
                tree.touch(&path, 9);
                paths.files.push(path);
            }
            let doomed: Vec<String> = paths
                .under(&dir)
                .into_iter()
                .filter(|p| p != &dir && paths.files.contains(p))
                .take(1)
                .collect();
            for p in doomed {
                tree.remove(&p);
                paths.forget_under(&p);
            }
            vec![Change::RescanSubtree(dir.into())]
        }
        _ => Vec::new(),
    }
}

fn run(seed: u64) {
    let mut rng = Rng(seed);
    let tree = FakeTree::new();
    let mut paths = Paths {
        dirs: Vec::new(),
        files: Vec::new(),
        next: 0,
    };
    for d in 0..4 {
        let dir = format!("{ROOT}/top{d}");
        tree.mkdir(&dir);
        paths.dirs.push(dir.clone());
        for f in 0..5 {
            let file = format!("{dir}/seed{f}.txt");
            tree.touch(&file, f);
            paths.files.push(file);
        }
    }
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine: Engine = start(&tree, &source, tree.build_index(), config());
    for round in 0..30 {
        let mut batch = Vec::new();
        for _ in 0..1 + rng.below(6) {
            batch.extend(random_op(&mut rng, &tree, &mut paths));
        }
        if batch.is_empty() {
            continue;
        }
        source.push(batch);
        let snap = wait_for_tree(&engine, &tree, &format!("seed {seed} round {round}"));
        assert_orders_match(&snap);
    }
    // A gap: changes the source never reported, then a full re-scan.
    for _ in 0..8 {
        let _ = random_op(&mut rng, &tree, &mut paths);
    }
    source.push(vec![Change::RescanVolume]);
    let snap = wait_for_tree(&engine, &tree, &format!("seed {seed} full rescan"));
    assert_orders_match(&snap);
    engine.shutdown();
}

#[test]
fn the_index_equals_a_fresh_scan_after_every_batch() {
    for seed in 0..6 {
        run(0xD1FF_0000 + seed);
    }
}
