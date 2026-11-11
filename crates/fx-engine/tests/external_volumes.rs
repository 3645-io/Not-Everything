//! External volumes in the engine (run 2.4): indexed on mount, marked
//! offline on unmount.
//!
//! Contract pinned here:
//! - `EngineConfig::with_volumes_folder(path)` names the folder volumes
//!   are mounted in (`/Volumes` for the app). A *volume root* is a live
//!   entry directly in that folder whose key is on another volume than the
//!   folder's own (a mount point, keyed by the mounted volume's root).
//! - When a volume root disappears (the path is gone, or a rescan doesn't
//!   list it), the volume goes offline instead of away: its entries stay,
//!   with the same names, paths and metadata, re-keyed onto a volume ID
//!   reserved for offline volumes (`VolumeId::is_offline`), so a device
//!   number the system gives the next volume never meets them.
//!   `EngineSnapshot::is_offline(id)` tells them apart.
//! - Offline entries survive full and subtree rescans, which don't list
//!   them. A volume mounted where an offline one was (same folder, same
//!   name) replaces it.
//! - Without a volumes folder nothing changes: a vanished mount point is
//!   removed with its entries, as before. A file deleted on a mounted
//!   volume is removed as anywhere else.

mod common;

use std::collections::HashMap;
use std::path::PathBuf;

use common::{
    FakeSource, FakeTree, check, config, rel_path, snapshot_entries, start, wait_for, wait_for_tree,
};
use fx_engine::{EngineConfig, EngineSnapshot};
use fx_index::Index;
use fx_model::{Change, Entry, FileKey, StartOutcome};

const VOLUMES: &str = "/vol/Volumes";

fn volumes_config() -> EngineConfig {
    config().with_volumes_folder(VOLUMES)
}

/// `/vol/Volumes` with volume 5 mounted at `Drive`, holding a folder and
/// three files; plus a file on the root volume.
fn tree_with_drive() -> FakeTree {
    let tree = FakeTree::new();
    tree.mkdir("/vol/Volumes");
    tree.touch("/vol/home.txt", 1);
    mount_drive(&tree, "Drive", 5);
    tree
}

fn mount_drive(tree: &FakeTree, name: &str, volume: u32) {
    let root = format!("{VOLUMES}/{name}");
    tree.mount(&root, volume);
    tree.mkdir(&format!("{root}/photos"));
    tree.touch(&format!("{root}/photos/beach.jpg"), 2_000);
    tree.touch(&format!("{root}/photos/{name}-only.jpg"), 3_000);
    tree.touch(&format!("{root}/notes.txt"), 40);
}

/// Live entries by path relative to the root, with their keys.
fn by_path(snap: &EngineSnapshot) -> HashMap<String, (FileKey, Entry)> {
    snapshot_entries(snap)
        .into_iter()
        .filter_map(|(k, e)| rel_path(snap, k).map(|p| (p, (k, e))))
        .collect()
}

fn offline(snap: &EngineSnapshot, key: FileKey) -> bool {
    let idx = snap.index();
    snap.is_offline(idx.lookup(key).expect("live"))
}

const DRIVE_FILES: [&str; 5] = [
    "Volumes/Drive",
    "Volumes/Drive/photos",
    "Volumes/Drive/photos/beach.jpg",
    "Volumes/Drive/photos/Drive-only.jpg",
    "Volumes/Drive/notes.txt",
];

/// Waits until every entry of the drive is offline, and returns the
/// snapshot.
fn wait_offline(engine: &fx_engine::Engine, what: &str) -> std::sync::Arc<EngineSnapshot> {
    wait_for(engine, what, |snap| {
        let paths = by_path(snap);
        DRIVE_FILES
            .iter()
            .all(|p| paths.get(*p).is_some_and(|(k, _)| offline(snap, *k)))
    })
}

#[test]
fn an_unmounted_volume_goes_offline_and_keeps_its_entries() {
    let tree = tree_with_drive();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = start(&tree, &source, tree.build_index(), volumes_config());
    let before = by_path(&engine.snapshot());
    for p in DRIVE_FILES {
        let (k, _) = &before[p];
        assert!(!offline(&engine.snapshot(), *k), "{p} online while mounted");
    }

    tree.remove("/vol/Volumes/Drive");
    source.push(vec![check("/vol/Volumes/Drive")]);
    let snap = wait_offline(&engine, "the drive goes offline");
    let after = by_path(&snap);
    for p in DRIVE_FILES {
        let (old_key, old) = &before[p];
        let (new_key, new) = &after[p];
        assert!(new_key.volume.is_offline(), "{p} keyed offline");
        assert_ne!(new_key.volume, old_key.volume);
        assert_eq!(new_key.file_id, old_key.file_id, "{p} keeps its file ID");
        assert_eq!(new.name, old.name);
        assert_eq!(new.size, old.size);
        assert_eq!(new.modified, old.modified);
        assert_eq!(new.kind, old.kind);
    }
    // The root volume is untouched.
    let (home, _) = &after["home.txt"];
    assert!(!offline(&snap, *home));
    assert!(!offline(&snap, after["Volumes"].0));
    common::assert_orders_match(&snap);
    engine.shutdown();
}

#[test]
fn changes_reported_inside_an_unmounted_volume_do_not_remove_its_entries() {
    let tree = tree_with_drive();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = start(&tree, &source, tree.build_index(), volumes_config());
    tree.remove("/vol/Volumes/Drive");
    // The volume's own stream reports paths on it as it goes, before the
    // mount point's check; later batches report more.
    source.push(vec![
        check("/vol/Volumes/Drive/notes.txt"),
        Change::RescanSubtree(PathBuf::from("/vol/Volumes/Drive/photos")),
        check("/vol/Volumes/Drive"),
    ]);
    source.push(vec![check("/vol/Volumes/Drive/photos/beach.jpg")]);
    wait_offline(&engine, "the whole drive goes offline");
    wait_for(&engine, "the later batch is applied", |_| {
        source.pending() == 0
    });
    wait_offline(&engine, "still offline, whole");
    engine.shutdown();
}

#[test]
fn a_volume_mounted_again_replaces_its_offline_entries() {
    let tree = tree_with_drive();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = start(&tree, &source, tree.build_index(), volumes_config());
    tree.remove("/vol/Volumes/Drive");
    source.push(vec![check("/vol/Volumes/Drive")]);
    wait_offline(&engine, "offline");

    // The same device number again, as macOS often gives it.
    mount_drive(&tree, "Drive", 5);
    source.push(vec![Change::RescanSubtree(PathBuf::from(
        "/vol/Volumes/Drive",
    ))]);
    let snap = wait_for_tree(&engine, &tree, "the drive is back, online, once");
    let volumes = snap.index().lookup(tree.key_of(VOLUMES).unwrap()).unwrap();
    let idx = snap.index();
    let in_folder = idx
        .live_ids()
        .filter(|&id| idx.parent(id) == volumes)
        .count();
    assert_eq!(in_folder, 1, "one Drive in the folder");
    common::assert_orders_match(&snap);
    engine.shutdown();
}

#[test]
fn a_mount_reported_as_a_changed_path_replaces_the_offline_entries_too() {
    let tree = tree_with_drive();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = start(&tree, &source, tree.build_index(), volumes_config());
    tree.remove("/vol/Volumes/Drive");
    source.push(vec![check("/vol/Volumes/Drive")]);
    wait_offline(&engine, "offline");

    mount_drive(&tree, "Drive", 9);
    source.push(vec![check("/vol/Volumes/Drive")]);
    wait_for_tree(&engine, &tree, "the drive is back, online, once");
    engine.shutdown();
}

#[test]
fn another_volume_with_the_same_device_number_leaves_the_offline_one_alone() {
    let tree = tree_with_drive();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = start(&tree, &source, tree.build_index(), volumes_config());
    tree.remove("/vol/Volumes/Drive");
    source.push(vec![check("/vol/Volumes/Drive")]);
    wait_offline(&engine, "offline");

    mount_drive(&tree, "Stick", 5);
    source.push(vec![check("/vol/Volumes/Stick")]);
    let snap = wait_for(&engine, "the stick is indexed", |snap| {
        by_path(snap).contains_key("Volumes/Stick/photos/Stick-only.jpg")
    });
    let paths = by_path(&snap);
    for p in DRIVE_FILES {
        assert!(offline(&snap, paths[p].0), "{p} still offline");
    }
    assert!(!offline(&snap, paths["Volumes/Stick/photos/beach.jpg"].0));
    common::assert_orders_match(&snap);
    engine.shutdown();
}

#[test]
fn rescans_keep_offline_volumes() {
    let tree = tree_with_drive();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = start(&tree, &source, tree.build_index(), volumes_config());
    tree.remove("/vol/Volumes/Drive");
    source.push(vec![check("/vol/Volumes/Drive")]);
    let first = wait_offline(&engine, "offline");

    tree.touch("/vol/new.txt", 3);
    source.push(vec![Change::RescanSubtree(PathBuf::from(VOLUMES))]);
    source.push(vec![Change::RescanVolume]);
    let snap = wait_for(&engine, "the rescans ran", |snap| {
        by_path(snap).contains_key("new.txt")
    });
    // Let any batch still in flight land.
    let snap = wait_for(&engine, "quiet", |s| {
        source.pending() == 0 && s.generation() >= snap.generation()
    });
    let paths = by_path(&snap);
    let first = by_path(&first);
    for p in DRIVE_FILES {
        assert_eq!(paths[p].0, first[p].0, "{p} kept, same offline key");
    }
    engine.shutdown();
}

#[test]
fn a_volume_gone_while_the_app_was_closed_goes_offline_in_the_rescan() {
    let tree = tree_with_drive();
    let index: Index = tree.build_index();
    tree.remove("/vol/Volumes/Drive");
    // History was lost: the writer rescans the loaded index.
    let source = FakeSource::new(StartOutcome::GapDetected);
    let engine = start(&tree, &source, index, volumes_config());
    let snap = wait_offline(&engine, "the drive goes offline in the rescan");
    common::assert_orders_match(&snap);
    engine.shutdown();
}

#[test]
fn without_a_volumes_folder_a_vanished_mount_is_removed() {
    let tree = tree_with_drive();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = start(&tree, &source, tree.build_index(), config());
    tree.remove("/vol/Volumes/Drive");
    source.push(vec![check("/vol/Volumes/Drive")]);
    wait_for_tree(&engine, &tree, "the drive is gone");
    engine.shutdown();
}

#[test]
fn a_mount_listed_but_not_entered_is_removed_on_unmount() {
    // A network share or a hidden system volume: the scanner lists the
    // mount point and nothing below it.
    let tree = tree_with_drive();
    tree.mount("/vol/Volumes/Share", 7);
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = start(&tree, &source, tree.build_index(), volumes_config());
    tree.remove("/vol/Volumes/Share");
    source.push(vec![check("/vol/Volumes/Share")]);
    let snap = wait_for(&engine, "the share is gone", |snap| {
        !by_path(snap).contains_key("Volumes/Share")
    });
    assert!(by_path(&snap).contains_key("Volumes/Drive/notes.txt"));
    engine.shutdown();
}

#[test]
fn a_mount_point_folder_left_behind_does_not_replace_the_offline_volume() {
    let tree = tree_with_drive();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = start(&tree, &source, tree.build_index(), volumes_config());
    tree.remove("/vol/Volumes/Drive");
    // The folder the volume was mounted on stays (a mount point made by
    // hand, as the disk-image tests do).
    tree.mkdir("/vol/Volumes/Drive");
    source.push(vec![Change::RescanSubtree(PathBuf::from(
        "/vol/Volumes/Drive",
    ))]);
    let snap = wait_offline(&engine, "offline beside the left-behind folder");
    let idx = snap.index();
    let named: Vec<_> = idx
        .live_ids()
        .filter(|&id| idx.name(id) == "Drive")
        .map(|id| snap.is_offline(id))
        .collect();
    assert_eq!(named.len(), 2, "the offline volume and the plain folder");
    assert!(named.contains(&true) && named.contains(&false));

    // Mounting again replaces both.
    tree.remove("/vol/Volumes/Drive");
    mount_drive(&tree, "Drive", 5);
    source.push(vec![check("/vol/Volumes/Drive")]);
    wait_for_tree(&engine, &tree, "the drive is back, once");
    engine.shutdown();
}

#[test]
fn files_deleted_on_a_mounted_volume_are_removed() {
    let tree = tree_with_drive();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = start(&tree, &source, tree.build_index(), volumes_config());
    tree.remove("/vol/Volumes/Drive/photos");
    tree.remove("/vol/Volumes/Drive/notes.txt");
    source.push(vec![
        check("/vol/Volumes/Drive/photos"),
        check("/vol/Volumes/Drive/notes.txt"),
    ]);
    let snap = wait_for_tree(&engine, &tree, "the deletions are applied");
    assert!(!by_path(&snap).contains_key("Volumes/Drive/notes.txt"));
    // A folder elsewhere is removed as before.
    tree.mkdir("/vol/Volumes/plain");
    source.push(vec![check("/vol/Volumes/plain")]);
    wait_for_tree(&engine, &tree, "a plain folder in the volumes folder");
    tree.remove("/vol/Volumes/plain");
    source.push(vec![check("/vol/Volumes/plain")]);
    wait_for_tree(
        &engine,
        &tree,
        "a plain folder there is removed, not offline",
    );
    engine.shutdown();
}

#[test]
fn offline_volumes_survive_a_save_and_a_restart() {
    let dir = std::env::temp_dir().join(format!("fx-offline-store-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = dir.join("index.fxstore");
    let tree = tree_with_drive();
    let source = FakeSource::new(StartOutcome::Resumed);
    let cfg = volumes_config().with_store_path(&store);
    let engine = start(&tree, &source, tree.build_index(), cfg.clone());
    tree.remove("/vol/Volumes/Drive");
    source.push(vec![check("/vol/Volumes/Drive")]);
    let before = by_path(&wait_offline(&engine, "offline"));
    engine.shutdown();

    let source = FakeSource::new(StartOutcome::Resumed);
    let engine =
        fx_engine::Engine::open(cfg, source.clone(), tree.clone(), tree.clone()).expect("open");
    let snap = engine.snapshot();
    let after = by_path(&snap);
    for p in DRIVE_FILES {
        assert_eq!(after[p].0, before[p].0, "{p} reloaded");
        assert!(offline(&snap, after[p].0), "{p} offline after the restart");
    }
    engine.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------- hidden from searches

/// The paths `query` finds, sorted.
fn found(engine: &fx_engine::Engine, query: &str) -> Vec<String> {
    let page = engine
        .search(
            query,
            fx_engine::QueryOptions::default(),
            fx_sort::SortKey::Path,
            fx_sort::Direction::Ascending,
            0,
            1_000,
        )
        .expect("search");
    let snap = engine.snapshot();
    let idx = snap.index();
    let mut paths: Vec<String> = page
        .ids
        .iter()
        .filter_map(|&id| rel_path(&snap, idx.key(fx_model::EntryId(id))))
        .collect();
    paths.sort();
    assert_eq!(
        page.total,
        paths.len(),
        "{query}: total counts what is shown"
    );
    paths
}

#[test]
fn an_offline_volume_is_left_out_of_searches_until_it_is_mounted_again() {
    // Until the interface has an offline marker (the owner, 11 Nov), the
    // entries are kept but no search returns them.
    let tree = tree_with_drive();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = start(&tree, &source, tree.build_index(), volumes_config());
    assert_eq!(
        found(&engine, "beach"),
        vec!["Volumes/Drive/photos/beach.jpg"]
    );
    assert_eq!(found(&engine, "Drive").len(), 2, "the volume and a file");

    tree.remove("/vol/Volumes/Drive");
    source.push(vec![check("/vol/Volumes/Drive")]);
    let snap = wait_offline(&engine, "offline");
    assert!(
        found(&engine, "beach").is_empty(),
        "offline files are hidden"
    );
    assert!(
        found(&engine, "Drive").is_empty(),
        "so is the volume itself"
    );
    let everything = found(&engine, "");
    assert!(
        everything.iter().all(|p| !p.starts_with("Volumes/Drive")),
        "an empty search lists no offline entry: {everything:?}"
    );
    assert!(everything.contains(&"home.txt".to_string()));
    assert!(everything.contains(&"Volumes".to_string()));
    // Still in the index, so a remount replaces them.
    assert!(by_path(&snap).contains_key("Volumes/Drive/photos/beach.jpg"));

    mount_drive(&tree, "Drive", 5);
    source.push(vec![check("/vol/Volumes/Drive")]);
    wait_for_tree(&engine, &tree, "the drive is back");
    assert_eq!(
        found(&engine, "beach"),
        vec!["Volumes/Drive/photos/beach.jpg"]
    );
    engine.shutdown();
}

#[test]
fn the_command_line_reader_leaves_offline_volumes_out_too() {
    let dir = std::env::temp_dir().join(format!("fx-offline-reader-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = dir.join("index.fxstore");
    let tree = tree_with_drive();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = start(
        &tree,
        &source,
        tree.build_index(),
        volumes_config().with_store_path(&store),
    );
    tree.remove("/vol/Volumes/Drive");
    source.push(vec![check("/vol/Volumes/Drive")]);
    wait_offline(&engine, "offline");
    engine.shutdown();

    let reader = fx_engine::StoreReader::open(&store).expect("open the store");
    let opts = fx_engine::ReadOptions::default();
    let search = |q: &str| {
        reader
            .search(
                q,
                &opts,
                fx_sort::SortKey::Path,
                fx_sort::Direction::Ascending,
                0,
                1_000,
            )
            .expect("search")
    };
    let beach = search("beach");
    assert_eq!(
        (beach.total, beach.ids.len()),
        (0, 0),
        "offline file hidden"
    );
    assert_eq!(search("home").total, 1, "the root volume's files are found");
    let all = search("");
    assert_eq!(all.total, all.ids.len());
    assert_eq!(
        all.total, 3,
        "the root, Volumes and home.txt; none of the drive's five entries"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
