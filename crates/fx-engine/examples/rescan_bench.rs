//! `rescan_bench`: what a subtree rescan costs, in time and in memory
//! above what the engine held just before it.
//!
//! ```text
//! cargo run --release -p fx-engine --example rescan_bench -- --entries 770000 --runs 3 --cache warm
//! ```
//!
//! macOS asks for such a rescan when FSEvents reports `MustScanSubDirs` for
//! a busy folder. The gate's samples, 10 s apart, see one only when it
//! lands on a reading, so this measures it directly: it indexes a folder
//! with the macOS scanner (`MacScanner`) and `MacPathStat` (a synthetic
//! tree of `--entries` entries materialized in a temporary folder, or
//! `--root DIR`), then asks the writer to rescan that folder `--runs`
//! times. For each rescan it reads the live heap (counted by this binary's
//! allocator) and the physical footprint (`proc_pid_rusage`, every 5 ms),
//! both above their values just before it, after freed pages were handed
//! back as the app does every 15 s. The heap is read three times: its peak
//! while the scanner walks the folder (the walk's own share, the scanner's
//! buffer included), its peak after the walk (the removal of whatever the
//! walk didn't list, then the commit that merges the changes into the sort
//! orders and publishes the result) and its peak over the whole rescan.
//!
//! A rescan commits only when it found a change. A real folder usually has
//! some; in the synthetic tree, `--touch N` makes some: before each rescan
//! it removes the files the previous one added and adds `N` new ones.
//!
//! The engine runs as the app does while its window is hidden, which is
//! when macOS's rescans usually come (`Engine::set_foreground(false)`);
//! `--window shown` runs it as with the window on screen.
//!
//! `--stream-kb N` sets the scanner's buffer (`ScanOptions::stream_budget`)
//! to compare sizes; the default is the scanner's own.
//!
//! Output: `metric=value` lines (see the crate README): `rescan_entries`,
//! `rescan_p50_ms`, `rescan_max_ms`, `rescan_walk_heap_peak_mb`,
//! `rescan_after_walk_heap_peak_mb`, `rescan_heap_peak_mb` and
//! `rescan_footprint_peak_mb` (the largest over the runs), the heap peaks
//! of each run (`rescan_run<N>_…`), and the longest `Engine::snapshot()`
//! call of a reader polling every millisecond while it waited, which shows
//! how long a search could wait for the writer's merge in place
//! (`rescan_run<N>_snapshot_wait_max_ms`, `rescan_snapshot_wait_max_ms`).
//! `--checks N` then commits `2N` batches of one change each, as the
//! hidden app does all day, and reports `checks_snapshot_wait_max_ms` and
//! `checks_commit_heap_peak_mb`.
//! Exit codes: 0 after the rescans,
//! 1 on an error or timeout, 2 for bad arguments. On hosts other than macOS
//! it prints a message and exits with 0.
//!
//! It writes only its own temporary folder (with `--entries`) and removes
//! it at the end; with `--root` it only reads directory listings.

use std::process::ExitCode;

#[cfg(not(target_os = "macos"))]
fn main() -> ExitCode {
    println!("rescan_bench requires macOS (the macOS scanner); nothing to do on this OS");
    ExitCode::SUCCESS
}

#[cfg(target_os = "macos")]
mod bench_common;

#[cfg(target_os = "macos")]
fn main() -> ExitCode {
    bench::main()
}

#[cfg(target_os = "macos")]
mod bench {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::ffi::{c_int, c_void};
    use std::io;
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;
    use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use crate::bench_common::{Flags, TempRoot, UsageError, header, metric, stats};
    use fx_bench::{TreeShape, generate_tree, materialize};
    use fx_engine::{Engine, EngineConfig, EngineHealth};
    use fx_fs::{ChangeSource, VolumeEnumerator};
    use fx_fs_macos::enumerator::MacScanner;
    use fx_fs_macos::path_stat::MacPathStat;
    use fx_fs_macos::scan::ScanOptions;
    use fx_index::Index;
    use fx_model::{Change, Entry, ResumeCursor, StartOutcome};

    const USAGE: &str = "\
usage:
  rescan_bench [--entries N [--touch N] | --root DIR] [--runs N]
               [--stream-kb N] [--window hidden|shown] [--timeout SECS]
               [--cache warm|cold|unknown]

  --entries N     materialize a synthetic tree of N entries (default 500000)
  --touch N       before each rescan, replace N files of the synthetic tree
                  (default 0: the rescans find nothing to commit)
  --checks N      after the rescans, N files added and removed one at a
                  time in the temporary folder (under the root): small
                  batches (default 0)
  --root DIR      rescan this folder instead (read only)
  --runs N        rescans to measure, 1..=20 (default 3)
  --stream-kb N   the scanner's buffer in KiB (default: the scanner's own)
  --window STATE  hidden (default, as when macOS asks for a rescan) or shown
  --timeout SECS  give up on a scan after SECS (default 600)
  --cache STATE   cache state label for the report (default unknown)";

    // ------------------------------------------------------------ heap

    /// Counts the bytes allocated and not yet freed.
    struct Counting;

    static LIVE: AtomicUsize = AtomicUsize::new(0);
    /// The most `LIVE` has reached since it was last reset.
    static PEAK: AtomicUsize = AtomicUsize::new(0);
    /// Where the rescan is: [`BEFORE_WALK`], [`WALKING`] or [`AFTER_WALK`].
    static PHASE: AtomicU8 = AtomicU8::new(BEFORE_WALK);
    const BEFORE_WALK: u8 = 0;
    const WALKING: u8 = 1;
    const AFTER_WALK: u8 = 2;
    /// The most `LIVE` has reached while the scanner walked, since it was
    /// last reset.
    static WALK_PEAK: AtomicUsize = AtomicUsize::new(0);
    /// The most `LIVE` has reached after the walk (the rest of the rescan
    /// and its commit), since it was last reset.
    static AFTER_PEAK: AtomicUsize = AtomicUsize::new(0);

    fn grew(by: usize) {
        let now = LIVE.fetch_add(by, Ordering::Relaxed) + by;
        PEAK.fetch_max(now, Ordering::Relaxed);
        match PHASE.load(Ordering::Relaxed) {
            WALKING => WALK_PEAK.fetch_max(now, Ordering::Relaxed),
            AFTER_WALK => AFTER_PEAK.fetch_max(now, Ordering::Relaxed),
            _ => 0,
        };
    }

    /// The macOS scanner, with the heap's peak read while it walks and
    /// after it.
    struct Walk(MacScanner);

    impl VolumeEnumerator for Walk {
        fn enumerate(&self, root: &Path, sink: &mut dyn FnMut(Entry)) -> fx_fs::Result<()> {
            WALK_PEAK.fetch_max(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
            PHASE.store(WALKING, Ordering::Relaxed);
            let listed = self.0.enumerate(root, sink);
            AFTER_PEAK.fetch_max(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
            PHASE.store(AFTER_WALK, Ordering::Relaxed);
            listed
        }
    }

    // SAFETY: every call is forwarded unchanged to the system allocator,
    // which upholds `GlobalAlloc`'s contract; the wrapper only counts bytes.
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

    // ------------------------------------------------------- footprint

    /// `struct rusage_info_v0` (`sys/resource.h`).
    #[repr(C)]
    #[derive(Default)]
    struct RusageInfoV0 {
        uuid: [u8; 16],
        user_time: u64,
        system_time: u64,
        pkg_idle_wkups: u64,
        interrupt_wkups: u64,
        pageins: u64,
        wired_size: u64,
        resident_size: u64,
        phys_footprint: u64,
        proc_start_abstime: u64,
        proc_exit_abstime: u64,
    }

    const RUSAGE_INFO_V0: c_int = 0;

    unsafe extern "C" {
        fn proc_pid_rusage(pid: c_int, flavor: c_int, buffer: *mut c_void) -> c_int;
        fn malloc_zone_pressure_relief(zone: *mut c_void, goal: usize) -> usize;
    }

    /// This process's physical footprint, 0 if it can't be read.
    fn footprint() -> u64 {
        let mut info = RusageInfoV0::default();
        let pid = std::process::id() as c_int;
        // SAFETY: `info` is a live, writable `rusage_info_v0` laid out as
        // in the SDK header, which is what the kernel fills for flavor 0.
        let rc = unsafe { proc_pid_rusage(pid, RUSAGE_INFO_V0, (&raw mut info).cast()) };
        if rc == 0 { info.phys_footprint } else { 0 }
    }

    /// Hands freed heap pages back to the system, as the app does every
    /// 15 s, so a reading starts from what is really in use.
    fn relieve() {
        // SAFETY: a null zone means every zone; a goal of 0 means as much as
        // possible. The call only returns free pages.
        unsafe {
            malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
        }
    }

    // ---------------------------------------------------------- source

    /// A change source that delivers a subtree rescan when asked.
    #[derive(Clone)]
    struct Trigger {
        state: Arc<Mutex<(Vec<Change>, u64)>>,
    }

    impl Trigger {
        fn new() -> Trigger {
            Trigger {
                state: Arc::new(Mutex::new((Vec::new(), 0))),
            }
        }

        fn lock(&self) -> std::sync::MutexGuard<'_, (Vec<Change>, u64)> {
            self.state.lock().unwrap_or_else(|p| p.into_inner())
        }

        /// Queues a rescan of `path`; returns the cursor the writer
        /// publishes once it has applied it.
        fn rescan(&self, path: &Path) -> ResumeCursor {
            let mut st = self.lock();
            st.0.push(Change::RescanSubtree(path.to_path_buf()));
            cursor(st.1 + 1)
        }

        /// Queues a check of the one path `path`, as FSEvents reports a
        /// single change; returns the cursor published once it is applied.
        fn check(&self, path: &Path) -> ResumeCursor {
            let mut st = self.lock();
            st.0.push(Change::CheckPath(path.to_path_buf()));
            cursor(st.1 + 1)
        }
    }

    fn cursor(event_id: u64) -> ResumeCursor {
        ResumeCursor::FsEvents {
            event_id,
            db_uuid: [7; 16],
        }
    }

    impl ChangeSource for Trigger {
        fn start(&mut self, _from: Option<&ResumeCursor>) -> fx_fs::Result<StartOutcome> {
            Ok(StartOutcome::Resumed)
        }

        fn next_batch(&mut self, deadline: Instant) -> fx_fs::Result<Vec<Change>> {
            loop {
                {
                    let mut st = self.lock();
                    if !st.0.is_empty() {
                        st.1 += 1;
                        return Ok(std::mem::take(&mut st.0));
                    }
                }
                let now = Instant::now();
                if now >= deadline {
                    return Ok(Vec::new());
                }
                std::thread::sleep((deadline - now).min(Duration::from_millis(5)));
            }
        }

        fn cursor(&self) -> ResumeCursor {
            cursor(self.lock().1)
        }
    }

    // ------------------------------------------------------------ main

    struct Args {
        entries: usize,
        touch: usize,
        checks: usize,
        root: Option<PathBuf>,
        runs: usize,
        stream_kb: Option<usize>,
        hidden: bool,
        timeout: Duration,
    }

    pub fn main() -> ExitCode {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.iter().any(|a| a == "--help" || a == "-h") {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        let parsed = Flags::parse(
            &args,
            &[
                "--entries",
                "--touch",
                "--checks",
                "--root",
                "--runs",
                "--stream-kb",
                "--window",
                "--timeout",
                "--cache",
            ],
            &[],
        )
        .and_then(|f| {
            if f.get("--root").is_some() && f.get("--entries").is_some() {
                return Err(UsageError("give --entries or --root, not both".into()));
            }
            if f.get("--root").is_some() && f.get("--touch").is_some() {
                return Err(UsageError(
                    "--touch changes the synthetic tree only; --root is read only".into(),
                ));
            }
            let hidden = match f.get("--window") {
                None | Some("hidden") => true,
                Some("shown") => false,
                Some(other) => {
                    return Err(UsageError(format!(
                        "--window expects hidden or shown, got '{other}'"
                    )));
                }
            };
            let a = Args {
                entries: f.number("--entries", 500_000, 1_000, 20_000_000)? as usize,
                touch: f.number("--touch", 0, 0, 100_000)? as usize,
                checks: f.number("--checks", 0, 0, 1_000)? as usize,
                root: f.get("--root").map(PathBuf::from),
                runs: f.number("--runs", 3, 1, 20)? as usize,
                stream_kb: match f.get("--stream-kb") {
                    Some(_) => Some(f.number("--stream-kb", 0, 1, 4 << 20)? as usize),
                    None => None,
                },
                hidden,
                timeout: Duration::from_secs(f.number("--timeout", 600, 1, 86_400)?),
            };
            Ok((a, f.cache()?))
        });
        let (args, cache) = match parsed {
            Ok(p) => p,
            Err(UsageError(msg)) => {
                eprintln!("rescan_bench: {msg}\n\n{USAGE}");
                return ExitCode::from(2);
            }
        };
        header("rescan", cache);
        match run(&args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("rescan_bench: error: {e}");
                ExitCode::from(1)
            }
        }
    }

    fn run(args: &Args) -> io::Result<()> {
        let tmp;
        let root = match &args.root {
            Some(root) => std::fs::canonicalize(root)?,
            None => {
                tmp = TempRoot::new("fx-rescan-bench")?;
                let tree = generate_tree(args.entries, 0x5ca9_0001, TreeShape::default());
                let t = Instant::now();
                materialize(&tree, tmp.path(), None)?;
                metric("materialize_ms", t.elapsed().as_millis());
                tmp.path().join("root")
            }
        };
        let mut opts = ScanOptions::default();
        if let Some(kb) = args.stream_kb {
            opts.stream_budget = kb << 10;
        }
        metric("stream_budget_bytes", opts.stream_budget);
        metric("window", if args.hidden { "hidden" } else { "shown" });
        metric("touch", args.touch);

        let source = Trigger::new();
        let engine = Engine::start(
            Index::new(),
            EngineConfig::new(&root)
                .with_batch_window(Duration::from_millis(20))
                .with_background_batch_window(Duration::from_millis(20)),
            source.clone(),
            MacPathStat::new(),
            Walk(MacScanner::new(opts)),
        )?;
        engine.set_foreground(!args.hidden);
        let t = Instant::now();
        wait(&engine, args.timeout, |e| {
            e.health() == EngineHealth::Live && e.snapshot().generation() > 0
        })?;
        metric("first_scan_ms", t.elapsed().as_millis());
        let entries = engine.snapshot().index().live_count();
        metric("rescan_entries", entries);

        // The footprint's highest reading since the last reset, every 5 ms.
        let fp_peak = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let sampler = {
            let (fp_peak, stop) = (Arc::clone(&fp_peak), Arc::clone(&stop));
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    fp_peak.fetch_max(footprint(), Ordering::Relaxed);
                    std::thread::sleep(Duration::from_millis(5));
                }
            })
        };

        let mut times = Vec::new();
        let mut heap_peak = 0usize;
        let mut walk_peak = 0usize;
        let mut after_peak = 0usize;
        let mut fp_peak_max = 0u64;
        let mut snapshot_wait_max = 0u64;
        let mut touched: Vec<PathBuf> = Vec::new();
        for run in 1..=args.runs {
            touch(&root, run, args.touch, &mut touched)?;
            relieve();
            std::thread::sleep(Duration::from_millis(300));
            relieve();
            let heap0 = LIVE.load(Ordering::Relaxed);
            PHASE.store(BEFORE_WALK, Ordering::Relaxed);
            PEAK.store(heap0, Ordering::Relaxed);
            WALK_PEAK.store(heap0, Ordering::Relaxed);
            AFTER_PEAK.store(heap0, Ordering::Relaxed);
            let fp0 = footprint();
            fp_peak.store(fp0, Ordering::Relaxed);

            let t = Instant::now();
            let want = source.rescan(&root);
            SNAPSHOT_WAIT_MAX_US.store(0, Ordering::Relaxed);
            wait(&engine, args.timeout, |e| {
                // A reader every millisecond: how long `snapshot()` keeps
                // it while the writer merges the orders in place.
                let asked = Instant::now();
                let snap = e.snapshot();
                let us = asked.elapsed().as_micros() as u64;
                SNAPSHOT_WAIT_MAX_US.fetch_max(us, Ordering::Relaxed);
                snap.cursor() == Some(want)
            })?;
            times.push(t.elapsed());
            let waited = SNAPSHOT_WAIT_MAX_US.load(Ordering::Relaxed);
            snapshot_wait_max = snapshot_wait_max.max(waited);
            metric(
                &format!("rescan_run{run}_snapshot_wait_max_ms"),
                format!("{:.3}", waited as f64 / 1e3),
            );
            let above = |peak: &AtomicUsize| peak.load(Ordering::Relaxed).saturating_sub(heap0);
            let (whole, walk, after) = (above(&PEAK), above(&WALK_PEAK), above(&AFTER_PEAK));
            metric(&format!("rescan_run{run}_walk_heap_peak_mb"), mb(walk));
            metric(
                &format!("rescan_run{run}_after_walk_heap_peak_mb"),
                mb(after),
            );
            metric(&format!("rescan_run{run}_heap_peak_mb"), mb(whole));
            heap_peak = heap_peak.max(whole);
            walk_peak = walk_peak.max(walk);
            after_peak = after_peak.max(after);
            fp_peak_max = fp_peak_max.max(fp_peak.load(Ordering::Relaxed).saturating_sub(fp0));
        }
        if args.checks > 0 {
            small_batches(&engine, &source, &root, args)?;
        }
        stop.store(true, Ordering::Relaxed);
        let _ = sampler.join();
        engine.shutdown();

        stats("rescan", &times);
        metric("rescan_walk_heap_peak_mb", mb(walk_peak));
        metric("rescan_after_walk_heap_peak_mb", mb(after_peak));
        metric("rescan_heap_peak_mb", mb(heap_peak));
        metric(
            "rescan_footprint_peak_mb",
            format!("{:.1}", fp_peak_max as f64 / 1e6),
        );
        metric(
            "rescan_snapshot_wait_max_ms",
            format!("{:.3}", snapshot_wait_max as f64 / 1e3),
        );
        Ok(())
    }

    /// The longest `Engine::snapshot()` call of the current rescan, in
    /// microseconds.
    static SNAPSHOT_WAIT_MAX_US: AtomicU64 = AtomicU64::new(0);

    /// `--checks N`: after the rescans, `N` small batches of one change each
    /// (a file this benchmark adds, then removes, in a folder of its own in
    /// the system's temporary folder, which must lie under the root), as
    /// the window-hidden app commits them all day. Reports the longest
    /// `snapshot()` call and the commit's heap peak over them.
    fn small_batches(
        engine: &Engine,
        source: &Trigger,
        root: &Path,
        args: &Args,
    ) -> io::Result<()> {
        let dir = TempRoot::new("fx-rescan-bench-checks")?;
        if !dir.path().starts_with(root) {
            metric(
                "checks_skipped",
                "the temporary folder lies outside the root",
            );
            return Ok(());
        }
        let (mut wait_max, mut peak_max) = (0u64, 0usize);
        for i in 0..args.checks {
            let path = dir.path().join(format!("check-{i}.txt"));
            for add in [true, false] {
                if add {
                    std::fs::write(&path, b"x")?;
                } else {
                    std::fs::remove_file(&path)?;
                }
                let heap0 = LIVE.load(Ordering::Relaxed);
                PHASE.store(BEFORE_WALK, Ordering::Relaxed);
                PEAK.store(heap0, Ordering::Relaxed);
                SNAPSHOT_WAIT_MAX_US.store(0, Ordering::Relaxed);
                let want = source.check(&path);
                wait(engine, args.timeout, |e| {
                    let asked = Instant::now();
                    let snap = e.snapshot();
                    let us = asked.elapsed().as_micros() as u64;
                    SNAPSHOT_WAIT_MAX_US.fetch_max(us, Ordering::Relaxed);
                    snap.cursor() == Some(want)
                })?;
                wait_max = wait_max.max(SNAPSHOT_WAIT_MAX_US.load(Ordering::Relaxed));
                peak_max = peak_max.max(PEAK.load(Ordering::Relaxed).saturating_sub(heap0));
            }
        }
        metric("checks_batches", args.checks * 2);
        metric(
            "checks_snapshot_wait_max_ms",
            format!("{:.3}", wait_max as f64 / 1e3),
        );
        metric("checks_commit_heap_peak_mb", mb(peak_max));
        Ok(())
    }

    /// Bytes as megabytes, with one decimal.
    fn mb(bytes: usize) -> String {
        format!("{:.1}", bytes as f64 / 1e6)
    }

    /// Before rescan `run` of the synthetic tree: removes the files the
    /// previous run added (`touched`) and adds `count` new ones in the
    /// tree's top folder, so the rescan has removals and new entries to
    /// commit. Only files this benchmark made in its own temporary folder
    /// are removed.
    fn touch(root: &Path, run: usize, count: usize, touched: &mut Vec<PathBuf>) -> io::Result<()> {
        for path in touched.drain(..) {
            std::fs::remove_file(path)?;
        }
        for i in 0..count {
            let path = root.join(format!("rescan-touch-{run}-{i}.txt"));
            std::fs::write(&path, b"x")?;
            touched.push(path);
        }
        Ok(())
    }

    /// Waits until `done` holds, polling every millisecond.
    fn wait(engine: &Engine, timeout: Duration, done: impl Fn(&Engine) -> bool) -> io::Result<()> {
        let t = Instant::now();
        loop {
            if done(engine) {
                return Ok(());
            }
            if let EngineHealth::Failed(why) = engine.health() {
                return Err(io::Error::other(format!("engine failed: {why}")));
            }
            if t.elapsed() > timeout {
                return Err(io::Error::other(format!(
                    "the engine did not get there within {} s",
                    timeout.as_secs()
                )));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
