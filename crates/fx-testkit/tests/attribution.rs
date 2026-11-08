//! Asset attribution tests: every asset file in the repository (anything
//! under a crate's `assets/` folder or `docs/img/`) has a row in
//! `ATTRIBUTION.md` naming it, with its author, source and licence, added
//! in the same change as the file; and every file the table names exists.
//! Outside a git work tree (a source archive) the tests that list the
//! tracked assets pass with a note. Only `std` is used.

mod common;

use common::{git_ls_files, in_git_work_tree, read, repo_root};

const ATTRIBUTION: &str = "ATTRIBUTION.md";

/// The tracked asset files, or `None` outside a git work tree.
fn asset_files() -> Option<Vec<String>> {
    if !in_git_work_tree("the asset attribution check") {
        return None;
    }
    Some(
        git_ls_files(&[])
            .into_iter()
            .filter(|p| {
                (p.starts_with("crates/") && p.contains("/assets/")) || p.starts_with("docs/img/")
            })
            .collect(),
    )
}

/// The paths the table names: the first cell of each row, in backticks.
fn attributed_paths(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.strip_prefix("| `"))
        .filter_map(|l| l.split_once('`').map(|(path, _)| path.to_owned()))
        .collect()
}

#[test]
fn the_repository_has_assets() {
    let Some(assets) = asset_files() else {
        return;
    };
    assert!(!assets.is_empty(), "no asset files found");
}

#[test]
fn every_asset_has_an_attribution_row() {
    let Some(assets) = asset_files() else {
        return;
    };
    let path = repo_root().join(ATTRIBUTION);
    assert!(path.is_file(), "{ATTRIBUTION} must exist");
    let named = attributed_paths(&read(&path));
    let missing: Vec<String> = assets.into_iter().filter(|f| !named.contains(f)).collect();
    assert!(
        missing.is_empty(),
        "assets without a row in {ATTRIBUTION}:\n  {}",
        missing.join("\n  ")
    );
}

#[test]
fn every_attribution_row_names_an_existing_file() {
    let path = repo_root().join(ATTRIBUTION);
    assert!(path.is_file(), "{ATTRIBUTION} must exist");
    let gone: Vec<String> = attributed_paths(&read(&path))
        .into_iter()
        .filter(|p| !repo_root().join(p).is_file())
        .collect();
    assert!(
        gone.is_empty(),
        "{ATTRIBUTION} names files that don't exist:\n  {}",
        gone.join("\n  ")
    );
}
