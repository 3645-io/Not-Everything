//! The macOS scanner's buffer: while the receiver is slower than the walk,
//! as the writer is when macOS asks it to rescan a big folder, the entries
//! the walk has listed and the receiver hasn't taken stay within the
//! scan's byte budget ([`ScanOptions::stream_budget`]). Until then the walk
//! could run 256 directories ahead whatever their size: about 45 MB of a
//! rescan of a 3.2M-entry disk.
//!
//! The heap is counted by a wrapping allocator in this test binary.
//!
//! Tests only ever create and remove their own temporary directory.

#![cfg(target_os = "macos")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fx_fs::VolumeEnumerator;
use fx_fs_macos::enumerator::MacScanner;
use fx_fs_macos::scan::{DEFAULT_BUF_SIZE, STREAM_CHUNK, ScanOptions, ScannedEntry};

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

/// A fresh temporary directory, removed on drop.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path =
            std::env::temp_dir().join(format!("fx-scan-heap-{tag}-{}-{nanos}", std::process::id()));
        fs::create_dir(&path).expect("create unique temp dir");
        let path = fs::canonicalize(&path).expect("canonicalize temp dir");
        TempDir { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Big folders, so one folder's entries outgrow a chunk.
const DIRS: usize = 16;
const FILES_PER_DIR: usize = 2_500;

/// The most heap one listed entry with a short name takes while it waits:
/// the entry itself and its name, generously.
const ENTRY_BYTES: usize = std::mem::size_of::<ScannedEntry>() + 32;

#[test]
fn a_slow_receiver_holds_the_walk_to_its_byte_budget() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = TempDir::new("budget");
    for d in 0..DIRS {
        let dir = tmp.path.join(format!("d{d:02}"));
        fs::create_dir(&dir).expect("create folder");
        for f in 0..FILES_PER_DIR {
            fs::File::create(dir.join(format!("f-{f:05}"))).expect("create file");
        }
    }
    let threads = 4;
    let budget = 1 << 20;
    let scanner = MacScanner::new(ScanOptions {
        threads,
        stream_budget: budget,
        ..ScanOptions::default()
    });

    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let mut listed = 0usize;
    scanner
        .enumerate(&tmp.path, &mut |_| {
            // The receiver stalls at the first entry, long enough for an
            // unbounded walk to list the whole tree, as a writer busy with
            // its index lets it.
            if listed == 0 {
                std::thread::sleep(Duration::from_millis(1_000));
            }
            listed += 1;
        })
        .expect("the tree enumerates");
    let peak = PEAK.load(Ordering::Relaxed).saturating_sub(before);

    assert_eq!(listed, DIRS * (FILES_PER_DIR + 1), "every entry, once");
    // What may be live at the peak: the budget; one chunk admitted past it
    // into an empty queue, the chunk the receiver holds and one being
    // filled by each walker; and each walker's own listing buffer, with
    // room for its paths and the queue of folders.
    let chunk = STREAM_CHUNK * ENTRY_BYTES;
    let bound = budget + (threads + 2) * chunk + threads * DEFAULT_BUF_SIZE + (256 << 10);
    eprintln!(
        "scan heap peak with a stalled receiver: {peak} B ({} entries, budget {budget} B, bound {bound} B)",
        listed
    );
    assert!(
        peak <= bound,
        "the walk held {peak} B ahead of a stalled receiver, more than {bound} B"
    );
}
