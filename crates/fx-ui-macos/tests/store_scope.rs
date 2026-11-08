//! Contract tests for the store scope: each checkpoint
//! store remembers whether it was built in limited (non-FDA) mode or with
//! Full Disk Access, so that launching with access granted over a store built
//! in limited mode starts a fresh full scan and never replays the limited
//! store's cursor (whose snapshot is missing the protected folders).
//!
//! The contract, in `fx_ui_macos::app_args`:
//!
//! - `StoreScope { Limited, Full }`, with `StoreScope::for_access(access)`:
//!   `Granted` selects `Full`; `Denied` and `Unknown` (both limited mode, as
//!   in `onboarding`) select `Limited`.
//! - `store_path_for(home, root, scope)`: one store path per root and scope.
//!   `Limited` and `Full` never share a path, and the `Full` path is
//!   `store_path(home, root)`, the path stores had before they had a scope,
//!   so those stores keep loading.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use fx_fs::AccessState;
use fx_ui_macos::app_args::{StoreScope, store_path, store_path_for};

const HOME: &str = "/Users/tester";

fn roots() -> Vec<PathBuf> {
    [
        "/",
        "/Users/tester",
        "/Users/tester/Projects",
        "/Users/tester/Projects/sub",
        "/Volumes/Data",
    ]
    .iter()
    .map(PathBuf::from)
    .collect()
}

#[test]
fn granted_selects_full_and_everything_else_limited() {
    assert_eq!(
        StoreScope::for_access(AccessState::Granted),
        StoreScope::Full
    );
    assert_eq!(
        StoreScope::for_access(AccessState::Denied),
        StoreScope::Limited
    );
    assert_eq!(
        StoreScope::for_access(AccessState::Unknown),
        StoreScope::Limited
    );
}

#[test]
fn limited_and_full_give_different_paths() {
    let home = Path::new(HOME);
    for root in roots() {
        let limited = store_path_for(home, &root, StoreScope::Limited);
        let full = store_path_for(home, &root, StoreScope::Full);
        assert_ne!(
            limited, full,
            "limited and full stores for {root:?} share a path"
        );
    }
}

#[test]
fn full_path_is_the_run_1_9_store_path() {
    for home in [Path::new(HOME), Path::new("/Users/other")] {
        for root in roots() {
            assert_eq!(
                store_path_for(home, &root, StoreScope::Full),
                store_path(home, &root),
                "the full-access store for {root:?} moved; stores saved before the scope split would be lost"
            );
        }
    }
}

#[test]
fn limited_path_is_stable_and_distinct_per_root() {
    let home = Path::new(HOME);
    let paths: Vec<PathBuf> = roots()
        .iter()
        .map(|r| store_path_for(home, r, StoreScope::Limited))
        .collect();
    for (i, root) in roots().iter().enumerate() {
        assert_eq!(
            store_path_for(home, root, StoreScope::Limited),
            paths[i],
            "limited store path for {root:?} is not stable across calls"
        );
    }
    for i in 0..paths.len() {
        for j in (i + 1)..paths.len() {
            assert_ne!(paths[i], paths[j], "two roots share a limited store");
        }
    }
}

#[test]
fn no_limited_store_collides_with_any_full_store() {
    let home = Path::new(HOME);
    let full: Vec<PathBuf> = roots()
        .iter()
        .map(|r| store_path_for(home, r, StoreScope::Full))
        .collect();
    for root in roots() {
        let limited = store_path_for(home, &root, StoreScope::Limited);
        assert!(
            !full.contains(&limited),
            "limited store for {root:?} is also some root's full store: {limited:?}"
        );
    }
}

#[test]
fn limited_store_lives_in_the_app_support_folder_of_home() {
    let home = Path::new(HOME);
    let p = store_path_for(home, Path::new("/"), StoreScope::Limited);
    assert!(
        p.starts_with(home.join("Library").join("Application Support")),
        "limited store {p:?} is not under ~/Library/Application Support"
    );
    assert!(
        p.file_name().is_some(),
        "limited store {p:?} has no file name"
    );
    let other = store_path_for(
        Path::new("/Users/other"),
        Path::new("/"),
        StoreScope::Limited,
    );
    assert_ne!(p, other, "limited store does not depend on the home folder");
}

/// A fresh, empty temporary home folder for one test.
fn temp_home(tag: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "fx-store-scope-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp home");
    dir
}

fn write_store(path: &Path) {
    std::fs::create_dir_all(path.parent().expect("store has a parent")).expect("store dir");
    std::fs::write(path, b"checkpoint built in this scope").expect("write store");
}

#[test]
fn granted_over_a_limited_store_selects_a_fresh_scan() {
    let home = temp_home("granted");
    let root = Path::new("/");

    // An earlier launch without Full Disk Access left a limited store.
    let limited = store_path_for(&home, root, StoreScope::for_access(AccessState::Denied));
    write_store(&limited);

    // Launching with access granted must not open that store: the selected
    // path has no store yet, so the engine starts with a full first scan.
    let selected = store_path_for(&home, root, StoreScope::for_access(AccessState::Granted));
    assert_ne!(selected, limited, "granted launch reuses the limited store");
    assert!(
        !selected.exists(),
        "granted launch over a limited store found a store at {selected:?} instead of scanning fresh"
    );
    assert!(
        limited.exists(),
        "selecting the full scope touched the limited store"
    );

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn each_scope_finds_its_own_store_again() {
    let home = temp_home("reopen");
    let root = Path::new("/Users/tester/Projects");

    let limited = store_path_for(&home, root, StoreScope::Limited);
    let full = store_path_for(&home, root, StoreScope::Full);
    write_store(&full);
    write_store(&limited);

    // A granted relaunch reopens the full store; a denied (or unknown) one
    // reopens the limited store. Neither picks up the other's.
    for (access, expected) in [
        (AccessState::Granted, &full),
        (AccessState::Denied, &limited),
        (AccessState::Unknown, &limited),
    ] {
        let selected = store_path_for(&home, root, StoreScope::for_access(access));
        assert_eq!(&selected, expected, "{access:?} selected the wrong store");
        assert!(selected.exists());
    }

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn denied_over_a_full_store_does_not_reuse_it() {
    // Access revoked after a full scan: the full snapshot would show
    // protected folders the limited change stream can no longer keep
    // current, so limited mode builds its own store.
    let home = temp_home("revoked");
    let root = Path::new("/");
    let full = store_path_for(&home, root, StoreScope::Full);
    write_store(&full);

    let selected = store_path_for(&home, root, StoreScope::for_access(AccessState::Denied));
    assert_ne!(selected, full);
    assert!(!selected.exists());

    let _ = std::fs::remove_dir_all(&home);
}
