//! A steady, seeded stream of file creates, renames and deletes inside a
//! new folder, so the app's CPU, energy and freshness can be measured while
//! the file system changes. `fx-bench churn` uses it.
//!
//! The stream is a pure function of the seed ([`plan`], [`Planner`]): the
//! planner keeps its own model of the files it made, so it never refers to
//! a file that doesn't exist, and tests can check the operations without
//! touching the disk. [`run()`] applies the same stream at a steady rate.
//!
//! The layout is 16 folders ([`folders`]), 8 directly in the new folder, 5
//! one level down and 3 two levels down, so files sit at depths 1 to 3.
//! About half the operations create a file (empty or a few bytes), a
//! quarter rename one (within its folder or into another) and a quarter
//! delete one. At the file cap a create becomes a delete; with no files a
//! rename or delete becomes a create.
//!
//! Every created name starts with a counter (`17-report.txt`), so names
//! never collide, even on a case- or normalization-insensitive volume, and
//! never start with a dot (a hidden file might not be indexed).

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::names::NameGen;
use crate::rng::Rng;

/// Number of folders the run makes before the stream starts.
pub const FOLDERS: usize = 16;

/// The largest file a create writes, in bytes.
pub const MAX_FILE_BYTES: usize = 64;

/// Mixed into the seed for the folder names, so they don't repeat the
/// first file names.
const FOLDER_SALT: u64 = 0x666f_6c64_6572_7321;
/// Mixed into the seed for the file names.
const FILE_SALT: u64 = 0x6669_6c65_7321_0000;

/// What a created file holds, repeated up to its length.
const CONTENT: &[u8] = b"not-everything churn\n";

/// One operation of the stream. Paths are relative to the run's folder,
/// with `/` separators.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// Create a new file holding `len` bytes (0 to [`MAX_FILE_BYTES`]).
    Create { path: String, len: usize },
    /// Rename an existing file to a new name, in the same or another folder.
    Rename { from: String, to: String },
    /// Delete an existing file.
    Delete { path: String },
}

/// The 16 folders of a run, relative to its folder, parents first.
pub fn folders(seed: u64) -> Vec<String> {
    let mut names = NameGen::new(seed ^ FOLDER_SALT);
    let mut out: Vec<String> = Vec::with_capacity(FOLDERS);
    for i in 0..FOLDERS {
        let name = format!("{i:02}-{}", names.dir_name());
        let path = match parent_of(i) {
            None => name,
            Some(p) => format!("{}/{name}", out[p]),
        };
        out.push(path);
    }
    out
}

/// Folders 0 to 7 sit in the run's folder, 8 to 12 inside folders 0 to 4,
/// and 13 to 15 inside folders 8 to 10.
fn parent_of(i: usize) -> Option<usize> {
    match i {
        0..8 => None,
        8..13 => Some(i - 8),
        _ => Some(i - 5),
    }
}

/// The first `n` operations of the stream for `seed`, with at most
/// `max_files` files alive at once.
pub fn plan(seed: u64, n: usize, max_files: usize) -> Vec<Op> {
    let mut p = Planner::new(seed, max_files);
    (0..n).map(|_| p.next_op()).collect()
}

/// The operation stream, one operation at a time.
#[derive(Clone, Debug)]
pub struct Planner {
    folders: Vec<String>,
    rng: Rng,
    names: NameGen,
    /// Live files: folder index and path.
    files: Vec<(usize, String)>,
    /// The counter that starts every new name.
    next_id: u64,
    max_files: usize,
}

/// The kind of operation drawn before the file cap is applied.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Create,
    Rename,
    Delete,
}

impl Planner {
    /// A stream for `seed` that keeps at most `max_files` files (at least 1).
    pub fn new(seed: u64, max_files: usize) -> Planner {
        Planner {
            folders: folders(seed),
            rng: Rng::new(seed),
            names: NameGen::new(seed ^ FILE_SALT),
            files: Vec::new(),
            next_id: 0,
            max_files: max_files.max(1),
        }
    }

    /// The folders the stream's files live in, as [`folders`] gives them.
    pub fn folders(&self) -> &[String] {
        &self.folders
    }

    /// The number of files alive after the operations so far.
    pub fn live_files(&self) -> usize {
        self.files.len()
    }

    /// The next operation.
    pub fn next_op(&mut self) -> Op {
        let drawn = match self.rng.below(4) {
            0 | 1 => Kind::Create,
            2 => Kind::Rename,
            _ => Kind::Delete,
        };
        let kind = if self.files.is_empty() {
            Kind::Create
        } else if drawn == Kind::Create && self.files.len() >= self.max_files {
            Kind::Delete
        } else {
            drawn
        };
        match kind {
            Kind::Create => {
                let folder = self.rng.index(FOLDERS);
                let path = self.new_path(folder);
                let len = if self.rng.below(2) == 0 {
                    0
                } else {
                    1 + self.rng.index(MAX_FILE_BYTES)
                };
                self.files.push((folder, path.clone()));
                Op::Create { path, len }
            }
            Kind::Rename => {
                let i = self.rng.index(self.files.len());
                let folder = if self.rng.below(2) == 0 {
                    self.files[i].0
                } else {
                    self.rng.index(FOLDERS)
                };
                let to = self.new_path(folder);
                let (_, from) = std::mem::replace(&mut self.files[i], (folder, to.clone()));
                Op::Rename { from, to }
            }
            Kind::Delete => {
                let i = self.rng.index(self.files.len());
                let (_, path) = self.files.swap_remove(i);
                Op::Delete { path }
            }
        }
    }

    /// A new, unique file path in `folder`.
    fn new_path(&mut self, folder: usize) -> String {
        let id = self.next_id;
        self.next_id += 1;
        format!("{}/{id}-{}", self.folders[folder], self.names.file_name())
    }
}

/// The settings of a run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Options {
    pub seed: u64,
    /// Operations per second.
    pub rate: f64,
    pub duration: Duration,
    pub max_files: usize,
}

/// What a run did.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChurnStats {
    /// Operations applied, not counting the folders made first.
    pub ops: u64,
    /// Seconds from the first operation's tick to the end of the run.
    pub seconds: f64,
}

impl ChurnStats {
    /// Operations per second over the run.
    pub fn rate(&self) -> f64 {
        if self.seconds > 0.0 {
            self.ops as f64 / self.seconds
        } else {
            0.0
        }
    }
}

/// Creates `dir` (its parent must exist; `dir` must not), makes the 16
/// folders, then applies the stream for `opts.seed` at `opts.rate`
/// operations per second until `opts.duration` has passed. `dir` and what
/// is left in it stay in place.
///
/// Operations are paced on a fixed tick by sleeping, never by spinning.
/// When the disk falls behind, the next operation runs as soon as the last
/// one finishes and pacing resumes from there: missed ticks are dropped, not
/// made up in a burst that would distort the load being measured.
///
/// Only paths the run made are touched. Files are created exclusively
/// (`create_new`, which also refuses a symlink in the way), renames check
/// that the new name is free first because `rename` would replace a file,
/// and deletes unlink the file itself, never a symlink's target.
pub fn run(dir: &Path, opts: &Options) -> io::Result<ChurnStats> {
    let period = Duration::try_from_secs_f64(1.0 / opts.rate)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "rate out of range"))?;
    // `create_dir` fails when anything already exists at `dir`, so the run
    // never works inside a folder it didn't make.
    fs::create_dir(dir)?;
    let mut planner = Planner::new(opts.seed, opts.max_files);
    for folder in planner.folders() {
        fs::create_dir(dir.join(folder))?;
    }

    let start = Instant::now();
    let end = start
        .checked_add(opts.duration)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "duration out of range"))?;
    let mut next = start;
    let mut ops = 0u64;
    while next < end {
        sleep_until(next);
        let op = planner.next_op();
        apply(dir, &op).map_err(|e| io::Error::new(e.kind(), format!("{op:?}: {e}")))?;
        ops += 1;
        next = (next + period).max(Instant::now());
    }
    sleep_until(end);
    Ok(ChurnStats {
        ops,
        seconds: start.elapsed().as_secs_f64(),
    })
}

fn sleep_until(t: Instant) {
    let now = Instant::now();
    if t > now {
        std::thread::sleep(t - now);
    }
}

/// Applies one operation under `dir`.
fn apply(dir: &Path, op: &Op) -> io::Result<()> {
    match op {
        Op::Create { path, len } => {
            let mut f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(dir.join(path))?;
            let bytes: Vec<u8> = CONTENT.iter().copied().cycle().take(*len).collect();
            f.write_all(&bytes)
        }
        Op::Rename { from, to } => {
            let to = dir.join(to);
            // rename(2) silently replaces an existing file; this check keeps
            // the run from replacing anything it didn't make.
            match fs::symlink_metadata(&to) {
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "the new name is already taken",
                    ));
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            fs::rename(dir.join(from), to)
        }
        Op::Delete { path } => fs::remove_file(dir.join(path)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_parents_come_first_and_reach_depth_three() {
        for i in 0..FOLDERS {
            if let Some(p) = parent_of(i) {
                assert!(p < i);
            }
        }
        let depth = |mut i: usize| {
            let mut d = 1;
            while let Some(p) = parent_of(i) {
                i = p;
                d += 1;
            }
            d
        };
        assert_eq!((0..FOLDERS).map(depth).max(), Some(3));
    }

    #[test]
    fn the_cap_turns_creates_into_deletes() {
        let mut p = Planner::new(1, 3);
        for _ in 0..500 {
            p.next_op();
            assert!(p.live_files() <= 3);
        }
    }

    #[test]
    fn content_is_cut_to_length() {
        let bytes: Vec<u8> = CONTENT.iter().copied().cycle().take(30).collect();
        assert_eq!(bytes.len(), 30);
        assert!(bytes.starts_with(CONTENT));
    }
}
