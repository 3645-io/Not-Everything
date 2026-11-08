//! The app's data folder, `~/Library/Application Support/Not-Everything/`
//! ([`crate::app_args::APP_FOLDER`]), is its owner's only.
//!
//! It holds the index, with the names of files in folders macOS protects
//! (Desktop, Documents, Mail and the like, once Full Disk Access is
//! granted), and the settings. So the app makes its folders private with
//! fx-store's [`make_dir_private`] (mode 0700): at launch the data folder
//! and the stores folder ([`make_store_folders_private`]), and the settings
//! folder on every save. A folder an earlier version left open is tightened
//! then. The app writes its files with mode 0600 ([`create_private_file`]);
//! the store writes its own the same way. Other users' programs then cannot
//! list or read them.
//!
//! The store's saves create a missing folder 0700 but never change one that
//! exists, so tightening the app's existing folders is the app's job.
//!
//! On hosts without Unix modes these create the folders and files plainly.

#![forbid(unsafe_code)]

use std::fs;
use std::io;
use std::path::Path;

/// The mode of the data folder and the folders in it, 0700: the owner may
/// read, write and enter them; nobody else may.
pub use fx_store::PRIVATE_DIR_MODE;

/// The mode of the files in the data folder, 0600: the owner may read and
/// write them; nobody else may.
pub use fx_store::PRIVATE_FILE_MODE;

/// Creates a folder and any missing parents with mode 0700, and sets the
/// folder itself to 0700 if it already existed with another mode.
/// Existing parents keep their mode.
pub use fx_store::make_dir_private;

/// Makes the folders of the store file `store` private, as the app does at
/// launch: the folder holding it (`stores` or `stores-limited`) and the
/// data folder above that, each with [`make_dir_private`]. The store file
/// itself is left to the store, which writes it 0600.
///
/// Both folders are tried, also when the first fails.
///
/// # Errors
///
/// The first folder that could not be created or tightened.
pub fn make_store_folders_private(store: &Path) -> io::Result<()> {
    let Some(stores) = store.parent().filter(|d| !d.as_os_str().is_empty()) else {
        return Ok(());
    };
    let data = match stores.parent().filter(|d| !d.as_os_str().is_empty()) {
        Some(data) => make_dir_private(data),
        None => Ok(()),
    };
    let stores = make_dir_private(stores);
    data.and(stores)
}

/// Opens `path` for writing, created or truncated, with mode
/// [`PRIVATE_FILE_MODE`], also when it already existed with another mode.
///
/// # Errors
///
/// Opening the file or changing its mode failed.
pub fn create_private_file(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        options.mode(PRIVATE_FILE_MODE);
        let file = options.open(path)?;
        // The mode above applies only to a new file.
        file.set_permissions(fs::Permissions::from_mode(PRIVATE_FILE_MODE))?;
        Ok(file)
    }
    #[cfg(not(unix))]
    {
        options.open(path)
    }
}
