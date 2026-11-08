//! "Your files first": the priority group of a published snapshot
//! and the grouped search order.
//!
//! Contract these tests rely on (in addition to `tests/common/mod.rs`):
//!
//! - `EngineConfig::with_priority_folders(include, exclude)` takes absolute
//!   folder paths (any `IntoIterator` of `Into<PathBuf>`). An entry is in the
//!   priority group when the nearest listed folder at or above it is in
//!   `include`; a folder listed in both counts as excluded. A listed folder
//!   at or above the engine's root decides for the root entry. Folders that
//!   do not exist (yet) are skipped and picked up once they appear.
//! - Hidden entries are never in the group: an entry whose name starts with
//!   `.`, and everything inside a hidden folder, wherever it is (the
//!   owner's rule). This covers the dot-folders in home, so the test
//!   config lists only `~/Library` to exclude.
//! - Every published snapshot carries the group for its own index:
//!   `EngineSnapshot::priority_group() -> Option<&fx_sort::MatchSet>`,
//!   `None` without priority folders.
//! - `Engine::search_grouped(query, sort, direction, priority_first, offset,
//!   limit)`: with `priority_first` the matches in the group come first in
//!   `sort`/`direction` order, then the other matches in the same order; the
//!   total is unchanged. Without it, or without priority folders, it is
//!   `Engine::search`.
//! - `EngineSnapshot::memory_report()` lists the group under
//!   `priority_group`.
//! - `Engine::positions_grouped(query, sort, direction, priority_first,
//!   paths)`, the grouped form of `Engine::positions`: each path's
//!   row in the results of `search_grouped` with the same flag; without the
//!   flag it is `Engine::positions`.

mod common;

use std::collections::HashSet;

use common::*;
use fx_engine::{Engine, EngineSnapshot, Page};
use fx_model::{EntryId, StartOutcome};
use fx_sort::{Direction, SortKey};

const HOME: &str = "/vol/Users/me";
const LIBRARY: &str = "/vol/Users/me/Library";
const CLOUD_STORAGE: &str = "/vol/Users/me/Library/CloudStorage";
const ICLOUD_DRIVE: &str = "/vol/Users/me/Library/Mobile Documents/com~apple~CloudDocs";
const CACHE: &str = "/vol/Users/me/.cache";
const GIT_OBJECTS: &str = "/vol/Users/me/Documents/.git/objects";
const DRIVE_TMP: &str = "/vol/Users/me/Library/CloudStorage/GoogleDrive-me/.tmp.drivedownload";

const KEYS: [SortKey; 5] = [
    SortKey::Name,
    SortKey::Path,
    SortKey::Size,
    SortKey::Modified,
    SortKey::Created,
];
const DIRECTIONS: [Direction; 2] = [Direction::Ascending, Direction::Descending];

/// A home folder with the places the owner reported, plus system folders.
fn home_tree() -> FakeTree {
    let tree = FakeTree::new();
    for dir in [
        "/vol/Users",
        HOME,
        "/vol/Users/me/Desktop",
        "/vol/Users/me/Downloads",
        "/vol/Users/me/Documents",
        "/vol/Users/me/Documents/.git",
        GIT_OBJECTS,
        LIBRARY,
        "/vol/Users/me/Library/LanguageModeling",
        CLOUD_STORAGE,
        "/vol/Users/me/Library/CloudStorage/GoogleDrive-me",
        DRIVE_TMP,
        "/vol/Users/me/Library/Mobile Documents",
        ICLOUD_DRIVE,
        "/vol/Users/me/Library/Mobile Documents/iCloud~app",
        CACHE,
        "/vol/opt",
        "/vol/opt/homebrew",
        "/vol/Applications",
        "/vol/Applications/Plumb.app",
    ] {
        tree.mkdir(dir);
    }
    for (file, size) in [
        ("/vol/Users/me/Desktop/plumb-desk.pdf", 40),
        ("/vol/Users/me/Downloads/Plumb guide.pdf", 900),
        ("/vol/Users/me/Library/LanguageModeling/plumb-lm.dat", 5),
        (
            "/vol/Users/me/Library/CloudStorage/GoogleDrive-me/plumb-drive.doc",
            300,
        ),
        (
            "/vol/Users/me/Library/Mobile Documents/com~apple~CloudDocs/plumb-icloud.txt",
            7,
        ),
        (
            "/vol/Users/me/Library/Mobile Documents/iCloud~app/plumb-app.txt",
            8,
        ),
        ("/vol/Users/me/.cache/plumb-cache.bin", 2_000),
        // Hidden entries inside your folders.
        ("/vol/Users/me/Desktop/.DS_Store", 6_148),
        ("/vol/Users/me/Documents/notes.txt", 20),
        ("/vol/Users/me/Documents/.git/objects/plumb-obj", 90),
        ("/vol/Users/me/.zshrc", 30),
        ("/vol/Users/me/.plumbrc", 31),
        (
            "/vol/Users/me/Library/CloudStorage/GoogleDrive-me/.tmp.drivedownload/plumb-dl.part",
            700,
        ),
        ("/vol/opt/homebrew/plumb.rb", 12),
        ("/vol/Applications/Plumb.app/plumb-res.png", 64),
    ] {
        tree.touch(file, size);
    }
    tree
}

fn your_files_config() -> fx_engine::EngineConfig {
    config().with_priority_folders([HOME, ICLOUD_DRIVE, CLOUD_STORAGE], [LIBRARY])
}

fn started(tree: &FakeTree, source: &FakeSource) -> Engine {
    let engine = start(tree, source, tree.build_index(), your_files_config());
    wait_for_tree(&engine, tree, "initial index");
    engine
}

fn grouped(
    engine: &Engine,
    q: &str,
    sort: SortKey,
    dir: Direction,
    off: usize,
    lim: usize,
) -> Page {
    engine
        .search_grouped(
            q,
            fx_engine::QueryOptions::default(),
            sort,
            dir,
            true,
            off,
            lim,
        )
        .expect("search succeeds")
}

fn plain(engine: &Engine, q: &str, sort: SortKey, dir: Direction, off: usize, lim: usize) -> Page {
    engine
        .search(q, fx_engine::QueryOptions::default(), sort, dir, off, lim)
        .expect("search succeeds")
}

fn id_of(snap: &EngineSnapshot, tree: &FakeTree, path: &str) -> EntryId {
    let key = tree.key_of(path).unwrap_or_else(|| panic!("{path} exists"));
    snap.index()
        .lookup(key)
        .unwrap_or_else(|| panic!("{path} is indexed"))
}

fn names(snap: &EngineSnapshot, ids: &[u32]) -> Vec<String> {
    ids.iter()
        .map(|&id| snap.index().name(EntryId(id)).to_string())
        .collect()
}

/// The names of `paths`' last components, as a set.
fn name_set(paths: &[&str]) -> HashSet<String> {
    paths
        .iter()
        .map(|p| p.rsplit('/').next().unwrap().to_string())
        .collect()
}

/// Checks the grouped order against the plain one for every key and
/// direction: the plain matches in `first` (by name), then the rest, each
/// part in plain order.
fn assert_grouped_everywhere(engine: &Engine, query: &str, first: &HashSet<String>) {
    let snap = engine.snapshot();
    for sort in KEYS {
        for dir in DIRECTIONS {
            let all = plain(engine, query, sort, dir, 0, 1000);
            let got = grouped(engine, query, sort, dir, 0, 1000);
            assert_eq!(got.generation, all.generation, "{sort:?} {dir:?}");
            assert_eq!(got.generation, snap.generation(), "{sort:?} {dir:?}");
            assert_eq!(got.total, all.total, "{sort:?} {dir:?}: total unchanged");
            let in_first = |&id: &u32| first.contains(snap.index().name(EntryId(id)));
            let mut want: Vec<u32> = all.ids.iter().copied().filter(in_first).collect();
            want.extend(all.ids.iter().copied().filter(|id| !in_first(id)));
            assert_eq!(
                names(&snap, &got.ids),
                names(&snap, &want),
                "{sort:?} {dir:?}: your files first, then the rest, each in plain order"
            );
            // Pages are windows of the same grouped order.
            for (offset, limit) in [(0, 2), (1, 3), (3, 4), (want.len().saturating_sub(1), 5)] {
                let page = grouped(engine, query, sort, dir, offset, limit);
                let start = offset.min(want.len());
                let end = (offset + limit).min(want.len());
                assert_eq!(page.ids, want[start..end], "{sort:?} {dir:?} page {offset}");
                assert_eq!(page.total, want.len());
            }
        }
    }
}

#[test]
fn desktop_and_cloud_drives_come_first_library_and_dot_folders_do_not() {
    let tree = home_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = started(&tree, &source);
    let snap = engine.snapshot();
    let group = snap.priority_group().expect("priority folders are set");

    for path in [
        HOME,
        "/vol/Users/me/Desktop",
        "/vol/Users/me/Desktop/plumb-desk.pdf",
        "/vol/Users/me/Downloads/Plumb guide.pdf",
        CLOUD_STORAGE,
        "/vol/Users/me/Library/CloudStorage/GoogleDrive-me/plumb-drive.doc",
        ICLOUD_DRIVE,
        "/vol/Users/me/Library/Mobile Documents/com~apple~CloudDocs/plumb-icloud.txt",
    ] {
        assert!(group.contains(id_of(&snap, &tree, path)), "{path} is yours");
    }
    for path in [
        ROOT,
        "/vol/Users",
        LIBRARY,
        "/vol/Users/me/Library/LanguageModeling/plumb-lm.dat",
        "/vol/Users/me/Library/Mobile Documents",
        "/vol/Users/me/Library/Mobile Documents/iCloud~app/plumb-app.txt",
        CACHE,
        "/vol/Users/me/.cache/plumb-cache.bin",
        "/vol/opt/homebrew/plumb.rb",
        "/vol/Applications/Plumb.app/plumb-res.png",
    ] {
        assert!(
            !group.contains(id_of(&snap, &tree, path)),
            "{path} is not yours"
        );
    }

    let first = name_set(&[
        "plumb-desk.pdf",
        "Plumb guide.pdf",
        "plumb-drive.doc",
        "plumb-icloud.txt",
    ]);
    let page = grouped(
        &engine,
        "plumb",
        SortKey::Name,
        Direction::Ascending,
        0,
        100,
    );
    assert_eq!(page.total, 13);
    let top: HashSet<String> = names(&snap, &page.ids[..4]).into_iter().collect();
    assert_eq!(top, first);
    assert_grouped_everywhere(&engine, "plumb", &first);

    // The empty query lists everything, your folders first.
    let all = grouped(
        &engine,
        "",
        SortKey::Name,
        Direction::Ascending,
        0,
        usize::MAX,
    );
    assert_eq!(all.total, snap.index().live_count());
    let k = group.len();
    assert!(all.ids[..k].iter().all(|&id| group.contains(EntryId(id))));
    assert!(all.ids[k..].iter().all(|&id| !group.contains(EntryId(id))));
    engine.shutdown();
}

#[test]
fn plain_search_ignores_the_group() {
    let tree = home_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = started(&tree, &source);
    for sort in KEYS {
        for dir in DIRECTIONS {
            let want = plain(&engine, "plumb", sort, dir, 0, 100);
            let got = engine
                .search_grouped(
                    "plumb",
                    fx_engine::QueryOptions::default(),
                    sort,
                    dir,
                    false,
                    0,
                    100,
                )
                .expect("search succeeds");
            assert_eq!(got, want, "{sort:?} {dir:?}");
        }
    }
    engine.shutdown();
}

#[test]
fn a_moved_file_or_folder_changes_group_on_the_next_snapshot() {
    let tree = home_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = started(&tree, &source);

    // Out of the Desktop, and Homebrew's file into it.
    tree.rename(
        "/vol/Users/me/Desktop/plumb-desk.pdf",
        "/vol/opt/plumb-desk.pdf",
    );
    tree.rename(
        "/vol/opt/homebrew/plumb.rb",
        "/vol/Users/me/Desktop/plumb.rb",
    );
    source.push(vec![
        check("/vol/Users/me/Desktop/plumb-desk.pdf"),
        check("/vol/opt/plumb-desk.pdf"),
        check("/vol/opt/homebrew/plumb.rb"),
        check("/vol/Users/me/Desktop/plumb.rb"),
    ]);
    let snap = wait_for_tree(&engine, &tree, "moves applied");
    let group = snap.priority_group().expect("priority folders are set");
    assert!(!group.contains(id_of(&snap, &tree, "/vol/opt/plumb-desk.pdf")));
    assert!(group.contains(id_of(&snap, &tree, "/vol/Users/me/Desktop/plumb.rb")));
    let first = name_set(&[
        "plumb.rb",
        "Plumb guide.pdf",
        "plumb-drive.doc",
        "plumb-icloud.txt",
    ]);
    assert_grouped_everywhere(&engine, "plumb", &first);

    // A folder carries its contents into the group.
    tree.rename(
        "/vol/Applications/Plumb.app",
        "/vol/Users/me/Desktop/Plumb.app",
    );
    source.push(vec![
        check("/vol/Applications/Plumb.app"),
        check("/vol/Users/me/Desktop/Plumb.app"),
    ]);
    let snap = wait_for_tree(&engine, &tree, "folder moved into the Desktop");
    let group = snap.priority_group().expect("priority folders are set");
    for path in [
        "/vol/Users/me/Desktop/Plumb.app",
        "/vol/Users/me/Desktop/Plumb.app/plumb-res.png",
    ] {
        assert!(group.contains(id_of(&snap, &tree, path)), "{path}");
    }
    let first = name_set(&[
        "plumb.rb",
        "Plumb guide.pdf",
        "plumb-drive.doc",
        "plumb-icloud.txt",
        "Plumb.app",
        "plumb-res.png",
    ]);
    assert_grouped_everywhere(&engine, "plumb", &first);

    // And out again, into the Library.
    tree.rename(
        "/vol/Users/me/Desktop/Plumb.app",
        "/vol/Users/me/Library/Plumb.app",
    );
    source.push(vec![
        check("/vol/Users/me/Desktop/Plumb.app"),
        check("/vol/Users/me/Library/Plumb.app"),
    ]);
    let snap = wait_for_tree(&engine, &tree, "folder moved into the Library");
    let group = snap.priority_group().expect("priority folders are set");
    assert!(!group.contains(id_of(
        &snap,
        &tree,
        "/vol/Users/me/Library/Plumb.app/plumb-res.png"
    )));
    engine.shutdown();
}

#[test]
fn a_cloud_drive_added_later_joins_the_group() {
    let tree = FakeTree::new();
    tree.mkdir("/vol/Users");
    tree.mkdir(HOME);
    tree.mkdir(LIBRARY);
    tree.touch("/vol/Users/me/Library/plumb-prefs.plist", 1);
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = started(&tree, &source);
    let snap = engine.snapshot();
    let group = snap.priority_group().expect("priority folders are set");
    assert!(group.contains(id_of(&snap, &tree, HOME)));
    assert!(!group.contains(id_of(
        &snap,
        &tree,
        "/vol/Users/me/Library/plumb-prefs.plist"
    )));

    // CloudStorage did not exist at start; a drive appears in it.
    tree.mkdir(CLOUD_STORAGE);
    tree.mkdir("/vol/Users/me/Library/CloudStorage/OneDrive-Personal");
    tree.touch(
        "/vol/Users/me/Library/CloudStorage/OneDrive-Personal/plumb-one.docx",
        3,
    );
    source.push(vec![check(
        "/vol/Users/me/Library/CloudStorage/OneDrive-Personal/plumb-one.docx",
    )]);
    let snap = wait_for_tree(&engine, &tree, "cloud drive indexed");
    let group = snap.priority_group().expect("priority folders are set");
    assert!(group.contains(id_of(
        &snap,
        &tree,
        "/vol/Users/me/Library/CloudStorage/OneDrive-Personal/plumb-one.docx"
    )));
    assert_grouped_everywhere(&engine, "plumb", &name_set(&["plumb-one.docx"]));
    engine.shutdown();
}

#[test]
fn a_root_inside_a_listed_folder_takes_its_flag() {
    let tree = home_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    // "/" lies above the root `/vol`: every entry is first unless the
    // Library (listed deeper) ends the group.
    let cfg = config().with_priority_folders(["/"], [LIBRARY]);
    let engine = start(&tree, &source, tree.build_index(), cfg);
    let snap = wait_for_tree(&engine, &tree, "initial index");
    let group = snap.priority_group().expect("priority folders are set");
    assert!(group.contains(id_of(&snap, &tree, ROOT)));
    assert!(group.contains(id_of(&snap, &tree, "/vol/opt/homebrew/plumb.rb")));
    assert!(!group.contains(id_of(&snap, &tree, LIBRARY)));
    assert!(!group.contains(id_of(
        &snap,
        &tree,
        "/vol/Users/me/Library/LanguageModeling/plumb-lm.dat"
    )));
    engine.shutdown();

    // A folder listed both ways counts as excluded.
    let cfg = config().with_priority_folders([HOME], [HOME]);
    let engine = start(&tree, &source, tree.build_index(), cfg);
    let snap = wait_for_tree(&engine, &tree, "initial index");
    let group = snap.priority_group().expect("priority folders are set");
    assert!(group.is_empty());
    engine.shutdown();
}

#[test]
fn without_priority_folders_there_is_no_group() {
    let tree = home_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = start(&tree, &source, tree.build_index(), config());
    let snap = wait_for_tree(&engine, &tree, "initial index");
    assert!(snap.priority_group().is_none());
    for sort in KEYS {
        for dir in DIRECTIONS {
            assert_eq!(
                grouped(&engine, "plumb", sort, dir, 0, 100),
                plain(&engine, "plumb", sort, dir, 0, 100)
            );
        }
    }
    engine.shutdown();
}

#[test]
fn the_memory_report_counts_the_group() {
    let tree = home_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = started(&tree, &source);
    let report = engine.snapshot().memory_report();
    assert!(report.column("priority_group").is_some_and(|b| b > 0));
    let sum: usize = report.columns.iter().map(|&(_, b)| b).sum();
    assert_eq!(report.total_bytes, sum);
    engine.shutdown();
}

#[test]
fn hidden_entries_are_never_first() {
    let tree = home_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = started(&tree, &source);
    let snap = engine.snapshot();
    let group = snap.priority_group().expect("priority folders are set");
    for path in [
        "/vol/Users/me/Desktop/.DS_Store",
        "/vol/Users/me/Documents/.git",
        GIT_OBJECTS,
        "/vol/Users/me/Documents/.git/objects/plumb-obj",
        "/vol/Users/me/.zshrc",
        "/vol/Users/me/.plumbrc",
        CACHE,
        "/vol/Users/me/.cache/plumb-cache.bin",
        DRIVE_TMP,
        "/vol/Users/me/Library/CloudStorage/GoogleDrive-me/.tmp.drivedownload/plumb-dl.part",
    ] {
        assert!(
            !group.contains(id_of(&snap, &tree, path)),
            "{path} is hidden"
        );
    }
    // Everything else is unchanged.
    for path in [
        HOME,
        "/vol/Users/me/Desktop",
        "/vol/Users/me/Desktop/plumb-desk.pdf",
        "/vol/Users/me/Documents",
        "/vol/Users/me/Documents/notes.txt",
        "/vol/Users/me/Library/CloudStorage/GoogleDrive-me",
        "/vol/Users/me/Library/CloudStorage/GoogleDrive-me/plumb-drive.doc",
    ] {
        assert!(group.contains(id_of(&snap, &tree, path)), "{path} is yours");
    }
    let first = name_set(&[
        "plumb-desk.pdf",
        "Plumb guide.pdf",
        "plumb-drive.doc",
        "plumb-icloud.txt",
    ]);
    assert_grouped_everywhere(&engine, "plumb", &first);

    // A file renamed to a dot-name leaves the group, and comes back.
    tree.rename(
        "/vol/Users/me/Desktop/plumb-desk.pdf",
        "/vol/Users/me/Desktop/.plumb-desk.pdf",
    );
    source.push(vec![
        check("/vol/Users/me/Desktop/plumb-desk.pdf"),
        check("/vol/Users/me/Desktop/.plumb-desk.pdf"),
    ]);
    let snap = wait_for_tree(&engine, &tree, "file hidden");
    let group = snap.priority_group().expect("priority folders are set");
    assert!(!group.contains(id_of(&snap, &tree, "/vol/Users/me/Desktop/.plumb-desk.pdf")));
    let first = name_set(&["Plumb guide.pdf", "plumb-drive.doc", "plumb-icloud.txt"]);
    assert_grouped_everywhere(&engine, "plumb", &first);
    tree.rename(
        "/vol/Users/me/Desktop/.plumb-desk.pdf",
        "/vol/Users/me/Desktop/plumb-desk.pdf",
    );
    source.push(vec![
        check("/vol/Users/me/Desktop/.plumb-desk.pdf"),
        check("/vol/Users/me/Desktop/plumb-desk.pdf"),
    ]);
    let snap = wait_for_tree(&engine, &tree, "file shown again");
    let group = snap.priority_group().expect("priority folders are set");
    assert!(group.contains(id_of(&snap, &tree, "/vol/Users/me/Desktop/plumb-desk.pdf")));
    engine.shutdown();
}

#[test]
fn a_listed_folder_inside_a_hidden_folder_is_not_first() {
    let tree = home_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let cfg = config().with_priority_folders([HOME, GIT_OBJECTS, DRIVE_TMP], [LIBRARY]);
    let engine = start(&tree, &source, tree.build_index(), cfg);
    let snap = wait_for_tree(&engine, &tree, "initial index");
    let group = snap.priority_group().expect("priority folders are set");
    for path in [
        GIT_OBJECTS,
        "/vol/Users/me/Documents/.git/objects/plumb-obj",
        DRIVE_TMP,
        "/vol/Users/me/Library/CloudStorage/GoogleDrive-me/.tmp.drivedownload/plumb-dl.part",
    ] {
        assert!(
            !group.contains(id_of(&snap, &tree, path)),
            "{path} is inside a hidden folder"
        );
    }
    assert!(group.contains(id_of(&snap, &tree, "/vol/Users/me/Documents/notes.txt")));
    engine.shutdown();
}

// -------------------------------------- positions in the grouped order

/// Asserts that `positions_grouped` with the flag finds each of `asked`
/// (index paths) at the row the grouped pages list it at, and without the
/// flag at its row in the plain pages, as `positions` does.
fn assert_grouped_positions(engine: &Engine, query: &str, asked: &[String]) {
    let snap = engine.snapshot();
    let refs: Vec<&str> = asked.iter().map(String::as_str).collect();
    let row_in = |ids: &[u32], path: &str| {
        ids.iter()
            .position(|&id| snap.index().path(EntryId(id)) == path)
    };
    for sort in KEYS {
        for dir in DIRECTIONS {
            let listed = grouped(engine, query, sort, dir, 0, 1000);
            let found = engine
                .positions_grouped(
                    query,
                    fx_engine::QueryOptions::default(),
                    sort,
                    dir,
                    true,
                    &refs,
                )
                .expect("not cancelled");
            assert_eq!(found.generation, listed.generation, "{sort:?} {dir:?}");
            for (k, path) in asked.iter().enumerate() {
                assert_eq!(
                    found.rows[k],
                    row_in(&listed.ids, path),
                    "{query:?} {sort:?} {dir:?} {path}: grouped row"
                );
            }

            let listed = plain(engine, query, sort, dir, 0, 1000);
            let found = engine
                .positions_grouped(
                    query,
                    fx_engine::QueryOptions::default(),
                    sort,
                    dir,
                    false,
                    &refs,
                )
                .expect("not cancelled");
            let unflagged = engine
                .positions(query, fx_engine::QueryOptions::default(), sort, dir, &refs)
                .expect("not cancelled");
            assert_eq!(found, unflagged, "{sort:?} {dir:?}");
            for (k, path) in asked.iter().enumerate() {
                assert_eq!(
                    found.rows[k],
                    row_in(&listed.ids, path),
                    "{query:?} {sort:?} {dir:?} {path}: plain row"
                );
            }
        }
    }
}

/// The selection lookup of "Your files first": the grouped search's rows,
/// for files in the group, outside it, both, and neither.
#[test]
fn positions_grouped_match_the_grouped_pages() {
    let tree = home_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = started(&tree, &source);
    let snap = engine.snapshot();
    let group = snap.priority_group().expect("priority folders are set");
    let path_of = |p: &str| snap.index().path(id_of(&snap, &tree, p));
    let yours: Vec<String> = [
        "/vol/Users/me/Downloads/Plumb guide.pdf",
        "/vol/Users/me/Desktop/plumb-desk.pdf",
        "/vol/Users/me/Library/CloudStorage/GoogleDrive-me/plumb-drive.doc",
    ]
    .map(path_of)
    .into();
    let others: Vec<String> = [
        "/vol/opt/homebrew/plumb.rb",
        "/vol/Users/me/Library/LanguageModeling/plumb-lm.dat",
        "/vol/Users/me/.cache/plumb-cache.bin",
    ]
    .map(path_of)
    .into();
    for path in &yours {
        assert!(group.contains(EntryId(snap.find_path(path).expect("found"))));
    }
    for path in &others {
        assert!(!group.contains(EntryId(snap.find_path(path).expect("found"))));
    }
    let mut every: Vec<String> = snap
        .index()
        .live_ids()
        .map(|id| snap.index().path(id))
        .collect();
    every.reverse();
    every.extend(["vol/nope.txt", ""].map(String::from));
    let mut mixed = others.clone();
    mixed.extend(yours.iter().cloned());
    for query in ["plumb", "", "zzz-no-match"] {
        for asked in [&yours, &others, &mixed, &every] {
            assert_grouped_positions(&engine, query, asked);
        }
    }
    engine.shutdown();
}
