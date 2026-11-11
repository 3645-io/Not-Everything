//! [`MacPathStat`]: `fx_fs::PathStat` for macOS (macOS only).
//!
//! Resolves one absolute path to the [`fx_model::Entry`] it names right
//! now, with `lstat` semantics: a symlink is reported as
//! [`EntryKind::Symlink`] and never followed.
//!
//! # Keys
//!
//! The entry follows the [`MacScanner`](crate::enumerator::MacScanner)
//! keying rule: a key is `(st_dev of the directory that lists the entry,
//! inode of the entry)`. So `stat(p)` lstats both `p` and its parent:
//!
//! * `key` = `(lstat(parent).st_dev, lstat(p).st_ino)`;
//! * `parent` = `(lstat(grandparent).st_dev, lstat(parent).st_ino)`, or
//!   `None` for `/`.
//!
//! For almost every path this equals `lstat`'s own `(st_dev, st_ino)`;
//! for a firmlinked directory seen from `/` (such as `/Users`) it pairs the
//! system volume's device with the Data volume's inode, exactly as the
//! scanner does (see
//! [Keys of firmlinked directories](crate::enumerator#keys-of-firmlinked-directories)).
//!
//! A mount point (a path whose device differs from its parent's and that
//! is not a firmlink) is keyed by the mounted volume's root, its own
//! `(st_dev, st_ino)`, as the scanner keys it (run 2.4), and so is the
//! parent of an entry directly on a volume's root.
//!
//! # Cloud files
//!
//! Only `lstat` is called: nothing is opened, read or listed, so a dataless
//! (cloud-only) item is never materialized. [`MacPathStat::new`] also
//! switches off dataless materialization for the process
//! ([`crate::cloud::disable_dataless_materialization`]), the same policy
//! the scanner applies, and dataless items carry
//! [`EntryFlags::DATALESS`].

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};

use fx_fs::PathStat;
use fx_model::{Entry, EntryFlags, EntryKind, FileKey, VolumeId};

use crate::scan::{is_hidden, is_system};

/// `UF_IMMUTABLE` (`sys/stat.h`).
const UF_IMMUTABLE: u32 = 0x0000_0002;
/// `SF_IMMUTABLE` (`sys/stat.h`).
const SF_IMMUTABLE: u32 = 0x0002_0000;

/// `fx_fs::PathStat` over `lstat`. See the module docs.
#[derive(Debug, Clone, Copy)]
pub struct MacPathStat {
    _private: (),
}

impl MacPathStat {
    /// A path stat that never materializes cloud files (switches off
    /// dataless materialization for the process; `lstat` alone would not
    /// trigger a download either).
    #[must_use]
    pub fn new() -> MacPathStat {
        let _ = crate::cloud::disable_dataless_materialization();
        MacPathStat { _private: () }
    }
}

impl Default for MacPathStat {
    /// The same as [`MacPathStat::new`]: a derived `Default` would skip the
    /// call that switches off dataless materialization.
    fn default() -> MacPathStat {
        MacPathStat::new()
    }
}

/// `lstat(path)`; `Ok(None)` when the path (or a parent component) is gone.
fn lstat(path: &Path) -> io::Result<Option<libc::stat>> {
    let c = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
    // SAFETY: `libc::stat` is plain old data; all-zero is a valid value.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid NUL-terminated string and `st` is writable.
    let rc = unsafe { libc::lstat(c.as_ptr(), &raw mut st) };
    if rc == 0 {
        return Ok(Some(st));
    }
    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        Some(libc::ENOENT | libc::ENOTDIR) => Ok(None),
        _ => Err(err),
    }
}

/// Builds a key from a device and an inode, converting the device the
/// same way the scanner does (`dev_t as u32`).
#[allow(clippy::cast_sign_loss)]
fn key(dev: libc::dev_t, ino: u64) -> FileKey {
    FileKey::new(VolumeId(dev as u32), ino)
}

/// The key of `path` (lstat `st`) listed in a folder on device
/// `parent_dev`: `(parent_dev, st_ino)`, except for a mount point, which is
/// keyed by its own `(st_dev, st_ino)`. A firmlinked folder (such as
/// `/Users`) is not a mount point even where its device differs.
fn listed_key(path: &Path, st: &libc::stat, parent_dev: libc::dev_t) -> FileKey {
    if st.st_dev != parent_dev && !is_firmlink(path) {
        key(st.st_dev, st.st_ino)
    } else {
        key(parent_dev, st.st_ino)
    }
}

/// Whether `path` is a firmlink of the system table (such as `/Users`).
fn is_firmlink(path: &Path) -> bool {
    crate::firmlinks::FirmlinkMap::system()
        .entries()
        .iter()
        .any(|(on_root, _)| on_root == path)
}

/// The path with `.` components and trailing slashes removed, so that
/// `parent()` and `file_name()` see the real last component.
fn clean(path: &Path) -> std::path::PathBuf {
    path.components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect()
}

impl PathStat for MacPathStat {
    fn stat(&self, path: &Path) -> fx_fs::Result<Option<Entry>> {
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("PathStat needs an absolute path: {}", path.display()),
            ));
        }
        let path = clean(path);
        let Some(st) = lstat(&path)? else {
            return Ok(None);
        };

        let (entry_key, parent_key) = match path.parent() {
            None => (key(st.st_dev, st.st_ino), None),
            Some(parent) => {
                let Some(pst) = lstat(parent)? else {
                    return Ok(None);
                };
                let parent_key = match parent.parent() {
                    None => key(pst.st_dev, pst.st_ino),
                    Some(grand) => match lstat(grand)? {
                        Some(gst) => listed_key(parent, &pst, gst.st_dev),
                        None => return Ok(None),
                    },
                };
                (listed_key(&path, &st, pst.st_dev), Some(parent_key))
            }
        };

        let kind = match st.st_mode & libc::S_IFMT {
            libc::S_IFDIR => EntryKind::Dir,
            libc::S_IFLNK => EntryKind::Symlink,
            _ => EntryKind::File,
        };
        let name_bytes = path.file_name().map_or(b"/" as &[u8], |n| n.as_bytes());
        let mut flags = EntryFlags::empty();
        flags.set(EntryFlags::HIDDEN, is_hidden(name_bytes, st.st_flags));
        flags.set(
            EntryFlags::SYSTEM,
            is_system(path.as_os_str().as_bytes(), st.st_flags),
        );
        flags.set(EntryFlags::DATALESS, crate::cloud::is_dataless(st.st_flags));
        flags.set(
            EntryFlags::LOCKED,
            st.st_flags & (UF_IMMUTABLE | SF_IMMUTABLE) != 0,
        );
        #[allow(clippy::cast_sign_loss)]
        let size = if kind == EntryKind::Dir {
            0
        } else {
            st.st_size.max(0) as u64
        };
        Ok(Some(Entry {
            key: entry_key,
            parent: parent_key,
            name: String::from_utf8_lossy(name_bytes).into_owned(),
            kind,
            flags,
            size,
            modified: st.st_mtime,
            created: st.st_birthtime,
        }))
    }
}
