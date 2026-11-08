//! Contract tests for `fx_ui_macos::data_folder`: the app's data folder
//! holds the index, with the names of files in protected folders, and the
//! settings, so only its owner may open it.
//!
//! - `make_dir_private(dir)` (fx-store's, the one the store uses) creates
//!   `dir` and any missing parents with mode 0700 (`PRIVATE_DIR_MODE`), and
//!   tightens an existing `dir` to 0700. Existing parents keep their mode.
//! - `make_store_folders_private(store)` does that for the folder of the
//!   store file `store` and for the data folder holding it, as the app
//!   does at launch. The store file itself is left alone.
//! - `create_private_file(path)` opens `path` for writing, created or
//!   truncated, with mode 0600 (`PRIVATE_FILE_MODE`), also when it already
//!   existed with a wider mode.
//!
//! Tests only create and remove their own temporary folders.

#![cfg(unix)]

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fx_ui_macos::data_folder::{
    PRIVATE_DIR_MODE, PRIVATE_FILE_MODE, create_private_file, make_dir_private,
    make_store_folders_private,
};

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> TempDir {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "fx-ui-data-folder-{tag}-{}-{nanos}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// The permission bits of `path`.
fn mode(path: &Path) -> u32 {
    fs::metadata(path).expect("stat").permissions().mode() & 0o777
}

/// Makes the folder `dir` with mode 0755, as a version before the private
/// data folder did.
fn open_folder(dir: &Path) {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(dir)
        .expect("create");
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).expect("chmod");
}

#[test]
fn the_modes_are_owner_only() {
    assert_eq!(PRIVATE_DIR_MODE, 0o700);
    assert_eq!(PRIVATE_FILE_MODE, 0o600);
}

#[test]
fn new_folders_and_their_missing_parents_are_owner_only() {
    let tmp = TempDir::new("new");
    let data = tmp.path.join("Application Support").join("Not-Everything");
    let stores = data.join("stores");
    make_dir_private(&stores).expect("create");
    assert!(stores.is_dir());
    assert_eq!(mode(&tmp.path.join("Application Support")), 0o700);
    assert_eq!(mode(&data), 0o700);
    assert_eq!(mode(&stores), 0o700);
}

#[test]
fn an_existing_open_folder_is_tightened() {
    let tmp = TempDir::new("open");
    let data = tmp.path.join("Not-Everything");
    open_folder(&data);
    fs::write(data.join("settings"), "kept\n").expect("write");
    make_dir_private(&data).expect("tighten");
    assert_eq!(mode(&data), 0o700);
    assert_eq!(
        fs::read_to_string(data.join("settings")).expect("read"),
        "kept\n",
        "tightening the folder leaves its files alone"
    );
}

#[test]
fn an_existing_private_folder_is_left_as_it_is() {
    let tmp = TempDir::new("private");
    let data = tmp.path.join("Not-Everything");
    make_dir_private(&data).expect("create");
    make_dir_private(&data).expect("again");
    assert_eq!(mode(&data), 0o700);
}

/// At launch the app tightens the data folder and the stores folder an
/// older version left open; the store's own saves no longer do.
#[test]
fn launch_tightens_the_data_and_stores_folders() {
    let tmp = TempDir::new("launch");
    let support = tmp.path.join("Application Support");
    let data = support.join("Not-Everything");
    let stores = data.join("stores");
    open_folder(&stores);
    open_folder(&data);
    open_folder(&support);
    let store = stores.join("index-1.fxdb");
    fs::write(&store, "old store\n").expect("write");
    fs::set_permissions(&store, fs::Permissions::from_mode(0o644)).expect("chmod");
    make_store_folders_private(&store).expect("tighten");
    assert_eq!(mode(&stores), 0o700);
    assert_eq!(mode(&data), 0o700);
    assert_eq!(
        mode(&support),
        0o755,
        "Application Support is not the app's"
    );
    assert_eq!(
        fs::read_to_string(&store).expect("read"),
        "old store\n",
        "the store file is left to the store"
    );
}

#[test]
fn launch_creates_missing_data_and_stores_folders_owner_only() {
    let tmp = TempDir::new("launch-new");
    let data = tmp.path.join("Not-Everything");
    let stores = data.join("stores-limited");
    make_store_folders_private(&stores.join("index-1.fxdb")).expect("create");
    assert_eq!(mode(&data), 0o700);
    assert_eq!(mode(&stores), 0o700);
    assert!(!stores.join("index-1.fxdb").exists(), "no store is written");
}

#[test]
fn a_new_file_is_owner_only() {
    let tmp = TempDir::new("file");
    let path = tmp.path.join("settings");
    let mut f = create_private_file(&path).expect("create");
    f.write_all(b"hotkey=option+space\n").expect("write");
    drop(f);
    assert_eq!(mode(&path), 0o600);
    assert_eq!(
        fs::read_to_string(&path).expect("read"),
        "hotkey=option+space\n"
    );
}

#[test]
fn an_existing_open_file_is_tightened_and_truncated() {
    let tmp = TempDir::new("open-file");
    let path = tmp.path.join("settings");
    fs::write(&path, "a much longer old text\n").expect("write");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod");
    let mut f = create_private_file(&path).expect("open");
    f.write_all(b"new\n").expect("write");
    drop(f);
    assert_eq!(mode(&path), 0o600);
    assert_eq!(fs::read_to_string(&path).expect("read"), "new\n");
}
