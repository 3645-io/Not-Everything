//! Where the app keeps the engine store for a root.
//!
//! This mirrors `fx_ui_macos::app_args::store_path` and `store_path_for`
//! without depending on the UI crate; `tests/store_path.rs` keeps them in
//! step. The full-access layout is
//! `<home>/Library/Application Support/<APP_FOLDER>/stores/<label>-<hash>.fxstore`,
//! where `<label>` is the root's last component with every non-ASCII-
//! alphanumeric character replaced by `_`, cut to 32 characters (`root` for
//! `/`), and `<hash>` is the 16-digit hex 64-bit FNV-1a hash of the root's
//! path bytes. A store built without Full Disk Access (the limited scope)
//! has the same file name in the sibling `stores-limited` folder.

use std::path::{Path, PathBuf};

use fx_channel::Channel;

/// The root searched when `-root` is not given.
pub const DEFAULT_ROOT: &str = "/";

/// The application folder under `~/Library/Application Support/`: the
/// app's name, `Not-Everything Preview` for a Preview build. Same value as
/// the app's.
pub const APP_FOLDER: &str = Channel::CURRENT.data_folder();

/// The store file the app saves for `root`, under the user's `home`.
pub fn default_store_path(home: &Path, root: &Path) -> PathBuf {
    store_path_for(home, root, StoreScope::Full)
}

/// The access scope the app built a store in. Mirrors
/// `fx_ui_macos::app_args::StoreScope`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StoreScope {
    /// Built without Full Disk Access: protected folders were left out.
    Limited,
    /// Built with Full Disk Access.
    Full,
}

/// The store file the app saves for `root` built in `scope`, under the
/// user's `home`. [`StoreScope::Full`] is [`default_store_path`];
/// [`StoreScope::Limited`] is the same file name in the sibling
/// `stores-limited` folder. Mirrors `fx_ui_macos::app_args::store_path_for`.
pub fn store_path_for(home: &Path, root: &Path, scope: StoreScope) -> PathBuf {
    let label: String = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "root".to_string())
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .take(32)
        .collect();
    let hash = fnv1a(&path_bytes(root));
    let stores = match scope {
        StoreScope::Full => "stores",
        StoreScope::Limited => "stores-limited",
    };
    home.join("Library")
        .join("Application Support")
        .join(APP_FOLDER)
        .join(stores)
        .join(format!("{label}-{hash:016x}.fxstore"))
}

/// The stores the command tries for `root` when `-store` is not given, in
/// order: the full store, then the limited one, both in [`APP_FOLDER`].
pub fn store_candidates(home: &Path, root: &Path) -> [(StoreScope, PathBuf); 2] {
    [StoreScope::Full, StoreScope::Limited].map(|scope| (scope, store_path_for(home, root, scope)))
}

/// The current user's home folder, from `$HOME`, if set and non-empty.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

#[cfg(unix)]
fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn path_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().into_owned().into_bytes()
}

/// 64-bit FNV-1a, stable across Rust versions and launches.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}
