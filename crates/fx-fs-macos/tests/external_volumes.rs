//! External volumes in the walker and the path stat (run 2.4), on real
//! APFS, HFS+, FAT32 and exFAT disk images attached with `hdiutil`
//! (macOS only).
//!
//! Contract pinned here:
//! - With `ScanOptions::external_volumes` set, a scan enters every mount
//!   below its root that `ExternalVolumes::indexes` accepts and lists what
//!   the volume holds; without it (the default) a mount point is listed
//!   but never entered, as before.
//! - A mount point's entry is keyed by the mounted volume's root, `(st_dev,
//!   st_ino)` as `lstat` reports them, entered or not, so two volumes'
//!   mount points never share a key; the entries below it are keyed by the
//!   usual rule. `MacPathStat` keys them the same way, so the writer finds
//!   the scanned entries again when it checks their paths.
//!
//! The images are attached with `-nobrowse` in a scratch folder (hence
//! `include_hidden`) and always detached, also when a test fails. Where
//! `hdiutil` cannot make or attach images, the tests skip.

#![cfg(target_os = "macos")]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fx_fs::PathStat;
use fx_fs_macos::path_stat::MacPathStat;
use fx_fs_macos::scan::{ScanOptions, ScannedEntry, relative_path, scan};
use fx_fs_macos::volumes::ExternalVolumes;
use fx_model::{FileKey, VolumeId};
use fx_testkit::disk_image::{DiskImage, ImageFs, Scratch, skip};

/// A scratch folder with `mnt/` holding one attached image per type in
/// `types`, each with `dir/sub/f-<type>.txt`. `None` when images can't be
/// made here.
fn setup(test: &str, types: &[ImageFs]) -> Option<(Scratch, Vec<DiskImage>)> {
    let scratch = Scratch::new(test).expect("scratch folder");
    std::fs::create_dir(scratch.path().join("mnt")).expect("mnt folder");
    let mut images = Vec::new();
    for &fs in types {
        let mp = scratch.path().join("mnt").join(fs.short());
        match DiskImage::attach_new(scratch.path(), fs.short(), fs, &mp) {
            Ok(img) => images.push(img),
            Err(e) => {
                skip(test, &e);
                return None;
            }
        }
        let sub = mp.join("dir/sub");
        std::fs::create_dir_all(&sub).expect("folders on the image");
        std::fs::write(sub.join(format!("f-{}.txt", fs.short())), b"x").expect("file");
    }
    Some((scratch, images))
}

fn external(scratch: &Path) -> ExternalVolumes {
    ExternalVolumes {
        folder: scratch.join("mnt"),
        include_hidden: true,
    }
}

fn by_path(entries: &[ScannedEntry]) -> HashMap<PathBuf, ScannedEntry> {
    (0..entries.len())
        .map(|i| (relative_path(entries, i), entries[i].clone()))
        .collect()
}

fn key_of(e: &ScannedEntry) -> FileKey {
    FileKey::new(VolumeId(e.dev), e.file_id)
}

fn detach_all(images: &mut [DiskImage]) {
    for img in images {
        img.detach().expect("detach");
    }
}

#[test]
fn a_scan_enters_external_volumes_of_every_type() {
    let Some((scratch, mut images)) = setup("scan-enters", &ImageFs::ALL) else {
        return;
    };
    let opts = ScanOptions {
        threads: 2,
        external_volumes: Some(external(scratch.path())),
        ..ScanOptions::default()
    };
    let result = scan(scratch.path(), &opts).expect("scan");
    let paths = by_path(result.entries.as_deref().expect("collected"));
    for fs in ImageFs::ALL {
        let file = PathBuf::from(format!("mnt/{0}/dir/sub/f-{0}.txt", fs.short()));
        assert!(paths.contains_key(&file), "{} not listed", file.display());
    }
    assert_eq!(result.stats.skipped_mount_points, 0);

    // Without the setting the mount points are listed, never entered.
    let plain = scan(scratch.path(), &ScanOptions::default()).expect("scan");
    let plain_paths = by_path(plain.entries.as_deref().expect("collected"));
    for fs in ImageFs::ALL {
        assert!(plain_paths.contains_key(&PathBuf::from(format!("mnt/{}", fs.short()))));
        assert!(
            !plain_paths.contains_key(&PathBuf::from(format!("mnt/{}/dir", fs.short()))),
            "{} entered without the setting",
            fs.short()
        );
    }
    assert_eq!(plain.stats.skipped_mount_points, 4);
    detach_all(&mut images);
}

#[test]
fn mount_point_keys_follow_the_volume_and_match_the_path_stat() {
    let types = [ImageFs::Apfs, ImageFs::ExFat];
    let Some((scratch, mut images)) = setup("keys", &types) else {
        return;
    };
    let stat = MacPathStat::new();
    for opts in [
        ScanOptions {
            threads: 1,
            external_volumes: Some(external(scratch.path())),
            ..ScanOptions::default()
        },
        ScanOptions {
            threads: 1,
            ..ScanOptions::default()
        },
    ] {
        let entered = opts.external_volumes.is_some();
        let result = scan(scratch.path(), &opts).expect("scan");
        let paths = by_path(result.entries.as_deref().expect("collected"));
        let mut mount_keys = Vec::new();
        for fs in types {
            let rel = PathBuf::from(format!("mnt/{}", fs.short()));
            let mp = &paths[&rel];
            let on_disk = stat
                .stat(&scratch.path().join(&rel))
                .expect("stat")
                .expect("mount point exists");
            let st = std::fs::symlink_metadata(scratch.path().join(&rel)).expect("lstat");
            use std::os::unix::fs::MetadataExt;
            #[allow(clippy::cast_possible_truncation)]
            let own = FileKey::new(VolumeId(st.dev() as u32), st.ino());
            assert_eq!(
                key_of(mp),
                own,
                "{rel:?}: keyed by the mounted volume's root"
            );
            assert_eq!(on_disk.key, own, "{rel:?}: the path stat agrees");
            mount_keys.push(own);
            if entered {
                let file_rel = rel.join(format!("dir/sub/f-{}.txt", fs.short()));
                let file = &paths[&file_rel];
                let file_stat = stat
                    .stat(&scratch.path().join(&file_rel))
                    .expect("stat")
                    .expect("file exists");
                assert_eq!(key_of(file), file_stat.key, "{file_rel:?}");
                let dir = &paths[&rel.join("dir")];
                let dir_stat = stat
                    .stat(&scratch.path().join(rel.join("dir")))
                    .expect("stat")
                    .expect("folder exists");
                assert_eq!(key_of(dir), dir_stat.key);
                assert_eq!(
                    FileKey::new(VolumeId(dir.parent_key.0), dir.parent_key.1),
                    own,
                    "a volume's top folder hangs off the mount point's key"
                );
                assert_eq!(dir_stat.parent, Some(own));
            }
        }
        assert_ne!(mount_keys[0], mount_keys[1]);
    }
    detach_all(&mut images);
}
