//! `VolumesSource` on real disk images (run 2.4, macOS only).
//!
//! Contract pinned here:
//! - A volume of every indexed type (APFS, HFS+, FAT32, exFAT) mounted in
//!   the external volumes folder while the source runs is reported as a
//!   `RescanSubtree` of its mount point within a few seconds; a file
//!   written on it then arrives as a `CheckPath`; unmounting it is
//!   reported as a `CheckPath` of its mount point and its stream stops.
//! - A source that resumed from its cursor starts by asking for a rescan
//!   of the whole external volumes folder and watches every volume already
//!   mounted; any other start outcome asks for nothing (the engine rescans
//!   everything).
//! - The cursor is the root source's.
//! - A root source error is passed on, and a volume's rescan or check that
//!   was waiting is kept for the next batch (no disk image needed: a fake
//!   mount table names a plain folder).
//!
//! The root source here is a quiet fake; the images are attached with
//! `-nobrowse` in a scratch folder and always detached. Where `hdiutil`
//! cannot make or attach images, the tests skip.

#![cfg(target_os = "macos")]

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use fx_fs::ChangeSource;
use fx_fs_macos::fsevents::volumes_source::{MOUNT_CHECK_INTERVAL, VolumesSource};
use fx_fs_macos::volumes::{ExternalVolumes, MountFlags, MountInfo};
use fx_model::{Change, ResumeCursor, StartOutcome};
use fx_testkit::disk_image::{DiskImage, ImageFs, Scratch, skip};

/// A root source whose `next_batch` fails while `failing` is set.
struct FlakyRoot {
    failing: Arc<AtomicBool>,
}

impl ChangeSource for FlakyRoot {
    fn start(&mut self, _from: Option<&ResumeCursor>) -> io::Result<StartOutcome> {
        Ok(StartOutcome::NoCursorSupport)
    }

    fn next_batch(&mut self, deadline: Instant) -> io::Result<Vec<Change>> {
        if self.failing.load(Ordering::SeqCst) {
            return Err(io::Error::other("stream stopped"));
        }
        std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
        Ok(Vec::new())
    }

    fn cursor(&self) -> ResumeCursor {
        CURSOR
    }
}

#[test]
fn a_root_error_keeps_a_waiting_volume_rescan() {
    let scratch = Scratch::new("source-error").expect("scratch");
    let mount_point = scratch.path().join("mnt/fake");
    std::fs::create_dir_all(&mount_point).expect("folder");
    let mounted = Arc::new(AtomicBool::new(false));
    let failing = Arc::new(AtomicBool::new(false));
    let lister = {
        let mounted = Arc::clone(&mounted);
        let mount_point = mount_point.clone();
        Box::new(move || {
            let mut mounts = Vec::new();
            if mounted.load(Ordering::SeqCst) {
                mounts.push(MountInfo {
                    mount_point: mount_point.clone(),
                    fs_type: "apfs".to_string(),
                    device: "/dev/disk99s1".to_string(),
                    flags: MountFlags::from_raw(0x1000, "apfs"),
                    is_root: false,
                });
            }
            Ok(mounts)
        })
    };
    let mut src = VolumesSource::with_lister(
        FlakyRoot {
            failing: Arc::clone(&failing),
        },
        scratch.path(),
        ExternalVolumes {
            folder: scratch.path().join("mnt"),
            include_hidden: false,
        },
        lister,
    );
    src.start(None).expect("start");
    assert!(src.watched().is_empty());

    // The volume appears; the next mount check queues its rescan, and the
    // root fails in that same call.
    mounted.store(true, Ordering::SeqCst);
    failing.store(true, Ordering::SeqCst);
    std::thread::sleep(MOUNT_CHECK_INTERVAL + Duration::from_millis(50));
    assert!(src.next_batch(Instant::now()).is_err());
    assert_eq!(src.watched(), vec![mount_point.clone()]);

    failing.store(false, Ordering::SeqCst);
    let batch = src
        .next_batch(Instant::now() + Duration::from_millis(50))
        .expect("next_batch");
    assert!(
        batch.contains(&Change::RescanSubtree(mount_point.clone())),
        "the rescan survived the error: {batch:?}"
    );
}

const CURSOR: ResumeCursor = ResumeCursor::FsEvents {
    event_id: 42,
    db_uuid: [3; 16],
};

/// A root source that reports nothing.
struct QuietRoot {
    outcome: StartOutcome,
}

impl ChangeSource for QuietRoot {
    fn start(&mut self, _from: Option<&ResumeCursor>) -> io::Result<StartOutcome> {
        Ok(self.outcome)
    }

    fn next_batch(&mut self, deadline: Instant) -> io::Result<Vec<Change>> {
        std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
        Ok(Vec::new())
    }

    fn cursor(&self) -> ResumeCursor {
        CURSOR
    }
}

fn source(scratch: &Path, outcome: StartOutcome) -> VolumesSource<QuietRoot> {
    VolumesSource::new(
        QuietRoot { outcome },
        scratch,
        ExternalVolumes {
            folder: scratch.join("mnt"),
            include_hidden: true,
        },
    )
}

/// Collects batches until `want` matches one change, for up to 15 s.
fn wait_for_change(
    src: &mut VolumesSource<QuietRoot>,
    what: &str,
    want: impl Fn(&Change) -> bool,
) -> Vec<Change> {
    let end = Instant::now() + Duration::from_secs(15);
    let mut seen = Vec::new();
    while Instant::now() < end {
        let batch = src
            .next_batch(Instant::now() + Duration::from_millis(100))
            .expect("next_batch");
        let hit = batch.iter().any(&want);
        seen.extend(batch);
        if hit {
            return seen;
        }
    }
    panic!("no change for {what} within 15 s; saw {seen:?}");
}

#[test]
fn volumes_of_every_type_are_reported_on_mount_on_change_and_on_unmount() {
    let scratch = Scratch::new("source-live").expect("scratch");
    std::fs::create_dir(scratch.path().join("mnt")).expect("mnt");
    let mut src = source(scratch.path(), StartOutcome::NoCursorSupport);
    assert_eq!(
        src.start(None).expect("start"),
        StartOutcome::NoCursorSupport
    );
    let first = src
        .next_batch(Instant::now() + Duration::from_millis(50))
        .expect("next_batch");
    assert!(first.is_empty(), "nothing mounted yet: {first:?}");

    for fs in ImageFs::ALL {
        let mp = scratch.path().join("mnt").join(fs.short());
        let mut img = match DiskImage::attach_new(scratch.path(), fs.short(), fs, &mp) {
            Ok(img) => img,
            Err(e) => {
                skip("volumes_source", &e);
                return;
            }
        };
        wait_for_change(&mut src, &format!("{} mounted", fs.short()), |c| {
            *c == Change::RescanSubtree(mp.clone())
        });
        assert_eq!(src.watched(), vec![mp.clone()]);

        // Give the new stream a moment, then change a file on the volume.
        std::thread::sleep(Duration::from_millis(300));
        let file = mp.join(format!("live-{}.txt", fs.short()));
        std::fs::write(&file, b"x").expect("write on the image");
        wait_for_change(&mut src, &format!("a file on {}", fs.short()), |c| {
            *c == Change::CheckPath(file.clone())
        });

        img.detach().expect("detach");
        wait_for_change(&mut src, &format!("{} unmounted", fs.short()), |c| {
            *c == Change::CheckPath(mp.clone())
        });
        assert!(src.watched().is_empty(), "{} still watched", fs.short());
    }
    assert_eq!(src.cursor(), CURSOR);
}

#[test]
fn a_resumed_start_rescans_the_folder_and_watches_mounted_volumes() {
    let scratch = Scratch::new("source-start").expect("scratch");
    std::fs::create_dir(scratch.path().join("mnt")).expect("mnt");
    let mp = scratch.path().join("mnt/apfs");
    let mut img = match DiskImage::attach_new(scratch.path(), "apfs", ImageFs::Apfs, &mp) {
        Ok(img) => img,
        Err(e) => {
            skip("volumes_source", &e);
            return;
        }
    };

    let mut resumed = source(scratch.path(), StartOutcome::Resumed);
    assert_eq!(
        resumed.start(Some(&CURSOR)).expect("start"),
        StartOutcome::Resumed
    );
    assert_eq!(resumed.watched(), vec![mp.clone()]);
    let batch = resumed
        .next_batch(Instant::now() + Duration::from_millis(50))
        .expect("next_batch");
    assert_eq!(
        batch,
        vec![Change::RescanSubtree(scratch.path().join("mnt"))],
        "one rescan of the folder, no per-volume rescan"
    );

    let mut gap = source(scratch.path(), StartOutcome::GapDetected);
    assert_eq!(
        gap.start(Some(&CURSOR)).expect("start"),
        StartOutcome::GapDetected
    );
    assert_eq!(gap.watched(), vec![mp.clone()]);
    let batch = gap
        .next_batch(Instant::now() + Duration::from_millis(50))
        .expect("next_batch");
    // The volume's own stream may report activity on it; no rescan is
    // asked for.
    assert!(
        !batch
            .iter()
            .any(|c| matches!(c, Change::RescanSubtree(_) | Change::RescanVolume)),
        "the full rescan covers it: {batch:?}"
    );

    // A source whose root is the folder itself watches no volume below it.
    let mut inside = VolumesSource::new(
        QuietRoot {
            outcome: StartOutcome::Resumed,
        },
        scratch.path().join("mnt"),
        ExternalVolumes {
            folder: scratch.path().join("mnt"),
            include_hidden: true,
        },
    );
    inside.start(Some(&CURSOR)).expect("start");
    assert!(inside.watched().is_empty());
    drop((resumed, gap, inside));
    img.detach().expect("detach");
}
