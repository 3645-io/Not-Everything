//! Writer-thread tests for fx-engine: batched apply, sort-order
//! merge (renames and reparents through `Batch::moved`), arc-swap snapshots,
//! subscribers, pause/resume, shutdown and full rescans. Only scripted fakes
//! are used (see `common`), so these run on every platform.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use common::*;
use fx_engine::{Engine, EngineConfig};
use fx_model::{Change, ResumeCursor, StartOutcome};

/// A small tree: docs/{a.txt, sub/{b.txt, c.txt}}, other/, top.txt.
fn sample_tree() -> FakeTree {
    let tree = FakeTree::new();
    tree.mkdir("/vol/docs");
    tree.touch("/vol/docs/a.txt", 10);
    tree.mkdir("/vol/docs/sub");
    tree.touch("/vol/docs/sub/b.txt", 20);
    tree.touch("/vol/docs/sub/c.txt", 5);
    tree.mkdir("/vol/other");
    tree.touch("/vol/top.txt", 1);
    tree
}

/// Starts an engine over the current tree with a `Resumed` source.
fn running(tree: &FakeTree) -> (FakeSource, Engine) {
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = start(tree, &source, tree.build_index(), config());
    (source, engine)
}

#[test]
fn batch_window_defaults_to_100ms_and_is_clamped_to_50_200ms() {
    assert_eq!(
        EngineConfig::new(ROOT).batch_window(),
        Duration::from_millis(100)
    );
    let w = |ms| {
        EngineConfig::new(ROOT)
            .with_batch_window(Duration::from_millis(ms))
            .batch_window()
    };
    assert_eq!(w(10), Duration::from_millis(50));
    assert_eq!(w(50), Duration::from_millis(50));
    assert_eq!(w(150), Duration::from_millis(150));
    assert_eq!(w(200), Duration::from_millis(200));
    assert_eq!(w(5_000), Duration::from_millis(200));
}

#[test]
fn initial_snapshot_serves_the_given_index_with_merged_orders() {
    let tree = sample_tree();
    let (_source, engine) = running(&tree);
    let snap = engine.snapshot();
    assert_eq!(snapshot_entries(&snap), tree.entries());
    assert_orders_match(&snap);
    engine.shutdown();
}

#[test]
fn start_passes_the_resume_cursor_to_the_source() {
    let tree = sample_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let saved = ResumeCursor::FsEvents {
        event_id: 41,
        db_uuid: DB_UUID,
    };
    let engine = start(
        &tree,
        &source,
        tree.build_index(),
        config().with_resume_from(saved),
    );
    let end = Instant::now() + WAIT;
    while source.started_from().is_none() {
        assert!(Instant::now() < end, "source was never started");
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(source.started_from(), Some(Some(saved)));
    engine.shutdown();
}

#[test]
fn create_is_reflected_after_one_batch_and_cursor_follows_publish() {
    let tree = sample_tree();
    let (source, engine) = running(&tree);
    let gen0 = engine.snapshot().generation();

    let key = tree.touch("/vol/docs/new.txt", 42);
    source.push(vec![check("/vol/docs/new.txt")]);

    let snap = wait_for_tree(&engine, &tree, "created file indexed");
    assert!(snap.generation() > gen0, "a batch bumps the generation");
    assert_eq!(rel_path(&snap, key).as_deref(), Some("docs/new.txt"));
    assert_eq!(source.pending(), 0);
    // The cursor in a snapshot covers exactly the batches applied in it.
    assert_eq!(
        snap.cursor(),
        Some(ResumeCursor::FsEvents {
            event_id: 1,
            db_uuid: DB_UUID
        })
    );
    assert_orders_match(&snap);
    engine.shutdown();
}

#[test]
fn folder_rename_keeps_keys_and_moves_children() {
    let tree = sample_tree();
    let (source, engine) = running(&tree);
    let docs = tree.key_of("/vol/docs").unwrap();
    let b = tree.key_of("/vol/docs/sub/b.txt").unwrap();

    tree.rename("/vol/docs", "/vol/papers");
    source.push(vec![check("/vol/docs"), check("/vol/papers")]);

    let snap = wait_for_tree(&engine, &tree, "renamed folder indexed");
    assert_eq!(rel_path(&snap, docs).as_deref(), Some("papers"));
    assert_eq!(rel_path(&snap, b).as_deref(), Some("papers/sub/b.txt"));
    assert_orders_match(&snap);
    engine.shutdown();
}

#[test]
fn folder_reparent_with_children_keeps_every_order_merged() {
    let tree = sample_tree();
    let (source, engine) = running(&tree);
    let sub = tree.key_of("/vol/docs/sub").unwrap();
    let c = tree.key_of("/vol/docs/sub/c.txt").unwrap();

    // Reparent and rename in one go: docs/sub -> other/zsub. The Path order
    // must move the whole subtree to its new place.
    tree.rename("/vol/docs/sub", "/vol/other/zsub");
    source.push(vec![check("/vol/docs/sub"), check("/vol/other/zsub")]);

    let snap = wait_for_tree(&engine, &tree, "reparented folder indexed");
    assert_eq!(rel_path(&snap, sub).as_deref(), Some("other/zsub"));
    assert_eq!(rel_path(&snap, c).as_deref(), Some("other/zsub/c.txt"));
    assert_orders_match(&snap);

    // And back to the front of the Path order, under a new name.
    tree.rename("/vol/other/zsub", "/vol/aaa");
    source.push(vec![check("/vol/other/zsub"), check("/vol/aaa")]);
    let snap = wait_for_tree(&engine, &tree, "folder moved to the root");
    assert_eq!(rel_path(&snap, c).as_deref(), Some("aaa/c.txt"));
    assert_orders_match(&snap);
    engine.shutdown();
}

#[test]
fn delete_of_a_folder_removes_its_descendants() {
    let tree = sample_tree();
    let (source, engine) = running(&tree);
    let b = tree.key_of("/vol/docs/sub/b.txt").unwrap();

    tree.remove("/vol/docs");
    source.push(vec![check("/vol/docs")]);

    let snap = wait_for_tree(&engine, &tree, "deleted folder gone");
    assert_eq!(rel_path(&snap, b), None);
    assert_orders_match(&snap);
    engine.shutdown();
}

#[test]
fn rescan_subtree_applies_unreported_changes_under_the_folder() {
    let tree = sample_tree();
    let (source, engine) = running(&tree);

    // Changes the source lost; only the subtree rescan can find them.
    tree.touch("/vol/docs/sub/new.bin", 999);
    tree.remove("/vol/docs/sub/b.txt");
    tree.rename("/vol/docs/a.txt", "/vol/docs/sub/a2.txt");
    tree.mkdir("/vol/docs/fresh");
    tree.touch("/vol/docs/fresh/x", 3);
    tree.set_size("/vol/docs/sub/c.txt", 77);
    source.push(vec![Change::RescanSubtree("/vol/docs".into())]);

    let snap = wait_for_tree(&engine, &tree, "subtree rescan applied");
    assert_orders_match(&snap);
    engine.shutdown();
}

#[test]
fn attribute_change_updates_size_and_modified_orders() {
    let tree = sample_tree();
    let (source, engine) = running(&tree);

    tree.set_size("/vol/top.txt", 1_000_000);
    tree.set_size("/vol/docs/sub/c.txt", 0);
    source.push(vec![check("/vol/top.txt"), check("/vol/docs/sub/c.txt")]);

    let snap = wait_for_tree(&engine, &tree, "sizes updated");
    assert_orders_match(&snap);
    engine.shutdown();
}

#[test]
fn many_mixed_batches_keep_every_order_equal_to_a_fresh_build() {
    let tree = sample_tree();
    for d in 0..6 {
        tree.mkdir(&format!("/vol/d{d}"));
        for f in 0..5 {
            tree.touch(&format!("/vol/d{d}/f{f}.txt"), (d * 10 + f) as u64);
        }
    }
    let (source, engine) = running(&tree);

    let steps: Vec<Box<dyn Fn(&FakeTree) -> Vec<Change>>> = vec![
        Box::new(|t: &FakeTree| {
            t.rename("/vol/d0", "/vol/d5/d0");
            vec![check("/vol/d0"), check("/vol/d5/d0")]
        }),
        Box::new(|t: &FakeTree| {
            t.touch("/vol/d1/Z.txt", 5);
            t.rename("/vol/d1/f0.txt", "/vol/d2/g0.txt");
            t.remove("/vol/d3");
            vec![
                check("/vol/d1/Z.txt"),
                check("/vol/d1/f0.txt"),
                check("/vol/d2/g0.txt"),
                check("/vol/d3"),
            ]
        }),
        Box::new(|t: &FakeTree| {
            t.rename("/vol/d5", "/vol/A5");
            t.set_size("/vol/d2/f1.txt", 12345);
            vec![check("/vol/d5"), check("/vol/A5"), check("/vol/d2/f1.txt")]
        }),
        Box::new(|t: &FakeTree| {
            t.rename("/vol/A5/d0", "/vol/d1/inner");
            t.mkdir("/vol/d4/deep");
            t.touch("/vol/d4/deep/leaf", 8);
            vec![Change::RescanSubtree("/vol".into())]
        }),
    ];
    for (i, step) in steps.iter().enumerate() {
        let changes = step(&tree);
        source.push(changes);
        let snap = wait_for_tree(&engine, &tree, &format!("step {i} applied"));
        assert_orders_match(&snap);
    }
    engine.shutdown();
}

#[test]
fn subscribers_receive_the_new_generation() {
    let tree = sample_tree();
    let (source, engine) = running(&tree);
    let rx = engine.subscribe();
    let gen0 = engine.snapshot().generation();

    tree.touch("/vol/other/n.txt", 1);
    source.push(vec![check("/vol/other/n.txt")]);
    wait_for_tree(&engine, &tree, "create applied");

    let end = Instant::now() + WAIT;
    let mut last = gen0;
    loop {
        let g = rx
            .recv_timeout(Duration::from_millis(100))
            .ok()
            .map(u64::from);
        if let Some(g) = g {
            assert!(g > last, "generations increase: {g} after {last}");
            last = g;
        }
        if last == engine.snapshot().generation() && last > gen0 {
            break;
        }
        assert!(Instant::now() < end, "subscriber never saw the generation");
    }
    engine.shutdown();
}

#[test]
fn pause_holds_changes_until_resume() {
    let tree = sample_tree();
    let (source, engine) = running(&tree);
    let before = tree.entries();

    engine.pause();
    // Let a window that may already be in flight finish.
    thread::sleep(Duration::from_millis(300));
    tree.touch("/vol/paused.txt", 1);
    source.push(vec![check("/vol/paused.txt")]);
    thread::sleep(Duration::from_millis(600));
    let snap = engine.snapshot();
    assert_eq!(
        snapshot_entries(&snap),
        before,
        "nothing applied while paused"
    );

    engine.resume();
    let snap = wait_for_tree(&engine, &tree, "change applied after resume");
    assert_orders_match(&snap);
    engine.shutdown();
}

#[test]
fn shutdown_joins_the_writer_thread() {
    let tree = sample_tree();
    let (source, engine) = running(&tree);
    let end = Instant::now() + WAIT;
    while source.polls() == 0 {
        assert!(Instant::now() < end, "writer never polled the source");
        thread::sleep(Duration::from_millis(5));
    }
    engine.shutdown();
    let polls = source.polls();
    thread::sleep(Duration::from_millis(400));
    assert_eq!(source.polls(), polls, "source polled after shutdown");
}

#[test]
fn readers_never_block_while_the_writer_applies_a_large_batch() {
    let tree = sample_tree();
    tree.mkdir("/vol/bulk");
    tree.add_files("/vol/bulk", "old", 2_000);
    let (source, engine) = running(&tree);
    let gen0 = engine.snapshot().generation();
    let live0 = engine.snapshot().index().live_count();

    tree.add_files("/vol/bulk", "new", 20_000);
    let done = AtomicBool::new(false);

    let (max_latency, reads_before_publish) = thread::scope(|s| {
        let reader = s.spawn(|| {
            let mut max = Duration::ZERO;
            let mut old_reads = 0u64;
            while !done.load(Ordering::Acquire) {
                let t = Instant::now();
                let snap = engine.snapshot();
                max = max.max(t.elapsed());
                // Every published snapshot is internally consistent.
                let live = snap.index().live_count();
                for k in ENGINE_KEYS {
                    assert_eq!(snap.order(k).unwrap().len(), live, "{k:?} order length");
                }
                if snap.generation() == gen0 {
                    old_reads += 1;
                    assert_eq!(live, live0, "the old snapshot keeps serving");
                }
            }
            (max, old_reads)
        });
        source.push(vec![Change::RescanSubtree("/vol/bulk".into())]);
        let snap = wait_for(&engine, "large batch published", |s| {
            s.index().live_count() == live0 + 20_000
        });
        done.store(true, Ordering::Release);
        assert_orders_match(&snap);
        reader.join().unwrap()
    });
    assert!(reads_before_publish > 0, "readers served during the apply");
    assert!(
        max_latency < Duration::from_millis(100),
        "snapshot() blocked for {max_latency:?}"
    );
    assert_eq!(snapshot_entries(&engine.snapshot()), tree.entries());
    engine.shutdown();
}

#[test]
fn gap_detected_start_rebuilds_to_equal_a_fresh_enumeration() {
    let tree = sample_tree();
    tree.mkdir("/vol/d");
    tree.add_files("/vol/d", "f", 50);
    let stale = tree.build_index();

    // Changes made while the app was not watching.
    tree.rename("/vol/docs/sub", "/vol/d/sub");
    tree.remove("/vol/other");
    tree.touch("/vol/d/late.txt", 9);
    tree.set_size("/vol/top.txt", 64);
    tree.rename("/vol/docs", "/vol/Docs2");

    let source = FakeSource::new(StartOutcome::GapDetected);
    let saved = ResumeCursor::FsEvents {
        event_id: 5,
        db_uuid: [1; 16],
    };
    let engine = start(&tree, &source, stale, config().with_resume_from(saved));
    let snap = wait_for_tree(&engine, &tree, "gap rescan finished");
    assert_orders_match(&snap);
    assert_eq!(source.started_from(), Some(Some(saved)));
    engine.shutdown();
}

#[test]
fn gap_detected_start_from_an_empty_index_runs_a_full_scan() {
    let tree = sample_tree();
    let source = FakeSource::new(StartOutcome::GapDetected);
    let engine = start(&tree, &source, fx_index::Index::new(), config());
    let snap = wait_for_tree(&engine, &tree, "full scan finished");
    assert_orders_match(&snap);
    engine.shutdown();
}

#[test]
fn rescan_volume_rebuilds_while_changes_keep_flowing() {
    let tree = sample_tree();
    let (source, engine) = running(&tree);

    tree.remove("/vol/docs/sub");
    tree.mkdir("/vol/other/x");
    tree.touch("/vol/other/x/y", 2);
    source.push(vec![Change::RescanVolume]);
    let snap = wait_for_tree(&engine, &tree, "volume rescan finished");
    assert_orders_match(&snap);

    // Normal batches still apply after the rescan.
    tree.touch("/vol/after.txt", 3);
    source.push(vec![check("/vol/after.txt")]);
    let snap = wait_for_tree(&engine, &tree, "batch after rescan");
    assert_orders_match(&snap);
    engine.shutdown();
}

// ---------------------------------------------------------------------------
// Background cost (run 2.14)
// ---------------------------------------------------------------------------

/// Drops every generation already sent to `rx`.
fn drain(rx: &std::sync::mpsc::Receiver<fx_engine::Generation>) {
    while rx.try_recv().is_ok() {}
}

#[test]
fn a_batch_that_changes_nothing_publishes_no_new_generation() {
    let tree = sample_tree();
    let (source, engine) = running(&tree);
    let rx = engine.subscribe();
    thread::sleep(Duration::from_millis(100));
    drain(&rx);
    let gen0 = engine.snapshot().generation();

    // A change notice for a file that did not change.
    source.push(vec![check("/vol/top.txt")]);
    let end = Instant::now() + WAIT;
    while engine.snapshot().cursor() != Some(source.current_cursor()) {
        assert!(Instant::now() < end, "the newer cursor was never published");
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        engine.snapshot().generation(),
        gen0,
        "nothing changed, so no new generation"
    );
    assert!(rx.try_recv().is_err(), "no subscriber was woken");
    engine.shutdown();
}

#[test]
fn the_engine_starts_in_the_foreground_with_a_2s_background_window() {
    assert_eq!(
        EngineConfig::new(ROOT).background_batch_window(),
        Duration::from_secs(2)
    );
    let tree = sample_tree();
    let (_source, engine) = running(&tree);
    assert!(engine.is_foreground());
    engine.set_foreground(false);
    assert!(!engine.is_foreground());
    engine.shutdown();
}

#[test]
fn a_hidden_window_collects_changes_for_the_background_window() {
    let tree = sample_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let config = config().with_background_batch_window(Duration::from_millis(800));
    let engine = start(&tree, &source, tree.build_index(), config);
    engine.set_foreground(false);
    // Let the writer finish the foreground wait it may be in.
    thread::sleep(Duration::from_millis(150));
    let gen0 = engine.snapshot().generation();

    let t0 = Instant::now();
    tree.touch("/vol/other/n.txt", 1);
    source.push(vec![check("/vol/other/n.txt")]);
    thread::sleep(Duration::from_millis(400));
    assert_eq!(
        engine.snapshot().generation(),
        gen0,
        "hidden: the change waits for the end of the background window"
    );
    wait_for_tree(&engine, &tree, "applied after the background window");
    assert!(
        t0.elapsed() >= Duration::from_millis(700),
        "applied after {:?}, before the background window ended",
        t0.elapsed()
    );
    engine.shutdown();
}

#[test]
fn showing_the_window_applies_pending_changes_soon() {
    let tree = sample_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let config = config().with_background_batch_window(Duration::from_secs(5));
    let engine = start(&tree, &source, tree.build_index(), config);
    engine.set_foreground(false);
    thread::sleep(Duration::from_millis(150));

    tree.touch("/vol/other/n.txt", 1);
    source.push(vec![check("/vol/other/n.txt")]);
    thread::sleep(Duration::from_millis(100));
    let shown = Instant::now();
    engine.set_foreground(true);
    wait_for_tree(&engine, &tree, "applied once the window shows");
    assert!(
        shown.elapsed() < Duration::from_millis(700),
        "applied {:?} after the window showed",
        shown.elapsed()
    );
    engine.shutdown();
}

#[test]
fn a_hidden_idle_writer_polls_the_source_rarely() {
    let tree = sample_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let config = config().with_background_batch_window(Duration::from_millis(500));
    let engine = start(&tree, &source, tree.build_index(), config);
    engine.set_foreground(false);
    thread::sleep(Duration::from_millis(150));
    let polls = source.polls();
    thread::sleep(Duration::from_secs(1));
    let woke = source.polls() - polls;
    assert!(
        woke <= 3,
        "hidden and idle, the writer waited on the source {woke} times in 1 s"
    );
    engine.shutdown();
}
