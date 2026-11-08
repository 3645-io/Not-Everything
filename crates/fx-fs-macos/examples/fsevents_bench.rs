//! `fsevents_bench`: measures the FSEvents change stream.
//!
//! ```text
//! cargo run --release -p fx-fs-macos --example fsevents_bench -- latency --ops 1000 --cache warm
//! cargo run --release -p fx-fs-macos --example fsevents_bench -- replay --ops 10000
//! ```
//!
//! * `latency`: starts a live [`VolumeStream`] filtered to a fresh temp
//!   directory and performs `N` scripted operations in rotation (create,
//!   rename, delete). Each sample is the time from just before the syscall
//!   to the moment the stream's callback received the matching event. It
//!   prints p50, p95, p99 and max (nearest rank, from `fx-bench`), the
//!   machine and cache label, and `GATE live p95 <= 50 ms: PASS/FAIL`.
//! * `replay`: takes a cursor from a running stream, stops it, applies `N`
//!   offline changes, resumes from the saved cursor (after a round trip
//!   through its byte encoding) and collects everything up to the
//!   HistoryDone marker. It prints `GATE replay exact: PASS/FAIL` (every
//!   scripted path was replayed, or covered by a subtree rescan of an
//!   ancestor, and nothing outside the scripted paths and their ancestors
//!   was), the time from start to HistoryDone and the event count.
//!
//! Exit codes: 0 after a completed run (gate results do not change it), 1
//! when the stream or a file operation fails, 2 for bad arguments. On hosts
//! other than macOS it prints a message and exits with 0.
//!
//! Read-only with respect to the user's files: the benchmark only creates,
//! renames and removes entries inside its own directory under
//! `std::env::temp_dir()`, and removes that directory at the end.

use std::process::ExitCode;

#[cfg(not(target_os = "macos"))]
fn main() -> ExitCode {
    println!("fsevents_bench requires macOS (FSEvents); nothing to do on this OS");
    ExitCode::SUCCESS
}

#[cfg(target_os = "macos")]
fn main() -> ExitCode {
    bench::main()
}

#[cfg(target_os = "macos")]
mod bench {
    use std::collections::BTreeSet;
    use std::fs;
    use std::io::{self, Write};
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;
    use std::sync::mpsc::RecvTimeoutError;
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use fx_bench::{CacheState, MachineInfo, Stats};
    use fx_fs_macos::fsevents::cursor::FsEventsCursor;
    use fx_fs_macos::fsevents::flags::EventFlags;
    use fx_fs_macos::fsevents::stream::{RawEvent, StreamConfig, VolumeStream};
    use fx_model::{ResumeCursor, StartOutcome};

    const USAGE: &str = "\
usage:
  fsevents_bench latency [--ops N] [--cache warm|cold|unknown]
  fsevents_bench replay  [--ops N]

  latency       live latency: N operations (create, rename, delete in
                rotation), each timed from just before the syscall to the
                receipt of its event (default N = 1000)
  replay        offline replay: save a cursor, stop, apply N changes,
                resume and compare the replayed paths with the script
                (default N = 1000)
  --ops N       number of scripted operations, 1..=1000000
  --cache STATE cache state label for the report (latency only;
                default unknown)

Gates: live p95 <= 50 ms; replay exact.";

    /// Gate: live latency p95.
    const GATE_P95: Duration = Duration::from_millis(50);
    /// Largest accepted `--ops`.
    const MAX_OPS: usize = 1_000_000;
    /// Default `--ops`.
    const DEFAULT_OPS: usize = 1_000;
    /// How long to wait for one live event before counting it as missed.
    const EVENT_TIMEOUT: Duration = Duration::from_secs(5);
    /// Files per bucket directory in the replay script.
    const FILES_PER_DIR: usize = 250;

    struct UsageError(String);

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mode {
        Latency,
        Replay,
    }

    struct Args {
        mode: Mode,
        ops: usize,
        cache: CacheState,
    }

    pub fn main() -> ExitCode {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.iter().any(|a| a == "--help" || a == "-h") {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        let args = match parse(&args) {
            Ok(a) => a,
            Err(UsageError(msg)) => {
                eprintln!("fsevents_bench: {msg}\n\n{USAGE}");
                return ExitCode::from(2);
            }
        };
        let result = match args.mode {
            Mode::Latency => latency(&args),
            Mode::Replay => replay(&args),
        };
        match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("fsevents_bench: error: {e}");
                ExitCode::from(1)
            }
        }
    }

    fn parse(args: &[String]) -> Result<Args, UsageError> {
        let mut it = args.iter();
        let mode = match it.next().map(String::as_str) {
            Some("latency") => Mode::Latency,
            Some("replay") => Mode::Replay,
            Some(other) => {
                return Err(UsageError(format!(
                    "unknown mode '{other}' (expected latency or replay)"
                )));
            }
            None => return Err(UsageError("missing mode (latency or replay)".into())),
        };
        let mut ops: Option<usize> = None;
        let mut cache: Option<CacheState> = None;
        while let Some(arg) = it.next() {
            let flag = arg.as_str();
            let mut value = || {
                it.next()
                    .map(String::as_str)
                    .ok_or_else(|| UsageError(format!("{flag} needs a value")))
            };
            let duplicate = match flag {
                "--ops" => ops.replace(op_count(value()?)?).is_some(),
                "--cache" if mode == Mode::Latency => {
                    cache.replace(cache_state(value()?)?).is_some()
                }
                "--cache" => {
                    return Err(UsageError("--cache only applies to latency".into()));
                }
                other if other.starts_with('-') => {
                    return Err(UsageError(format!("unknown flag '{other}'")));
                }
                other => return Err(UsageError(format!("unexpected argument '{other}'"))),
            };
            if duplicate {
                return Err(UsageError(format!("{flag} given more than once")));
            }
        }
        Ok(Args {
            mode,
            ops: ops.unwrap_or(DEFAULT_OPS),
            cache: cache.unwrap_or(CacheState::Unknown),
        })
    }

    /// Parses `--ops`: a decimal integer in 1..=MAX_OPS; `_` separators
    /// are allowed.
    fn op_count(text: &str) -> Result<usize, UsageError> {
        let digits: String = text.chars().filter(|&c| c != '_').collect();
        let n = (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
            .then(|| digits.parse::<usize>().ok())
            .flatten();
        match n {
            Some(n) if (1..=MAX_OPS).contains(&n) => Ok(n),
            _ => Err(UsageError(format!(
                "--ops expects an integer in 1..={MAX_OPS}, got '{text}'"
            ))),
        }
    }

    fn cache_state(text: &str) -> Result<CacheState, UsageError> {
        match text {
            "warm" => Ok(CacheState::Warm),
            "cold" => Ok(CacheState::Cold),
            "unknown" => Ok(CacheState::Unknown),
            other => Err(UsageError(format!(
                "--cache expects warm, cold or unknown, got '{other}'"
            ))),
        }
    }

    /// A unique, canonical directory under `std::env::temp_dir()` that
    /// removes itself, and only itself, on drop.
    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(tag: &str) -> io::Result<TempRoot> {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let dir = std::env::temp_dir().join(format!(
                "fx-fsevents-bench-{tag}-{}-{nanos}",
                std::process::id()
            ));
            fs::create_dir(&dir)?;
            // Canonical, so `/var/...` reads `/private/var/...` like the
            // paths the stream reports.
            Ok(TempRoot(fs::canonicalize(&dir)?))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn filtered_config(root: &Path, no_defer: bool) -> StreamConfig {
        StreamConfig {
            path_filter: Some(root.to_path_buf()),
            no_defer,
            ..StreamConfig::new(root.to_path_buf())
        }
    }

    fn is_marker(ev: &RawEvent) -> bool {
        ev.flags.contains(EventFlags::HISTORY_DONE)
    }

    fn ms(d: Duration) -> String {
        format!("{:.3}", d.as_nanos() as f64 / 1_000_000.0)
    }

    fn pass_fail(ok: bool) -> &'static str {
        if ok { "PASS" } else { "FAIL" }
    }

    fn print_machine() {
        let m = MachineInfo::detect();
        println!(
            "machine: {} {}, {} threads, {}",
            m.os, m.arch, m.parallelism, m.cpu_brand
        );
    }

    // ----- latency -----------------------------------------------------

    /// One scripted live operation.
    enum LiveOp {
        Create(PathBuf),
        Rename(PathBuf, PathBuf),
        Delete(PathBuf),
    }

    impl LiveOp {
        /// The `i`-th operation: create, rename, delete in rotation, each
        /// file `k = i / 3` with its own names so no path is reused.
        fn nth(root: &Path, i: usize) -> LiveOp {
            let k = i / 3;
            let created = root.join(format!("f{k}.txt"));
            let renamed = root.join(format!("r{k}.txt"));
            match i % 3 {
                0 => LiveOp::Create(created),
                1 => LiveOp::Rename(created, renamed),
                _ => LiveOp::Delete(renamed),
            }
        }

        /// Runs the syscall.
        fn apply(&self) -> io::Result<()> {
            match self {
                LiveOp::Create(p) => fs::File::create(p).map(drop),
                LiveOp::Rename(from, to) => fs::rename(from, to),
                LiveOp::Delete(p) => fs::remove_file(p),
            }
        }

        /// The path and flag of the event that marks this operation as
        /// seen. A rename is matched on its new name.
        fn expected(&self) -> (&Path, EventFlags) {
            match self {
                LiveOp::Create(p) => (p, EventFlags::ITEM_CREATED),
                LiveOp::Rename(_, to) => (to, EventFlags::ITEM_RENAMED),
                LiveOp::Delete(p) => (p, EventFlags::ITEM_REMOVED),
            }
        }

        fn kind(&self) -> usize {
            match self {
                LiveOp::Create(_) => 0,
                LiveOp::Rename(..) => 1,
                LiveOp::Delete(_) => 2,
            }
        }
    }

    /// Waits for an event at `path` whose flags contain `want`, received
    /// at or after `since`. Other events are skipped. `None` on timeout.
    fn wait_for(
        stream: &VolumeStream,
        path: &Path,
        want: EventFlags,
        since: Instant,
    ) -> io::Result<Option<RawEvent>> {
        let deadline = Instant::now() + EVENT_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match stream.recv_timeout(left) {
                Ok(ev) if ev.path == path && ev.flags.contains(want) && ev.received >= since => {
                    return Ok(Some(ev));
                }
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => return Ok(None),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::other("stream channel disconnected"));
                }
            }
        }
    }

    fn latency(args: &Args) -> io::Result<()> {
        let root = TempRoot::new("latency")?;
        let config = filtered_config(root.path(), true);
        let stream_latency = config.latency;
        let (stream, _) = VolumeStream::start(config, None)?;
        // Let the stream settle before the first change.
        thread::sleep(Duration::from_millis(200));

        let mut samples = Vec::with_capacity(args.ops);
        let mut per_kind: [Vec<Duration>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        let mut missed = 0usize;
        for i in 0..args.ops {
            let op = LiveOp::nth(root.path(), i);
            let (path, want) = op.expected();
            let t = Instant::now();
            op.apply()?;
            match wait_for(&stream, path, want, t)? {
                Some(ev) => {
                    let d = ev.received.saturating_duration_since(t);
                    samples.push(d);
                    per_kind[op.kind()].push(d);
                }
                None => missed += 1,
            }
            if (i + 1) % 1000 == 0 {
                eprintln!("fsevents_bench: {} / {} ops", i + 1, args.ops);
                let _ = io::stderr().flush();
            }
        }
        drop(stream);

        println!(
            "fsevents_bench latency: {} ops (create, rename, delete in rotation), cache {}, \
             stream latency {} ms, no_defer on",
            args.ops,
            args.cache,
            ms(stream_latency)
        );
        print_machine();
        let Some(stats) = Stats::from_samples(&samples) else {
            println!("no event was received ({missed} missed)");
            println!("GATE live p95 <= 50 ms: FAIL");
            return Ok(());
        };
        println!(
            "all:    p50 {} ms, p95 {} ms, p99 {} ms, max {} ms (n={}, missed {missed})",
            ms(stats.p50),
            ms(stats.p95),
            ms(stats.p99),
            ms(stats.max),
            stats.count
        );
        for (name, kind) in ["create", "rename", "delete"].iter().zip(&per_kind) {
            if let Some(s) = Stats::from_samples(kind) {
                println!(
                    "{:<7} p50 {} ms, p95 {} ms, p99 {} ms, max {} ms (n={})",
                    format!("{name}:"),
                    ms(s.p50),
                    ms(s.p95),
                    ms(s.p99),
                    ms(s.max),
                    s.count
                );
            }
        }
        if missed > 0 {
            println!("note: {missed} operation(s) got no matching event within {EVENT_TIMEOUT:?}");
        }
        let ok = missed == 0 && stats.p95 <= GATE_P95;
        println!("GATE live p95 <= 50 ms: {}", pass_fail(ok));
        Ok(())
    }

    // ----- replay ------------------------------------------------------

    /// One scripted offline operation.
    enum OfflineOp {
        Mkdir(PathBuf),
        Create(PathBuf),
        Modify(PathBuf),
        Rename(PathBuf, PathBuf),
        Remove(PathBuf),
    }

    impl OfflineOp {
        fn apply(&self) -> io::Result<()> {
            match self {
                OfflineOp::Mkdir(p) => fs::create_dir(p),
                OfflineOp::Create(p) => fs::write(p, b"created"),
                OfflineOp::Modify(p) => fs::OpenOptions::new()
                    .append(true)
                    .open(p)
                    .and_then(|mut f| f.write_all(b"+modified")),
                OfflineOp::Rename(from, to) => fs::rename(from, to),
                OfflineOp::Remove(p) => fs::remove_file(p),
            }
        }

        fn paths(&self) -> Vec<&Path> {
            match self {
                OfflineOp::Mkdir(p)
                | OfflineOp::Create(p)
                | OfflineOp::Modify(p)
                | OfflineOp::Remove(p) => vec![p],
                OfflineOp::Rename(from, to) => vec![from, to],
            }
        }
    }

    /// Exactly `n` operations: bucket directories of [`FILES_PER_DIR`]
    /// files; each file is created, modified, renamed and (four times out
    /// of five) removed again.
    fn offline_script(root: &Path, n: usize) -> Vec<OfflineOp> {
        let mut ops = Vec::with_capacity(n + 4);
        let mut k = 0usize;
        while ops.len() < n {
            let dir = root.join(format!("d{}", k / FILES_PER_DIR));
            if k % FILES_PER_DIR == 0 {
                ops.push(OfflineOp::Mkdir(dir.clone()));
            }
            let created = dir.join(format!("f{k}.txt"));
            let renamed = dir.join(format!("r{k}.txt"));
            ops.push(OfflineOp::Create(created.clone()));
            ops.push(OfflineOp::Modify(created.clone()));
            ops.push(OfflineOp::Rename(created, renamed.clone()));
            if k % 5 != 0 {
                ops.push(OfflineOp::Remove(renamed));
            }
            k += 1;
        }
        ops.truncate(n);
        ops
    }

    fn replay(args: &Args) -> io::Result<()> {
        let root = TempRoot::new("replay")?;
        let dir = root.path();

        // 1. Take a cursor from a running stream, stop it, and "save" it.
        let saved = {
            let (mut stream, _) = VolumeStream::start(filtered_config(dir, false), None)?;
            stream.flush_sync();
            let cursor = FsEventsCursor::try_from(stream.cursor()).map_err(io::Error::other)?;
            stream.stop();
            cursor.to_bytes()
        };

        // 2. Change the tree while no stream runs.
        let script = offline_script(dir, args.ops);
        let mut touched: BTreeSet<PathBuf> = BTreeSet::new();
        let applied = Instant::now();
        for op in &script {
            op.apply()?;
            touched.extend(op.paths().into_iter().map(Path::to_path_buf));
        }
        let apply_time = applied.elapsed();
        // Give fseventsd a moment to take in the last changes.
        thread::sleep(Duration::from_millis(500));

        // 3. Resume from the saved cursor and collect the history.
        let restored = FsEventsCursor::from_bytes(&saved).map_err(io::Error::other)?;
        let timeout = Duration::from_secs(10 + (args.ops / 500) as u64);
        let started = Instant::now();
        let (stream, outcome) = VolumeStream::start(
            filtered_config(dir, false),
            Some(&ResumeCursor::from(restored)),
        )?;
        let mut history = Vec::new();
        let mut done = None;
        if outcome == StartOutcome::Resumed {
            loop {
                let left = (started + timeout).saturating_duration_since(Instant::now());
                match stream.recv_timeout(left) {
                    Ok(ev) if is_marker(&ev) => {
                        done = Some(ev.received.saturating_duration_since(started));
                        break;
                    }
                    Ok(ev) => history.push(ev),
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => {
                        return Err(io::Error::other("stream channel disconnected"));
                    }
                }
            }
        }
        let resumed_cursor = FsEventsCursor::try_from(stream.cursor()).map_err(io::Error::other)?;
        let dropped_before_cursor = stream.dropped_before_cursor();
        drop(stream);

        // 4. Compare. Every scripted path must be reported, directly or
        //    covered by a subtree rescan of an ancestor; nothing outside
        //    the scripted paths, their ancestors and the root may be.
        let reported: BTreeSet<&Path> = history.iter().map(|e| e.path.as_path()).collect();
        let rescans: Vec<&Path> = history
            .iter()
            .filter(|e| e.flags.contains(EventFlags::MUST_SCAN_SUB_DIRS) || e.rescan_subtree)
            .map(|e| e.path.as_path())
            .collect();
        // During replay the stream sets `rescan_subtree` on directory
        // renames (see `RawEvent::rescan_subtree`); count the paths covered
        // only by those.
        let kernel_rescans: Vec<&Path> = history
            .iter()
            .filter(|e| e.flags.contains(EventFlags::MUST_SCAN_SUB_DIRS))
            .map(|e| e.path.as_path())
            .collect();
        let mut direct = 0usize;
        let mut covered_by_rescan = 0usize;
        let mut covered_only_by_added = 0usize;
        let mut missing: Vec<&Path> = Vec::new();
        for p in &touched {
            if reported.contains(p.as_path()) {
                direct += 1;
                continue;
            }
            if rescans.iter().any(|r| p.starts_with(r)) {
                covered_by_rescan += 1;
                if !kernel_rescans.iter().any(|r| p.starts_with(r)) {
                    covered_only_by_added += 1;
                }
            } else {
                missing.push(p);
            }
        }
        let mut allowed: BTreeSet<&Path> = BTreeSet::new();
        allowed.insert(dir);
        for p in &touched {
            allowed.extend(p.ancestors().take_while(|a| a.starts_with(dir)));
        }
        let extra: Vec<&Path> = reported
            .iter()
            .copied()
            .filter(|p| !allowed.contains(p))
            .collect();
        let before_cursor = history.iter().filter(|e| e.id <= restored.event_id).count();

        println!(
            "fsevents_bench replay: {} offline ops on {} paths, applied in {} ms",
            script.len(),
            touched.len(),
            ms(apply_time)
        );
        print_machine();
        println!(
            "cursor: saved event id {}, start outcome {outcome:?}, cursor after replay {}",
            restored.event_id, resumed_cursor.event_id
        );
        match done {
            Some(d) => println!("replay time to HistoryDone: {} ms", ms(d)),
            None if outcome == StartOutcome::Resumed => {
                println!("replay time to HistoryDone: no marker within {timeout:?}");
            }
            None => println!("replay time to HistoryDone: not resumed"),
        }
        println!(
            "events: {} replayed, {} distinct paths, {} subtree rescans, \
             {covered_by_rescan} scripted paths covered only by a rescan",
            history.len(),
            reported.len(),
            rescans.len()
        );
        println!(
            "scripted paths: {direct} reported directly, \
             {covered_only_by_added} covered only by an added directory-rename rescan"
        );
        println!(
            "missing: {} {:?}",
            missing.len(),
            &missing[..missing.len().min(5)]
        );
        println!("extra: {} {:?}", extra.len(), &extra[..extra.len().min(5)]);
        println!("dropped before the saved cursor: {dropped_before_cursor} event(s)");
        if before_cursor > 0 {
            println!("note: {before_cursor} delivered event(s) at or before the saved cursor");
        }
        let ok = outcome == StartOutcome::Resumed
            && done.is_some()
            && missing.is_empty()
            && extra.is_empty()
            && before_cursor == 0;
        println!("GATE replay exact: {}", pass_fail(ok));
        Ok(())
    }
}
