//! The soak test (run 2.2; metric C3 of `docs/gates/performance.md`).
//!
//! Runs the real engine, with the macOS FSEvents source, path stat and
//! scanner configured as the app configures them, over a fresh folder in
//! `--dir` (default `$TMPDIR`), and applies a long seeded stream of random
//! file operations to it (`fx_testkit::soak`: creates, rewrites, replaces,
//! renames and case-only renames, moves of files and folders, deletes of
//! files and whole folders, folders moved in from outside and out of it,
//! name swaps, deep trees and bursts) at `--rate` operations a second.
//!
//! Every `--check-every` it stops and waits for the published index to
//! equal a fresh scan of the folder (ground truth): the same paths, each
//! with the same file key, kind and file size, which is what the catch-up
//! check (C2) compares, and every merged sort order equal to a fresh build.
//! The index must stay equal for 250 ms to count. Whatever still differs
//! after `--settle-timeout` is counted, printed with the operations that
//! led to it, and the engine is restarted from a fresh scan so later checks
//! start clean. Every `--restart-every` checks the engine is closed (which
//! saves its index), some operations run while it is down, and it is opened
//! again from the saved cursor, so FSEvents history replay is soaked too.
//!
//! It prints one line per check, then `key=value` metrics:
//! `soak_differences` (paths that differed plus sort orders that differed:
//! C3, which must be 0), its parts, the operations, checks and restarts,
//! settle times, the most entries at one check that were otherwise equal
//! but had another modification time (`soak_metadata_differences`, with
//! the folders among them; reported, not counted), and the
//! process's memory footprint from the first check with the tree near its
//! cap to the end (`soak_memory_growth_pct`; the plan allows 5%).
//!
//! Exit codes: 0 without differences, 1 with differences, 2 for bad
//! arguments, 3 when the harness itself failed (an operation's I/O error,
//! a folder that no longer matches the planner's model, or an engine that
//! does not match a fresh scan even after a fresh start).
//!
//! It only creates, changes and removes entries inside the folder it makes
//! (`fx-soak-...` in `--dir`), and removes that folder at the end unless
//! `--keep` is given.
//!
//! ```text
//! cargo run --release -p fx-engine --example soak -- --duration 24h
//! ```

use std::process::ExitCode;

#[cfg(not(target_os = "macos"))]
fn main() -> ExitCode {
    println!("soak: the soak runs the macOS FSEvents source; nothing to do on this system");
    ExitCode::SUCCESS
}

#[cfg(target_os = "macos")]
fn main() -> ExitCode {
    mac::main()
}

#[cfg(target_os = "macos")]
mod mac {
    use std::collections::{BTreeMap, HashMap, VecDeque};
    use std::ffi::{c_int, c_void};
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use fx_bench::{MachineInfo, Stats};
    use fx_engine::{ENGINE_SORT_KEYS, Engine, EngineConfig, EngineSnapshot};
    use fx_fs::VolumeEnumerator;
    use fx_fs_macos::enumerator::MacScanner;
    use fx_fs_macos::fsevents::source::FsEventsSource;
    use fx_fs_macos::fsevents::stream::StreamConfig;
    use fx_fs_macos::path_stat::MacPathStat;
    use fx_fs_macos::scan::ScanOptions;
    use fx_model::{Entry, EntryId, EntryKind, FileKey};
    use fx_sort::Order;
    use fx_testkit::soak::{self, Diff, Kind, Limits, Listing, Planner, diff, parse_duration};

    const USAGE: &str = "\
usage: soak [--duration D] [--seed N] [--rate OPS] [--check-every D]
            [--restart-every N] [--max-entries N] [--settle-timeout D]
            [--dir PATH] [--keep]

  --duration D        how long to run: 90 (seconds), 90s, 20m or 24h
                      (default 10m)
  --seed N            the operation stream's seed (default 1)
  --rate OPS          operations a second, 1..=10000 (default 10)
  --check-every D     time between comparisons with a fresh scan (default 30s)
  --restart-every N   close and reopen the engine every N checks, with
                      operations made while it is down; 0 never (default 10)
  --max-entries N     files and folders alive at once, 10..=1000000
                      (default 10000)
  --settle-timeout D  how long a check waits for the index to equal the
                      fresh scan (default 30s)
  --dir PATH          where the soak folder is made (default $TMPDIR)
  --keep              leave the soak folder in place at the end";

    /// How long the index must stay equal to the scan for a check to pass:
    /// more than two batch windows, so a late batch would show.
    const STABLE: Duration = Duration::from_millis(250);
    /// Time between polls while a check waits.
    const POLL: Duration = Duration::from_millis(50);
    /// Steps made while the engine is down at each restart.
    const OFFLINE_STEPS: usize = 25;
    /// Operations remembered for the report of a difference.
    const HISTORY: usize = 200;
    /// Paths printed per kind of difference.
    const SHOWN: usize = 20;

    struct Args {
        duration: Duration,
        seed: u64,
        rate: u64,
        check_every: Duration,
        restart_every: u64,
        max_entries: usize,
        settle_timeout: Duration,
        dir: PathBuf,
        keep: bool,
    }

    fn parse_args(args: &[String]) -> Result<Args, String> {
        let mut a = Args {
            duration: Duration::from_secs(600),
            seed: 1,
            rate: 10,
            check_every: Duration::from_secs(30),
            restart_every: 10,
            max_entries: 10_000,
            settle_timeout: Duration::from_secs(30),
            dir: std::env::temp_dir(),
            keep: false,
        };
        let mut it = args.iter();
        while let Some(flag) = it.next() {
            if flag == "--keep" {
                a.keep = true;
                continue;
            }
            let value = it
                .next()
                .ok_or_else(|| format!("{flag} needs a value"))?
                .as_str();
            let duration = || {
                parse_duration(value)
                    .filter(|d| !d.is_zero())
                    .ok_or_else(|| {
                        format!("{flag} expects a duration such as 90s, 20m or 24h, got '{value}'")
                    })
            };
            let number = |lo: u64, hi: u64| {
                value
                    .parse::<u64>()
                    .ok()
                    .filter(|n| (lo..=hi).contains(n))
                    .ok_or_else(|| {
                        format!("{flag} expects a whole number in {lo}..={hi}, got '{value}'")
                    })
            };
            match flag.as_str() {
                "--duration" => a.duration = duration()?,
                "--seed" => a.seed = number(0, u64::MAX)?,
                "--rate" => a.rate = number(1, 10_000)?,
                "--check-every" => a.check_every = duration()?,
                "--restart-every" => a.restart_every = number(0, 1_000_000)?,
                "--max-entries" => a.max_entries = number(10, 1_000_000)? as usize,
                "--settle-timeout" => a.settle_timeout = duration()?,
                "--dir" => a.dir = PathBuf::from(value),
                _ => return Err(format!("unknown argument '{flag}'")),
            }
        }
        Ok(a)
    }

    pub fn main() -> ExitCode {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.iter().any(|a| a == "-h" || a == "--help") {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        let args = match parse_args(&args) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("soak: {e}\n{USAGE}");
                return ExitCode::from(2);
            }
        };
        match run(&args) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::from(1),
            Err(e) => {
                eprintln!("soak: {e}");
                ExitCode::from(3)
            }
        }
    }

    // ------------------------------------------------------------ folders

    /// The soak's own folder: `root` (watched), `outside` (where folders
    /// moved in are built) and the engine's stores. Removed on drop unless
    /// kept.
    struct SoakDir {
        base: PathBuf,
        keep: bool,
    }

    impl SoakDir {
        fn new(parent: &Path, seed: u64, keep: bool) -> io::Result<SoakDir> {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let base = parent.join(format!("fx-soak-{seed}-{}-{nanos}", std::process::id()));
            // `create_dir` fails when anything is there, so the soak never
            // works in a folder it didn't make.
            fs::create_dir(&base)?;
            // Canonical, so paths match what FSEvents reports
            // (`/private/var/...` for `$TMPDIR`).
            let base = fs::canonicalize(&base)?;
            let dir = SoakDir { base, keep };
            for sub in ["root", "outside", "stores"] {
                fs::create_dir(dir.base.join(sub))?;
            }
            Ok(dir)
        }

        fn root(&self) -> PathBuf {
            self.base.join("root")
        }

        fn outside(&self) -> PathBuf {
            self.base.join("outside")
        }

        fn store(&self, n: u64) -> PathBuf {
            self.base.join("stores").join(format!("index-{n}.fxdb"))
        }
    }

    impl Drop for SoakDir {
        fn drop(&mut self) {
            if self.keep {
                println!("soak folder kept at {}", self.base.display());
            } else {
                let _ = fs::remove_dir_all(&self.base);
            }
        }
    }

    /// Opens the engine as the app does (`fx-ui-macos`' `open_engine`):
    /// the FSEvents stream filtered to the root, the default batch window
    /// and checkpoint policy, and a store. A store that holds a cursor
    /// resumes from it.
    fn open(root: &Path, store: &Path) -> io::Result<Engine> {
        let mut stream = StreamConfig::new(root.to_path_buf());
        stream.path_filter = Some(root.to_path_buf());
        Engine::open(
            EngineConfig::new(root).with_store_path(store),
            FsEventsSource::new(stream),
            MacPathStat::new(),
            MacScanner::new(ScanOptions::default()),
        )
    }

    // --------------------------------------------------------- listings

    /// What the comparison looks at, per path: as the catch-up check (C2)
    /// compares.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Item {
        key: FileKey,
        kind: EntryKind,
        /// File size; 0 for folders and symlinks.
        size: u64,
    }

    /// Items and modification times by path below the root.
    #[derive(Default)]
    struct View {
        items: BTreeMap<String, Item>,
        modified: HashMap<String, i64>,
    }

    /// Paths of `entries` below the root: the root (no parent) is left
    /// out, and a walk stops at a parent that is the root or not in the map
    /// (the scanner does not list the folder it scans).
    fn view(entries: &HashMap<FileKey, Entry>) -> View {
        let mut out = View::default();
        for e in entries.values().filter(|e| e.parent.is_some()) {
            let mut parts = vec![e.name.as_str()];
            let mut parent = e.parent;
            while let Some(p) = parent.and_then(|k| entries.get(&k)) {
                if p.parent.is_none() || parts.len() > 4096 {
                    break;
                }
                parts.push(p.name.as_str());
                parent = p.parent;
            }
            parts.reverse();
            let path = parts.join("/");
            let size = if e.kind == EntryKind::File { e.size } else { 0 };
            out.modified.insert(path.clone(), e.modified);
            out.items.insert(
                path,
                Item {
                    key: e.key,
                    kind: e.kind,
                    size,
                },
            );
        }
        out
    }

    fn index_view(snap: &EngineSnapshot) -> View {
        let idx = snap.index();
        let ids: Vec<EntryId> = idx.live_ids().collect();
        let entries: HashMap<FileKey, Entry> = ids
            .into_iter()
            .map(|id| {
                let p = idx.parent(id);
                let e = Entry {
                    key: idx.key(id),
                    parent: if p.is_none() { None } else { Some(idx.key(p)) },
                    name: idx.name(id).to_string(),
                    kind: idx.kind(id),
                    flags: idx.flags(id),
                    size: idx.size(id),
                    modified: idx.modified(id),
                    created: idx.created(id),
                };
                (e.key, e)
            })
            .collect();
        view(&entries)
    }

    fn scan_view(root: &Path) -> io::Result<View> {
        let mut entries = HashMap::new();
        MacScanner::new(ScanOptions::default())
            .enumerate(root, &mut |e: Entry| {
                entries.insert(e.key, e);
            })
            .map_err(|e| io::Error::other(format!("fresh scan: {e}")))?;
        Ok(view(&entries))
    }

    /// The scan in the planner's terms, to check the folder against its
    /// model. A symlink (the planner makes none) never matches.
    fn as_listing(v: &View) -> Listing {
        v.items
            .iter()
            .map(|(p, it)| {
                let value = match it.kind {
                    EntryKind::File => (Kind::File, it.size),
                    EntryKind::Dir => (Kind::Dir, 0),
                    EntryKind::Symlink => (Kind::File, u64::MAX),
                };
                (p.clone(), value)
            })
            .collect()
    }

    /// The merged sort orders that differ from a fresh build.
    fn order_differences(snap: &EngineSnapshot) -> Vec<String> {
        let idx = snap.index();
        let mut out = Vec::new();
        for &key in ENGINE_SORT_KEYS.iter() {
            match snap.order(key) {
                None => out.push(format!("no {key:?} order")),
                Some(order) => {
                    if let Err(e) = order.validate(idx) {
                        out.push(format!("{key:?} order invalid: {e:?}"));
                    } else if order.ids != Order::build(idx, key).ids {
                        out.push(format!("{key:?} order differs from a fresh build"));
                    }
                }
            }
        }
        out
    }

    // ------------------------------------------------------------ checks

    /// The outcome of one check.
    struct Check {
        /// From the start of the check to the first of the equal polls.
        settled: Option<Duration>,
        diff: Diff,
        orders: Vec<String>,
        /// Equal entries whose modification times differ: files, folders.
        metadata: (usize, usize),
        entries: usize,
        index: View,
        scan: View,
    }

    /// Waits until the index equals a fresh scan and the orders equal fresh
    /// builds, for [`STABLE`] in a row, or until `timeout`.
    fn settle(engine: &Engine, root: &Path, timeout: Duration) -> io::Result<Check> {
        let t0 = Instant::now();
        let mut first_equal: Option<Instant> = None;
        loop {
            let scan = scan_view(root)?;
            let snap = engine.snapshot();
            let index = index_view(&snap);
            let d = diff(&scan.items, &index.items);
            let orders = order_differences(&snap);
            let now = Instant::now();
            let equal = d.is_empty() && orders.is_empty();
            if equal {
                let since = *first_equal.get_or_insert(now);
                if now - since >= STABLE {
                    return Ok(finish(Some(since - t0), d, orders, index, scan));
                }
            } else {
                first_equal = None;
            }
            if now - t0 >= timeout {
                return Ok(finish(None, d, orders, index, scan));
            }
            thread::sleep(POLL);
        }
    }

    fn finish(
        settled: Option<Duration>,
        diff: Diff,
        orders: Vec<String>,
        index: View,
        scan: View,
    ) -> Check {
        let mut files = 0;
        let mut dirs = 0;
        for (path, item) in &scan.items {
            if index.items.get(path) != Some(item) {
                continue;
            }
            if index.modified.get(path) != scan.modified.get(path) {
                if item.kind == EntryKind::Dir {
                    dirs += 1;
                } else {
                    files += 1;
                }
            }
        }
        Check {
            settled,
            diff,
            orders,
            metadata: (files, dirs),
            entries: scan.items.len(),
            index,
            scan,
        }
    }

    /// Prints what differed, with the operations that led to it.
    fn report(check: &Check, history: &VecDeque<String>) {
        let describe = |v: &View, p: &str| {
            v.items
                .get(p)
                .map(|it| format!("{:?} key {} size {}", it.kind, it.key.file_id, it.size))
                .unwrap_or_default()
        };
        for (label, paths, side) in [
            ("missing from the index", &check.diff.missing, &check.scan),
            ("extra in the index", &check.diff.extra, &check.index),
            ("changed", &check.diff.changed, &check.scan),
        ] {
            for p in paths.iter().take(SHOWN) {
                let detail = if label == "changed" {
                    format!(
                        "on disk {}, indexed {}",
                        describe(&check.scan, p),
                        describe(&check.index, p)
                    )
                } else {
                    describe(side, p)
                };
                println!("  {label}: {p} ({detail})");
            }
            if paths.len() > SHOWN {
                println!("  ... and {} more {label}", paths.len() - SHOWN);
            }
        }
        for o in &check.orders {
            println!("  {o}");
        }
        println!("  the last {} operations, oldest first:", history.len());
        for op in history {
            println!("    {op}");
        }
    }

    // --------------------------------------------------------- footprint

    unsafe extern "C" {
        /// `libproc.h`: fills `buffer` with the `rusage_info` of `flavor`.
        fn proc_pid_rusage(pid: c_int, flavor: c_int, buffer: *mut c_void) -> c_int;
    }

    /// `RUSAGE_INFO_V6` (`sys/resource.h`).
    const RUSAGE_INFO_V6: c_int = 6;

    /// This process's physical footprint in bytes, as Activity Monitor's
    /// Memory column shows it.
    fn footprint() -> Option<u64> {
        // `struct rusage_info_v6` is 464 bytes, 58 words; `ri_phys_footprint`
        // is its tenth word (after the 16-byte UUID and seven counters).
        let mut info = [0u64; 58];
        let pid = c_int::try_from(std::process::id()).ok()?;
        // SAFETY: `info` is 464 writable, 8-byte-aligned bytes, the size of
        // `struct rusage_info_v6`, which is what the kernel writes for the
        // `RUSAGE_INFO_V6` flavor.
        let rc = unsafe { proc_pid_rusage(pid, RUSAGE_INFO_V6, info.as_mut_ptr().cast()) };
        (rc == 0).then_some(info[9])
    }

    // --------------------------------------------------------------- run

    #[derive(Default)]
    struct Tally {
        ops: u64,
        steps: u64,
        checks: u64,
        restarts: u64,
        resyncs: u64,
        index_differences: u64,
        order_differences: u64,
        /// The most equal entries with different modification times at one
        /// check, and how many of those were folders.
        metadata_max: usize,
        metadata_max_dirs: usize,
        unsettled: u64,
        settle: Vec<Duration>,
        entries_max: usize,
        entries_end: usize,
        footprint_steady: Option<u64>,
        footprint_end: Option<u64>,
        footprint_max: u64,
    }

    fn mb(bytes: u64) -> f64 {
        bytes as f64 / 1_048_576.0
    }

    /// Applies one step, counting its operations and remembering them.
    fn apply_step(
        planner: &mut Planner,
        dir: &SoakDir,
        tally: &mut Tally,
        history: &mut VecDeque<String>,
        note: &str,
    ) -> io::Result<u64> {
        let step = planner.next_step();
        let mut weight = 0;
        for op in &step.ops {
            soak::apply(&dir.root(), &dir.outside(), op)
                .map_err(|e| io::Error::new(e.kind(), format!("{op:?}: {e}")))?;
            weight += op.weight();
            if history.len() == HISTORY {
                history.pop_front();
            }
            history.push_back(format!("{note}{op:?}"));
        }
        tally.ops += weight;
        tally.steps += 1;
        Ok(weight)
    }

    fn run(args: &Args) -> io::Result<bool> {
        let machine = MachineInfo::detect();
        println!(
            "soak: seed {}, {} ops/s, check every {} s, restart every {} checks, at most {} entries, {} s",
            args.seed,
            args.rate,
            args.check_every.as_secs(),
            args.restart_every,
            args.max_entries,
            args.duration.as_secs()
        );
        println!(
            "machine: {} ({} {}, {} threads)",
            machine.cpu_brand, machine.os, machine.arch, machine.parallelism
        );
        let dir = SoakDir::new(&args.dir, args.seed, args.keep)?;
        let root = dir.root();
        let mut planner = Planner::new(args.seed, Limits::new(args.max_entries));
        let mut tally = Tally::default();
        let mut history: VecDeque<String> = VecDeque::with_capacity(HISTORY);
        let mut store_n = 0;
        let mut engine = open(&root, &dir.store(store_n))?;
        let first = settle(&engine, &root, args.settle_timeout)?;
        if first.settled.is_none() {
            return Err(io::Error::other(
                "the first scan of the empty folder never matched",
            ));
        }

        let period = Duration::from_secs_f64(1.0 / args.rate as f64);
        let start = Instant::now();
        let end = start + args.duration;
        while Instant::now() < end {
            // Operations until the next check.
            let phase_end = (Instant::now() + args.check_every).min(end);
            let mut tick = Instant::now();
            while Instant::now() < phase_end {
                let weight = apply_step(&mut planner, &dir, &mut tally, &mut history, "")?;
                tick += period * u32::try_from(weight).unwrap_or(u32::MAX);
                let now = Instant::now();
                if tick > now {
                    thread::sleep((tick - now).min(phase_end.saturating_duration_since(now)));
                } else {
                    // Fell behind: carry on from now rather than catch up
                    // in a burst.
                    tick = now;
                }
            }
            tally.checks += 1;
            let restart = args.restart_every > 0 && tally.checks % args.restart_every == 0;
            if restart {
                engine
                    .close()
                    .map_err(|e| io::Error::other(format!("checkpoint on close: {e:?}")))?;
                for _ in 0..OFFLINE_STEPS {
                    apply_step(&mut planner, &dir, &mut tally, &mut history, "(offline) ")?;
                }
                engine = open(&root, &dir.store(store_n))?;
                tally.restarts += 1;
            }

            let check = settle(&engine, &root, args.settle_timeout)?;
            let model = diff(&planner.listing(), &as_listing(&check.scan));
            if !model.is_empty() {
                return Err(io::Error::other(format!(
                    "check {}: the folder no longer matches the planner's model \
                     (missing {:?}, extra {:?}, changed {:?})",
                    tally.checks,
                    &model.missing[..model.missing.len().min(SHOWN)],
                    &model.extra[..model.extra.len().min(SHOWN)],
                    &model.changed[..model.changed.len().min(SHOWN)]
                )));
            }
            let fp = footprint();
            if let Some(f) = fp {
                tally.footprint_max = tally.footprint_max.max(f);
                tally.footprint_end = Some(f);
                if tally.footprint_steady.is_none() && check.entries * 10 >= args.max_entries * 9 {
                    tally.footprint_steady = Some(f);
                }
            }
            tally.entries_max = tally.entries_max.max(check.entries);
            tally.entries_end = check.entries;
            let (files, dirs) = check.metadata;
            if files + dirs > tally.metadata_max {
                tally.metadata_max = files + dirs;
                tally.metadata_max_dirs = dirs;
            }
            let differences = check.diff.count() + check.orders.len();
            match check.settled {
                Some(d) => tally.settle.push(d),
                None => tally.unsettled += 1,
            }
            println!(
                "check {}{}: {} differences, {}, {} entries, {} ops, footprint {}, {:.1} min",
                tally.checks,
                if restart { " (after a restart)" } else { "" },
                differences,
                check.settled.map_or("not settled".to_string(), |d| format!(
                    "settled in {} ms",
                    d.as_millis()
                )),
                check.entries,
                tally.ops,
                fp.map_or("unknown".to_string(), |f| format!("{:.1} MB", mb(f))),
                start.elapsed().as_secs_f64() / 60.0
            );
            if differences > 0 {
                tally.index_differences += check.diff.count() as u64;
                tally.order_differences += check.orders.len() as u64;
                report(&check, &history);
                // Start clean from a fresh scan, so the next check counts
                // only new differences.
                drop(engine);
                store_n += 1;
                tally.resyncs += 1;
                engine = open(&root, &dir.store(store_n))?;
                let fresh = settle(&engine, &root, args.settle_timeout)?;
                if fresh.settled.is_none() {
                    report(&fresh, &history);
                    return Err(io::Error::other(
                        "after a fresh start the index still differs from a fresh scan",
                    ));
                }
            }
        }
        let elapsed = start.elapsed();
        engine
            .close()
            .map_err(|e| io::Error::other(format!("checkpoint on close: {e:?}")))?;

        let differences = tally.index_differences + tally.order_differences;
        let metric = |k: &str, v: String| println!("soak_{k}={v}");
        metric("differences", differences.to_string());
        metric("index_differences", tally.index_differences.to_string());
        metric("order_differences", tally.order_differences.to_string());
        metric("metadata_differences", tally.metadata_max.to_string());
        metric(
            "metadata_dir_differences",
            tally.metadata_max_dirs.to_string(),
        );
        metric("unsettled_checks", tally.unsettled.to_string());
        metric("ops", tally.ops.to_string());
        metric("steps", tally.steps.to_string());
        metric("checks", tally.checks.to_string());
        metric("restarts", tally.restarts.to_string());
        metric("resyncs", tally.resyncs.to_string());
        metric("seed", args.seed.to_string());
        metric("minutes", format!("{:.1}", elapsed.as_secs_f64() / 60.0));
        metric("entries_max", tally.entries_max.to_string());
        metric("entries_end", tally.entries_end.to_string());
        if let Some(s) = Stats::from_samples(&tally.settle) {
            let ms = |d: Duration| format!("{:.1}", d.as_secs_f64() * 1000.0);
            metric("settle_p50_ms", ms(s.p50));
            metric("settle_p99_ms", ms(s.p99));
            metric("settle_max_ms", ms(s.max));
        }
        if tally.footprint_max > 0 {
            metric("footprint_max_bytes", tally.footprint_max.to_string());
        }
        match (tally.footprint_steady, tally.footprint_end) {
            (Some(steady), Some(last)) if steady > 0 => {
                metric("footprint_steady_bytes", steady.to_string());
                metric("footprint_end_bytes", last.to_string());
                let growth = (last as f64 - steady as f64) / steady as f64 * 100.0;
                metric("memory_growth_pct", format!("{growth:.1}"));
            }
            _ => println!(
                "soak: memory growth not measured: the tree never reached 90% of --max-entries"
            ),
        }
        println!(
            "soak: {} differences in {} checks over {:.1} min ({} ops)",
            differences,
            tally.checks,
            elapsed.as_secs_f64() / 60.0,
            tally.ops
        );
        Ok(differences == 0)
    }
}
