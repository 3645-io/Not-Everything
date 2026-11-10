//! The heap an engine holds once its index is built (run 2.17), counted by
//! a wrapping allocator in this test binary. Until run 2.17 the writer kept
//! a second copy of every entry (a path tree, about 360 B per entry); the
//! bound below holds only without it.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use common::*;
use fx_index::Index;
use fx_model::StartOutcome;

/// Counts the bytes allocated and not yet freed.
struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every call is forwarded unchanged to the system allocator, which
// upholds `GlobalAlloc`'s contract; the wrapper only counts bytes.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller upholds `alloc`'s contract for `layout`.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            LIVE.fetch_add(layout.size(), Ordering::Relaxed);
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: as for `alloc`.
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            LIVE.fetch_add(layout.size(), Ordering::Relaxed);
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
            LIVE.fetch_add(new_size, Ordering::Relaxed);
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
