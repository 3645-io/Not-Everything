//! On-disk materialization of a synthetic [`Tree`].
//!
//! [`materialize`] creates entry `i` of a tree at `root.join(tree.path(i))`,
//! directories as directories and files as empty files. It only ever
//! creates: it never deletes, renames, moves, truncates or overwrites
//! anything, and it never writes outside `root`.

use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use crate::tree::Tree;

/// What [`materialize`] created.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MaterializeStats {
    /// Entries created (directories plus files).
    pub entries: usize,
    /// Directories created, including the tree's root entry.
    pub dirs: usize,
    /// Empty files created.
    pub files: usize,
}

/// Creates the first `limit` entries of `tree` (all of them if `limit` is
/// `None` or exceeds the tree's length) under `root`.
///
/// Entry 0, the tree's root, becomes `root/root`. Parents precede children
/// in a [`Tree`], so any prefix is a valid tree and every parent directory
/// exists before its children are created.
///
/// `root` must be an existing, empty directory; otherwise an error is
/// returned and nothing is created. Every entry is created exclusively
/// (`create_dir` and `create_new`), so an existing path is never
/// overwritten. Names that could escape `root` (empty, `.`, `..`, or
/// containing a path separator or NUL) and broken parent links are rejected
/// with [`io::ErrorKind::InvalidData`] before anything is created for them.
///
/// On error, entries created so far are left in place.
pub fn materialize(tree: &Tree, root: &Path, limit: Option<usize>) -> io::Result<MaterializeStats> {
    check_root(root)?;

    let n = limit.map_or(tree.len(), |l| l.min(tree.len()));
    let mut stats = MaterializeStats::default();
    // Full path of every directory created so far, and for each entry its
    // slot in `dir_paths` (`NO_DIR` for files).
    const NO_DIR: u32 = u32::MAX;
    let mut dir_paths: Vec<PathBuf> = Vec::new();
    let mut dir_slot: Vec<u32> = Vec::with_capacity(n);

    for i in 0..n {
        let name = tree.names.get(i);
        check_name(i, name)?;

        let parent = tree.parent[i];
        let base: &Path = if parent == u32::MAX {
            if i != 0 {
                return Err(invalid(format!(
                    "entry {i} has no parent but is not entry 0"
                )));
            }
            root
        } else {
            let p = parent as usize;
            if p >= i {
                return Err(invalid(format!(
                    "entry {i} has parent {p}, which does not precede it"
                )));
            }
            match dir_slot[p] {
                NO_DIR => {
                    return Err(invalid(format!(
                        "entry {i} has parent {p}, which is not a directory"
                    )));
                }
                slot => &dir_paths[slot as usize],
            }
        };
        let path = base.join(name);

        if tree.is_dir[i] {
            let slot = u32::try_from(dir_paths.len())
                .ok()
                .filter(|&s| s != NO_DIR)
                .ok_or_else(|| invalid("too many directories".to_string()))?;
            fs::create_dir(&path)?;
            dir_paths.push(path);
            dir_slot.push(slot);
            stats.dirs += 1;
        } else {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            dir_slot.push(NO_DIR);
            stats.files += 1;
        }
        stats.entries += 1;
    }
    Ok(stats)
}

/// Errors unless `root` is an existing, empty directory.
fn check_root(root: &Path) -> io::Result<()> {
    let meta = fs::metadata(root).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("output directory {}: {e}", root.display()),
        )
    })?;
    if !meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("output path {} is not a directory", root.display()),
        ));
    }
    if fs::read_dir(root)?.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("output directory {} is not empty", root.display()),
        ));
    }
    Ok(())
}

/// Rejects names that are not a single, plain path component.
fn check_name(i: usize, name: &str) -> io::Result<()> {
    let bad = name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\0'])
        || (cfg!(windows) && name.contains(['\\', ':']));
    if bad {
        return Err(invalid(format!("entry {i} has an unsafe name {name:?}")));
    }
    Ok(())
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}
