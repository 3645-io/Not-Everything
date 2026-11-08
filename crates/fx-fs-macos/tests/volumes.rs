//! Tests for `fx_fs_macos::volumes` (mount table).
//!
//! Contract pinned here:
//! - The pure part of the module compiles on every host (CI also runs on
//!   Ubuntu): `MountInfo`, `MountFlags`, `MountPolicy`, `mount_policy` and
//!   `MountFlags::from_raw`. Only `list_mounts` is macOS-only.
//! - `pub struct MountInfo { pub mount_point: PathBuf, pub fs_type: String,
//!   pub device: String, pub flags: MountFlags, pub is_root: bool }` derives
//!   `Debug + Clone + PartialEq + Eq`. `device` is `f_mntfromname` (e.g.
//!   `/dev/disk3s1s1`), `fs_type` is `f_fstypename` (e.g. `apfs`).
//! - `pub struct MountFlags { pub local: bool, pub readonly: bool,
//!   pub network: bool }` derives `Debug + Clone + Copy + PartialEq + Eq`.
//! - `MountFlags::from_raw(raw: u32, fs_type: &str) -> MountFlags` classifies
//!   a `statfs.f_flags` word with the `sys/mount.h` values `MNT_RDONLY =
//!   0x1` and `MNT_LOCAL = 0x1000`: `local` is `MNT_LOCAL` set, `readonly`
//!   is `MNT_RDONLY` set, and `network` is `MNT_LOCAL` clear or the type is
//!   one of `smbfs`, `nfs`, `afpfs`, `webdav`.
//! - `mount_policy(&MountInfo) -> MountPolicy` (`Index | ListOnly`, derives
//!   `Debug + Clone + Copy + PartialEq + Eq`) is pure. In the MVP only the
//!   local root `/` is indexed; every other mount point is `ListOnly`, and a
//!   network mount is never indexed.
//! - `list_mounts() -> std::io::Result<Vec<MountInfo>>` (macOS) reads
//!   `getfsstat(MNT_NOWAIT)`.

use std::path::{Path, PathBuf};

use fx_fs_macos::volumes::{MountFlags, MountInfo, MountPolicy, mount_policy};

const MNT_RDONLY: u32 = 0x0000_0001;
const MNT_LOCAL: u32 = 0x0000_1000;
/// Unrelated bits that must not change the classification
/// (`MNT_NOSUID`, `MNT_JOURNALED`, `MNT_DONTBROWSE`).
const NOISE: u32 = 0x0000_0008 | 0x0080_0000 | 0x0010_0000;

fn mount(point: &str, fs_type: &str, device: &str, raw: u32) -> MountInfo {
    MountInfo {
        mount_point: PathBuf::from(point),
        fs_type: fs_type.to_string(),
        device: device.to_string(),
        flags: MountFlags::from_raw(raw, fs_type),
        is_root: point == "/",
    }
}

// ------------------------------------------------------- flag classification

#[test]
fn local_apfs_volume_is_local_and_not_network() {
    let f = MountFlags::from_raw(MNT_LOCAL | NOISE, "apfs");
    assert_eq!(
        f,
        MountFlags {
            local: true,
            readonly: false,
            network: false
        }
    );
}

#[test]
fn sealed_system_volume_is_local_and_readonly() {
    let f = MountFlags::from_raw(MNT_LOCAL | MNT_RDONLY, "apfs");
    assert!(f.local);
    assert!(f.readonly);
    assert!(!f.network);
}

#[test]
fn missing_mnt_local_means_network() {
    let f = MountFlags::from_raw(NOISE, "apfs");
    assert!(!f.local);
    assert!(f.network);
}

#[test]
fn network_fs_types_are_network_even_if_marked_local() {
    for ty in ["smbfs", "nfs", "afpfs", "webdav"] {
        let f = MountFlags::from_raw(MNT_LOCAL, ty);
        assert!(f.network, "{ty} must be classified as network");
    }
}

#[test]
fn local_non_network_types_are_not_network() {
    for ty in ["apfs", "hfs", "msdos", "exfat", "devfs"] {
        let f = MountFlags::from_raw(MNT_LOCAL, ty);
        assert!(!f.network, "{ty} with MNT_LOCAL must not be network");
        assert!(f.local);
    }
}

// --------------------------------------------------------------- mount policy

#[test]
fn root_is_indexed() {
    let root = mount("/", "apfs", "/dev/disk3s1s1", MNT_LOCAL | MNT_RDONLY);
    assert!(root.is_root);
    assert_eq!(mount_policy(&root), MountPolicy::Index);
}

#[test]
fn data_volume_is_listed_but_not_entered() {
    let data = mount("/System/Volumes/Data", "apfs", "/dev/disk3s5", MNT_LOCAL);
    assert_eq!(mount_policy(&data), MountPolicy::ListOnly);
}

#[test]
fn other_system_volumes_are_list_only() {
    for (point, dev) in [
        ("/System/Volumes/VM", "/dev/disk3s6"),
        ("/System/Volumes/Preboot", "/dev/disk3s2"),
        ("/System/Volumes/Update", "/dev/disk3s4"),
        ("/System/Volumes/xarts", "/dev/disk1s2"),
        ("/System/Volumes/iSCPreboot", "/dev/disk1s1"),
        ("/System/Volumes/Hardware", "/dev/disk1s3"),
    ] {
        let m = mount(point, "apfs", dev, MNT_LOCAL);
        assert_eq!(mount_policy(&m), MountPolicy::ListOnly, "{point}");
    }
}

#[test]
fn devfs_and_autofs_are_list_only() {
    let dev = mount("/dev", "devfs", "devfs", MNT_LOCAL);
    assert_eq!(mount_policy(&dev), MountPolicy::ListOnly);
    let home = mount("/System/Volumes/Data/home", "autofs", "map auto_home", 0);
    assert_eq!(mount_policy(&home), MountPolicy::ListOnly);
}

#[test]
fn external_volumes_wait_for_run_2_4() {
    let usb = mount("/Volumes/USB", "msdos", "/dev/disk5s1", MNT_LOCAL);
    assert_eq!(mount_policy(&usb), MountPolicy::ListOnly);
    let ext = mount("/Volumes/Backup", "apfs", "/dev/disk7s1", MNT_LOCAL);
    assert_eq!(mount_policy(&ext), MountPolicy::ListOnly);
}

#[test]
fn network_mounts_are_never_entered() {
    for ty in ["smbfs", "nfs", "afpfs", "webdav"] {
        let m = mount("/Volumes/share", ty, "//user@server/share", 0);
        assert!(m.flags.network);
        assert_eq!(mount_policy(&m), MountPolicy::ListOnly, "{ty}");
    }
}

#[test]
fn a_network_root_is_not_indexed() {
    // Defensive: even a (nonsensical) network mount at `/` is not entered.
    let m = MountInfo {
        mount_point: PathBuf::from("/"),
        fs_type: "nfs".to_string(),
        device: "server:/export".to_string(),
        flags: MountFlags {
            local: false,
            readonly: false,
            network: true,
        },
        is_root: true,
    };
    assert_eq!(mount_policy(&m), MountPolicy::ListOnly);
}

#[test]
fn policy_is_pure_and_repeatable() {
    let root = mount("/", "apfs", "/dev/disk3s1s1", MNT_LOCAL | MNT_RDONLY);
    let copy = root.clone();
    assert_eq!(mount_policy(&root), mount_policy(&copy));
    assert_eq!(root, copy);
    assert_eq!(root.mount_point.as_path(), Path::new("/"));
}

// ------------------------------------------------------ real mount table

#[cfg(target_os = "macos")]
mod macos {
    use std::path::Path;

    use fx_fs_macos::volumes::{MountPolicy, list_mounts, mount_policy};

    #[test]
    fn real_mount_table_has_exactly_one_indexed_root() {
        let mounts = list_mounts().expect("getfsstat");
        assert!(!mounts.is_empty());

        let roots: Vec<_> = mounts.iter().filter(|m| m.is_root).collect();
        assert_eq!(roots.len(), 1, "exactly one root mount: {mounts:#?}");
        let root = roots[0];
        assert_eq!(root.mount_point, Path::new("/"));
        assert!(root.flags.local);
        assert!(!root.flags.network);
        assert!(!root.fs_type.is_empty());
        assert!(!root.device.is_empty());
        assert_eq!(mount_policy(root), MountPolicy::Index);

        let indexed: Vec<_> = mounts
            .iter()
            .filter(|m| mount_policy(m) == MountPolicy::Index)
            .collect();
        assert_eq!(indexed.len(), 1, "only `/` is indexed: {indexed:#?}");
    }

    #[test]
    fn real_mount_points_are_absolute_and_root_flag_matches() {
        for m in list_mounts().expect("getfsstat") {
            assert!(m.mount_point.is_absolute(), "{m:?}");
            assert_eq!(m.is_root, m.mount_point == Path::new("/"), "{m:?}");
            if m.flags.network {
                assert_eq!(mount_policy(&m), MountPolicy::ListOnly, "{m:?}");
            }
        }
    }

    #[test]
    fn real_data_volume_is_listed_but_not_entered() {
        let mounts = list_mounts().expect("getfsstat");
        // Every supported macOS (10.15+) mounts the Data volume here.
        let data = mounts
            .iter()
            .find(|m| m.mount_point == Path::new("/System/Volumes/Data"))
            .expect("/System/Volumes/Data is mounted");
        assert!(!data.is_root);
        assert!(data.flags.local);
        assert_eq!(mount_policy(data), MountPolicy::ListOnly);
    }
}
