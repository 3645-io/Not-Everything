//! Engine configuration.

use std::path::{Path, PathBuf};
use std::time::Duration;

use fx_model::ResumeCursor;

/// Default batch window of the writer thread.
pub const DEFAULT_BATCH_WINDOW: Duration = Duration::from_millis(100);
/// Shortest batch window the writer accepts.
pub const MIN_BATCH_WINDOW: Duration = Duration::from_millis(50);
/// Longest batch window the writer accepts.
pub const MAX_BATCH_WINDOW: Duration = Duration::from_millis(200);
/// Default batch window while no window shows the results
/// ([`Engine::set_foreground`](crate::Engine::set_foreground)): nobody
/// watches the results then, so the writer collects changes for up to 2 s
/// and merges the sort orders up to 20 times less often than with the
/// 100 ms window.
pub const DEFAULT_BACKGROUND_BATCH_WINDOW: Duration = Duration::from_secs(2);
/// Longest background batch window the writer accepts.
pub const MAX_BACKGROUND_BATCH_WINDOW: Duration = Duration::from_secs(10);
/// Default delay between the last published change and the idle
/// checkpoint: none. A store holds a whole volume (about 251 MB for a whole
/// disk), so the app saves it once a day and when it quits instead; a quiet
/// spell, such as a laptop at night, would otherwise save it several times
/// a day.
pub const DEFAULT_CHECKPOINT_IDLE: Option<Duration> = None;
/// Default longest delay between the first unsaved change and a checkpoint,
/// even while changes keep arriving: two days, a backstop for a Mac whose
/// app never finds a moment for its daily save.
pub const DEFAULT_CHECKPOINT_MAX_DELAY: Duration = Duration::from_secs(48 * 3600);
/// Default number of consecutive `next_batch` errors after which the writer
/// restarts the change source from the last published cursor.
pub const DEFAULT_RESTART_THRESHOLD: u32 = 3;
/// Default shortest wait between two restart attempts of the change source.
pub const DEFAULT_RESTART_BACKOFF: Duration = Duration::from_millis(250);
/// Longest wait between two restart attempts, unless the configured shortest
/// wait is longer.
pub const MAX_RESTART_BACKOFF: Duration = Duration::from_secs(5);

/// How an [`Engine`](crate::Engine) watches one volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineConfig {
    root: PathBuf,
    batch_window: Duration,
    background_batch_window: Duration,
    resume_from: Option<ResumeCursor>,
    store_path: Option<PathBuf>,
    checkpoint_idle: Option<Duration>,
    checkpoint_max_delay: Duration,
    utc_offset_secs: Option<i32>,
    restart_threshold: u32,
    restart_backoff: Duration,
    priority_include: Vec<PathBuf>,
    priority_exclude: Vec<PathBuf>,
    volumes_folder: Option<PathBuf>,
}

impl EngineConfig {
    /// A config for the volume mounted at `root` (an absolute path), with the
    /// default 100 ms batch window, no resume cursor, no checkpoint file, no
    /// idle checkpoint and the default 48 h longest checkpoint delay.
    pub fn new(root: impl Into<PathBuf>) -> EngineConfig {
        EngineConfig {
            root: root.into(),
            batch_window: DEFAULT_BATCH_WINDOW,
            background_batch_window: DEFAULT_BACKGROUND_BATCH_WINDOW,
            resume_from: None,
            store_path: None,
            checkpoint_idle: DEFAULT_CHECKPOINT_IDLE,
            checkpoint_max_delay: DEFAULT_CHECKPOINT_MAX_DELAY,
            utc_offset_secs: None,
            restart_threshold: DEFAULT_RESTART_THRESHOLD,
            restart_backoff: DEFAULT_RESTART_BACKOFF,
            priority_include: Vec::new(),
            priority_exclude: Vec::new(),
            volumes_folder: None,
        }
    }

    /// Sets the folder volumes are mounted in (run 2.4): `/Volumes` for
    /// the app, an absolute path at or below the root.
    ///
    /// A *volume root* is a live entry directly in that folder whose key is
    /// on another volume than the folder's own (a mount point the scanner
    /// entered, keyed by the mounted volume's root). When a volume root
    /// disappears from the disk (its path is gone, or a rescan does not
    /// list it), the volume goes offline instead of away: its entries stay
    /// in the index, re-keyed onto a reserved offline volume ID
    /// ([`fx_model::VolumeId::is_offline`];
    /// [`EngineSnapshot::is_offline`](crate::EngineSnapshot::is_offline)),
    /// and searches leave them out until the interface can mark them.
    /// Rescans keep offline entries, and a volume mounted with the same
    /// name in the folder replaces them. Without a volumes folder (the
    /// default) a vanished mount point is removed like any folder.
    pub fn with_volumes_folder(mut self, folder: impl Into<PathBuf>) -> EngineConfig {
        self.volumes_folder = Some(folder.into());
        self
    }

    /// The folder volumes are mounted in, if set
    /// ([`with_volumes_folder`](Self::with_volumes_folder)).
    pub fn volumes_folder(&self) -> Option<&Path> {
        self.volumes_folder.as_deref()
    }

    /// Sets the folders of the priority group ("Your files first"): an
    /// entry is in the group when the nearest listed folder at or above it
    /// is in `include`, unless it is hidden: an entry whose name starts with
    /// `.`, and everything inside a hidden folder, is never in the group.
    /// For the app, `include` is the home folder, iCloud Drive and
    /// `~/Library/CloudStorage`, and `exclude` is `~/Library`.
    ///
    /// Paths are absolute. A folder listed in both counts as excluded. A
    /// listed folder at or above the root decides for the root entry (the
    /// deepest one wins); a folder that does not exist yet is skipped until
    /// it appears, so a cloud drive added inside a listed folder later joins
    /// at once. Every published snapshot carries the group for its own index
    /// ([`EngineSnapshot::priority_group`](crate::EngineSnapshot::priority_group)),
    /// and [`Engine::search_grouped`](crate::Engine::search_grouped) lists it
    /// first. Without priority folders (the default) there is no group.
    pub fn with_priority_folders<I, E>(mut self, include: I, exclude: E) -> EngineConfig
    where
        I: IntoIterator,
        I::Item: Into<PathBuf>,
        E: IntoIterator,
        E::Item: Into<PathBuf>,
    {
        self.priority_include = include.into_iter().map(Into::into).collect();
        self.priority_exclude = exclude.into_iter().map(Into::into).collect();
        self
    }

    /// The priority folders: those that start the group, then those that
    /// end it (see [`with_priority_folders`](Self::with_priority_folders)).
    pub fn priority_folders(&self) -> (&[PathBuf], &[PathBuf]) {
        (&self.priority_include, &self.priority_exclude)
    }

    /// Sets how many consecutive `next_batch` errors make the writer restart
    /// the change source from the last published cursor (default
    /// [`DEFAULT_RESTART_THRESHOLD`]; at least 1).
    pub fn with_restart_threshold(mut self, threshold: u32) -> EngineConfig {
        self.restart_threshold = threshold.max(1);
        self
    }

    /// Sets the shortest wait between two restart attempts of the change
    /// source (default [`DEFAULT_RESTART_BACKOFF`]). Later waits double, up
    /// to [`MAX_RESTART_BACKOFF`] or this value, whichever is longer.
    pub fn with_restart_backoff(mut self, backoff: Duration) -> EngineConfig {
        self.restart_backoff = backoff;
        self
    }

    /// Consecutive `next_batch` errors that trigger a source restart.
    pub fn restart_threshold(&self) -> u32 {
        self.restart_threshold
    }

    /// The shortest wait between two restart attempts.
    pub fn restart_backoff(&self) -> Duration {
        self.restart_backoff
    }

    /// Sets how long the writer collects changes before applying them,
    /// clamped to 50–200 ms.
    pub fn with_batch_window(mut self, window: Duration) -> EngineConfig {
        self.batch_window = window.clamp(MIN_BATCH_WINDOW, MAX_BATCH_WINDOW);
        self
    }

    /// Sets the batch window used while no window shows the results
    /// (default [`DEFAULT_BACKGROUND_BATCH_WINDOW`]), clamped to
    /// [`MIN_BATCH_WINDOW`]..=[`MAX_BACKGROUND_BATCH_WINDOW`].
    pub fn with_background_batch_window(mut self, window: Duration) -> EngineConfig {
        self.background_batch_window = window.clamp(MIN_BATCH_WINDOW, MAX_BACKGROUND_BATCH_WINDOW);
        self
    }

    /// Sets the cursor the index was saved with; the change source catches
    /// up from it. [`Engine::open`](crate::Engine::open) replaces it with the
    /// cursor saved in the store.
    pub fn with_resume_from(mut self, cursor: ResumeCursor) -> EngineConfig {
        self.resume_from = Some(cursor);
        self
    }

    /// Sets the checkpoint file; without one the engine never checkpoints.
    /// When saving, the engine creates a missing parent directory with mode
    /// 0700 and never changes the mode of one that exists.
    pub fn with_store_path(mut self, path: impl Into<PathBuf>) -> EngineConfig {
        self.store_path = Some(path.into());
        self
    }

    /// Sets how long after the last published change an idle checkpoint is
    /// written (by default none: [`DEFAULT_CHECKPOINT_IDLE`]).
    pub fn with_checkpoint_idle(mut self, idle: Duration) -> EngineConfig {
        self.checkpoint_idle = Some(idle);
        self
    }

    /// Sets the longest delay between the first change not yet saved and a
    /// checkpoint, so a volume that never stays idle for the idle delay is
    /// still saved (default [`DEFAULT_CHECKPOINT_MAX_DELAY`]).
    pub fn with_checkpoint_max_delay(mut self, delay: Duration) -> EngineConfig {
        self.checkpoint_max_delay = delay;
        self
    }

    /// Fixes the local time zone's offset east of UTC, in seconds, used by
    /// date functions such as `dm:today`. By default the engine asks the
    /// operating system at every search (UTC where it cannot).
    pub fn with_utc_offset(mut self, offset_secs: i32) -> EngineConfig {
        self.utc_offset_secs = Some(offset_secs);
        self
    }

    /// The volume root: absolute paths reported by the change source are
    /// resolved relative to it, starting at the index's root entry.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The (clamped) batch window.
    pub fn batch_window(&self) -> Duration {
        self.batch_window
    }

    /// The batch window used while no window shows the results.
    pub fn background_batch_window(&self) -> Duration {
        self.background_batch_window
    }

    /// The cursor the change source is started from, if any.
    pub fn resume_from(&self) -> Option<ResumeCursor> {
        self.resume_from
    }

    /// The checkpoint file, if any.
    pub fn store_path(&self) -> Option<&Path> {
        self.store_path.as_deref()
    }

    /// How long after the last published change an idle checkpoint is
    /// written; `None` when there is no idle checkpoint.
    pub fn checkpoint_idle(&self) -> Option<Duration> {
        self.checkpoint_idle
    }

    /// The longest delay between the first unsaved change and a checkpoint.
    pub fn checkpoint_max_delay(&self) -> Duration {
        self.checkpoint_max_delay
    }

    /// The fixed UTC offset, if one was set with
    /// [`with_utc_offset`](Self::with_utc_offset).
    pub fn utc_offset(&self) -> Option<i32> {
        self.utc_offset_secs
    }

    /// Sets or clears the resume cursor (startup from the store).
    pub(crate) fn set_resume_from(&mut self, cursor: Option<ResumeCursor>) {
        self.resume_from = cursor;
    }
}
