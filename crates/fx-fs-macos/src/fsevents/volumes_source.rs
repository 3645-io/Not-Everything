//! [`VolumesSource`]: the root volume's change source plus one FSEvents
//! stream per mounted external volume (run 2.4, macOS only).
//!
//! The root source (the boot volume's [`FsEventsSource`])
//! keeps the resume cursor; the external volumes have none, so each is
//! rescanned when it is mounted and when the source starts:
//!
//! * **Mounts.** The mount table (`getfsstat`) is read at start, at most
//!   once every [`MOUNT_CHECK_INTERVAL`] while batches are collected, and
//!   at once when the root source reports a change directly in the
//!   external volumes folder (macOS creates and removes `/Volumes/<name>`
//!   around a mount). The check rides on the writer's own calls, so it
//!   adds no thread and no wakeup.
//! * **A volume appears** that [`ExternalVolumes::indexes`] accepts: its
//!   stream starts from now, then a [`Change::RescanSubtree`] of its mount
//!   point asks the engine to index it (Apple's order: watch, then scan).
//! * **A volume disappears:** its stream stops and a [`Change::CheckPath`]
//!   of its mount point lets the engine find it gone; with
//!   `EngineConfig::with_volumes_folder` the engine marks it offline.
//! * **Start:** when the root source resumed from its cursor, one
//!   [`Change::RescanSubtree`] of the external volumes folder rescans
//!   every mounted external volume and lets the engine take the ones
//!   unmounted while the app was closed offline. Any other outcome makes
//!   the engine rescan everything, external volumes included (the scanner
//!   enters them with the same [`ExternalVolumes`]).
//! * **Events** of an external volume's stream arrive with the root
//!   source's batches: the source drains every external stream without
//!   waiting after waiting on the root source, so a change on an external
//!   volume reaches the engine within about one batch window when the
//!   root volume is quiet. A volume-level event of an external stream
//!   (its history reset or its own unmount) becomes a subtree rescan of
//!   its mount point instead of a rescan of every volume; a stream that
//!   fails is dropped and the volume rescanned at the next mount check.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fx_fs::ChangeSource;
use fx_model::{Change, ResumeCursor, StartOutcome};

use super::source::FsEventsSource;
use super::stream::StreamConfig;
use crate::volumes::{ExternalVolumes, MountInfo, list_mounts};

/// Longest time between two reads of the mount table while the source
/// is in use.
pub const MOUNT_CHECK_INTERVAL: Duration = Duration::from_secs(1);

/// Reads the mount table; [`list_mounts`] in the app, a fake in tests.
pub type MountLister = Box<dyn FnMut() -> io::Result<Vec<MountInfo>> + Send>;

/// See the module docs.
pub struct VolumesSource<R> {
    root: R,
    /// The engine's root, canonical: only volumes below it are watched.
    root_path: PathBuf,
    external: ExternalVolumes,
    /// The external volumes being watched, by mount point; `None` for one
    /// whose stream could not start (retried at the next mount check
    /// after it is gone and back).
    streams: BTreeMap<PathBuf, Option<FsEventsSource>>,
    /// Changes waiting for the next batch.
    pending: Vec<Change>,
    last_check: Option<Instant>,
    lister: MountLister,
}

impl<R: ChangeSource> VolumesSource<R> {
    /// Wraps `root`, the change source of the volume holding `root_path`
    /// (canonical, such as `/`), watching the external volumes `external`
    /// accepts below it.
    pub fn new(root: R, root_path: impl Into<PathBuf>, external: ExternalVolumes) -> Self {
        VolumesSource::with_lister(root, root_path, external, Box::new(list_mounts))
    }

    /// As [`new`](Self::new), reading the mount table with `lister`.
    pub fn with_lister(
        root: R,
        root_path: impl Into<PathBuf>,
        external: ExternalVolumes,
        lister: MountLister,
    ) -> Self {
        VolumesSource {
            root,
            root_path: root_path.into(),
            external,
            streams: BTreeMap::new(),
            pending: Vec::new(),
            last_check: None,
            lister,
        }
    }

    /// The mount points of the external volumes being watched.
    pub fn watched(&self) -> Vec<PathBuf> {
        self.streams.keys().cloned().collect()
    }

    /// Whether the folder external volumes are mounted in lies strictly
    /// below the root, so its volumes are part of the index.
    fn folder_below_root(&self) -> bool {
        self.external.folder != self.root_path && self.external.folder.starts_with(&self.root_path)
    }

    /// The external volumes mounted now that are indexed.
    fn mounted(&mut self) -> Option<Vec<PathBuf>> {
        if !self.folder_below_root() {
            return Some(Vec::new());
        }
        let mounts = (self.lister)().ok()?;
        Some(
            mounts
                .iter()
                .filter(|m| self.external.indexes(m))
                .map(|m| m.mount_point.clone())
                .collect(),
        )
    }

    /// Reads the mount table (at most once a [`MOUNT_CHECK_INTERVAL`]
    /// unless `force` is set), starting streams for new volumes (queueing a
    /// rescan of each when `rescan`) and stopping those of volumes that
    /// are gone (queueing a check of their mount point).
    fn check_mounts(&mut self, force: bool, rescan: bool) {
        let now = Instant::now();
        if !force
            && self
                .last_check
                .is_some_and(|t| now.duration_since(t) < MOUNT_CHECK_INTERVAL)
        {
            return;
        }
        self.last_check = Some(now);
        let Some(mounted) = self.mounted() else {
            return;
        };
        let gone: Vec<PathBuf> = self
            .streams
            .keys()
            .filter(|p| !mounted.contains(p))
            .cloned()
            .collect();
        for mount_point in gone {
            self.streams.remove(&mount_point);
            self.pending.push(Change::CheckPath(mount_point));
        }
        for mount_point in mounted {
            if self.streams.contains_key(&mount_point) {
                continue;
            }
            let stream = start_stream(&mount_point);
            self.streams.insert(mount_point.clone(), stream);
            if rescan {
                self.pending.push(Change::RescanSubtree(mount_point));
            }
        }
    }

    /// Whether `change` is about an entry directly in the external
    /// volumes folder, where macOS adds and removes mount points.
    fn in_folder(&self, change: &Change) -> bool {
        let path = match change {
            Change::CheckPath(p) | Change::RescanSubtree(p) => p,
            _ => return false,
        };
        path.parent() == Some(self.external.folder.as_path()) || path == &self.external.folder
    }

    /// Takes every external stream's waiting changes, without waiting.
    fn drain_external(&mut self, out: &mut Vec<Change>) {
        let now = Instant::now();
        let mut failed = Vec::new();
        for (mount_point, stream) in &mut self.streams {
            let Some(source) = stream.as_mut() else {
                continue;
            };
            match source.next_batch(now) {
                Ok(batch) => {
                    out.extend(batch.into_iter().map(|c| external_change(mount_point, c)));
                }
                Err(_) => failed.push(mount_point.clone()),
            }
        }
        for mount_point in failed {
            // Dropped: the next mount check starts it again if the volume
            // is still there, and the rescan covers what it missed.
            self.streams.remove(&mount_point);
        }
    }
}

/// An external volume's change, with a volume-level rescan narrowed to its
/// mount point.
fn external_change(mount_point: &Path, change: Change) -> Change {
    match change {
        Change::RescanVolume => Change::RescanSubtree(mount_point.to_path_buf()),
        other => other,
    }
}

/// Starts a live stream (no history) on the volume mounted at
/// `mount_point`; `None` if it fails.
fn start_stream(mount_point: &Path) -> Option<FsEventsSource> {
    let mut config = StreamConfig::new(mount_point.to_path_buf());
    config.path_filter = Some(mount_point.to_path_buf());
    let mut source = FsEventsSource::new(config);
    source.start(None).ok()?;
    Some(source)
}

impl<R: ChangeSource> ChangeSource for VolumesSource<R> {
    fn start(&mut self, from: Option<&ResumeCursor>) -> fx_fs::Result<StartOutcome> {
        let outcome = self.root.start(from)?;
        self.streams.clear();
        self.pending.clear();
        // A full rescan follows any other outcome and enters the external
        // volumes itself.
        let resumed = outcome == StartOutcome::Resumed;
        self.check_mounts(true, false);
        if resumed && self.folder_below_root() {
            self.pending
                .push(Change::RescanSubtree(self.external.folder.clone()));
        }
        Ok(outcome)
    }

    fn next_batch(&mut self, deadline: Instant) -> fx_fs::Result<Vec<Change>> {
        self.check_mounts(false, true);
        let mut out = std::mem::take(&mut self.pending);
        let wait_until = if out.is_empty() {
            deadline
        } else {
            Instant::now()
        };
        let root = match self.root.next_batch(wait_until) {
            Ok(root) => root,
            Err(e) => {
                // Kept for the next batch: a failed call must not lose a
                // volume's rescan or check.
                self.pending = out;
                return Err(e);
            }
        };
        let mounts_moved = root.iter().any(|c| self.in_folder(c));
        out.extend(root);
        self.drain_external(&mut out);
        if mounts_moved {
            self.check_mounts(true, true);
            out.append(&mut self.pending);
            // The mount point's own check would scan a new volume a second
            // time: the subtree rescan covers it.
            let rescans: Vec<PathBuf> = out
                .iter()
                .filter_map(|c| match c {
                    Change::RescanSubtree(p) => Some(p.clone()),
                    _ => None,
                })
                .collect();
            out.retain(|c| !matches!(c, Change::CheckPath(p) if rescans.contains(p)));
        }
        Ok(out)
    }

    fn cursor(&self) -> ResumeCursor {
        self.root.cursor()
    }
}
