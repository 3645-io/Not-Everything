//! Name swaps reported as FSEvents reports them, live and in the history
//! replayed after a restart.
//!
//! Found by the soak (run 2.2, `--restart-every 2`): a file and a folder
//! swapped names while the engine was down (`a` to `t`, `b` to `a`, `t` to
//! `b`), then a folder inside the swapped folder was deleted. After the
//! reopen the deleted folder and its subtree were still indexed, under the
//! folder's new name.
//!
//! The replay is coalesced per path and comes roughly in path order: `a`
//! (renamed, once a file and once a folder, so a subtree rescan), `b` (the
//! same), `t` (a file, so a path check). Changes below `a` are not replayed
//! or, when they are, the source drops them in favour of the rescan of `a`
//! (`coalesce`). So the rescan of `a` must remove what the folder no longer
//! holds. It didn't: it took the entry the index had at `a` (the file,
//! about to be found at `b`) for the folder it walked, so it compared the
//! walk with the file's subtree and removed the file instead of the deleted
//! folder.
//!
//! Each test applies the change to a [`FakeTree`] after the engine started
//! over the tree as it was (the index a restart loads), reports the changed
//! paths in every order, in one batch and in two, and compares the
//! published index with a fresh enumeration.

mod common;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use common::{ENGINE_KEYS, FakeSource, FakeTree, ROOT, WAIT, config, snapshot_entries, start};
use fx_engine::{Engine, EngineSnapshot};
use fx_model::{Change, Entry, FileKey, StartOutcome};
use fx_sort::Order;

/// Engines running at once while every order of a case is tried.
const PARALLEL: usize = 48;

fn check(rel: &str) -> Change {
    Change::CheckPath(Path::new(ROOT).join(rel))
}

fn rescan(rel: &str) -> Change {
    Change::RescanSubtree(Path::new(ROOT).join(rel))
}

/// Waits until every pushed batch is applied: the published snapshot then
/// carries the source's cursor.
fn sync(engine: &Engine, source: &FakeSource) -> Option<Arc<EngineSnapshot>> {
    let end = Instant::now() + WAIT;
    loop {
        if source.pending() == 0 {
            let cursor = source.current_cursor();
            let snap = engine.snapshot();
            if snap.cursor() == Some(cursor) {
                return Some(snap);
            }
        }
        if Instant::now() >= end {
            return None;
        }
        thread::sleep(Duration::from_millis(1));
    }
}

/// Root-relative path of `key` in `entries`.
fn path_in(entries: &HashMap<FileKey, Entry>, key: FileKey) -> String {
    let mut parts = Vec::new();
    let mut cur = entries.get(&key);
    while let Some(e) = cur {
        if e.parent.is_none() || parts.len() > 64 {
            break;
        }
        parts.push(e.name.clone());
        cur = e.parent.and_then(|p| entries.get(&p));
    }
    parts.reverse();
    parts.join("/")
}

/// What the published index has that a fresh enumeration doesn't, and the
/// other way round, plus any sort order that differs from a fresh build.
fn differences(snap: &EngineSnapshot, tree: &FakeTree) -> Vec<String> {
    let got = snapshot_entries(snap);
    let want = tree.entries();
    let mut out = Vec::new();
    for (k, e) in &want {
        match got.get(k) {
            None => out.push(format!("missing {}", path_in(&want, *k))),
            Some(g) if g != e => out.push(format!(
                "changed {} (indexed as {})",
                path_in(&want, *k),
                path_in(&got, *k)
            )),
            Some(_) => {}
        }
    }
    for k in got.keys().filter(|k| !want.contains_key(k)) {
        out.push(format!("extra {}", path_in(&got, *k)));
    }
    let idx: &fx_index::Snapshot = snap.index();
    for key in ENGINE_KEYS {
        match snap.order(key) {
            Some(order)
                if order.validate(idx).is_ok() && order.ids == Order::build(idx, key).ids => {}
            _ => out.push(format!("{key:?} order differs from a fresh build")),
        }
    }
    out.sort();
    out
}

/// One case: the tree the index was built from, what changed on disk
/// while the engine was down, and the changes the source reports for it.
struct Case {
    before: fn(&FakeTree),
    change: fn(&FakeTree),
    reported: Vec<Change>,
}

/// Starts an engine over `case.before`, applies `case.change` to the tree
/// and delivers `batches` one after the other. Returns what differs once
/// they are applied.
fn run(case: &Case, batches: &[Vec<Change>]) -> Vec<String> {
    let tree = FakeTree::new();
    (case.before)(&tree);
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = start(&tree, &source, tree.build_index(), config());
    (case.change)(&tree);
    let mut out = Vec::new();
    for batch in batches {
        source.push(batch.clone());
        if sync(&engine, &source).is_none() {
            out.push("the batch was never applied".to_string());
            break;
        }
    }
    if out.is_empty() {
        out = differences(&engine.snapshot(), &tree);
    }
    engine.shutdown();
    out
}

/// Every order of `items` (Heap's algorithm).
fn permutations(items: &[Change]) -> Vec<Vec<Change>> {
    fn heap(k: usize, a: &mut Vec<Change>, out: &mut Vec<Vec<Change>>) {
        if k <= 1 {
            out.push(a.clone());
            return;
        }
        heap(k - 1, a, out);
        for i in 0..k - 1 {
            if k % 2 == 0 {
                a.swap(i, k - 1);
            } else {
                a.swap(0, k - 1);
            }
            heap(k - 1, a, out);
        }
    }
    let mut a = items.to_vec();
    let mut out = Vec::new();
    heap(a.len(), &mut a, &mut out);
    out
}

/// Runs `case` with its reported changes in every order, as one batch and
/// split in two at every point, and returns the deliveries that left the
/// index different from the tree, with what differed.
fn every_order(case: &Case) -> Vec<String> {
    let mut deliveries: Vec<Vec<Vec<Change>>> = Vec::new();
    for order in permutations(&case.reported) {
        for split in 0..order.len() {
            let (first, second) = order.split_at(split);
            let batches = if first.is_empty() {
                vec![second.to_vec()]
            } else {
                vec![first.to_vec(), second.to_vec()]
            };
            deliveries.push(batches);
        }
    }
    let mut failures = Vec::new();
    for chunk in deliveries.chunks(PARALLEL) {
        let results: Vec<(Vec<Vec<Change>>, Vec<String>)> = thread::scope(|s| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|batches| s.spawn(move || (batches.clone(), run(case, batches))))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for (batches, diff) in results {
            if !diff.is_empty() {
                failures.push(format!("{batches:?}: {diff:?}"));
            }
        }
    }
    failures
}

fn assert_converges(case: &Case) {
    let failures = every_order(case);
    assert!(
        failures.is_empty(),
        "{} deliveries left the index different from the tree; the first:\n{}",
        failures.len(),
        failures
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The soak's case at its smallest, delivered as the replay delivers it.
#[test]
fn a_folder_swapped_into_a_files_name_loses_what_was_deleted_inside_it() {
    let case = Case {
        before: |t| {
            t.touch("/vol/a", 1);
            t.mkdir("/vol/b");
            t.mkdir("/vol/b/gone");
            t.touch("/vol/b/gone/x", 2);
            t.touch("/vol/b/kept", 3);
        },
        change: |t| {
            t.rename("/vol/a", "/vol/t");
            t.rename("/vol/b", "/vol/a");
            t.rename("/vol/t", "/vol/b");
            t.remove("/vol/a/gone");
        },
        reported: vec![rescan("a"), rescan("b"), check("t")],
    };
    assert_eq!(
        run(&case, std::slice::from_ref(&case.reported)),
        Vec::<String>::new()
    );
}

/// The same swap with every order and batch split of the replayed paths,
/// and with the live events' path checks of both names too.
#[test]
fn a_file_and_folder_swap_converges_in_any_order() {
    let case = Case {
        before: |t| {
            t.touch("/vol/a", 1);
            t.mkdir("/vol/b");
            t.mkdir("/vol/b/gone");
            t.touch("/vol/b/gone/x", 2);
            t.touch("/vol/b/kept", 3);
        },
        change: |t| {
            t.rename("/vol/a", "/vol/t");
            t.rename("/vol/b", "/vol/a");
            t.rename("/vol/t", "/vol/b");
            t.remove("/vol/a/gone");
            t.touch("/vol/a/new", 4);
        },
        reported: vec![rescan("a"), rescan("b"), check("t"), check("a"), check("b")],
    };
    assert_converges(&case);
}

/// Two folders swap names, and each loses and gains an entry afterwards.
#[test]
fn a_folder_swap_converges_in_any_order() {
    let case = Case {
        before: |t| {
            t.mkdir("/vol/a");
            t.touch("/vol/a/a1", 1);
            t.mkdir("/vol/a/a2");
            t.touch("/vol/a/a2/deep", 2);
            t.mkdir("/vol/b");
            t.touch("/vol/b/b1", 3);
            t.touch("/vol/b/b2", 4);
        },
        change: |t| {
            t.rename("/vol/a", "/vol/t");
            t.rename("/vol/b", "/vol/a");
            t.rename("/vol/t", "/vol/b");
            t.remove("/vol/a/b2");
            t.remove("/vol/b/a2");
            t.touch("/vol/b/new", 5);
        },
        reported: vec![rescan("a"), rescan("b"), rescan("t")],
    };
    assert_converges(&case);
}

/// A folder and a file in one folder swap names live (the soak's other
/// case): every order of the live events, the folder's as subtree rescans
/// (`ITEM_RENAMED | ITEM_IS_DIR`), the file's as path checks.
#[test]
fn a_live_folder_and_file_swap_converges_in_any_order() {
    let case = Case {
        before: |t| {
            t.mkdir("/vol/p");
            t.mkdir("/vol/p/x");
            t.touch("/vol/p/x/c", 1);
            t.touch("/vol/p/y", 2);
        },
        change: |t| {
            t.rename("/vol/p/x", "/vol/p/z");
            t.rename("/vol/p/y", "/vol/p/x");
            t.rename("/vol/p/z", "/vol/p/y");
        },
        reported: vec![
            rescan("p/x"),
            rescan("p/z"),
            check("p/y"),
            check("p/x"),
            rescan("p/y"),
        ],
    };
    assert_converges(&case);
}

/// A folder replaced by a file of the same name, and a file by a folder,
/// reported only as subtree rescans (a `MustScanSubDirs`, or a replayed
/// rename of the path).
#[test]
fn a_rescanned_path_that_changed_kind_is_reconciled() {
    let case = Case {
        before: |t| {
            t.mkdir("/vol/d");
            t.touch("/vol/d/inside", 1);
            t.touch("/vol/f", 2);
        },
        change: |t| {
            t.remove("/vol/d");
            t.touch("/vol/d", 3);
            t.remove("/vol/f");
            t.mkdir("/vol/f");
            t.touch("/vol/f/new", 4);
        },
        reported: vec![rescan("d"), rescan("f")],
    };
    assert_converges(&case);
}
