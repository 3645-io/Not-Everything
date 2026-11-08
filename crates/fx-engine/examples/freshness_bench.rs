//! `freshness_bench`: measures how fast file-system changes reach search
//! results through the engine (the freshness gate).
//!
//! ```text
//! cargo run --release -p fx-engine --example freshness_bench -- --ops 1000 --cache warm
//! ```
//!
//! It starts an [`Engine`] with the real FSEvents change source, macOS
//! `PathStat` and the macOS scanner over a fresh temp directory, then runs
//! `N` scripted operations in rotation (create, rename, delete; each file
//! has its own names, so no path is reused). For every operation it keeps a
//! search open for that file's name and waits for the first published
//! generation whose search results show the change. The sample is the time
//! from just before the syscall to the moment that generation was announced
//! to subscribers.
//!
//! It prints p50, p95, p99 and max (nearest rank, from `fx-bench`), per
//! operation kind as well, the machine, the cache label and the batch
//! window, the scale as `freshness_ops=N` (with `freshness_samples` and
//! `freshness_missed`), then the gate lines:
//!
//! * `GATE freshness p99 <= 1 s: PASS/FAIL (...)` (the gate);
//! * `GATE stretch freshness p95 <= 250 ms: PASS/FAIL (...)`.
//!
//! Exit codes: 0 after a completed run (gate results do not change it), 1
//! when the engine or a file operation fails, 2 for bad arguments. On hosts
//! other than macOS it prints a message and exits with 0.
//!
//! Read-only with respect to the user's files: the benchmark only creates,
//! renames and removes entries inside its own directory under
//! `std::env::temp_dir()`, and removes that directory at the end.

use std::process::ExitCode;

#[cfg(not(target_os = "macos"))]
fn main() -> ExitCode {
    println!("freshness_bench requires macOS (FSEvents); nothing to do on this OS");
    ExitCode::SUCCESS
}

#[cfg(target_os = "macos")]
fn main() -> ExitCode {
    bench::main()
}

#[cfg(target_os = "macos")]
mod bench {
    use std::collections::HashMap;
    use std::fs;
    use std::io::{self, Write};
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;
    use std::sync::mpsc::{Receiver, RecvTimeoutError};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use fx_bench::{CacheState, MachineInfo, Stats};
    use fx_engine::{Engine, EngineConfig, Generation};
    use fx_fs_macos::enumerator::MacScanner;
    use fx_fs_macos::fsevents::source::FsEventsSource;
    use fx_fs_macos::fsevents::stream::StreamConfig;
    use fx_fs_macos::path_stat::MacPathStat;
    use fx_fs_macos::scan::ScanOptions;
    use fx_index::Index;
    use fx_model::EntryId;
    use fx_sort::{Direction, SortKey};

    const USAGE: &str = "\
usage:
  freshness_bench [--ops N] [--window MS] [--cache warm|cold|unknown]

  --ops N       scripted operations (create, rename, delete in rotation),
                1..=1000000 (default 1000)
  --window MS   engine batch window in ms, 50..=200 (default 100)
  --cache STATE cache state label for the report (default unknown)

Gates: freshness p99 <= 1 s; stretch p95 <= 250 ms.";

    /// Gate: p99 from operation to the generation that shows it.
    const GATE_P99: Duration = Duration::from_secs(1);
    /// Stretch goal: p95.
    const STRETCH_P95: Duration = Duration::from_millis(250);
    const MAX_OPS: usize = 1_000_000;
    const DEFAULT_OPS: usize = 1_000;
    const DEFAULT_WINDOW_MS: u64 = 100;
    /// How long to wait for one operation before counting it as missed.
    const OP_TIMEOUT: Duration = Duration::from_secs(5);
    /// How long the initial scan and the warm-up change may take.
    const SETUP_TIMEOUT: Duration = Duration::from_secs(30);

    struct UsageError(String);

    struct Args {
        ops: usize,
        window: Duration,
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
                eprintln!("freshness_bench: {msg}\n\n{USAGE}");
                return ExitCode::from(2);
            }
        };
        match run(&args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("freshness_bench: error: {e}");
                ExitCode::from(1)
            }
        }
    }

    fn parse(args: &[String]) -> Result<Args, UsageError> {
        let mut ops: Option<usize> = None;
        let mut window: Option<Duration> = None;
        let mut cache: Option<CacheState> = None;
        let mut it = args.iter();
        while let Some(arg) = it.next() {
            let flag = arg.as_str();
            let mut value = || {
                it.next()
                    .map(String::as_str)
                    .ok_or_else(|| UsageError(format!("{flag} needs a value")))
            };
            let duplicate = match flag {
                "--ops" => ops
                    .replace(number(flag, value()?, 1, MAX_OPS as u64)? as usize)
                    .is_some(),
                "--window" => window
                    .replace(Duration::from_millis(number(flag, value()?, 50, 200)?))
                    .is_some(),
                "--cache" => cache.replace(cache_state(value()?)?).is_some(),
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
            ops: ops.unwrap_or(DEFAULT_OPS),
            window: window.unwrap_or(Duration::from_millis(DEFAULT_WINDOW_MS)),
            cache: cache.unwrap_or(CacheState::Unknown),
        })
    }

    /// Parses a decimal integer in `lo..=hi`; `_` separators are allowed.
    fn number(flag: &str, text: &str, lo: u64, hi: u64) -> Result<u64, UsageError> {
        let digits: String = text.chars().filter(|&c| c != '_').collect();
        let n = (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
            .then(|| digits.parse::<u64>().ok())
            .flatten();
        match n {
            Some(n) if (lo..=hi).contains(&n) => Ok(n),
            _ => Err(UsageError(format!(
                "{flag} expects an integer in {lo}..={hi}, got '{text}'"
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
        fn new() -> io::Result<TempRoot> {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let dir = std::env::temp_dir()
                .join(format!("fx-freshness-bench-{}-{nanos}", std::process::id()));
            fs::create_dir(&dir)?;
            // Canonical, so `/var/...` reads `/private/var/...` like the
            // paths FSEvents reports.
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

    /// One scripted operation on file `k`.
    enum Op {
        Create(PathBuf),
        Rename(PathBuf, PathBuf),
        Delete(PathBuf),
    }

    impl Op {
        /// The `i`-th operation: create, rename, delete in rotation on file
        /// `k = i / 3`, named `op{k}_c.txt` and then `op{k}_r.txt`.
        fn nth(root: &Path, i: usize) -> (Op, String) {
            let k = i / 3;
            let created = root.join(format!("op{k}_c.txt"));
            let renamed = root.join(format!("op{k}_r.txt"));
            let op = match i % 3 {
                0 => Op::Create(created),
                1 => Op::Rename(created, renamed),
                _ => Op::Delete(renamed),
            };
            // `op{k}_` matches both names of file k and no other file.
            (op, format!("op{k}_"))
        }

        fn apply(&self) -> io::Result<()> {
            match self {
                Op::Create(p) => fs::File::create(p).map(drop),
                Op::Rename(from, to) => fs::rename(from, to),
                Op::Delete(p) => fs::remove_file(p),
            }
        }

        /// The names the open search shows once the change is in.
        fn expected(&self) -> Vec<String> {
            let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned());
            match self {
                Op::Create(p) | Op::Rename(_, p) => name(p).into_iter().collect(),
                Op::Delete(_) => Vec::new(),
            }
        }

        fn kind(&self) -> usize {
            match self {
                Op::Create(_) => 0,
                Op::Rename(..) => 1,
                Op::Delete(_) => 2,
            }
        }
    }

    /// Sorted names of the hits for `query`, with the generation searched;
    /// `None` when a newer snapshot was published while searching.
    fn hit_names(engine: &Engine, query: &str) -> io::Result<Option<(u64, Vec<String>)>> {
        let snap = engine.snapshot();
        let page = engine
            .search(
                query,
                fx_engine::QueryOptions::default(),
                SortKey::Name,
                Direction::Ascending,
                0,
                64,
            )
            .map_err(|_| io::Error::other("search cancelled"))?;
        if page.generation.get() != snap.generation() {
            return Ok(None);
        }
        let mut names: Vec<String> = page
            .ids
            .iter()
            .map(|&id| snap.index().name(EntryId(id)).to_string())
            .collect();
        names.sort();
        Ok(Some((page.generation.get(), names)))
    }

    /// Generation announcements with the instant each was received.
    struct Announcements {
        rx: Receiver<Generation>,
        seen: HashMap<u64, Instant>,
    }

    impl Announcements {
        /// Records every announcement already queued.
        fn drain(&mut self) {
            while let Ok(g) = self.rx.try_recv() {
                self.seen.insert(g.get(), Instant::now());
            }
        }

        /// Waits for the next announcement until `deadline`; false on
        /// timeout.
        fn wait(&mut self, deadline: Instant) -> io::Result<bool> {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(g) => {
                    self.seen.insert(g.get(), Instant::now());
                    self.drain();
                    Ok(true)
                }
                Err(RecvTimeoutError::Timeout) => Ok(false),
                Err(RecvTimeoutError::Disconnected) => {
                    Err(io::Error::other("engine stopped announcing generations"))
                }
            }
        }

        /// When generation `g` was announced, waiting for it if needed.
        fn announced(&mut self, g: u64, deadline: Instant) -> io::Result<Option<Instant>> {
            loop {
                if let Some(&t) = self.seen.get(&g) {
                    return Ok(Some(t));
                }
                if !self.wait(deadline)? {
                    return Ok(None);
                }
            }
        }
    }

    /// Waits until the search for `query` shows exactly `want` and returns
    /// the announcement instant of the first generation that does.
    fn wait_until_shown(
        engine: &Engine,
        ann: &mut Announcements,
        query: &str,
        want: &[String],
        timeout: Duration,
    ) -> io::Result<Option<Instant>> {
        let deadline = Instant::now() + timeout;
        loop {
            ann.drain();
            if let Some((g, names)) = hit_names(engine, query)? {
                if names == want {
                    // Every generation published after `subscribe` is
                    // announced, and the change was made after it.
                    return ann.announced(g, deadline);
                }
            }
            if !ann.wait(deadline)? {
                return Ok(None);
            }
        }
    }

    fn ms(d: Duration) -> String {
        format!("{:.3}", d.as_nanos() as f64 / 1_000_000.0)
    }

    fn pass_fail(ok: bool) -> &'static str {
        if ok { "PASS" } else { "FAIL" }
    }

    fn run(args: &Args) -> io::Result<()> {
        let root = TempRoot::new()?;
        let mut stream = StreamConfig::new(root.path().to_path_buf());
        stream.path_filter = Some(root.path().to_path_buf());
        stream.no_defer = true;
        let config = EngineConfig::new(root.path()).with_batch_window(args.window);
        let window = config.batch_window();
        let engine = Engine::start(
            Index::new(),
            config,
            FsEventsSource::new(stream),
            MacPathStat::new(),
            MacScanner::new(ScanOptions::default()),
        )?;
        let mut ann = Announcements {
            rx: engine.subscribe(),
            seen: HashMap::new(),
        };

        // Warm up: the initial scan is in and FSEvents delivers changes.
        let probe = root.path().join("warmup_probe.txt");
        fs::File::create(&probe)?;
        let want = vec!["warmup_probe.txt".to_string()];
        let deadline = Instant::now() + SETUP_TIMEOUT;
        // Polled: its generation may have been published before `subscribe`.
        loop {
            if hit_names(&engine, "warmup_probe")?.is_some_and(|(_, names)| names == want) {
                break;
            }
            if Instant::now() >= deadline {
                return Err(io::Error::other("the warm-up change never reached search"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        let mut samples = Vec::with_capacity(args.ops);
        let mut per_kind: [Vec<Duration>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        let mut missed = 0usize;
        for i in 0..args.ops {
            let (op, query) = Op::nth(root.path(), i);
            let want = op.expected();
            ann.drain();
            let t = Instant::now();
            op.apply()?;
            match wait_until_shown(&engine, &mut ann, &query, &want, OP_TIMEOUT)? {
                Some(at) => {
                    let d = at.saturating_duration_since(t);
                    samples.push(d);
                    per_kind[op.kind()].push(d);
                }
                None => missed += 1,
            }
            if (i + 1) % 1000 == 0 {
                eprintln!("freshness_bench: {} / {} ops", i + 1, args.ops);
                let _ = io::stderr().flush();
            }
        }
        engine.shutdown();

        let m = MachineInfo::detect();
        println!(
            "freshness_bench: {} ops (create, rename, delete in rotation), cache {}, \
             batch window {} ms, no_defer on",
            args.ops,
            args.cache,
            ms(window)
        );
        println!(
            "machine: {} {}, {} threads, {}",
            m.os, m.arch, m.parallelism, m.cpu_brand
        );
        // The scale this run measured, as a gate metric (crate README).
        println!("freshness_ops={}", args.ops);
        println!("freshness_samples={}", samples.len());
        println!("freshness_missed={missed}");
        let Some(stats) = Stats::from_samples(&samples) else {
            println!("no operation reached search ({missed} missed)");
            println!("GATE freshness p99 <= 1 s: FAIL (no samples)");
            println!("GATE stretch freshness p95 <= 250 ms: FAIL (no samples)");
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
            println!("note: {missed} operation(s) did not reach search within {OP_TIMEOUT:?}");
        }
        println!(
            "GATE freshness p99 <= 1 s: {} (p99 {} ms, missed {missed})",
            pass_fail(missed == 0 && stats.p99 <= GATE_P99),
            ms(stats.p99)
        );
        println!(
            "GATE stretch freshness p95 <= 250 ms: {} (p95 {} ms)",
            pass_fail(missed == 0 && stats.p95 <= STRETCH_P95),
            ms(stats.p95)
        );
        Ok(())
    }
}
