//! The heap an engine holds once its index is built (run 2.17), counted by
//! a wrapping allocator in this test binary. Until run 2.17 the writer kept
//! a second copy of every entry (a path tree, about 360 B per entry); the
//! bound below holds only without it.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use common::*;
use fx_index::Index;
use fx_model::StartOutcome;

/// Counts the bytes allocated and not yet freed.
struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
/// The most `LIVE` has reached since it was last reset.
static PEAK: AtomicUsize = AtomicUsize::new(0);
/// The tests read the same counters, so they run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn grew(by: usize) {
    let now = LIVE.fetch_add(by, Ordering::Relaxed) + by;
    PEAK.fetch_max(now, Ordering::Relaxed);
}

// SAFETY: every call is forwarded unchanged to the system allocator, which
// upholds `GlobalAlloc`'s contract; the wrapper only counts bytes.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller upholds `alloc`'s contract for `layout`.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            grew(layout.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: as for `alloc`.
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            grew(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from this allocator with `layout`.
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `ptr` came from this allocator with `layout`, and the
        // caller upholds `realloc`'s contract for `new_size`.
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            grew(new_size);
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        }
        p
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Live heap bytes per indexed entry an engine may hold after its first
/// scan of short-named fake entries: the index columns, the four sort
/// orders and the writer's children index fit well inside it; another
/// copy of every entry does not.
const MAX_BYTES_PER_ENTRY: usize = 200;

#[test]
fn the_engine_keeps_no_second_copy_of_the_entries() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let tree = FakeTree::new();
    for d in 0..1000 {
        tree.mkdir(&format!("{ROOT}/d{d:04}"));
    }
    for d in 0..1000 {
        tree.add_files(&format!("{ROOT}/d{d:04}"), "file-", 100);
    }
    let entries = tree.entries().len();
    let source = FakeSource::new(StartOutcome::Resumed);
    thread::sleep(Duration::from_millis(50));
    let before = LIVE.load(Ordering::Relaxed);
    // An empty index: the engine builds its index with a first full scan.
    let engine = start(&tree, &source, Index::new(), config());
    drop(wait_for_tree(&engine, &tree, "first scan published"));
    thread::sleep(Duration::from_millis(300));
    let held = LIVE.load(Ordering::Relaxed).saturating_sub(before);
    let per_entry = held / entries;
    eprintln!(
        "engine heap after the first scan: {held} B, {per_entry} B per entry ({entries} entries)"
    );
    assert!(
        per_entry <= MAX_BYTES_PER_ENTRY,
        "the engine holds {per_entry} B per entry, more than {MAX_BYTES_PER_ENTRY} B"
    );
    engine.shutdown();
}

/// The most extra heap a subtree rescan may take at its peak, per entry it
/// lists. It streams the entries into the index, as a full rescan does
/// (run 2.17): a list of every entry and a map of the old ones took more
/// than 300 B per entry, and macOS asks for such rescans of busy folders.
/// What is left is a bit per entry to mark the entries it listed, which
/// took a byte per entry before. (The test's enumerator holds the whole
/// tree while it walks, so what the rescan allocates after the walk, less
/// than that list, doesn't show here; `rescan_bench` measures it.)
const MAX_RESCAN_PEAK_PER_ENTRY: f64 = 0.5;

#[test]
fn a_subtree_rescan_streams_its_entries() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let tree = FakeTree::new();
    for d in 0..1000 {
        tree.mkdir(&format!("{ROOT}/d{d:04}"));
        tree.add_files(&format!("{ROOT}/d{d:04}"), "file-", 100);
    }
    let entries = tree.entries().len();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = start(&tree, &source, Index::new(), config());
    drop(wait_for_tree(&engine, &tree, "first scan published"));
    // A first change builds what the writer keeps for good (its children
    // index, 4 B per entry), so the rescan below shows its own peak.
    let old_cursor = source.current_cursor();
    tree.set_size(&format!("{ROOT}/d0000/file-0.dat"), 1);
    source.push(vec![check(&format!("{ROOT}/d0000/file-0.dat"))]);
    drop(wait_for(&engine, "the first change's cursor", |s| {
        let now = source.current_cursor();
        now != old_cursor && s.cursor() == Some(now)
    }));
    thread::sleep(Duration::from_millis(300));

    // The test's enumerator builds its own list of the tree before it hands
    // out any entry (the macOS scanner streams): only what the writer adds
    // on top of that counts.
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    fx_fs::VolumeEnumerator::enumerate(&tree, std::path::Path::new(ROOT), &mut |_| {})
        .expect("the tree enumerates");
    let enumerator = PEAK.load(Ordering::Relaxed).saturating_sub(before);

    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let old_cursor = source.current_cursor();
    source.push(vec![fx_model::Change::RescanSubtree(ROOT.into())]);
    // The source's cursor moves when the writer takes the batch, and the
    // writer publishes it once the rescan is applied.
    wait_for(&engine, "the rescan's cursor", |s| {
        let now = source.current_cursor();
        now != old_cursor && s.cursor() == Some(now)
    });
    let peak = PEAK
        .load(Ordering::Relaxed)
        .saturating_sub(before)
        .saturating_sub(enumerator);
    let per_entry = peak as f64 / entries as f64;
    eprintln!(
        "subtree rescan peak beyond the enumerator's {enumerator} B: {peak} B, \
         {per_entry:.2} B per entry ({entries} entries)"
    );
    assert!(
        per_entry <= MAX_RESCAN_PEAK_PER_ENTRY,
        "a subtree rescan took {per_entry:.2} B per entry at its peak, more than {MAX_RESCAN_PEAK_PER_ENTRY} B"
    );
    let snap = wait_for_tree(&engine, &tree, "the index after the rescan");
    assert_orders_match(&snap);
    engine.shutdown();
}

/// The most extra heap a commit of a small batch may take while no window
/// shows, per indexed entry. With no snapshot pinned, the writer merges the
/// batch into its four sort orders in place: well under one order's 4 B
/// per entry. Copying the orders while the published snapshot still held
/// the old ones took 16 B per entry, about 51 MB at 3.2M entries, at the
/// end of every rescan that changed anything.
const MAX_HIDDEN_COMMIT_PEAK_PER_ENTRY: usize = 2;

#[test]
fn a_hidden_commit_merges_the_orders_in_place() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let tree = FakeTree::new();
    for d in 0..1000 {
        tree.mkdir(&format!("{ROOT}/d{d:04}"));
        tree.add_files(&format!("{ROOT}/d{d:04}"), "file-", 100);
    }
    let entries = tree.entries().len();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = start(&tree, &source, Index::new(), config());
    drop(wait_for_tree(&engine, &tree, "first scan published"));
    engine.set_foreground(false);

    // A first batch builds what the writer keeps for good (its children
    // index, built when it first looks a path up), so the measured batch
    // shows the commit's own peak. It checks a file that hasn't changed: a
    // batch that changes nothing merges nothing, so the writer has no
    // timing of a merge in place yet. A timed one would set the cost the
    // writer expects of the next: in this debug build, on a loaded machine,
    // enough to expect the measured batch past its budget and copy the
    // orders instead.
    let old_cursor = source.current_cursor();
    source.push(vec![check(&format!("{ROOT}/d0000/file-0.dat"))]);
    // The source's cursor moves when the writer takes the batch, and the
    // writer publishes it once the batch is applied (with no new
    // generation, since nothing changed).
    drop(wait_for(&engine, "the first batch's cursor", |s| {
        let now = source.current_cursor();
        now != old_cursor && s.cursor() == Some(now)
    }));
    thread::sleep(Duration::from_millis(300));

    // Changed sizes, new files and removed files, spread over the tree.
    let mut changes = Vec::new();
    for d in (0..1000).step_by(50) {
        let dir = format!("{ROOT}/d{d:04}");
        tree.set_size(&format!("{dir}/file-1.dat"), 123_456);
        tree.touch(&format!("{dir}/new.txt"), 7);
        tree.remove(&format!("{dir}/file-2.dat"));
        for name in ["file-1.dat", "new.txt", "file-2.dat"] {
            changes.push(check(&format!("{dir}/{name}")));
        }
    }
    let generations = engine.subscribe();
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let old_cursor = source.current_cursor();
    source.push(changes);
    // Waits for the batch's snapshot without reading one meanwhile: a
    // snapshot the test held when the writer took the orders over would
    // make it copy them, as a window's does.
    loop {
        generations
            .recv_timeout(WAIT)
            .expect("timed out waiting for the batch's snapshot");
        let now = source.current_cursor();
        if now != old_cursor && engine.snapshot().cursor() == Some(now) {
            break;
        }
    }
    let peak = PEAK.load(Ordering::Relaxed).saturating_sub(before);
    eprintln!(
        "hidden commit of 60 changes: peak {peak} B above the engine's heap, \
         {:.2} B per entry ({entries} entries)",
        peak as f64 / entries as f64
    );
    assert!(
        peak <= entries * MAX_HIDDEN_COMMIT_PEAK_PER_ENTRY,
        "a hidden commit of 60 changes took {peak} B at its peak, more than \
         {MAX_HIDDEN_COMMIT_PEAK_PER_ENTRY} B per entry ({entries} entries): \
         the writer took the copy path (16 B per entry) instead of merging \
         the batch into its orders in place"
    );
    let snap = wait_for_tree(&engine, &tree, "the index after the batch");
    assert_orders_match(&snap);
    engine.shutdown();
}
