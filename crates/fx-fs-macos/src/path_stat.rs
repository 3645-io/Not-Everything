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
//! # Names
//!
//! The entry's name is the one the volume stores, as the scanner lists it,
//! which is not always the last component of the path: on a
//! case-insensitive volume (the default on macOS) the old spelling of a
//! case-only rename still finds the entry, and an event or a rescan may
//! name it so. FSEvents does not promise the order of a rename's two paths,
//! and its history may keep only one spelling of a path, so taking the
//! name from the path put the old letter case back in the index (found by
//! the soak, run 2.2). The name is therefore read with `getattrlist(2)` and
//! `ATTR_CMN_NAME`, without following a symlink, and the path's own name is
//! kept when:
//!
//! * the volume is case-sensitive, which `pathconf(2)` with
//!   `_PC_CASE_SENSITIVE` tells once per device: no other spelling finds
//!   the entry there, so the extra call is skipped;
//! * the path has no parent (`/`), or the entry is on another device than
//!   its parent (a mount point, or a firmlinked folder such as `/Users`
//!   seen from `/`): there `ATTR_CMN_NAME` gives the volume's name, while
//!   the index needs the folder's name as its parent lists it;
//! * the stored name differs from the path's in more than letter case
//!   (compared in Unicode lowercase): the name of another hard link, on a
//!   file system that would report one, or a spelling in another Unicode
//!   normalization form, which this does not compare;
//! * the call fails.
//!
//! The extra call costs about what one `lstat` does (about 1.4 µs each on
//! an M4 Pro, measured with the same path): one more system call on top of
//! the three the keys need.
//!
//! # Cloud files
//!
//! Only `lstat` and `getattrlist` (for the name) are called: nothing is
//! opened, read or listed, so a dataless (cloud-only) item is never
//! materialized. [`MacPathStat::new`] also switches off dataless
//! materialization for the process
//! ([`crate::cloud::disable_dataless_materialization`]), the same policy
//! the scanner applies, and dataless items carry
//! [`EntryFlags::DATALESS`].

use std::ffi::{CStr, CString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};
use std::sync::Mutex;

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

/// `path` as a C string.
fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))
}

/// `lstat(path)`; `Ok(None)` when the path (or a parent component) is gone.
fn lstat(c: &CStr) -> io::Result<Option<libc::stat>> {
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

/// The size of the reply buffer for [`stored_name`]: the reply's length
/// (`u32`), the name's `attrreference_t` (8 bytes) and the longest name
/// with its NUL, `NAME_MAX * 3 + 1` = 766 bytes (`getattrlist(2)`).
/// `getattrlist` silently truncates a reply that doesn't fit, which
/// [`name_in_reply`] refuses.
const NAME_REPLY_SIZE: usize = 1024;

/// The name `path`'s last component has on disk, from `getattrlist` with
/// `ATTR_CMN_NAME` (a symlink is not followed), written into `reply`.
/// `None` when the call fails or the reply holds no name.
fn stored_name<'a>(path: &CStr, reply: &'a mut [u8; NAME_REPLY_SIZE]) -> Option<&'a [u8]> {
    let mut list = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_NAME,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    // SAFETY: `path` is a valid NUL-terminated string, `list` a valid
    // `attrlist` and `reply` writable for its whole length, the size
    // passed; the kernel writes no more than that.
    let rc = unsafe {
        libc::getattrlist(
            path.as_ptr(),
            (&raw mut list).cast(),
            reply.as_mut_ptr().cast(),
            reply.len(),
            libc::FSOPT_NOFOLLOW,
        )
    };
    if rc != 0 {
        return None;
    }
    name_in_reply(reply)
}

/// The name in a `getattrlist` reply that asked for `ATTR_CMN_NAME` alone:
/// the reply's length (`u32`), then an `attrreference_t` (the name's offset
/// from the reference itself, `i32`, and its length with the NUL, `u32`),
/// then the name. `None` for a reply longer than `reply` (truncated), a
/// name outside the reply, or an empty name.
fn name_in_reply(reply: &[u8]) -> Option<&[u8]> {
    let u32_at = |at: usize| -> Option<u32> {
        let bytes = reply.get(at..at + 4)?;
        Some(u32::from_ne_bytes(bytes.try_into().ok()?))
    };
    let len = usize::try_from(u32_at(0)?).ok()?;
    let offset = usize::try_from(i32::from_ne_bytes(u32_at(4)?.to_ne_bytes())).ok()?;
    let name_len = usize::try_from(u32_at(8)?).ok()?;
    let reply = reply.get(..len)?;
    let start = 4usize.checked_add(offset)?;
    let name = reply.get(start..start.checked_add(name_len)?)?;
    let name = name.strip_suffix(&[0]).unwrap_or(name);
    (!name.is_empty()).then_some(name)
}

/// Whether `stored` and `given` are one name in different letter case:
/// equal, or equal in Unicode lowercase (in ASCII case for names that
/// aren't UTF-8). Spellings in different Unicode normalization forms don't
/// match.
fn same_name_ignoring_case(stored: &[u8], given: &[u8]) -> bool {
    if stored == given {
        return true;
    }
    match (std::str::from_utf8(stored), std::str::from_utf8(given)) {
        (Ok(a), Ok(b)) => a
            .chars()
            .flat_map(char::to_lowercase)
            .eq(b.chars().flat_map(char::to_lowercase)),
        _ => stored.eq_ignore_ascii_case(given),
    }
}

/// The volumes seen so far, by device, and whether each is case-sensitive.
static CASE_SENSITIVE: Mutex<Vec<(libc::dev_t, bool)>> = Mutex::new(Vec::new());

/// How many devices [`CASE_SENSITIVE`] remembers. When it is full it
/// starts again, so volumes mounted and unmounted all day can't grow it.
const CASE_SENSITIVE_CAP: usize = 64;

/// Whether the volume of device `dev` is case-sensitive, asked of the
/// folder `dir` on it (`pathconf` with `_PC_CASE_SENSITIVE`) the first time
/// the device is seen. A volume that doesn't answer counts as
/// case-insensitive: its names are read, which costs a call but is never
/// wrong, since a stored name is only taken when it matches the path's
/// ignoring case.
fn case_sensitive(dev: libc::dev_t, dir: &CStr) -> bool {
    if let Ok(seen) = CASE_SENSITIVE.lock() {
        if let Some(&(_, sensitive)) = seen.iter().find(|(d, _)| *d == dev) {
            return sensitive;
        }
    }
    // SAFETY: `dir` is a valid NUL-terminated string.
    let answer = unsafe { libc::pathconf(dir.as_ptr(), libc::_PC_CASE_SENSITIVE) };
    let sensitive = answer == 1;
    if let Ok(mut seen) = CASE_SENSITIVE.lock() {
        if seen.len() >= CASE_SENSITIVE_CAP {
            seen.clear();
        }
        seen.push((dev, sensitive));
    }
    sensitive
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
        let c = c_path(&path)?;
        let Some(st) = lstat(&c)? else {
            return Ok(None);
        };

        // The parent folder when the entry is on its device: only then is
        // the stored name read ("Names" in the module docs).
        let (entry_key, parent_key, same_device_parent) = match path.parent() {
            None => (key(st.st_dev, st.st_ino), None, None),
            Some(parent) => {
                let c_parent = c_path(parent)?;
                let Some(pst) = lstat(&c_parent)? else {
                    return Ok(None);
                };
                let parent_key = match parent.parent() {
                    None => key(pst.st_dev, pst.st_ino),
                    Some(grand) => match lstat(&c_path(grand)?)? {
                        Some(gst) => listed_key(parent, &pst, gst.st_dev),
                        None => return Ok(None),
                    },
                };
                (
                    listed_key(&path, &st, pst.st_dev),
                    Some(parent_key),
                    (pst.st_dev == st.st_dev).then_some(c_parent),
                )
            }
        };

        let kind = match st.st_mode & libc::S_IFMT {
            libc::S_IFDIR => EntryKind::Dir,
            libc::S_IFLNK => EntryKind::Symlink,
            _ => EntryKind::File,
        };
        let given = path.file_name().map_or(b"/" as &[u8], |n| n.as_bytes());
        let mut reply = [0u8; NAME_REPLY_SIZE];
        let stored = match same_device_parent {
            Some(parent) if !case_sensitive(st.st_dev, &parent) => stored_name(&c, &mut reply),
            _ => None,
        };
        let name_bytes = stored
            .filter(|s| same_name_ignoring_case(s, given))
            .unwrap_or(given);
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

#[cfg(test)]
mod tests {
    use super::{name_in_reply, same_name_ignoring_case};

    /// A reply as `getattrlist` writes it for `ATTR_CMN_NAME`: its length,
    /// the `attrreference_t` (offset 8 from the reference, so the name
    /// starts at byte 12), the name and its NUL, padded to 4 bytes.
    fn reply(name: &[u8]) -> Vec<u8> {
        let name_len = u32::try_from(name.len() + 1).unwrap();
        let mut out = vec![0; 4];
        out.extend_from_slice(&8i32.to_ne_bytes());
        out.extend_from_slice(&name_len.to_ne_bytes());
        out.extend_from_slice(name);
        out.push(0);
        while out.len() % 4 != 0 {
            out.push(0);
        }
        let len = u32::try_from(out.len()).unwrap();
        out[..4].copy_from_slice(&len.to_ne_bytes());
        out
    }

    #[test]
    fn reads_the_name_in_a_reply() {
        assert_eq!(
            name_in_reply(&reply(b"REPORT.TXT")),
            Some(&b"REPORT.TXT"[..])
        );
        // In a larger buffer, as the kernel leaves it.
        let mut padded = reply("\u{c9}T\u{c9}".as_bytes());
        padded.resize(1024, 0xff);
        assert_eq!(name_in_reply(&padded), Some("\u{c9}T\u{c9}".as_bytes()));
    }

    #[test]
    fn refuses_a_truncated_or_malformed_reply() {
        let good = reply(b"name.txt");
        // Shorter than its length says: truncated.
        assert_eq!(name_in_reply(&good[..good.len() - 4]), None);
        assert_eq!(name_in_reply(&good[..6]), None);
        assert_eq!(name_in_reply(&[]), None);
        // The name runs past the reply.
        let mut long = good.clone();
        long[8..12].copy_from_slice(&200u32.to_ne_bytes());
        assert_eq!(name_in_reply(&long), None);
        // A negative offset.
        let mut negative = good;
        negative[4..8].copy_from_slice(&(-8i32).to_ne_bytes());
        assert_eq!(name_in_reply(&negative), None);
        // An empty name.
        assert_eq!(name_in_reply(&reply(b"")), None);
    }

    #[test]
    fn names_match_ignoring_letter_case_only() {
        assert!(same_name_ignoring_case(b"REPORT.TXT", b"report.txt"));
        assert!(same_name_ignoring_case(b"Docs", b"Docs"));
        assert!(same_name_ignoring_case(
            "\u{c9}T\u{c9}.TXT".as_bytes(),
            "\u{e9}t\u{e9}.txt".as_bytes()
        ));
        // A volume's name at its mount point, another hard link's name.
        assert!(!same_name_ignoring_case(b"Macintosh HD - Data", b"Data"));
        assert!(!same_name_ignoring_case(b"first.txt", b"second.txt"));
        // Another normalization form (NFD against NFC) is not compared.
        assert!(!same_name_ignoring_case(
            "e\u{301}.txt".as_bytes(),
            "\u{e9}.txt".as_bytes()
        ));
        // Names that aren't UTF-8 compare in ASCII case.
        assert!(same_name_ignoring_case(b"A\xff", b"a\xff"));
        assert!(!same_name_ignoring_case(b"A\xff", b"b\xff"));
    }
}
