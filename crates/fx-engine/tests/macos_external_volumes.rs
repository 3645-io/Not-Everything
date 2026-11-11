//! External volumes end to end on macOS (run 2.4): the engine with the
//! real scanner, path stat and `VolumesSource` indexes a disk image when it
//! is attached and marks it offline when it is detached; attached again,
//! it is online once more, with no offline copy left.
//!
//! The images are attached with `-nobrowse` in a scratch folder that
//! stands in for `/Volumes` (hence `include_hidden`) and always detached.
//! Where `hdiutil` cannot make or attach images, the test skips.

#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use fx_engine::{Engine, EngineConfig, EngineSnapshot};
use fx_fs_macos::enumerator::MacScanner;
use fx_fs_macos::fsevents::source::FsEventsSource;
use fx_fs_macos::fsevents::stream::StreamConfig;
use fx_fs_macos::fsevents::volumes_source::VolumesSource;
use fx_fs_macos::path_stat::MacPathStat;
use fx_fs_macos::scan::ScanOptions;
use fx_fs_macos::volumes::ExternalVolumes;
use fx_index::Index;
use fx_model::EntryId;
use fx_testkit::disk_image::{DiskImage, ImageFs, Scratch, skip};

/// Far above what a quiet Mac needs, so a loaded machine or a debug build
/// does not fail the test.
const LIMIT: Duration = Duration::from_secs(30);

fn external(root: &Path) -> ExternalVolumes {
    ExternalVolumes {
        folder: root.join("mnt"),
        include_hidden: true,
    }
}

fn start(root: &Path) -> Engine {
    let mut stream = StreamConfig::new(root.to_path_buf());
    stream.path_filter = Some(root.to_path_buf());
    stream.no_defer = true;
    let source = VolumesSource::new(FsEventsSource::new(stream), root, external(root));
    let scanner = MacScanner::new(ScanOptions {
        external_volumes: Some(external(root)),
        ..ScanOptions::default()
    });
    let config = EngineConfig::new(root)
        .with_batch_window(Duration::from_millis(50))
        .with_volumes_folder(root.join("mnt"));
    Engine::start(Index::new(), config, source, MacPathStat::new(), scanner).expect("engine")
}

/// The live entry at `rel` (below the root), if any, and whether it is
/// offline. With two entries at one path (an offline volume beside the
/// folder it was mounted on), the offline one.
fn find(snap: &EngineSnapshot, rel: &str) -> Option<(EntryId, bool)> {
    let idx = snap.index();
    let mut found = None;
    for id in idx.live_ids() {
        if path_of(snap, id) == rel {
            let offline = snap.is_offline(id);
            if found.is_none() || offline {
                found = Some((id, offline));
            }
        }
    }
    found
}

fn path_of(snap: &EngineSnapshot, mut id: EntryId) -> String {
    let idx = snap.index();
    let mut parts = Vec::new();
    loop {
        let p = idx.parent(id);
        if p.is_none() {
            break;
        }
        parts.push(idx.name(id).to_string());
        id = p;
    }
    parts.reverse();
    parts.join("/")
}

fn wait(engine: &Engine, what: &str, cond: impl Fn(&EngineSnapshot) -> bool) {
    let end = Instant::now() + LIMIT;
    while Instant::now() < end {
        if cond(&engine.snapshot()) {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for: {what}");
}

fn count_named(snap: &EngineSnapshot, name: &str) -> usize {
    let idx = snap.index();
    idx.live_ids().filter(|&id| idx.name(id) == name).count()
}

#[test]
fn a_disk_image_is_indexed_on_attach_offline_on_detach_and_back_on_reattach() {
    for fs in [ImageFs::Apfs, ImageFs::ExFat] {
        let scratch = Scratch::new("e2e").expect("scratch");
        let root: PathBuf = scratch.path().to_path_buf();
        std::fs::create_dir(root.join("mnt")).expect("mnt");
        std::fs::write(root.join("home.txt"), b"h").expect("file on the root volume");
        let engine = start(&root);
        wait(&engine, "the first scan", |s| find(s, "home.txt").is_some());

        let mp = root.join("mnt").join(fs.short());
        let mut img = match DiskImage::attach_new(&root, fs.short(), fs, &mp) {
            Ok(img) => img,
            Err(e) => {
                skip("macos_external_volumes", &e);
                engine.shutdown();
                return;
            }
        };
        std::fs::create_dir_all(mp.join("photos")).expect("folder on the image");
        std::fs::write(mp.join("photos/beach.jpg"), b"jpeg").expect("file on the image");
        let file = format!("mnt/{}/photos/beach.jpg", fs.short());
        wait(&engine, "the image is indexed", |s| {
            find(s, &file).is_some_and(|(_, offline)| !offline)
        });

        img.detach().expect("detach");
        wait(&engine, "the image is offline", |s| {
            find(s, &file).is_some_and(|(_, offline)| offline)
                && find(s, &format!("mnt/{}", fs.short())).is_some_and(|(_, off)| off)
        });

        img.attach().expect("attach again");
        wait(&engine, "the image is back online, once", |s| {
            find(s, &file).is_some_and(|(_, offline)| !offline)
                && count_named(s, "beach.jpg") == 1
                && count_named(s, fs.short()) == 1
        });
        let snap = engine.snapshot();
        let idx = snap.index();
        assert!(
            !idx.live_ids().any(|id| snap.is_offline(id)),
            "no offline entry is left"
        );
        img.detach().expect("detach");
        engine.shutdown();
    }
}
