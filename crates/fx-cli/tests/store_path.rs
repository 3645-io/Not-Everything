//! The CLI finds the store the app saved.
//!
//! `fx_cli::store_path::default_store_path(home: &Path, root: &Path) ->
//! PathBuf` must give exactly what the app's
//! `fx_ui_macos::app_args::store_path` gives (the full-access scope), so
//! `not-everything` with no `-store` reads the app's index. The CLI mirrors
//! that code instead of depending on the UI crate; this test (through a
//! `[dev-dependencies]` entry on `fx-ui-macos`) keeps the two in step.
//!
//! The store scope: `fx_cli::store_path::store_path_for(home, root,
//! StoreScope)` with `StoreScope::Full` and `StoreScope::Limited` must give
//! what `fx_ui_macos::app_args::store_path_for` gives for the same scope. A
//! limited store (built without Full Disk Access) has the same file name in
//! a sibling `stores-limited` folder; the full scope is
//! `default_store_path`.
//!
//! `fx_cli::store_path::store_candidates(home, root) -> [(StoreScope,
//! PathBuf); 2]` lists the stores the command tries, in order: the full
//! store, then the limited one, both in the app's own data folder and
//! nowhere else.

use std::path::{Path, PathBuf};

use fx_channel::Channel;
use fx_cli::store_path::{
    APP_FOLDER as CLI_APP_FOLDER, StoreScope, default_store_path, store_candidates, store_path_for,
};
use fx_ui_macos::app_args::{
    APP_FOLDER, DEFAULT_ROOT, StoreScope as UiScope, store_path as ui_store_path,
    store_path_for as ui_store_path_for,
};

const SCOPES: &[(StoreScope, UiScope)] = &[
    (StoreScope::Full, UiScope::Full),
    (StoreScope::Limited, UiScope::Limited),
];

const HOMES: &[&str] = &["/Users/tester", "/Users/someone else", "/var/root"];

const ROOTS: &[&str] = &[
    "/",
    "/Users/tester",
    "/Users/tester/Projects",
    "/Users/tester/Projects/sub",
    "/Volumes/Data",
    "/Volumes/My Disk (2)",
    "/Users/tester/Café ünïcode",
    "/Users/tester/a-very-long-folder-name-that-exceeds-thirty-two-characters",
    "/tmp/dots.and-dashes_here",
    "relative/dir",
];

#[test]
fn matches_the_ui_store_path_for_every_root() {
    for home in HOMES {
        for root in ROOTS {
            let home = Path::new(home);
            let root = Path::new(root);
            assert_eq!(
                default_store_path(home, root),
                ui_store_path(home, root),
                "home {home:?}, root {root:?}"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn matches_the_ui_store_path_for_a_non_utf8_root() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let home = Path::new("/Users/tester");
    let root = Path::new(OsStr::from_bytes(b"/Volumes/bad\xffname"));
    assert_eq!(default_store_path(home, root), ui_store_path(home, root));
}

#[test]
fn matches_the_ui_store_path_for_the_default_root() {
    let home = Path::new("/Users/tester");
    let root = Path::new(DEFAULT_ROOT);
    assert_eq!(default_store_path(home, root), ui_store_path(home, root));
}

/// FNV-1a 64, written out here so the layout is pinned independently too.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn stores_dir(home: &Path) -> PathBuf {
    home.join("Library")
        .join("Application Support")
        .join(APP_FOLDER)
        .join("stores")
}

#[test]
fn reference_hash_is_fnv1a_64() {
    // Published FNV-1a 64-bit test vectors.
    assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
}

#[test]
fn layout_for_the_volume_root() {
    let home = Path::new("/Users/tester");
    let expected = stores_dir(home).join(format!("root-{:016x}.fxstore", fnv1a(b"/")));
    assert_eq!(default_store_path(home, Path::new("/")), expected);
}

#[test]
fn layout_for_a_folder_root() {
    let home = Path::new("/Users/tester");
    let root = "/Users/tester/My Projects.v2";
    let expected = stores_dir(home).join(format!(
        "My_Projects_v2-{:016x}.fxstore",
        fnv1a(root.as_bytes())
    ));
    assert_eq!(default_store_path(home, Path::new(root)), expected);
}

#[test]
fn label_is_ascii_and_at_most_32_characters() {
    let home = Path::new("/Users/tester");
    let root = "/x/Ünïcode-and-a-name-well-over-thirty-two-chars";
    let p = default_store_path(home, Path::new(root));
    let file = p.file_name().unwrap().to_str().unwrap().to_string();
    let hash = format!("-{:016x}.fxstore", fnv1a(root.as_bytes()));
    let label = file.strip_suffix(&hash).expect("ends with -<hash>.fxstore");
    assert_eq!(label.chars().count(), 32, "{label}");
    assert!(
        label.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
        "{label}"
    );
    assert!(label.starts_with("_n_code_and_a_name"), "{label}");
    assert_eq!(p.parent(), Some(stores_dir(home).as_path()));
}

#[test]
fn distinct_roots_get_distinct_stores() {
    let home = Path::new("/Users/tester");
    let a = default_store_path(home, Path::new("/Users/tester/Projects"));
    let b = default_store_path(home, Path::new("/Volumes/Backup/Projects"));
    assert_ne!(a, b);
    assert_eq!(
        a,
        default_store_path(home, Path::new("/Users/tester/Projects"))
    );
}

// ------------------------------------------------------------ scopes

#[test]
fn matches_the_ui_store_path_for_every_scope_and_root() {
    for &(scope, ui_scope) in SCOPES {
        for home in HOMES {
            for root in ROOTS.iter().chain(&[DEFAULT_ROOT]) {
                let home = Path::new(home);
                let root = Path::new(root);
                assert_eq!(
                    store_path_for(home, root, scope),
                    ui_store_path_for(home, root, ui_scope),
                    "scope {scope:?}, home {home:?}, root {root:?}"
                );
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn matches_the_ui_store_path_for_every_scope_and_a_non_utf8_root() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let home = Path::new("/Users/tester");
    let root = Path::new(OsStr::from_bytes(b"/Volumes/bad\xffname"));
    for &(scope, ui_scope) in SCOPES {
        assert_eq!(
            store_path_for(home, root, scope),
            ui_store_path_for(home, root, ui_scope),
            "scope {scope:?}"
        );
    }
}

#[test]
fn the_full_scope_is_the_default_store_path() {
    for home in HOMES {
        for root in ROOTS {
            let home = Path::new(home);
            let root = Path::new(root);
            assert_eq!(
                store_path_for(home, root, StoreScope::Full),
                default_store_path(home, root),
                "home {home:?}, root {root:?}"
            );
        }
    }
}

#[test]
fn the_limited_scope_has_the_same_file_in_stores_limited() {
    let home = Path::new("/Users/tester");
    for root in ROOTS {
        let root = Path::new(root);
        let full = store_path_for(home, root, StoreScope::Full);
        let limited = store_path_for(home, root, StoreScope::Limited);
        assert_ne!(full, limited, "root {root:?}");
        assert_eq!(limited.file_name(), full.file_name(), "root {root:?}");
        assert_eq!(
            limited.parent(),
            Some(
                home.join("Library")
                    .join("Application Support")
                    .join(APP_FOLDER)
                    .join("stores-limited")
                    .as_path()
            ),
            "root {root:?}"
        );
    }
    let expected = home
        .join("Library")
        .join("Application Support")
        .join(APP_FOLDER)
        .join("stores-limited")
        .join(format!("root-{:016x}.fxstore", fnv1a(b"/")));
    assert_eq!(
        store_path_for(home, Path::new("/"), StoreScope::Limited),
        expected
    );
}

#[test]
fn store_scope_is_a_plain_copyable_value() {
    let scope = StoreScope::Limited;
    let copy = scope;
    assert_eq!(scope, copy);
    assert_ne!(StoreScope::Full, StoreScope::Limited);
}

/// The stores live in the app's data folder: `Not-Everything`, or
/// `Not-Everything Preview` for a Preview build's command.
#[test]
fn app_folder_is_the_channel_data_folder() {
    assert_eq!(CLI_APP_FOLDER, Channel::CURRENT.data_folder());
    assert_eq!(CLI_APP_FOLDER, APP_FOLDER);
}

/// The command tries the full store, then the limited one, and only in the
/// app's own data folder.
#[test]
fn the_command_tries_the_full_then_the_limited_store_in_the_app_folder() {
    for home in HOMES {
        for root in ROOTS.iter().chain(&[DEFAULT_ROOT]) {
            let (home, root) = (Path::new(home), Path::new(root));
            let candidates = store_candidates(home, root);
            assert_eq!(
                candidates,
                [
                    (
                        StoreScope::Full,
                        store_path_for(home, root, StoreScope::Full)
                    ),
                    (
                        StoreScope::Limited,
                        store_path_for(home, root, StoreScope::Limited)
                    ),
                ],
                "home {home:?}, root {root:?}"
            );
            let folder = home
                .join("Library")
                .join("Application Support")
                .join(CLI_APP_FOLDER);
            for (scope, path) in &candidates {
                assert!(path.starts_with(&folder), "{scope:?}: {path:?}");
            }
        }
    }
}
