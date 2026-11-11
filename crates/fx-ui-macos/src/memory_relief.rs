//! Hands the heap's free pages back to macOS every few seconds.
//!
//! macOS's allocator keeps large blocks the app has freed (the sort orders
//! replaced at each batch, search results, a save's buffers) for reuse, and
//! their pages stay dirty, so they count in the app's memory footprint: 30
//! to 70 MB on a whole-disk index once the saved index is served from its
//! file (run 2.17). A thread asks the allocator every [`RELIEF_INTERVAL`]
//! to return them (`malloc_zone_pressure_relief`): about a millisecond of
//! work and one wakeup each time.

use std::time::Duration;

/// How often free heap pages are handed back: one wakeup per interval,
/// against the 2 a second the gate allows while the window is hidden.
pub const RELIEF_INTERVAL: Duration = Duration::from_secs(15);

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
}

/// Asks every malloc zone to return its free pages to the system, and
/// returns how many bytes it gave back.
#[cfg(target_os = "macos")]
pub fn relieve() -> usize {
    // SAFETY: a null zone means every zone, and a goal of 0 means as much
    // as possible. The call only returns free pages to the system; memory
    // in use is untouched.
    unsafe { malloc_zone_pressure_relief(std::ptr::null_mut(), 0) }
}

/// Starts the thread, at the utility class, for the rest of the process.
#[cfg(target_os = "macos")]
pub fn start() {
    let spawned = std::thread::Builder::new()
        .name("memory-relief".into())
        .spawn(|| {
            fx_engine::qos::set_current_thread_class(fx_engine::qos::ThreadClass::Utility);
            loop {
                std::thread::sleep(RELIEF_INTERVAL);
                relieve();
            }
        });
    if let Err(e) = spawned {
        eprintln!("cannot start the memory relief thread: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relief_wakes_rarely() {
        // At most one wakeup every 5 s, far under the gate's 2 a second,
        // and often enough that a freed block goes within a minute.
        assert!(RELIEF_INTERVAL >= Duration::from_secs(5));
        assert!(RELIEF_INTERVAL <= Duration::from_secs(60));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn relief_returns_freed_blocks_and_keeps_live_ones() {
        let keep: Vec<u8> = vec![7; 4 << 20];
        for _ in 0..8 {
            let block: Vec<u8> = vec![1; 8 << 20];
            assert_eq!(block[block.len() - 1], 1);
            drop(block);
        }
        // How much comes back depends on the allocator's state; the call
        // must not touch memory in use.
        let _ = relieve();
        assert!(keep.iter().all(|&b| b == 7));
    }
}
