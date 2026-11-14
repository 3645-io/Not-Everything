//! `cloud_check`: checks that indexing a folder of cloud files downloads
//! none of them, for the Phase 2 line "iCloud and File Provider folders:
//! indexing triggers 0 downloads".
//!
//! ```text
//! cargo run --release -p fx-fs-macos --example cloud_check -- --root DIR
//! ```
//!
//! A file provider (iCloud Drive, or an app such as Dropbox or OneDrive in
//! `~/Library/CloudStorage`) can keep an item in the cloud only: it is then
//! *dataless*, with its metadata on disk and `SF_DATALESS` set, and reading
//! it, or listing it if it is a folder, would make the system download it
//! ([`fx_fs_macos::cloud`]). The check:
//!
//! 1. switches dataless materialization off for the process, as the app's
//!    scanner and path stat do;
//! 2. counts the dataless items below the root with `lstat` alone, listing
//!    only folders that are not dataless, so it never asks for a download
//!    itself;
//! 3. runs the app's scanner over the root (`scan::scan`, the walker under
//!    `MacScanner`, with every entry counted and none kept), then the app's
//!    path stat (`MacPathStat`, what a change on disk goes through) on each
//!    dataless item;
//! 4. counts again, and counts as a download each item that was dataless
//!    before and is still there but no longer dataless.
//!
//! It prints `key=value` lines only, never a path:
//!
//! - `cloud_dataless_before`, `cloud_dataless_after`: the dataless items
//!   below the root before the scan and after the stats;
//! - `skipped_dataless`: dataless folders the scanner listed and didn't
//!   enter;
//! - `edeadlk`: folders the scanner couldn't list and stats that failed
//!   with `EDEADLK`, which with materialization off means the item needed a
//!   download: downloads the policy refused;
//! - `cloud_downloads`: the items that came down; the gate is 0.
//!
//! Exit codes: 0 when nothing was downloaded, 1 when an item was, 2 for bad
//! arguments, a root that can't be listed, or a host other than macOS.
//!
//! Point it at a folder that holds cloud-only items: iCloud Drive
//! (`~/Library/Mobile Documents`) or a provider's folder in
//! `~/Library/CloudStorage`. It only lists folders and reads metadata: it
//! never opens, writes, moves or deletes a file. Something else on the Mac
//! (Finder, Spotlight, an app syncing) can download an item while it runs,
//! so a 1 is worth one run more before it counts.

use std::process::ExitCode;

// The counting half, which tests/cloud_check.rs runs on a plain folder.
#[cfg(target_os = "macos")]
#[path = "cloud_check/count.rs"]
mod count;

#[cfg(not(target_os = "macos"))]
fn main() -> ExitCode {
    eprintln!("cloud_check requires macOS");
    ExitCode::from(2)
}

#[cfg(target_os = "macos")]
fn main() -> ExitCode {
    check::main()
}

#[cfg(target_os = "macos")]
mod check {
    use std::fs::{self, Metadata};
    use std::io;
    use std::os::macos::fs::MetadataExt;
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;

    use fx_fs::PathStat;
    use fx_fs_macos::cloud::{
        OpenSkip, classify_open_error, disable_dataless_materialization, is_dataless,
    };
    use fx_fs_macos::path_stat::MacPathStat;
    use fx_fs_macos::scan::{ScanOptions, scan};

    use crate::count::{Report, census, downloads};

    const USAGE: &str = "\
usage:
  cloud_check --root DIR

  --root DIR   the folder to check: one that holds cloud-only items, such as
               iCloud Drive or a provider's folder in ~/Library/CloudStorage

Prints key=value counts, never a path. Exits 0 when indexing downloaded
nothing, 1 when it downloaded an item, 2 for bad arguments or a root that
can't be listed.";

    pub fn main() -> ExitCode {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let root = match parse(&args) {
            Ok(Some(root)) => root,
            Ok(None) => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            Err(msg) => {
                eprintln!("cloud_check: {msg}\n\n{USAGE}");
                return ExitCode::from(2);
            }
        };
        match run(&root) {
            Ok(report) => {
                print!("{}", report.lines());
                ExitCode::from(report.exit_code())
            }
            Err(e) => {
                // The error only: the root's path is the caller's own.
                eprintln!("cloud_check: the root can't be checked: {e}");
                ExitCode::from(2)
            }
        }
    }

    /// `Ok(None)` for `--help`.
    fn parse(args: &[String]) -> Result<Option<PathBuf>, String> {
        let mut root = None;
        let mut it = args.iter();
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "-h" | "--help" => return Ok(None),
                "--root" => {
                    let value = it.next().ok_or("--root needs a folder")?;
                    if root.replace(PathBuf::from(value)).is_some() {
                        return Err("--root is given twice".into());
                    }
                }
                other => return Err(format!("unknown argument '{other}'")),
            }
        }
        root.map(Some).ok_or_else(|| "--root is required".into())
    }

    fn lstat_dataless(_: &Path, meta: &Metadata) -> bool {
        is_dataless(meta.st_flags())
    }

    fn run(root: &Path) -> io::Result<Report> {
        // Before anything below the root is touched, as the app does. The
        // census never lists a dataless folder or opens a file either way.
        if let Err(e) = disable_dataless_materialization() {
            eprintln!("cloud_check: warning: materialization stays on: {e}");
        }
        let before = census(root, &lstat_dataless)?;

        let opts = ScanOptions {
            collect: false,
            ..ScanOptions::default()
        };
        let stats = scan(root, &opts)?.stats;
        let path_stat = MacPathStat::new();
        let mut stat_edeadlk = 0;
        for path in &before.dataless {
            let refused = match path_stat.stat(path) {
                Ok(_) => false,
                Err(e) => {
                    e.raw_os_error().and_then(classify_open_error) == Some(OpenSkip::Dataless)
                }
            };
            stat_edeadlk += u64::from(refused);
        }

        let after = census(root, &lstat_dataless)?;
        let unreadable = before.unreadable.max(after.unreadable);
        if unreadable > 0 {
            eprintln!(
                "cloud_check: {unreadable} folders below the root could not be listed and were left out"
            );
        }
        let came_down = downloads(&before, &|path| {
            fs::symlink_metadata(path)
                .ok()
                .map(|meta| is_dataless(meta.st_flags()))
        });
        Ok(Report {
            dataless_before: len(&before.dataless),
            dataless_after: len(&after.dataless),
            skipped_dataless: stats.skipped_dataless,
            edeadlk: stats.skipped_deadlk + stat_edeadlk,
            downloads: came_down,
        })
    }

    fn len<T>(set: &std::collections::BTreeSet<T>) -> u64 {
        u64::try_from(set.len()).unwrap_or(u64::MAX)
    }
}
