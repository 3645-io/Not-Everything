//! The store is private to the user who runs it (Unix only).
//!
//! A checkpoint holds the names of every indexed file, including names in
//! folders other programs may not list (Full Disk Access). So the store's
//! files are written with mode 0600 and the folders it makes with mode
//! 0700. A folder that already exists keeps its mode: the store may be
//! pointed at any folder, such as the home folder or a shared `/tmp`.
//!
//! Contract pinned here:
//! - `write_checkpoint` leaves the target with mode 0600, whatever mode an
//!   earlier file at that path had.
//! - `fx_store::create_private_dir(dir)` creates `dir` and its missing
//!   parents with mode 0700 and never changes the mode of a folder that
//!   already exists, `dir` included.
//! - `fx_store::make_dir_private(dir)` creates `dir` the same way and sets
//!   an existing `dir` to 0700; its parents keep their mode. Only the app
//!   calls it, on its own folders.
//! - `fx_store::PRIVATE_DIR_MODE` is 0700 and `PRIVATE_FILE_MODE` 0600.
//! - `open_or_rebuild` creates a missing folder for the target that way
//!   when it writes a rebuilt checkpoint, leaves an existing one as it is
//!   (also one it may not change), and the rebuilt file is 0600.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fx_index::IndexParts;
use fx_store::{RebuiltIndex, SaveInput, VerifyMode, open_or_rebuild, write_checkpoint};

/// A unique directory under the system temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!("fx-store-permissions-{tag}-{}", unique()));
        fs::create_dir_all(&dir).expect("create temp dir");
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A file at `path` removed on drop.
struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// A name part no other test run uses.
fn unique() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "{}-{}-{nanos}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// The permission bits of `path`.
fn mode(path: &Path) -> u32 {
    fs::metadata(path).expect("metadata").permissions().mode() & 0o777
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod");
}

/// A folder this user may write in but not change the mode of: the shared
/// `/tmp`, which belongs to root. `None`, and the test that asks is
/// skipped, when the tests run as root (who may change any mode), when
/// `/tmp` belongs to this user, or when it can't be written here. `own` is
/// a folder this test made, so its owner is this user.
fn shared_folder(own: &Path) -> Option<PathBuf> {
    let me = fs::metadata(own).ok()?.uid();
    let dir = fs::canonicalize("/tmp").ok()?;
    let owner = fs::metadata(&dir).ok()?.uid();
    if me == 0 || owner == me {
        return None;
    }
    let probe = dir.join(format!(".fx-store-probe-{}", unique()));
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .ok()?;
    let _ = fs::remove_file(&probe);
    Some(dir)
}

fn save_empty(path: &Path) {
    let parts = IndexParts::default();
    write_checkpoint(
        path,
        &SaveInput {
            parts: &parts,
            volumes: &[],
            orders: &[],
            clean_shutdown: true,
        },
    )
    .expect("write checkpoint");
}

fn rebuild_empty(path: &Path) -> Result<(), fx_store::StoreError> {
    open_or_rebuild(path, VerifyMode::Full, || {
        Ok::<_, std::io::Error>(RebuiltIndex::default())
    })
    .map(drop)
}

#[test]
fn the_modes_are_owner_only() {
    assert_eq!(fx_store::PRIVATE_DIR_MODE, 0o700);
    assert_eq!(fx_store::PRIVATE_FILE_MODE, 0o600);
}

#[test]
fn a_new_checkpoint_is_private() {
    let tmp = TempDir::new("new");
    let path = tmp.0.join("index.fxs");
    save_empty(&path);
    assert_eq!(mode(&path), 0o600, "{}", path.display());
}

#[test]
fn an_existing_checkpoint_is_tightened_when_written() {
    let tmp = TempDir::new("existing");
    let path = tmp.0.join("index.fxs");
    save_empty(&path);
    set_mode(&path, 0o644);
    save_empty(&path);
    assert_eq!(mode(&path), 0o600);
}

#[test]
fn a_checkpoint_in_an_open_folder_is_private_and_the_folder_kept() {
    let tmp = TempDir::new("open-folder");
    let dir = tmp.0.join("stores");
    fs::create_dir(&dir).expect("mkdir");
    set_mode(&dir, 0o755);
    let path = dir.join("index.fxs");
    save_empty(&path);
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(&dir), 0o755, "an existing folder keeps its mode");
}

#[test]
fn private_dirs_are_created_0700() {
    let tmp = TempDir::new("create");
    let parent = tmp.0.join("app");
    let dir = parent.join("stores");
    fx_store::create_private_dir(&dir).expect("create private dir");
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&parent), 0o700, "a parent made on the way is private");
}

#[test]
fn create_private_dir_leaves_an_existing_folder_alone() {
    let tmp = TempDir::new("keep");
    let dir = tmp.0.join("stores");
    fs::create_dir(&dir).expect("mkdir");
    set_mode(&dir, 0o755);
    set_mode(&tmp.0, 0o755);
    fx_store::create_private_dir(&dir).expect("create private dir");
    assert_eq!(mode(&dir), 0o755, "an existing folder keeps its mode");
    assert_eq!(mode(&tmp.0), 0o755, "an existing parent keeps its mode");
}

#[test]
fn make_dir_private_creates_a_missing_folder_0700() {
    let tmp = TempDir::new("make-new");
    let parent = tmp.0.join("app");
    let dir = parent.join("stores");
    fx_store::make_dir_private(&dir).expect("make dir private");
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&parent), 0o700, "a parent made on the way is private");
}

#[test]
fn make_dir_private_tightens_an_existing_folder_and_keeps_its_parent() {
    let tmp = TempDir::new("tighten");
    let dir = tmp.0.join("stores");
    fs::create_dir(&dir).expect("mkdir");
    set_mode(&dir, 0o755);
    set_mode(&tmp.0, 0o755);
    fs::write(dir.join("kept"), "kept\n").expect("write");
    fx_store::make_dir_private(&dir).expect("make dir private");
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&tmp.0), 0o755, "an existing parent keeps its mode");
    assert_eq!(
        fs::read_to_string(dir.join("kept")).expect("read"),
        "kept\n",
        "tightening the folder leaves its files alone"
    );
}

#[test]
fn a_rebuild_writes_a_private_folder_and_file() {
    let tmp = TempDir::new("rebuild-new");
    let app = tmp.0.join("app");
    let dir = app.join("stores");
    let path = dir.join("index.fxs");
    rebuild_empty(&path).expect("rebuild");
    assert_eq!(mode(&app), 0o700);
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&path), 0o600);
}

#[test]
fn a_rebuild_leaves_an_existing_folder_as_it_is() {
    let tmp = TempDir::new("rebuild-existing");
    let dir = tmp.0.join("stores");
    fs::create_dir(&dir).expect("mkdir");
    set_mode(&dir, 0o755);
    let path = dir.join("index.fxs");
    rebuild_empty(&path).expect("rebuild");
    assert_eq!(mode(&dir), 0o755, "an existing folder keeps its mode");
    assert_eq!(mode(&path), 0o600);
}

/// A folder that belongs to someone else can't have its mode changed; a
/// save into it must not try.
#[test]
fn a_rebuild_into_a_folder_it_may_not_change_succeeds() {
    let tmp = TempDir::new("shared");
    let Some(shared) = shared_folder(&tmp.0) else {
        eprintln!("skipped: no folder here that this user may write in but not change");
        return;
    };
    let before = mode(&shared);
    let path = shared.join(format!("fx-store-shared-{}.fxs", unique()));
    let _cleanup = RemoveOnDrop(path.clone());
    rebuild_empty(&path).expect("a save into a folder whose mode can't be changed");
    assert_eq!(mode(&shared), before, "the shared folder keeps its mode");
    assert_eq!(mode(&path), 0o600);
}
