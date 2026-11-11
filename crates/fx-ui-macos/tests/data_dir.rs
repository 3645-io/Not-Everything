//! Contract tests for where the app keeps its data: the stores and the
//! settings file live in one data folder.
//!
//! - `app_args::app_data_folder(home)`: the default data folder,
//!   `<home>/Library/Application Support/<APP_FOLDER>/`.
//! - `app_args::store_path_in(data, root, scope)`: a root's store in the
//!   data folder `data`, in `stores/` (Full Disk Access) or
//!   `stores-limited/`. `store_path_for(home, root, scope)` is the same in
//!   the default data folder.
//! - `settings::settings_path_in(data)`: `data/settings`.
//!   `settings_path(home)` is the same in the default data folder.
//! - With the `measure` feature, `--data-dir DIR` makes `DIR` the data
//!   folder (`AppArgs::data_folder`), so a measurement run never touches the
//!   daily app's index or settings.

use std::path::{Path, PathBuf};

use fx_ui_macos::app_args::{
    APP_FOLDER, AppArgs, StoreScope, app_data_folder, store_path, store_path_for, store_path_in,
};
use fx_ui_macos::settings::{SETTINGS_FILE_NAME, settings_path, settings_path_in};

const HOME: &str = "/Users/me";

fn roots() -> Vec<PathBuf> {
    ["/", "/Users/me", "/Users/me/Projects", "/Volumes/Data"]
        .iter()
        .map(PathBuf::from)
        .collect()
}

const SCOPES: [StoreScope; 2] = [StoreScope::Full, StoreScope::Limited];

#[test]
fn the_default_data_folder_is_under_application_support() {
    assert_eq!(
        app_data_folder(Path::new(HOME)),
        PathBuf::from("/Users/me/Library/Application Support").join(APP_FOLDER)
    );
}

#[test]
fn stores_live_in_the_data_folder() {
    let data = Path::new("/Users/me/measure-data");
    for root in roots() {
        let full = store_path_in(data, &root, StoreScope::Full);
        let limited = store_path_in(data, &root, StoreScope::Limited);
        assert_eq!(full.parent(), Some(data.join("stores").as_path()));
        assert_eq!(
            limited.parent(),
            Some(data.join("stores-limited").as_path())
        );
        assert_ne!(full, limited, "{}", root.display());
    }
}

#[test]
fn the_default_stores_are_the_stores_of_the_default_data_folder() {
    let home = Path::new(HOME);
    let data = app_data_folder(home);
    for root in roots() {
        for scope in SCOPES {
            assert_eq!(
                store_path_for(home, &root, scope),
                store_path_in(&data, &root, scope),
                "{} {scope:?}",
                root.display()
            );
        }
        assert_eq!(
            store_path(home, &root),
            store_path_in(&data, &root, StoreScope::Full)
        );
    }
}

#[test]
fn a_store_keeps_its_file_name_in_any_data_folder() {
    let home = Path::new(HOME);
    let data = Path::new("/Users/me/measure-data");
    for root in roots() {
        for scope in SCOPES {
            assert_eq!(
                store_path_in(data, &root, scope).file_name(),
                store_path_for(home, &root, scope).file_name()
            );
        }
    }
}

#[test]
fn settings_live_in_the_data_folder() {
    let data = Path::new("/Users/me/measure-data");
    assert_eq!(settings_path_in(data), data.join(SETTINGS_FILE_NAME));
    let home = Path::new(HOME);
    assert_eq!(
        settings_path(home),
        settings_path_in(&app_data_folder(home))
    );
}

#[test]
fn without_a_data_dir_the_app_uses_the_default_data_folder() {
    let home = Path::new(HOME);
    let args = AppArgs::default();
    assert_eq!(args.data_dir(), None);
    assert_eq!(args.data_folder(home), app_data_folder(home));
}

/// `--data-dir DIR` moves the stores and the settings into `DIR`: none of
/// them is left under the home folder.
#[cfg(feature = "measure")]
#[test]
fn data_dir_moves_the_stores_and_the_settings() {
    let dir = std::env::temp_dir().join(format!("fx-ui-data-dir-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let parsed = AppArgs::parse(["--data-dir".to_string(), dir.to_string_lossy().into_owned()]);
    let _ = std::fs::remove_dir(&dir);
    let args = parsed.expect("an existing folder parses");
    let home = Path::new(HOME);
    let data = args.data_folder(home);
    assert_eq!(data, dir);
    for root in roots() {
        for scope in SCOPES {
            let store = store_path_in(&data, &root, scope);
            assert!(store.starts_with(&dir), "{}", store.display());
            assert!(!store.starts_with(home), "{}", store.display());
        }
    }
    let settings = settings_path_in(&data);
    assert!(settings.starts_with(&dir));
    assert!(!settings.starts_with(home));
}
