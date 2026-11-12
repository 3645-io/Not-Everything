//! Volume and mount table.
//!
//! [`list_mounts`] (macOS only) reads the mounted file systems with
//! `getfsstat(MNT_NOWAIT)` and returns one [`MountInfo`] per mount.
//! [`mount_policy`] decides, purely from a [`MountInfo`], whether the
//! scanner indexes a mount or only lists it:
//!
//! * The local boot volume at `/` is indexed.
//! * With [`ExternalVolumes`] given ([`mount_policy_with`]; run 2.4),
//!   external volumes are indexed too: a local, non-network APFS, HFS+,
//!   FAT or exFAT volume mounted directly in the external volumes folder
//!   (`/Volumes`) that the Finder shows (`MNT_DONTBROWSE` clear, so Time
//!   Machine snapshots, the Recovery volume and `-nobrowse` disk images are
//!   left out unless [`ExternalVolumes::include_hidden`] is set).
//! * Every other mount point, including the Data volume at
//!   `/System/Volumes/Data` (reached through firmlinks from `/`), the other
//!   system volumes, `devfs` and `autofs`, is listed but not entered.
//! * Network mounts (`MNT_LOCAL` clear, or `smbfs`, `nfs`, `afpfs`,
//!   `webdav`) are never entered (Phase 6).
//!
//! Everything except [`list_mounts`] is pure and compiles on every host.

use std::path::{Path, PathBuf};

/// `MNT_RDONLY` (`sys/mount.h`).
const MNT_RDONLY: u32 = 0x0000_0001;
/// `MNT_LOCAL` (`sys/mount.h`).
const MNT_LOCAL: u32 = 0x0000_1000;
/// `MNT_DONTBROWSE` (`sys/mount.h`): the Finder does not show the mount.
const MNT_DONTBROWSE: u32 = 0x0010_0000;

/// The folder macOS mounts external volumes in.
pub const EXTERNAL_VOLUMES_FOLDER: &str = "/Volumes";

/// File-system types indexed on external volumes: APFS, HFS+, FAT
/// (`msdos`) and exFAT.
pub const EXTERNAL_FS_TYPES: &[&str] = &["apfs", "hfs", "msdos", "exfat"];

/// File-system types that are always network mounts.
const NETWORK_FS_TYPES: &[&str] = &["smbfs", "nfs", "afpfs", "webdav"];

/// The classification of a mount's `statfs.f_flags` word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountFlags {
    /// `MNT_LOCAL` is set.
    pub local: bool,
    /// `MNT_RDONLY` is set.
    pub readonly: bool,
    /// `MNT_LOCAL` is clear, or the type is a network file system.
    pub network: bool,
    /// `MNT_DONTBROWSE` is clear: the Finder shows the mount.
    pub browsable: bool,
}

impl MountFlags {
    /// Classifies a raw `statfs.f_flags` word for a mount of type
    /// `fs_type` (`f_fstypename`).
    #[must_use]
    pub fn from_raw(raw: u32, fs_type: &str) -> MountFlags {
        let local = raw & MNT_LOCAL != 0;
        MountFlags {
            local,
            readonly: raw & MNT_RDONLY != 0,
            network: !local || NETWORK_FS_TYPES.contains(&fs_type),
            browsable: raw & MNT_DONTBROWSE == 0,
        }
    }
}

/// One mounted file system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountInfo {
    /// Where it is mounted (`f_mntonname`).
    pub mount_point: PathBuf,
    /// File-system type (`f_fstypename`), e.g. `apfs`.
    pub fs_type: String,
    /// Mounted device or source (`f_mntfromname`), e.g. `/dev/disk3s1s1`.
    pub device: String,
    /// Classified mount flags.
    pub flags: MountFlags,
    /// True for the mount at `/`.
    pub is_root: bool,
}

/// What the scanner does with a mount.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountPolicy {
    /// Scan and index it.
    Index,
    /// Show the mount point as an entry but never enter it.
    ListOnly,
}

/// Which external volumes are indexed (run 2.4): those mounted directly
/// in [`folder`](Self::folder), of a type in [`EXTERNAL_FS_TYPES`], local
/// and not network, and shown by the Finder unless
/// [`include_hidden`](Self::include_hidden) is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalVolumes {
    /// The folder external volumes are mounted in, canonical (no symlinks):
    /// [`EXTERNAL_VOLUMES_FOLDER`] for the app. Only mounts directly in it
    /// are indexed.
    pub folder: PathBuf,
    /// Index mounts the Finder hides (`MNT_DONTBROWSE`) too. Off for the
    /// app, where those are Time Machine snapshots, the Recovery volume and
    /// disk images attached with `-nobrowse`; tests turn it on to index
    /// the disk images they attach that way.
    pub include_hidden: bool,
}

impl ExternalVolumes {
    /// The app's setting: browsable volumes in `/Volumes`.
    #[must_use]
    pub fn system() -> ExternalVolumes {
        ExternalVolumes {
            folder: PathBuf::from(EXTERNAL_VOLUMES_FOLDER),
            include_hidden: false,
        }
    }

    /// Whether `mount` is an external volume this setting indexes. Never
    /// true for the root mount.
    #[must_use]
    pub fn indexes(&self, mount: &MountInfo) -> bool {
        !mount.is_root
            && mount.flags.local
            && !mount.flags.network
            && (mount.flags.browsable || self.include_hidden)
            && EXTERNAL_FS_TYPES.contains(&mount.fs_type.as_str())
            && mount.mount_point.parent() == Some(self.folder.as_path())
            && mount.mount_point.file_name().is_some()
    }
}

/// The policy without external volumes: only the local, non-network root
/// `/` is indexed; every other mount is [`MountPolicy::ListOnly`]. Pure.
#[must_use]
pub fn mount_policy(mount: &MountInfo) -> MountPolicy {
    mount_policy_with(mount, None)
}

/// The policy: the local, non-network root `/` is indexed, and so is every
/// external volume `external` indexes ([`ExternalVolumes::indexes`]);
/// every other mount is [`MountPolicy::ListOnly`]. Pure.
#[must_use]
pub fn mount_policy_with(mount: &MountInfo, external: Option<&ExternalVolumes>) -> MountPolicy {
    let is_root = mount.is_root && mount.mount_point == Path::new("/");
    let local_root = is_root && mount.flags.local && !mount.flags.network;
    if local_root || external.is_some_and(|e| e.indexes(mount)) {
        MountPolicy::Index
    } else {
        MountPolicy::ListOnly
    }
}

#[cfg(target_os = "macos")]
pub use macos::list_mounts;

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::{CStr, OsStr, c_char, c_int};
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};

    use super::{MountFlags, MountInfo};

    /// Lists the mounted file systems with `getfsstat(MNT_NOWAIT)`.
    ///
    /// # Errors
    ///
    /// The `getfsstat` error.
    pub fn list_mounts() -> io::Result<Vec<MountInfo>> {
        let entry_size = c_int::try_from(std::mem::size_of::<libc::statfs>())
            .map_err(|_| io::Error::other("statfs too large"))?;
        // SAFETY: a null buffer asks only for the number of mounts.
        let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
        if count < 0 {
            return Err(io::Error::last_os_error());
        }
        // Mounts can appear between the two calls: leave room and retry
        // while the buffer comes back full.
        let mut capacity = usize::try_from(count).unwrap_or(0) + 8;
        loop {
            // SAFETY: `statfs` is plain old data; all-zero is a valid value.
            let mut buf: Vec<libc::statfs> = vec![unsafe { std::mem::zeroed() }; capacity];
            let bytes = c_int::try_from(capacity)
                .ok()
                .and_then(|n| n.checked_mul(entry_size))
                .ok_or_else(|| io::Error::other("too many mounts"))?;
            // SAFETY: `buf` holds `capacity` entries, `bytes` bytes in all.
            let got = unsafe { libc::getfsstat(buf.as_mut_ptr(), bytes, libc::MNT_NOWAIT) };
            if got < 0 {
                return Err(io::Error::last_os_error());
            }
            let got = usize::try_from(got).unwrap_or(0);
            if got >= capacity {
                capacity *= 2;
                continue;
            }
            buf.truncate(got);
            return Ok(buf.iter().map(to_mount_info).collect());
        }
    }

    fn to_mount_info(st: &libc::statfs) -> MountInfo {
        let mount_point = PathBuf::from(OsStr::from_bytes(c_field(&st.f_mntonname)));
        let fs_type = String::from_utf8_lossy(c_field(&st.f_fstypename)).into_owned();
        let device = String::from_utf8_lossy(c_field(&st.f_mntfromname)).into_owned();
        let flags = MountFlags::from_raw(st.f_flags, &fs_type);
        let is_root = mount_point == Path::new("/");
        MountInfo {
            mount_point,
            fs_type,
            device,
            flags,
            is_root,
        }
    }

    /// The bytes of a fixed-size, NUL-terminated `char` array (the whole
    /// array if it has no NUL).
    fn c_field(field: &[c_char]) -> &[u8] {
        // SAFETY: `c_char` and `u8` have the same size and alignment.
        let bytes: &[u8] =
            unsafe { std::slice::from_raw_parts(field.as_ptr().cast::<u8>(), field.len()) };
        CStr::from_bytes_until_nul(bytes).map_or(bytes, CStr::to_bytes)
    }
}
