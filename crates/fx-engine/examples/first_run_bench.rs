//! `first_run_bench`: the first full scan of a root into the engine
//! (Phase 1 gate: about 2M entries in <= 15 s warm, results shown
//! progressively).
//!
//! ```text
//! cargo run --release -p fx-engine --example first_run_bench -- --root / --cache warm
//! ```
//!
//! It starts an [`Engine`] over an empty index with the macOS scanner
//! (`MacScanner`, default `ScanOptions`), `MacPathStat` and the FSEvents
//! source for `--root`, so the writer runs the first-run full scan. It
//! reports the entries indexed, the seconds until the scan is published
//! and the engine is live, the time to the first non-empty snapshot, and
//! how many non-empty snapshots were published up to then (the progressive
//! count: 1 means the scan appeared at once). The final snapshot's memory
//! report follows as `memory_*` metrics. `--stream-kb N` sets the scanner's
//! buffer (`ScanOptions::stream_budget`) to compare sizes.
//!
//! Output: `metric=value` lines (see the crate README) and the gate lines
//! `GATE first run scan <= 15 s`, `GATE first run results shown
//! progressively` and `GATE memory <= 100 B per entry`. Exit codes: 0
//! after a completed scan, 1 on an error or timeout, 2 for bad arguments.
//! On hosts other than macOS it prints a message and exits with 0.
//!
//! Read-only: it only reads directory listings under `--root` and writes
//! nothing (no store). The scanner does not materialise dataless (cloud)
//! files.

use std::process::ExitCode;

#[cfg(not(target_os = "macos"))]
fn main() -> ExitCode {
    println!("first_run_bench requires macOS (the macOS scanner); nothing to do on this OS");
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
    use std::io;
    use std::path::PathBuf;
    use std::process::ExitCode;
    use std::time::{Duration, Instant};

    use crate::bench_common::{Flags, UsageError, gate, header, memory, metric, ms};
    use fx_engine::{Engine, EngineConfig, EngineHealth};
    use fx_fs_macos::enumerator::MacScanner;
    use fx_fs_macos::fsevents::source::FsEventsSource;
    use fx_fs_macos::fsevents::stream::StreamConfig;
    use fx_fs_macos::path_stat::MacPathStat;
    use fx_fs_macos::scan::ScanOptions;
    use fx_index::Index;

    const USAGE: &str = "\
usage:
  first_run_bench [--root DIR] [--timeout SECS] [--stream-kb N]
                  [--cache warm|cold|unknown]

  --root DIR      the folder to scan (default /)
  --timeout SECS  give up after SECS, 1..=86400 (default 600)
  --stream-kb N   the scanner's buffer in KiB (default: the scanner's own)
  --cache STATE   cache state label for the report (default unknown)

Gates: first run scan <= 15 s (for about 2M entries); results shown
progressively; memory <= 100 B per entry.";

    const GATE_SCAN: Duration = Duration::from_secs(15);

    pub fn main() -> ExitCode {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.iter().any(|a| a == "--help" || a == "-h") {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        let flags = ["--root", "--timeout", "--stream-kb", "--cache"];
        let parsed = Flags::parse(&args, &flags, &[]).and_then(|f| {
            let timeout = Duration::from_secs(f.number("--timeout", 600, 1, 86_400)?);
            let root = PathBuf::from(f.get("--root").unwrap_or("/"));
            let mut scan = ScanOptions::default();
            if f.get("--stream-kb").is_some() {
                scan.stream_budget = (f.number("--stream-kb", 0, 1, 4 << 20)? as usize) << 10;
            }
            Ok((root, timeout, scan, f.cache()?))
        });
        let (root, timeout, scan, cache) = match parsed {
            Ok(p) => p,
            Err(UsageError(msg)) => {
                eprintln!("first_run_bench: {msg}\n\n{USAGE}");
                return ExitCode::from(2);
            }
        };
        header("first_run", cache);
        match run(root, timeout, scan) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("first_run_bench: error: {e}");
                ExitCode::from(1)
            }
        }
    }

    fn run(root: PathBuf, timeout: Duration, scan: ScanOptions) -> io::Result<()> {
        let root = std::fs::canonicalize(&root)?;
        if !root.is_dir() {
            return Err(io::Error::other(format!(
                "{} is not a folder",
                root.display()
            )));
        }
        metric("root", root.display());
        metric("stream_budget_bytes", scan.stream_budget);
        let mut stream = StreamConfig::new(root.clone());
        stream.path_filter = Some(root.clone());
        let t = Instant::now();
        let engine = Engine::start(
            Index::new(),
            EngineConfig::new(&root),
            FsEventsSource::new(stream),
            MacPathStat::new(),
            MacScanner::new(scan),
        )?;
        let announced = engine.subscribe();
        let mut nonempty = 0usize;
        let mut first_result: Option<Duration> = None;
        let mut last_generation = 0;
        let done = loop {
            let live = engine.health() == EngineHealth::Live;
            // Count every generation announced so far that holds entries.
            while let Ok(g) = announced.try_recv() {
                let g = g.get();
                if g > last_generation {
                    last_generation = g;
                    let snap = engine.snapshot();
                    if snap.index().live_count() > 0 {
                        nonempty += 1;
                        first_result.get_or_insert_with(|| t.elapsed());
                    }
                }
            }
            if live && last_generation > 0 {
                break t.elapsed();
            }
            if let EngineHealth::Failed(why) = engine.health() {
                return Err(io::Error::other(format!("engine failed: {why}")));
            }
            if t.elapsed() > timeout {
                return Err(io::Error::other(format!(
                    "the scan did not finish within {} s",
                    timeout.as_secs()
                )));
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        let snap = engine.snapshot();
        engine.shutdown();
        let entries = snap.index().live_count();
        let secs = done.as_secs_f64();
        metric("entries", entries);
        metric("first_run_entries", entries);
        metric("scan_seconds", format!("{secs:.3}"));
        metric(
            "entries_per_second",
            format!("{:.0}", entries as f64 / secs.max(1e-9)),
        );
        if let Some(d) = first_result {
            metric("first_result_ms", ms(d));
        }
        metric("progressive_snapshots", nonempty);
        gate(
            "first run scan <= 15 s",
            done <= GATE_SCAN,
            &format!("{secs:.3} s for {entries} entries"),
        );
        gate(
            "first run results shown progressively",
            nonempty >= 2,
            &format!("{nonempty} non-empty snapshots up to the full scan"),
        );
        memory(&snap.memory_report());
        Ok(())
    }
}
