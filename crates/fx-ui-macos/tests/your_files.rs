//! Tests for `fx_ui_macos::your_files`: the folders that make up
//! "your files" for the **Your files first** setting.
//!
//! Contract exercised here:
//!
//! - `your_folders(home) -> YourFolders { include, exclude }`: `include` is
//!   the home folder, iCloud Drive
//!   (`~/Library/Mobile Documents/com~apple~CloudDocs`) and the whole
//!   `~/Library/CloudStorage`; `exclude` is `~/Library`. Hidden entries
//!   (names starting with `.`, such as `~/.cache` or `.DS_Store`) are left
//!   out by the engine wherever they are, so they are not listed here.

use std::path::{Path, PathBuf};

use fx_ui_macos::your_files::{YourFolders, your_folders};

fn paths(list: &[&str]) -> Vec<PathBuf> {
    list.iter().map(PathBuf::from).collect()
}

#[test]
fn home_icloud_drive_and_cloud_storage_start_library_ends() {
    assert_eq!(
        your_folders(Path::new("/Users/me")),
        YourFolders {
            include: paths(&[
                "/Users/me",
                "/Users/me/Library/Mobile Documents/com~apple~CloudDocs",
                "/Users/me/Library/CloudStorage",
            ]),
            exclude: paths(&["/Users/me/Library"]),
        }
    );
}
