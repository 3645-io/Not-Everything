//! "Your files" for the **Your files first** setting: the folders
//! whose results the window lists before the rest, under every sort.
//!
//! Your folders are the home folder outside `~/Library`, plus iCloud Drive
//! (`~/Library/Mobile Documents/com~apple~CloudDocs`) and the whole
//! `~/Library/CloudStorage`, where Google Drive, OneDrive, Dropbox and any
//! cloud drive added later live. The engine takes them as priority folders
//! ([`fx_engine::EngineConfig::with_priority_folders`]): the nearest listed
//! folder at or above an entry decides. Hidden entries (names starting with
//! `.`, such as `~/.cache` or `.DS_Store`) and everything inside a hidden
//! folder are never yours; the engine leaves them out wherever they are.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

/// iCloud Drive, under the home folder.
pub const ICLOUD_DRIVE: &str = "Library/Mobile Documents/com~apple~CloudDocs";

/// The folder holding every cloud drive (File Provider), under the home
/// folder.
pub const CLOUD_STORAGE: &str = "Library/CloudStorage";

/// The folders that start (`include`) and end (`exclude`) your files.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct YourFolders {
    /// The home folder, iCloud Drive and `~/Library/CloudStorage`.
    pub include: Vec<PathBuf>,
    /// `~/Library`.
    pub exclude: Vec<PathBuf>,
}

/// Your folders for the home folder `home`.
pub fn your_folders(home: &Path) -> YourFolders {
    YourFolders {
        include: vec![
            home.to_path_buf(),
            home.join(ICLOUD_DRIVE),
            home.join(CLOUD_STORAGE),
        ],
        exclude: vec![home.join("Library")],
    }
}
