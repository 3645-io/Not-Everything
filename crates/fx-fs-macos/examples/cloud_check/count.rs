//! The counting half of the `cloud_check` example, kept apart from its
//! macOS calls so that `tests/cloud_check.rs` can run it on a plain folder
//! with a stand-in for the `SF_DATALESS` test: a census of the dataless
//! items below a root, the downloads between two moments, and the
//! `key=value` report.

use std::collections::BTreeSet;
use std::fs::{self, Metadata};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// The dataless items below a root, found with `lstat` alone.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Census {
    /// Every dataless item, file or folder, strictly below the root.
    pub dataless: BTreeSet<PathBuf>,
    /// Folders below the root that could not be listed (skipped).
    pub unreadable: u64,
}

/// Walks everything strictly below `root`, on the root's own device, and
/// collects the items `dataless` picks out. `dataless` gets each item's
/// path and its `lstat` metadata (a symlink is never followed). A dataless
/// folder is recorded and never listed, since listing it would ask its file
/// provider for a download, and nothing is opened but the folders listed.
///
/// # Errors
///
/// When `root` is not a folder (a symlink to one included) or can't be
/// listed. A folder below it that can't be listed only counts in
/// [`Census::unreadable`].
pub fn census(root: &Path, dataless: &dyn Fn(&Path, &Metadata) -> bool) -> io::Result<Census> {
    let meta = fs::symlink_metadata(root)?;
    if !meta.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the root must be a folder, not a file or a symlink",
        ));
    }
    let device = meta.dev();
    let mut out = Census::default();
    let mut stack = Vec::new();
    // The root's own listing must work: an error there is the caller's.
    list(root, device, dataless, &mut out, &mut stack)?;
    while let Some(dir) = stack.pop() {
        if list(&dir, device, dataless, &mut out, &mut stack).is_err() {
            out.unreadable += 1;
        }
    }
    Ok(out)
}

/// Lists one folder: records its dataless items and pushes the folders to
/// enter (not dataless, not a symlink, on `device`).
fn list(
    dir: &Path,
    device: u64,
    dataless: &dyn Fn(&Path, &Metadata) -> bool,
    out: &mut Census,
    stack: &mut Vec<PathBuf>,
) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        // Gone since the listing: nothing to count.
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if dataless(&path, &meta) {
            out.dataless.insert(path);
        } else if meta.file_type().is_dir() && meta.dev() == device {
            stack.push(path);
        }
    }
    Ok(())
}

/// The items dataless in `before` that are still there and no longer
/// dataless: their contents came down from the cloud. `dataless_now`
/// `lstat`s a path again: `Some(dataless)`, or `None` when it is gone (an
/// item deleted or moved meanwhile is not a download).
pub fn downloads(before: &Census, dataless_now: &dyn Fn(&Path) -> Option<bool>) -> u64 {
    let count = before
        .dataless
        .iter()
        .filter(|path| dataless_now(path) == Some(false))
        .count();
    u64::try_from(count).unwrap_or(u64::MAX)
}

/// What `cloud_check` prints: counts only, never a path.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Dataless items below the root before the scan.
    pub dataless_before: u64,
    /// Dataless items below the root after the scan and the stats.
    pub dataless_after: u64,
    /// Dataless folders the scanner listed and didn't enter
    /// (`ScanStats::skipped_dataless`).
    pub skipped_dataless: u64,
    /// Folders the scanner couldn't list and stats that failed because the
    /// item needed a download (`EDEADLK`, with materialization off): the
    /// downloads the policy refused.
    pub edeadlk: u64,
    /// Items dataless before and not after ([`downloads`]).
    pub downloads: u64,
}

impl Report {
    /// The `key=value` lines, in a fixed order.
    pub fn lines(&self) -> String {
        format!(
            "cloud_dataless_before={}\ncloud_dataless_after={}\nskipped_dataless={}\n\
             edeadlk={}\ncloud_downloads={}\n",
            self.dataless_before,
            self.dataless_after,
            self.skipped_dataless,
            self.edeadlk,
            self.downloads
        )
    }

    /// 0 when nothing was downloaded, 1 when anything was.
    pub fn exit_code(&self) -> u8 {
        u8::from(self.downloads > 0)
    }
}
