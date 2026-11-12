//! Parallel, read-only `getattrlistbulk(2)` directory walker (macOS only).
//!
//! [`scan`] walks everything strictly below a root directory. The root
//! itself is not an entry: entries directly in the root have
//! `parent == u32::MAX`.
//!
//! How it works:
//!
//! * N worker threads share a LIFO queue of directories to scan (a
//!   `Mutex` + `Condvar` with an in-flight counter for termination). With
//!   `threads == 1` the walk runs on the calling thread with a plain local
//!   stack and no locking.
//! * Each directory is opened with `O_RDONLY | O_DIRECTORY | O_NOFOLLOW |
//!   O_CLOEXEC` by its full path (falling back to an `openat` chain from
//!   the root when the path is longer than `PATH_MAX`), then read with
//!   `getattrlistbulk` into a reusable per-worker buffer that is decoded by
//!   [`crate::attrbuf`]. After opening, the descriptor is `fstat`ed and its
//!   inode compared with the file ID the parent's listing reported; a
//!   directory replaced in between is skipped and counted as an error.
//! * Exclusions ([`ScanOptions::exclusions`]) are applied to every entry
//!   *before* the walker descends: an excluded directory is neither emitted
//!   nor opened ([`fx_fs::Verdict::SkipSubtree`]), an excluded file is not
//!   emitted ([`fx_fs::Verdict::SkipEntry`]), and a traverse-only ancestor of
//!   an include-only root is opened but not emitted. The engine gets the
//!   entry's absolute path (scan root joined with the relative path) and two
//!   platform flags, defined here as:
//!   - **hidden**: the name starts with `.` or `UF_HIDDEN` is set;
//!   - **system**: `SF_RESTRICTED` is set (System Integrity Protection) or
//!     the path is `/System` or lies under it (which includes
//!     `/System/Volumes/Data`, never entered anyway).
//! * The walker descends only when [`descend_decision`] says
//!   [`Descend::Descend`]: symlinks are never followed, mount points and
//!   automount triggers below the root are listed but not entered, and
//!   dataless (cloud-only, `SF_DATALESS`) directories are never entered.
//!   Mount points are recognized by `ATTR_DIR_MOUNTSTATUS` and, because
//!   APFS's `getattrlistbulk` reports mount status 0 for some of them
//!   (`/System/Volumes/Data` among them), also by the kernel's mount table
//!   ([`crate::volumes::list_mounts`]) read at scan start. A mount below the
//!   root is entered only if [`crate::volumes::mount_policy_with`] says
//!   [`MountPolicy::Index`] for [`ScanOptions::external_volumes`]: without
//!   that setting (the default) only the scan root's own volume is walked;
//!   with it, the external volumes it accepts are walked too (run 2.4). A
//!   mount point's entry, entered or not, is keyed by the mounted volume's
//!   root as `lstat` reports it, except a network or automount one, which
//!   is never looked into (see [`ScannedEntry::dev`]).
//! * Firmlinks: `/System/Volumes/Data` and everything below it
//!   ([`crate::firmlinks::is_data_volume_alias`]) is listed but never
//!   entered from a root above it, so a scan of `/` reaches the Data volume
//!   only through the firmlinked directories (such as `/Users`) and yields
//!   each file once. Such skips are counted in
//!   [`ScanStats::skipped_firmlink_aliases`].
//! * At scan start the process's dataless-file materialization policy is
//!   switched off ([`crate::cloud::disable_dataless_materialization`]) and
//!   read back into [`ScanStats::materialization_disabled`], so the scan
//!   can never trigger a cloud download. The policy stays off for the rest
//!   of the process.
//! * Per-directory failures are counted in [`ScanStats::errors`] and the
//!   walk continues; they are never retried. `EDEADLK` (dataless) and
//!   `EACCES`/`EPERM` (no permission) are also counted in their own fields.
//!   Only a failure to open or read the root itself returns `Err`.
//!
//! The walker only opens directories for reading; it never writes, deletes,
//! renames or moves anything.

use std::collections::HashSet;
use std::ffi::{OsStr, c_int, c_void};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use fx_fs::{ExclusionConfig, Exclusions};

use crate::attrbuf::{
    ATTR_BIT_MAP_COUNT, Descend, Entries, EntryKind, REQUESTED_DIR_ATTRS, REQUESTED_FILE_ATTRS,
    REQUESTED_OPTIONS, RawEntry, SkipReason, WALKER_COMMON_ATTRS, descend_decision,
};
use crate::budget;
use crate::cloud::{OpenSkip, classify_open_error};
use crate::firmlinks::{DATA_VOLUME, FirmlinkMap};
use crate::volumes::{ExternalVolumes, MountPolicy, list_mounts, mount_policy_with};

/// `UF_HIDDEN` (`sys/stat.h`): hidden from the Finder.
pub const UF_HIDDEN: u32 = 0x0000_8000;
/// `SF_RESTRICTED` (`sys/stat.h`): protected by System Integrity Protection.
pub const SF_RESTRICTED: u32 = 0x0008_0000;

/// The walker's hidden rule: the name starts with `.` or `UF_HIDDEN` is set.
#[must_use]
pub fn is_hidden(name: &[u8], flags: u32) -> bool {
    name.first() == Some(&b'.') || flags & UF_HIDDEN != 0
}

/// The walker's system rule: `SF_RESTRICTED` is set, or the absolute path
/// is `/System` or lies under it.
#[must_use]
pub fn is_system(abs_path: &[u8], flags: u32) -> bool {
    flags & SF_RESTRICTED != 0 || abs_path == b"/System" || abs_path.starts_with(b"/System/")
}

// ------------------------------------------------------------------- FFI

/// `struct attrlist` from `sys/attr.h`.
#[repr(C)]
struct AttrList {
    bitmapcount: u16,
    reserved: u16,
    commonattr: u32,
    volattr: u32,
    dirattr: u32,
    fileattr: u32,
    forkattr: u32,
}

unsafe extern "C" {
    fn getattrlistbulk(
        dirfd: c_int,
        attr_list: *mut c_void,
        attr_buf: *mut c_void,
        attr_buf_size: usize,
        options: u64,
    ) -> c_int;
}

/// Flags every directory is opened with.
const OPEN_FLAGS: c_int = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

/// Smallest buffer accepted: one record with a maximal (255 UTF-8
/// characters) name fits.
const MIN_BUF_SIZE: usize = 1024;

/// Default per-worker `getattrlistbulk` buffer size.
pub const DEFAULT_BUF_SIZE: usize = 256 * 1024;

/// Buffer window for the first `getattrlistbulk` call on each directory.
const FIRST_CALL_BUF_SIZE: usize = 16 * 1024;

/// Bytes taken by the first `count` records of `buf` (by their length
/// fields; stops early on a malformed length).
fn used_bytes(buf: &[u8], count: usize) -> usize {
    let mut pos = 0usize;
    for _ in 0..count {
        let Some(field) = buf.get(pos..pos + 4) else {
            break;
        };
        let len = u32::from_le_bytes([field[0], field[1], field[2], field[3]]) as usize;
        if len == 0 {
            break;
        }
        pos = pos.saturating_add(len);
    }
    pos.min(buf.len())
}

/// Marker for "no parent" / "no worker".
const NONE: u32 = u32::MAX;

/// An open directory file descriptor, closed on drop.
struct Fd(c_int);

impl Drop for Fd {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a descriptor this walker opened and owns.
        unsafe {
            libc::close(self.0);
        }
    }
}

/// Opens `path` (NUL-terminated) relative to `dirfd` (or `AT_FDCWD`).
fn open_at(dirfd: c_int, path: &[u8]) -> io::Result<Fd> {
    debug_assert_eq!(path.last(), Some(&0));
    loop {
        // SAFETY: `path` is NUL-terminated and outlives the call.
        let fd = unsafe { libc::openat(dirfd, path.as_ptr().cast(), OPEN_FLAGS) };
        if fd >= 0 {
            return Ok(Fd(fd));
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EINTR) {
            return Err(err);
        }
    }
}

/// `(st_dev, st_ino)` of `path` (absolute, without a NUL) as `lstat`
/// reports them; `None` if it fails. The device is converted the way the
/// keys are (`dev_t as u32`).
#[allow(clippy::cast_sign_loss)]
fn lstat_identity(path: &[u8]) -> Option<(u32, u64)> {
    let mut c = Vec::with_capacity(path.len() + 1);
    c.extend_from_slice(path);
    c.push(0);
    // SAFETY: `stat` is plain old data; all-zero is a valid value.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is NUL-terminated and `st` is valid for writes.
    if unsafe { libc::lstat(c.as_ptr().cast(), &mut st) } != 0 {
        return None;
    }
    Some((st.st_dev as u32, st.st_ino))
}

/// Device and inode number of an open descriptor (`fstat`).
fn fd_identity(fd: &Fd) -> io::Result<(libc::dev_t, u64)> {
    // SAFETY: `stat` is plain old data; all-zero is a valid value.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `fd.0` is an open descriptor and `st` is valid for writes.
    if unsafe { libc::fstat(fd.0, &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((st.st_dev, st.st_ino))
}

// ----------------------------------------------------------------- types

/// Options for [`scan`].
#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// Number of worker threads. `1` walks on the calling thread; `0` means
    /// the available parallelism, capped at 8.
    pub threads: usize,
    /// Whether to return the scanned entries (otherwise only stats).
    pub collect: bool,
    /// Per-worker `getattrlistbulk` buffer size in bytes (at least 1 KiB is
    /// used).
    pub buf_size: usize,
    /// What to leave out; applied before descending (see the module docs).
    /// The default excludes nothing.
    pub exclusions: Exclusions,
    /// External volumes to enter (run 2.4): a mount below the root that
    /// [`ExternalVolumes::indexes`] accepts is walked like any folder.
    /// `None` (the default) enters no mount below the root.
    pub external_volumes: Option<ExternalVolumes>,
    /// Heap, in bytes, that the entries a streaming scan has listed and its
    /// receiver hasn't taken yet may hold ([`crate::budget`]). Used by
    /// [`MacScanner`](crate::enumerator::MacScanner).
    pub stream_budget: usize,
}

/// Default [`ScanOptions::stream_budget`]: 1 MiB, about nine chunks of
/// entries. Measured on an M4 Pro over a 3.2M-entry disk (`rescan_bench`,
/// `first_run_bench`): a rescan's walk peaked at 37 MB of heap with any
/// budget from 256 KiB to 4 MiB, against 76 to 84 MB with the 256-message
/// channel before it, and neither the rescan nor the first scan got
/// slower. A larger budget only holds more: on a tree of small folders,
/// 8 MiB held more than the 256 messages did.
pub const DEFAULT_STREAM_BUDGET: usize = 1 << 20;

/// The most entries a streaming scan sends in one message: a big folder's
/// entries go in several, so no message (and no walker's pending list)
/// outgrows the budget.
pub const STREAM_CHUNK: usize = 1024;

/// An exclusion engine that excludes nothing.
fn no_exclusions() -> Exclusions {
    Exclusions::new(ExclusionConfig::default())
        .expect("the default exclusion config is always valid")
}

impl Default for ScanOptions {
    fn default() -> Self {
        ScanOptions {
            threads: 0,
            collect: true,
            buf_size: DEFAULT_BUF_SIZE,
            exclusions: no_exclusions(),
            external_volumes: None,
            stream_budget: DEFAULT_STREAM_BUDGET,
        }
    }
}

impl ScanOptions {
    fn effective_threads(&self) -> usize {
        if self.threads > 0 {
            self.threads
        } else {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
                .min(8)
        }
    }
}

/// Counters for one scan. Everything except `elapsed` is independent of the
/// thread count.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanStats {
    /// Entries found below the root (the root itself is not counted).
    pub entries: u64,
    /// Entries that are directories.
    pub dirs: u64,
    /// Entries that are regular files.
    pub files: u64,
    /// Entries that are symbolic links.
    pub symlinks: u64,
    /// Directories that could not be opened or read, plus entries that came
    /// back with a per-entry error or could not be decoded.
    pub errors: u64,
    /// Mount points below the root that were listed but not entered.
    pub skipped_mount_points: u64,
    /// Automount triggers below the root that were listed but not entered.
    pub skipped_automount_triggers: u64,
    /// Dataless (cloud-only) directories that were listed but not entered.
    pub skipped_dataless: u64,
    /// Firmlink aliases (`/System/Volumes/Data` from a root above it) that
    /// were listed but not entered and are not in the mount table (those
    /// count in `skipped_mount_points`).
    pub skipped_firmlink_aliases: u64,
    /// Entries left out by [`ScanOptions::exclusions`] (an excluded
    /// directory counts once; its subtree is never listed).
    pub excluded: u64,
    /// Directories whose open or listing failed with `EDEADLK` (dataless);
    /// skipped, never retried. Also counted in `errors`.
    pub skipped_deadlk: u64,
    /// Directories whose open or listing failed with `EACCES` or `EPERM`;
    /// skipped, never retried. Also counted in `errors`.
    pub skipped_permission_denied: u64,
    /// Whether `getiopolicy_np` read dataless materialization back as off
    /// after the scan switched it off.
    pub materialization_disabled: bool,
    /// Total length of all entry names in bytes.
    pub name_bytes: u64,
    /// Wall-clock time of the scan.
    pub elapsed: Duration,
}

impl ScanStats {
    fn add(&mut self, other: &ScanStats) {
        self.entries += other.entries;
        self.dirs += other.dirs;
        self.files += other.files;
        self.symlinks += other.symlinks;
        self.errors += other.errors;
        self.skipped_mount_points += other.skipped_mount_points;
        self.skipped_automount_triggers += other.skipped_automount_triggers;
        self.skipped_dataless += other.skipped_dataless;
        self.skipped_firmlink_aliases += other.skipped_firmlink_aliases;
        self.excluded += other.excluded;
        self.skipped_deadlk += other.skipped_deadlk;
        self.skipped_permission_denied += other.skipped_permission_denied;
        self.name_bytes += other.name_bytes;
    }
}

/// One collected entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedEntry {
    /// The entry's name (one path component).
    pub name: Box<OsStr>,
    /// Object type.
    pub kind: EntryKind,
    /// Data fork length (meaningful for files; 0 when not returned).
    pub size: u64,
    /// Modification time, seconds since the Unix epoch.
    pub mtime_sec: i64,
    /// Modification time, nanoseconds part.
    pub mtime_nsec: u32,
    /// File ID (inode number, `st_ino`). For a mount point, the mounted
    /// volume root's (see `dev`).
    pub file_id: u64,
    /// BSD flags (`st_flags`).
    pub flags: u32,
    /// Creation (birth) time, seconds since the Unix epoch.
    pub crtime_sec: i64,
    /// Device (`st_dev`) of the directory the entry was listed in. With
    /// `file_id` this forms the entry's key; for a firmlinked directory
    /// seen from `/` (such as `/Users`) it is the system volume's device,
    /// not the one `lstat` reports (see the `enumerator` module docs). For
    /// a mount point it is the mounted volume's device: a mount point is
    /// keyed by the volume's root as `lstat` reports it (run 2.4).
    pub dev: u32,
    /// The walker's hidden rule ([`is_hidden`]).
    pub hidden: bool,
    /// The walker's system rule ([`is_system`]).
    pub system: bool,
    /// Index of the parent directory in the collected entries, or
    /// `u32::MAX` for entries directly in the scan root (and for entries
    /// whose parent was not emitted: a traverse-only ancestor of an
    /// include-only root).
    pub parent: u32,
    /// `(dev, file_id)` of the parent directory as it was (or, for the root
    /// and traverse-only directories, would have been) emitted. For entries
    /// directly in the root it is the root's `fstat` identity.
    pub parent_key: (u32, u64),
}

/// The result of [`scan`].
#[derive(Debug, Clone, Default)]
pub struct ScanResult {
    /// Counters.
    pub stats: ScanStats,
    /// The entries, when [`ScanOptions::collect`] was set. Parents may come
    /// after their children; the order is unspecified.
    pub entries: Option<Vec<ScannedEntry>>,
}

/// Whether a directory opened by path is the one its parent's listing
/// reported, given the file ID from the listing (`listed_file_id`, 0 when
/// not known), the device of the parent directory, the `(dev, ino)` of the
/// opened directory, and whether its path is a firmlink (such as `/Users`
/// under `/`, from [`FirmlinkMap`]).
///
/// The IDs can only be compared on one volume. A firmlink's listing
/// reports the firmlink's own ID while opening it gives the Data volume
/// directory's inode, and both volumes may report the same `st_dev`, so a
/// firmlink always matches. Mount points are never queued, so another
/// device means a volume crossing the walker meant to make.
pub fn reopened_dir_matches(
    listed_file_id: u64,
    parent_dev: libc::dev_t,
    dev: libc::dev_t,
    ino: u64,
    firmlink: bool,
) -> bool {
    listed_file_id == 0 || firmlink || dev != parent_dev || ino == listed_file_id
}

/// Rebuilds entry `index`'s path relative to the scan root by following the
/// parent links.
///
/// # Panics
///
/// Panics if `index` or a parent link is out of range.
pub fn relative_path(entries: &[ScannedEntry], index: usize) -> PathBuf {
    let mut chain = Vec::new();
    let mut cur = index;
    loop {
        let e = &entries[cur];
        chain.push(&*e.name);
        if e.parent == NONE {
            break;
        }
        cur = e.parent as usize;
        if chain.len() > entries.len() {
            panic!("parent cycle at entry {index}");
        }
    }
    chain.iter().rev().collect()
}

// ---------------------------------------------------------------- walker

/// A directory waiting to be scanned.
struct WorkItem {
    /// Path relative to the root, without a leading slash (empty = root).
    rel: Vec<u8>,
    /// Worker that collected this directory's own entry (`NONE` for root or
    /// when not collecting).
    worker: u32,
    /// The directory's index in that worker's local entries.
    index: u32,
    /// File ID the parent's listing reported for this directory (0 = not
    /// known, for the root). Checked against `st_ino` after opening.
    file_id: u64,
    /// Device of the parent directory (unused for the root).
    parent_dev: libc::dev_t,
    /// The directory's own key as emitted, `(dev, file_id)` (unused for the
    /// root, whose key comes from `fstat`).
    key: (u32, u64),
}

/// Read-only context shared by all workers of one scan.
struct Ctx<'a> {
    root: &'a [u8],
    /// List-only mount points strictly below the root, as root-relative
    /// paths.
    mounts: HashSet<Vec<u8>>,
    /// Mount points strictly below the root that are entered (external
    /// volumes, [`ScanOptions::external_volumes`]), as root-relative paths.
    entered: HashSet<Vec<u8>>,
    /// Network and automount mount points strictly below the root, keyed
    /// by the listing, never `lstat`ed.
    remote: HashSet<Vec<u8>>,
    /// `/System/Volumes/Data` relative to the canonical root, when the root
    /// is a strict ancestor of it.
    alias_rel: Option<Vec<u8>>,
    /// Firmlinks strictly below the root (such as `/Users` under `/`), as
    /// root-relative paths.
    firmlinks: HashSet<Vec<u8>>,
    exclusions: &'a Exclusions,
    /// Where a streaming scan ([`scan_streaming`]) sends each directory's
    /// entries; `None` for a collecting scan.
    emit: Option<&'a budget::Sender<Vec<ScannedEntry>>>,
}

/// Per-thread walker state.
struct Worker<'a> {
    id: u32,
    root: &'a [u8],
    ctx: &'a Ctx<'a>,
    collect: bool,
    /// Absolute path of the directory being scanned, with a trailing `/`.
    dir_abs: Vec<u8>,
    /// Key children of the directory being scanned report as `parent_key`.
    cur_key: (u32, u64),
    buf: Vec<u8>,
    path: Vec<u8>,
    entries: Vec<ScannedEntry>,
    /// For each local entry, the worker holding its parent (`NONE` for the
    /// root); the entry's `parent` field is the index in that worker.
    parent_workers: Vec<u32>,
    stats: ScanStats,
    children: Vec<WorkItem>,
    /// Device of the directory currently being scanned.
    cur_dev: libc::dev_t,
}

impl<'a> Worker<'a> {
    fn new(id: u32, ctx: &'a Ctx<'a>, opts: &ScanOptions) -> Self {
        Worker {
            id,
            root: ctx.root,
            ctx,
            collect: opts.collect,
            dir_abs: Vec::with_capacity(1024),
            cur_key: (0, 0),
            buf: vec![0u8; opts.buf_size.max(MIN_BUF_SIZE)],
            path: Vec::with_capacity(1024),
            entries: Vec::new(),
            parent_workers: Vec::new(),
            stats: ScanStats::default(),
            children: Vec::new(),
            cur_dev: 0,
        }
    }

    /// Opens the directory of `item`.
    fn open_dir(&mut self, item: &WorkItem) -> io::Result<Fd> {
        self.path.clear();
        self.path.extend_from_slice(self.root);
        if !item.rel.is_empty() {
            if self.path.last() != Some(&b'/') {
                self.path.push(b'/');
            }
            self.path.extend_from_slice(&item.rel);
        }
        self.path.push(0);
        match open_at(libc::AT_FDCWD, &self.path) {
            Err(e) if e.raw_os_error() == Some(libc::ENAMETOOLONG) && !item.rel.is_empty() => {
                self.open_by_components(&item.rel)
            }
            r => r,
        }
    }

    /// Opens `rel` below the root one component at a time (for paths longer
    /// than `PATH_MAX`).
    fn open_by_components(&mut self, rel: &[u8]) -> io::Result<Fd> {
        self.path.clear();
        self.path.extend_from_slice(self.root);
        self.path.push(0);
        let mut fd = open_at(libc::AT_FDCWD, &self.path)?;
        for comp in rel.split(|&c| c == b'/') {
            self.path.clear();
            self.path.extend_from_slice(comp);
            self.path.push(0);
            fd = open_at(fd.0, &self.path)?;
        }
        Ok(fd)
    }

    /// Scans one directory, recording its entries and queueing its
    /// subdirectories in `self.children`. Returns `Err` only when the
    /// directory could not be opened or read at all; the caller decides
    /// whether that is fatal (root) or counted (everything else).
    fn scan_dir(&mut self, item: &WorkItem) -> io::Result<()> {
        let fd = self.open_dir(item)?;
        let (dev, ino) = fd_identity(&fd)?;
        // The directory was opened by path after its parent was listed, so
        // it may have been renamed or replaced in between. Make sure it is
        // the object the listing reported.
        let firmlink = self.ctx.firmlinks.contains(&item.rel);
        if !reopened_dir_matches(item.file_id, item.parent_dev, dev, ino, firmlink) {
            return Err(io::Error::other(
                "directory was replaced between listing and opening",
            ));
        }
        self.cur_dev = dev;
        self.cur_key = if item.rel.is_empty() {
            (dev as u32, ino)
        } else {
            item.key
        };
        self.dir_abs.clear();
        self.dir_abs.extend_from_slice(self.root);
        if self.dir_abs.last() != Some(&b'/') {
            self.dir_abs.push(b'/');
        }
        if !item.rel.is_empty() {
            self.dir_abs.extend_from_slice(&item.rel);
            self.dir_abs.push(b'/');
        }
        let mut attrs = AttrList {
            bitmapcount: ATTR_BIT_MAP_COUNT,
            reserved: 0,
            commonattr: WALKER_COMMON_ATTRS,
            volattr: 0,
            dirattr: REQUESTED_DIR_ATTRS,
            fileattr: REQUESTED_FILE_ATTRS,
            forkattr: 0,
        };
        let mut first = true;
        // The kernel allocates (and zeroes) a buffer of the requested size
        // on every call, and most directories are small, so each directory
        // starts with a small window and switches to the full buffer only
        // once a call comes back (nearly) full.
        let mut len = self.buf.len().min(FIRST_CALL_BUF_SIZE);
        loop {
            // SAFETY: `attrs` and `self.buf` are valid for the call and
            // `len` is at most the buffer's real length.
            let n = unsafe {
                getattrlistbulk(
                    fd.0,
                    (&raw mut attrs).cast(),
                    self.buf.as_mut_ptr().cast(),
                    len,
                    REQUESTED_OPTIONS,
                )
            };
            if n < 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                if first {
                    return Err(err);
                }
                // Failed part-way: keep what was read, count the error.
                self.stats.errors += 1;
                return Ok(());
            }
            first = false;
            if n == 0 {
                return Ok(());
            }
            let buf = std::mem::take(&mut self.buf);
            let filled = &buf[..len];
            for raw in Entries::with_crtime(filled, n as usize) {
                match raw {
                    Ok(raw) => self.record(item, &raw),
                    Err(_) => self.stats.errors += 1,
                }
            }
            if len < buf.len() && used_bytes(filled, n as usize) + MIN_BUF_SIZE > len {
                len = buf.len();
            }
            self.buf = buf;
        }
    }

    /// Records one entry of the directory `dir`.
    fn record(&mut self, dir: &WorkItem, raw: &RawEntry<'_>) {
        // Exclusions first: an excluded entry is neither emitted nor
        // descended into.
        let abs_len = self.dir_abs.len();
        self.dir_abs.extend_from_slice(raw.name);
        let hidden = is_hidden(raw.name, raw.flags);
        let system = is_system(&self.dir_abs, raw.flags);
        let verdict = self.ctx.exclusions.check(
            Path::new(OsStr::from_bytes(&self.dir_abs)),
            &String::from_utf8_lossy(raw.name),
            raw.kind == EntryKind::Dir,
            hidden,
            system,
        );
        self.dir_abs.truncate(abs_len);
        if !verdict.emits() && !(verdict.descends() && raw.kind == EntryKind::Dir) {
            self.stats.excluded += 1;
            return;
        }
        let emit = verdict.emits();

        let s = &mut self.stats;
        if !emit {
            // Traverse-only: walked through, not emitted or counted.
            s.excluded += 1;
        } else {
            s.entries += 1;
            s.name_bytes += raw.name.len() as u64;
            match raw.kind {
                EntryKind::Dir => s.dirs += 1,
                EntryKind::File => s.files += 1,
                EntryKind::Symlink => s.symlinks += 1,
                EntryKind::Other => {}
            }
            if raw.error.is_some() {
                s.errors += 1;
            }
        }
        let mut decision = descend_decision(raw, false);
        match decision {
            Descend::Skip(SkipReason::AutomountTrigger) => s.skipped_automount_triggers += 1,
            Descend::Skip(SkipReason::Dataless) => s.skipped_dataless += 1,
            _ => {}
        }

        // The folder's path below the root, for folders the walk enters and
        // for mount points.
        let rel = matches!(
            decision,
            Descend::Descend | Descend::Skip(SkipReason::MountPoint)
        )
        .then(|| {
            let mut rel = Vec::with_capacity(dir.rel.len() + 1 + raw.name.len());
            rel.extend_from_slice(&dir.rel);
            if !rel.is_empty() {
                rel.push(b'/');
            }
            rel.extend_from_slice(raw.name);
            rel
        });
        // getattrlistbulk does not report every mount point (APFS returns
        // mount status 0 for /System/Volumes/Data), so the mount table is
        // checked as well. An external volume the options accept is
        // entered (run 2.4).
        let entered = rel
            .as_ref()
            .is_some_and(|rel| !self.ctx.entered.is_empty() && self.ctx.entered.contains(rel));
        let listed_mount = !entered
            && (decision == Descend::Skip(SkipReason::MountPoint)
                || rel.as_ref().is_some_and(|rel| {
                    !self.ctx.mounts.is_empty() && self.ctx.mounts.contains(rel)
                }));
        if entered {
            decision = Descend::Descend;
        } else if listed_mount {
            self.stats.skipped_mount_points += 1;
            decision = Descend::Skip(SkipReason::MountPoint);
        }

        // Keying rule: (st_dev of the directory being listed, file ID from
        // getattrlistbulk). For a firmlinked directory seen from `/` (such
        // as /Users or /Applications) this pairs the system volume's device
        // with a Data-volume inode, so the key differs from what lstat
        // reports; its children are listed with the Data volume's device
        // and match lstat. The reconcile and FSEvents run (1.8) must use
        // the same rule or remap these keys. See the `enumerator` docs.
        //
        // A mount point is the exception (run 2.4): the listing reports the
        // covered folder's ID, while lstat, and so the path stat, sees the
        // mounted volume's root. It is keyed by that root's own (st_dev,
        // st_ino), which is unique while the volume is mounted (two
        // volumes' roots often share an inode number, never a device), and
        // the volume's entries hang off that key. A network or automount
        // mount point keeps the listing's key: an lstat could wait on a
        // server, or trigger the mount.
        let remote = || {
            rel.as_ref()
                .is_some_and(|rel| !self.ctx.remote.is_empty() && self.ctx.remote.contains(rel))
        };
        let key = if (entered || listed_mount) && !remote() {
            self.dir_abs.extend_from_slice(raw.name);
            let own = lstat_identity(&self.dir_abs);
            self.dir_abs.truncate(abs_len);
            own.unwrap_or((self.cur_dev as u32, raw.file_id))
        } else {
            (self.cur_dev as u32, raw.file_id)
        };
        let (worker, index) = if self.collect && emit {
            let index = self.entries.len() as u32;
            self.entries.push(ScannedEntry {
                name: OsStr::from_bytes(raw.name).into(),
                kind: raw.kind,
                size: raw.size.unwrap_or(0),
                mtime_sec: raw.mtime_sec,
                mtime_nsec: u32::try_from(raw.mtime_nsec).unwrap_or(0),
                file_id: key.1,
                flags: raw.flags,
                crtime_sec: raw.crtime_sec,
                dev: key.0,
                hidden,
                system,
                parent: dir.index,
                parent_key: self.cur_key,
            });
            self.parent_workers.push(dir.worker);
            if self.ctx.emit.is_some() && self.entries.len() >= STREAM_CHUNK {
                // A big folder goes in chunks. Its subfolders are queued
                // only after its last chunk, so the order holds.
                self.flush();
            }
            (self.id, index)
        } else {
            (NONE, NONE)
        };

        if let (Descend::Descend, Some(rel)) = (decision, rel) {
            // Firmlink aliases are never entered from above, even when the
            // mount table lacks them; a skip is counted once, as a mount
            // point if the table has it (counted above).
            if !entered && self.ctx.alias_rel.as_deref() == Some(rel.as_slice()) {
                self.stats.skipped_firmlink_aliases += 1;
            } else {
                self.children.push(WorkItem {
                    rel,
                    worker,
                    index,
                    file_id: raw.file_id,
                    parent_dev: self.cur_dev,
                    key,
                });
            }
        }
    }

    /// Scans a non-root directory, counting a failure as an error. Failures
    /// are never retried; `EDEADLK` (dataless) and `EACCES`/`EPERM` are
    /// also counted in their own fields.
    fn scan_counted(&mut self, item: &WorkItem) {
        if let Err(err) = self.scan_dir(item) {
            self.stats.errors += 1;
            match err.raw_os_error().and_then(classify_open_error) {
                Some(OpenSkip::Dataless) => self.stats.skipped_deadlk += 1,
                Some(OpenSkip::PermissionDenied) => self.stats.skipped_permission_denied += 1,
                Some(OpenSkip::Vanished) | None => {}
            }
        }
    }

    /// In a streaming scan, sends the entries recorded since the last call
    /// (one directory's, or a chunk of a big one's), first waiting while
    /// the receiver is a budget's worth behind. Called after a directory is
    /// scanned and before its subdirectories are queued, so every
    /// directory's own entry is sent before any of its children can be
    /// listed: the receiver gets each entry after its parent, whichever
    /// worker lists what.
    fn flush(&mut self) {
        let Some(tx) = self.ctx.emit else {
            return;
        };
        if self.entries.is_empty() {
            return;
        }
        let batch = std::mem::take(&mut self.entries);
        self.parent_workers.clear();
        let bytes = batch_bytes(&batch);
        // A receiver that hung up gets nothing more; the walk still ends
        // normally with its counters.
        let _ = tx.send(batch, bytes);
    }

    /// Single-threaded walk of everything queued in `self.children`.
    fn run_local(&mut self) {
        let mut stack = std::mem::take(&mut self.children);
        while let Some(item) = stack.pop() {
            self.scan_counted(&item);
            self.flush();
            stack.append(&mut self.children);
        }
    }

    /// Multi-threaded walk: takes work from `shared` until the queue is
    /// empty and no directory is in flight.
    fn run_shared(&mut self, shared: &Shared) {
        let mut finished = false;
        while let Some(item) = shared.exchange(finished, &mut self.children) {
            self.scan_counted(&item);
            self.flush();
            finished = true;
        }
    }
}

/// The shared work queue.
struct Shared {
    state: Mutex<QueueState>,
    cv: Condvar,
    threads: usize,
}

struct QueueState {
    items: Vec<WorkItem>,
    in_flight: usize,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, QueueState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Publishes `children` (and marks the previous item done when
    /// `finished`), then takes the next item, waiting if needed. Returns
    /// `None` when the walk is complete.
    fn exchange(&self, finished: bool, children: &mut Vec<WorkItem>) -> Option<WorkItem> {
        let mut st = self.lock();
        if finished {
            st.in_flight -= 1;
        }
        let pushed = children.len();
        st.items.append(children);
        // This thread takes one item itself; wake others for the rest.
        for _ in 0..pushed.saturating_sub(1).min(self.threads - 1) {
            self.cv.notify_one();
        }
        loop {
            if let Some(item) = st.items.pop() {
                st.in_flight += 1;
                return Some(item);
            }
            if st.in_flight == 0 {
                drop(st);
                self.cv.notify_all();
                return None;
            }
            st = self.cv.wait(st).unwrap_or_else(|e| e.into_inner());
        }
    }
}

/// Scans everything strictly below `root`. See the module docs.
///
/// Returns `Err` only when `root` itself cannot be opened or read (for
/// example it does not exist, is not a directory, or is a symlink).
pub fn scan(root: &Path, opts: &ScanOptions) -> io::Result<ScanResult> {
    scan_inner(root, opts, None)
}

/// Scans like [`scan`], but sends the entries to `out` as the walk lists
/// them, one directory's entries per message (at most [`STREAM_CHUNK`]),
/// instead of collecting them all: a caller that consumes them as they
/// come never holds every entry at once (run 2.17). Every entry arrives
/// after its parent directory's own entry; entries directly in the root,
/// and below a traverse-only directory, arrive with a parent that is never
/// sent. `parent` indices are not meaningful; `parent_key` is. Returns the
/// counters (`entries` of the result is `None`). `out`'s budget bounds the
/// heap held by entries the receiver hasn't taken yet, so a slow receiver
/// slows the walk instead of letting entries pile up.
pub fn scan_streaming(
    root: &Path,
    opts: &ScanOptions,
    out: &budget::Sender<Vec<ScannedEntry>>,
) -> io::Result<ScanStats> {
    let mut opts = opts.clone();
    opts.collect = true;
    scan_inner(root, &opts, Some(out)).map(|r| r.stats)
}

/// The heap a batch of listed entries holds, as the streaming scan counts
/// it against its budget: the batch's buffer and every name, rounded up to
/// the allocator's 16-byte quantum.
fn batch_bytes(batch: &Vec<ScannedEntry>) -> usize {
    let names: usize = batch
        .iter()
        .map(|e| e.name.len().next_multiple_of(16))
        .sum();
    batch.capacity() * std::mem::size_of::<ScannedEntry>() + names
}

fn scan_inner(
    root: &Path,
    opts: &ScanOptions,
    emit: Option<&budget::Sender<Vec<ScannedEntry>>>,
) -> io::Result<ScanResult> {
    let start = Instant::now();
    let root_bytes = root.as_os_str().as_bytes();
    if root_bytes.is_empty() || root_bytes.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "scan root must be a non-empty path without NUL bytes",
        ));
    }
    // A failure to set the policy is visible in the read-back; the descend
    // policy never enters dataless directories either way.
    let _ = crate::cloud::disable_dataless_materialization();
    let materialization_disabled = crate::cloud::materialization_disabled().unwrap_or(false);

    let threads = opts.effective_threads();
    let canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let sets = mount_sets(&canon, opts.external_volumes.as_ref());
    let ctx = Ctx {
        root: root_bytes,
        mounts: sets.list_only,
        entered: sets.entered,
        remote: sets.remote,
        alias_rel: Path::new(DATA_VOLUME)
            .strip_prefix(&canon)
            .ok()
            .filter(|rel| !rel.as_os_str().is_empty())
            .map(|rel| rel.as_os_str().as_bytes().to_vec()),
        firmlinks: firmlinks_below(&canon),
        exclusions: &opts.exclusions,
        emit,
    };
    let ctx = &ctx;
    let mut main = Worker::new(0, ctx, opts);
    let root_item = WorkItem {
        rel: Vec::new(),
        worker: NONE,
        index: NONE,
        file_id: 0,
        parent_dev: 0,
        key: (0, 0),
    };
    main.scan_dir(&root_item)?;
    main.flush();

    let workers = if threads <= 1 {
        main.run_local();
        vec![main]
    } else {
        let shared = Shared {
            state: Mutex::new(QueueState {
                items: std::mem::take(&mut main.children),
                in_flight: 0,
            }),
            cv: Condvar::new(),
            threads,
        };
        std::thread::scope(|s| {
            let handles: Vec<_> = (1..threads)
                .map(|id| {
                    let shared = &shared;
                    s.spawn(move || {
                        let mut w = Worker::new(id as u32, ctx, opts);
                        w.run_shared(shared);
                        w
                    })
                })
                .collect();
            main.run_shared(&shared);
            let mut all = vec![main];
            for h in handles {
                match h.join() {
                    Ok(w) => all.push(w),
                    Err(panic) => std::panic::resume_unwind(panic),
                }
            }
            all
        })
    };

    let mut result = merge(workers, opts.collect && emit.is_none(), start)?;
    result.stats.materialization_disabled = materialization_disabled;
    Ok(result)
}

/// The mount points strictly below a scan's root, as paths relative to it.
#[derive(Default)]
struct MountSets {
    /// Listed, not entered.
    list_only: HashSet<Vec<u8>>,
    /// Entered: external volumes the options accept.
    entered: HashSet<Vec<u8>>,
    /// Network and automount mounts: never looked into, not even with
    /// `lstat` (a server that does not answer would hang the walk), so
    /// keyed by the listing like any folder.
    remote: HashSet<Vec<u8>>,
}

/// The mount points strictly below the canonical root `canon`, sorted by
/// what [`mount_policy_with`] (with `external`) does with them, from the
/// kernel's mount table ([`list_mounts`], `getfsstat`). Mount paths are
/// canonical, hence the canonical root. If the table cannot be read, none:
/// the `ATTR_DIR_MOUNTSTATUS` check still keeps the walk out of mount
/// points, and no external volume is entered.
fn mount_sets(canon: &Path, external: Option<&ExternalVolumes>) -> MountSets {
    let mut sets = MountSets::default();
    let Ok(mounts) = list_mounts() else {
        return sets;
    };
    for m in &mounts {
        let Ok(rel) = m.mount_point.strip_prefix(canon) else {
            continue;
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let rel = rel.as_os_str().as_bytes().to_vec();
        if m.flags.network || m.fs_type == "autofs" {
            sets.remote.insert(rel.clone());
        }
        match mount_policy_with(m, external) {
            MountPolicy::Index => sets.entered.insert(rel),
            MountPolicy::ListOnly => sets.list_only.insert(rel),
        };
    }
    sets
}

/// Firmlinks of the system table ([`FirmlinkMap::system`]) strictly below
/// the canonical root `canon`, as paths relative to it.
fn firmlinks_below(canon: &Path) -> HashSet<Vec<u8>> {
    FirmlinkMap::system()
        .entries()
        .iter()
        .filter_map(|(on_root, _)| on_root.strip_prefix(canon).ok())
        .filter(|rel| !rel.as_os_str().is_empty())
        .map(|rel| rel.as_os_str().as_bytes().to_vec())
        .collect()
}

/// Combines the per-worker stats and entries, turning (worker, local index)
/// parent links into global indices.
fn merge(workers: Vec<Worker<'_>>, collect: bool, start: Instant) -> io::Result<ScanResult> {
    let mut stats = ScanStats::default();
    for w in &workers {
        stats.add(&w.stats);
    }
    let entries = if collect {
        let total: usize = workers.iter().map(|w| w.entries.len()).sum();
        if total >= NONE as usize {
            return Err(io::Error::other("too many entries for u32 parent links"));
        }
        let mut offsets = Vec::with_capacity(workers.len());
        let mut acc = 0u32;
        for w in &workers {
            offsets.push(acc);
            acc += w.entries.len() as u32;
        }
        let mut out = Vec::with_capacity(total);
        for w in workers {
            for (mut e, pw) in w.entries.into_iter().zip(w.parent_workers) {
                if pw != NONE {
                    e.parent += offsets[pw as usize];
                }
                out.push(e);
            }
        }
        Some(out)
    } else {
        None
    };
    stats.elapsed = start.elapsed();
    Ok(ScanResult { stats, entries })
}
