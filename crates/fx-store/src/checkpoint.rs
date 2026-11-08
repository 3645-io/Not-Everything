//! The startup policy: open the checkpoint, or rebuild it.
//!
//! Following the startup flow in `docs/PLAN.md` ("Proposed architecture",
//! "Saving and loading"): map the saved index if it is valid; if it is
//! missing, from an older format version or corrupt, rebuild the index
//! through the caller (a full scan), write a fresh checkpoint atomically and
//! map that. A file written without a clean shutdown still loads; the
//! engine sees `clean_shutdown() == false` and replays changes from the
//! saved resume cursors.
//!
//! Clean-room: written from docs/PLAN.md and the in-repo code only.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime};

use fx_index::IndexParts;
use fx_model::{ResumeCursor, VolumeId};

use crate::error::{LoadError, StoreError};
use crate::mapped::MappedIndex;
use crate::writer::{SaveInput, create_private_dir, temp_name_prefix, write_checkpoint};

/// How much of an existing checkpoint [`open_or_rebuild`] checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifyMode {
    /// Only what [`MappedIndex::open`] checks: header, volume table and
    /// column table. O(1) in the data size; column contents are not read.
    HeaderOnly,
    /// Also [`MappedIndex::verify_checksums`], which reads every column.
    Full,
}

/// Why [`open_or_rebuild`] rejected the existing file and rebuilt it.
///
/// This is the loader's [`LoadError`]. Every variant except
/// [`LoadError::Io`] leads to a rebuild; an I/O error is returned instead.
pub type RejectReason = LoadError;

/// The owned result of a rebuild, written as the new checkpoint.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RebuiltIndex {
    /// The index columns, from `fx_index::Snapshot::to_parts`.
    pub parts: IndexParts,
    /// Per-volume resume cursors (unique volume IDs).
    pub volumes: Vec<(VolumeId, ResumeCursor)>,
    /// Saved sort orders: `u8` key code and a `u32` permutation of the live
    /// entry IDs (unique keys; see [`SaveInput::orders`]).
    pub orders: Vec<(u8, Vec<u32>)>,
    /// The clean-shutdown flag written into the new checkpoint.
    pub clean_shutdown: bool,
}

/// What [`open_or_rebuild`] produced.
#[derive(Debug)]
pub enum Opened {
    /// The existing checkpoint was valid and is mapped.
    Loaded(MappedIndex),
    /// The existing checkpoint was rejected; the rebuilt index was written
    /// in its place and is mapped.
    Rebuilt {
        /// The freshly written checkpoint.
        index: MappedIndex,
        /// Why the old file was rejected, for the log.
        reason: RejectReason,
    },
}

impl Opened {
    /// The mapped checkpoint.
    pub fn index(&self) -> &MappedIndex {
        match self {
            Opened::Loaded(index) | Opened::Rebuilt { index, .. } => index,
        }
    }

    /// The checkpoint's clean-shutdown flag. `false` means the engine must
    /// replay changes since the saved resume cursors.
    pub fn clean_shutdown(&self) -> bool {
        self.index().clean_shutdown()
    }
}

/// Opens the checkpoint at `path`, or rebuilds and rewrites it.
///
/// Steps:
///
/// 1. [`cleanup_stale_temps`] for `path`: removes this checkpoint's own
///    leftover temp files older than [`STALE_TEMP_MIN_AGE`] (best effort:
///    a failure there never blocks startup).
/// 2. [`MappedIndex::open`], plus [`MappedIndex::verify_checksums`] with
///    [`VerifyMode::Full`]. On success returns [`Opened::Loaded`], whatever
///    the clean-shutdown flag says.
/// 3. If the file is missing, of another format version, or corrupt in
///    any way the loader detects, calls `rebuild` exactly once, writes its
///    result with [`write_checkpoint`] (after [`create_private_dir`]
///    creates a missing parent directory with mode 0700; an existing one
///    keeps its mode), reopens it with the same
///    verification and returns [`Opened::Rebuilt`] with the rejection
///    reason.
///
/// Errors: an I/O error other than "not found" while opening is returned as
/// [`StoreError::Load`] without calling `rebuild`. A failed `rebuild` is
/// returned as [`StoreError::Rebuild`] and leaves the old file in place.
/// Write errors are the [`create_private_dir`] and [`write_checkpoint`]
/// errors; a rewritten file that fails to reopen is [`StoreError::Load`].
pub fn open_or_rebuild<E, F>(
    path: impl AsRef<Path>,
    verify: VerifyMode,
    rebuild: F,
) -> Result<Opened, StoreError>
where
    F: FnOnce() -> Result<RebuiltIndex, E>,
    E: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let path = path.as_ref();
    let _ = cleanup_stale_temps(path);

    let reason = match open_verified(path, verify) {
        Ok(index) => return Ok(Opened::Loaded(index)),
        Err(LoadError::Io { source }) => {
            return Err(StoreError::Load(LoadError::Io { source }));
        }
        Err(reason) => reason,
    };

    let rebuilt = rebuild().map_err(|e| StoreError::Rebuild(e.into()))?;
    let orders: Vec<(u8, &[u32])> = rebuilt
        .orders
        .iter()
        .map(|(key, order)| (*key, order.as_slice()))
        .collect();
    let input = SaveInput {
        parts: &rebuilt.parts,
        volumes: &rebuilt.volumes,
        orders: &orders,
        clean_shutdown: rebuilt.clean_shutdown,
    };
    // Creates a missing folder for the store; an existing one, or a bare
    // file name's working directory, is left as it is.
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        create_private_dir(dir)?;
    }
    write_checkpoint(path, &input)?;
    drop(orders);
    drop(rebuilt);

    let index = open_verified(path, verify).map_err(StoreError::Load)?;
    Ok(Opened::Rebuilt { index, reason })
}

/// Opens `path` and, with [`VerifyMode::Full`], verifies every column.
fn open_verified(path: &Path, verify: VerifyMode) -> Result<MappedIndex, LoadError> {
    let index = MappedIndex::open(path)?;
    if verify == VerifyMode::Full {
        index.verify_checksums()?;
    }
    Ok(index)
}

/// The directory holding `path` (`.` for a bare file name).
fn parent_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

/// How old (by modification time) a temp file must be before
/// [`cleanup_stale_temps`] removes it: 10 minutes.
///
/// fx-store cannot cheaply ask whether the process that created a temp
/// file is still running (it has no direct libc dependency for
/// `kill(pid, 0)`), so a temp file modified within this window is assumed
/// to belong to a checkpoint that some process is still writing, and is
/// kept. A checkpoint write takes seconds even at millions of entries, so
/// ten minutes leaves a wide margin; an older temp file is a leftover of an
/// interrupted write. A young leftover is removed by a later start.
pub const STALE_TEMP_MIN_AGE: Duration = Duration::from_secs(10 * 60);

/// Removes leftover temp files of interrupted checkpoints of `target` (the
/// checkpoint file's path) from its directory, and returns how many were
/// removed.
///
/// Only regular files named like [`write_checkpoint`]'s temp files for
/// this target are touched: [`temp_name_prefix`]`(target)` (that is
/// `.<file name>.tmp.`) followed by `<pid>.<counter>` in decimal. Temp
/// files of other checkpoints in the same directory, and everything else
/// (including directories and symlinks), are kept.
///
/// Temp files that may belong to a running writer are skipped: those
/// carrying this process's ID, and those modified less than
/// [`STALE_TEMP_MIN_AGE`] ago (or whose modification time cannot be read
/// or lies in the future).
///
/// A file that disappears while cleaning is not an error. Other I/O errors
/// (reading the directory, reading a file's metadata, removing a file) are
/// returned, as is a `target` without a file name (`InvalidInput`).
pub fn cleanup_stale_temps(target: impl AsRef<Path>) -> io::Result<usize> {
    cleanup_stale_temps_older_than(target, STALE_TEMP_MIN_AGE)
}

/// [`cleanup_stale_temps`] with a custom minimum age instead of
/// [`STALE_TEMP_MIN_AGE`]. `Duration::ZERO` removes matching temp files
/// of other processes whatever their age. Meant for tests.
#[doc(hidden)]
pub fn cleanup_stale_temps_older_than(
    target: impl AsRef<Path>,
    min_age: Duration,
) -> io::Result<usize> {
    let target = target.as_ref();
    let prefix = temp_name_prefix(target).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "index path has no file name")
    })?;
    let own_pid = std::process::id();
    let now = SystemTime::now();
    let mut removed = 0;
    for entry in fs::read_dir(parent_dir(target))? {
        let entry = match entry {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        let name = entry.file_name();
        let Some(pid) = temp_pid(&prefix, &name) else {
            continue;
        };
        if pid == own_pid {
            continue;
        }
        // Not following symlinks: a link named like a temp file is kept.
        let metadata = match fs::symlink_metadata(entry.path()) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        if !metadata.is_file() || !old_enough(&metadata, now, min_age) {
            continue;
        }
        match fs::remove_file(entry.path()) {
            Ok(()) => removed += 1,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(removed)
}

/// Whether a file with `metadata` was last modified at least `min_age`
/// before `now`. An unreadable or future modification time counts as
/// young, unless `min_age` is zero.
fn old_enough(metadata: &fs::Metadata, now: SystemTime, min_age: Duration) -> bool {
    if min_age.is_zero() {
        return true;
    }
    metadata
        .modified()
        .ok()
        .and_then(|mtime| now.duration_since(mtime).ok())
        .is_some_and(|age| age >= min_age)
}

/// The process ID in `name` when it is `prefix` followed by
/// `<pid>.<counter>` in decimal (the writer's temp name for one target),
/// `None` otherwise.
fn temp_pid(prefix: &OsStr, name: &OsStr) -> Option<u32> {
    let rest = name
        .as_encoded_bytes()
        .strip_prefix(prefix.as_encoded_bytes())?;
    let dot = rest.iter().position(|&c| c == b'.')?;
    let (pid, counter) = (&rest[..dot], &rest[dot + 1..]);
    if !is_decimal(pid) || !is_decimal(counter) {
        return None;
    }
    // The counter is a u64 and the PID a u32 in the writer.
    std::str::from_utf8(counter).ok()?.parse::<u64>().ok()?;
    std::str::from_utf8(pid).ok()?.parse::<u32>().ok()
}

/// Non-empty and ASCII digits only.
fn is_decimal(b: &[u8]) -> bool {
    !b.is_empty() && b.iter().all(u8::is_ascii_digit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_names_are_recognised_per_target() {
        let pid = |target: &str, name: &str| {
            let prefix = temp_name_prefix(Path::new(target)).expect("prefix");
            temp_pid(&prefix, OsStr::new(name))
        };
        assert_eq!(pid("index.fxs", ".index.fxs.tmp.123.0"), Some(123));
        assert_eq!(pid("/some/dir/a", ".a.tmp.1.99"), Some(1));
        assert_eq!(pid("x.tmp", ".x.tmp.tmp.7.8"), Some(7));
        // Another target's temp, even one whose name extends this one.
        assert_eq!(pid("x", ".x.tmp.tmp.7.8"), None);
        assert_eq!(pid("index.fxs", ".other.fxs.tmp.1.2"), None);
        assert_eq!(pid("index", ".index.fxs.tmp.1.2"), None);
        for other in [
            "index.fxs",
            "index.fxs.tmp.1.2",
            "..tmp.1.2",
            ".index.fxs.tmp.1",
            ".index.fxs.tmp.1.",
            ".index.fxs.tmp..2",
            ".index.fxs.tmp.a.2",
            ".index.fxs.tmp.1.2x",
            ".index.fxs.tmp.1.2.3",
            ".index.fxs.tmp.99999999999.2",
            ".index.fxs.bak.1.2",
            ".hidden",
            "tmp.log",
        ] {
            assert_eq!(pid("index.fxs", other), None, "{other}");
        }
    }
}
