//! Limited (non-FDA) mode change filtering.
//!
//! Without Full Disk Access the scan skips the folders that would raise a
//! TCC consent prompt (`fx_fs_macos::fda::consent_prompt_paths`). Change
//! events must skip them too: the engine's writer stats every
//! [`Change::CheckPath`], and a stat inside a protected folder can raise the
//! prompt. That includes FSEvents history replayed from a saved cursor at
//! launch.
//!
//! [`ExcludingChangeSource`] wraps any [`ChangeSource`] and drops the
//! path-bearing changes at or below an excluded folder before they reach the
//! engine. It is platform-agnostic, so it builds and tests on any host.

use std::path::{Path, PathBuf};
use std::time::Instant;

use fx_fs::ChangeSource;
use fx_model::{Change, ResumeCursor, StartOutcome};

/// A [`ChangeSource`] that drops changes at or below any of a list of
/// excluded folders.
///
/// `start` and `cursor` are delegated unchanged, so the cursor still moves
/// past dropped events (a batch made only of excluded events is returned
/// empty, and the next launch does not replay it). Errors are propagated.
///
/// Matching is on whole path components ([`Path::starts_with`]), so
/// `~/Documents2` is not excluded by `~/Documents`, and a trailing slash
/// makes no difference. Dropped:
///
/// - [`Change::CheckPath`] at or below an excluded folder;
/// - [`Change::RescanSubtree`] at or below an excluded folder.
///
/// Passed through unchanged:
///
/// - a [`Change::RescanSubtree`] for an ancestor of an excluded folder (such
///   as the home folder): the scanner's own exclusions already skip the
///   protected folders inside it;
/// - [`Change::RescanVolume`], for the same reason;
/// - [`Change::Upsert`] and [`Change::Remove`]: they carry an entry or a
///   key, not a path, so there is nothing to match, and applying them needs
///   no stat. (The FSEvents source emits only `CheckPath` and rescans.)
///
/// An empty exclusion list passes everything, which is how the app runs
/// when Full Disk Access is granted.
#[derive(Debug)]
pub struct ExcludingChangeSource<S: ChangeSource> {
    inner: S,
    excluded: Vec<PathBuf>,
}

impl<S: ChangeSource> ExcludingChangeSource<S> {
    /// Wraps `inner`, dropping changes at or below any of `excluded`
    /// (absolute folder paths, matched by whole components).
    pub fn new(inner: S, excluded: Vec<PathBuf>) -> Self {
        Self { inner, excluded }
    }

    /// The excluded folders.
    pub fn excluded(&self) -> &[PathBuf] {
        &self.excluded
    }

    /// The wrapped source.
    pub fn inner(&self) -> &S {
        &self.inner
    }

    /// Unwraps the inner source.
    pub fn into_inner(self) -> S {
        self.inner
    }

    /// Whether `path` is at or below an excluded folder.
    pub fn is_excluded(&self, path: &Path) -> bool {
        is_excluded(&self.excluded, path)
    }

    /// Whether `change` passes the filter.
    pub fn allows(&self, change: &Change) -> bool {
        match change {
            Change::CheckPath(path) | Change::RescanSubtree(path) => !self.is_excluded(path),
            Change::Upsert(_) | Change::Remove { .. } | Change::RescanVolume => true,
        }
    }
}

/// Whether `path` is at or below any of `excluded`, comparing whole path
/// components.
pub fn is_excluded(excluded: &[PathBuf], path: &Path) -> bool {
    excluded.iter().any(|folder| path.starts_with(folder))
}

impl<S: ChangeSource> ChangeSource for ExcludingChangeSource<S> {
    fn start(&mut self, from: Option<&ResumeCursor>) -> fx_fs::Result<StartOutcome> {
        self.inner.start(from)
    }

    fn next_batch(&mut self, deadline: Instant) -> fx_fs::Result<Vec<Change>> {
        let mut batch = self.inner.next_batch(deadline)?;
        if !self.excluded.is_empty() {
            batch.retain(|change| self.allows(change));
        }
        Ok(batch)
    }

    fn cursor(&self) -> ResumeCursor {
        self.inner.cursor()
    }
}
