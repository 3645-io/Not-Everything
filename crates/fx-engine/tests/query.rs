//! Query API of the engine.
//!
//! Contract these tests rely on (in addition to `tests/common/mod.rs`):
//!
//! - `fx_engine::EngineSnapshot` implements `fx_query::SearchView`: IDs are
//!   the raw `u32` entry IDs of its index snapshot.
//! - `Engine::search(&self, query: &str, sort: fx_sort::SortKey,
//!   direction: fx_sort::Direction, offset: usize, limit: usize)` returns a
//!   `fx_engine::Page` (or a `Result<Page, E: Debug>`) with public fields
//!   `generation` (the snapshot generation searched, as `u64` or
//!   `Generation`), `total` (`usize`, every match) and `ids` (the window
//!   `offset .. offset + limit` of the matches in `sort`/`direction` order,
//!   as `u32` or `EntryId`).
//! - Plain text terms match names, case-insensitively, as in fx-query.
//!
//! fx-engine needs fx-query as a dependency for these tests.

mod common;

use std::fmt::Debug;
use std::thread;
use std::time::{Duration, Instant};

use common::*;
use fx_engine::{Engine, EngineSnapshot, Generation, Page};
use fx_model::{EntryId, EntryKind, StartOutcome};
use fx_query::SearchView;
use fx_sort::{Direction, SortKey};

/// Accepts both `Page` and `Result<Page, E>` from `Engine::search`.
trait IntoPage {
    fn into_page(self) -> Page;
}

impl IntoPage for Page {
    fn into_page(self) -> Page {
        self
    }
}

impl<E: Debug> IntoPage for Result<Page, E> {
    fn into_page(self) -> Page {
        self.expect("search succeeds")
    }
}

/// Raw numbers for the page's generation and IDs, whichever newtype is used.
trait Raw {
    fn raw(self) -> u64;
}

impl Raw for u64 {
    fn raw(self) -> u64 {
        self
    }
}

impl Raw for u32 {
    fn raw(self) -> u64 {
        u64::from(self)
    }
}

impl Raw for EntryId {
    fn raw(self) -> u64 {
        u64::from(self.0)
    }
}

impl Raw for Generation {
    fn raw(self) -> u64 {
        self.get()
    }
}

struct Hits {
    generation: u64,
    total: usize,
    ids: Vec<u32>,
}

fn search(engine: &Engine, q: &str, sort: SortKey, dir: Direction, off: usize, lim: usize) -> Hits {
    let page = engine
        .search(q, fx_engine::QueryOptions::default(), sort, dir, off, lim)
        .into_page();
    Hits {
        generation: page.generation.raw(),
        total: page.total,
        ids: page.ids.iter().map(|&id| id.raw() as u32).collect(),
    }
}

fn names(snap: &EngineSnapshot, ids: &[u32]) -> Vec<String> {
    let idx: &fx_index::Snapshot = snap.index();
    ids.iter()
        .map(|&id| idx.name(EntryId(id)).to_string())
        .collect()
}

/// Polls `engine.search(query)` (Name order, all results) until the sorted
/// result names equal `want`.
fn wait_for_names(engine: &Engine, query: &str, want: &[&str]) -> Hits {
    let mut want: Vec<String> = want.iter().map(|s| s.to_string()).collect();
    want.sort();
    let end = Instant::now() + WAIT;
    loop {
        let hits = search(engine, query, SortKey::Name, Direction::Ascending, 0, 1000);
        let snap = engine.snapshot();
        if snap.generation() == hits.generation {
            let mut got = names(&snap, &hits.ids);
            got.sort();
            if got == want && hits.total == want.len() {
                return hits;
            }
            assert!(
                Instant::now() < end,
                "timed out waiting for {query:?} -> {want:?}; got {got:?} (total {})",
                hits.total
            );
        }
        assert!(Instant::now() < end, "timed out waiting for {query:?}");
        thread::sleep(Duration::from_millis(5));
    }
}

fn sample_tree() -> FakeTree {
    let tree = FakeTree::new();
    tree.mkdir("/vol/docs");
    tree.mkdir("/vol/stuff");
    tree.touch("/vol/docs/notes.txt", 10);
    tree.touch("/vol/stuff/photo.jpg", 2_000);
    tree
}

fn started(tree: &FakeTree, source: &FakeSource) -> Engine {
    let engine = start(tree, source, tree.build_index(), config());
    wait_for_tree(&engine, tree, "initial index");
    engine
}

#[test]
fn engine_snapshot_is_a_search_view_over_its_index() {
    fn as_view<V: SearchView>(v: &V) -> &V {
        v
    }
    let tree = sample_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = started(&tree, &source);
    let snap = engine.snapshot();
    let view = as_view(&*snap);
    let idx: &fx_index::Snapshot = snap.index();
    let live: Vec<EntryId> = idx.live_ids().collect();
    assert_eq!(live.len(), tree.entries().len());
    for id in live {
        let raw = id.0;
        assert!((raw as usize) < view.len(), "live ID {raw} inside the view");
        assert!(view.is_live(raw));
        assert_eq!(view.name(raw), idx.name(id));
        assert_eq!(view.is_folder(raw), idx.kind(id) == EntryKind::Dir);
        let parent = idx.parent(id);
        let want_parent = if parent.is_none() {
            None
        } else {
            Some(parent.0)
        };
        assert_eq!(view.parent(raw), want_parent, "parent of {}", idx.name(id));
        if idx.kind(id) == EntryKind::File {
            assert_eq!(view.size(raw), Some(idx.size(id)));
            assert_eq!(view.modified(raw), Some(idx.modified(id)));
        }
    }
    engine.shutdown();
}

#[test]
fn search_results_follow_creates_and_deletes() {
    let tree = sample_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = started(&tree, &source);

    let before = search(
        &engine,
        "report",
        SortKey::Name,
        Direction::Ascending,
        0,
        100,
    );
    assert_eq!(before.total, 0);
    assert!(before.ids.is_empty());

    tree.touch("/vol/docs/report.txt", 5);
    source.push(vec![check("/vol/docs/report.txt")]);
    let created = wait_for_names(&engine, "report", &["report.txt"]);
    assert!(
        created.generation > before.generation,
        "a published batch bumps the page generation"
    );

    tree.touch("/vol/stuff/Report-2.txt", 6);
    source.push(vec![check("/vol/stuff/Report-2.txt")]);
    wait_for_names(&engine, "report", &["report.txt", "Report-2.txt"]);

    tree.remove("/vol/docs/report.txt");
    source.push(vec![check("/vol/docs/report.txt")]);
    wait_for_names(&engine, "report", &["Report-2.txt"]);

    // Deleting a folder removes everything under it from the results.
    tree.remove("/vol/stuff");
    source.push(vec![check("/vol/stuff")]);
    let gone = wait_for_names(&engine, "report", &[]);
    assert!(gone.ids.is_empty());
    wait_for_names(&engine, "photo", &[]);
    engine.shutdown();
}

#[test]
fn page_generation_is_the_snapshot_searched() {
    let tree = sample_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = started(&tree, &source);
    let hits = search(&engine, "notes", SortKey::Name, Direction::Ascending, 0, 10);
    assert_eq!(hits.generation, engine.snapshot().generation());
    assert_eq!(hits.total, 1);
    engine.shutdown();
}

fn paging_tree() -> FakeTree {
    let tree = FakeTree::new();
    tree.mkdir("/vol/a");
    tree.mkdir("/vol/b");
    tree.mkdir("/vol/b/deep");
    for n in 0..25u64 {
        let dir = ["/vol/a", "/vol/b", "/vol/b/deep"][n as usize % 3];
        // Sizes in a scrambled order, with some ties.
        let size = (n * 37) % 11 * 100;
        tree.touch(&format!("{dir}/Item{:02}.dat", (n * 7) % 25), size);
    }
    tree.add_files("/vol/a", "other", 10);
    tree
}

/// The expected matches of "item" in `key` order: the engine's own order
/// filtered by name, read in `dir`.
fn expected(snap: &EngineSnapshot, key: SortKey, dir: Direction) -> Vec<u32> {
    let idx: &fx_index::Snapshot = snap.index();
    let order = snap.order(key).expect("engine keeps the order");
    let mut ids: Vec<u32> = order
        .ids
        .iter()
        .copied()
        .filter(|&id| idx.name(EntryId(id)).to_lowercase().contains("item"))
        .collect();
    if dir == Direction::Descending {
        ids.reverse();
    }
    ids
}

#[test]
fn paging_walks_every_sort_order_in_both_directions() {
    let tree = paging_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = started(&tree, &source);
    let snap = engine.snapshot();
    for key in [
        SortKey::Name,
        SortKey::Path,
        SortKey::Size,
        SortKey::Modified,
    ] {
        for dir in [Direction::Ascending, Direction::Descending] {
            let want = expected(&snap, key, dir);
            assert_eq!(want.len(), 25);
            let mut got = Vec::new();
            for offset in (0..30).step_by(10) {
                let page = search(&engine, "item", key, dir, offset, 10);
                assert_eq!(page.generation, snap.generation());
                assert_eq!(page.total, 25, "{key:?} {dir:?} total");
                let len = 25usize.saturating_sub(offset).min(10);
                assert_eq!(page.ids.len(), len, "{key:?} {dir:?} offset {offset}");
                got.extend(page.ids);
            }
            assert_eq!(got, want, "{key:?} {dir:?} pages in order");
        }
    }
    engine.shutdown();
}

#[test]
fn pages_are_sorted_by_the_requested_key() {
    let tree = paging_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = started(&tree, &source);
    let snap = engine.snapshot();
    let idx: &fx_index::Snapshot = snap.index();

    let by_name = search(&engine, "item", SortKey::Name, Direction::Ascending, 0, 100);
    let got = names(&snap, &by_name.ids);
    let mut want: Vec<String> = (0..25).map(|n| format!("Item{n:02}.dat")).collect();
    want.sort();
    assert_eq!(got, want, "Name ascending");

    let by_name_desc = search(
        &engine,
        "item",
        SortKey::Name,
        Direction::Descending,
        0,
        100,
    );
    let mut rev = want.clone();
    rev.reverse();
    assert_eq!(names(&snap, &by_name_desc.ids), rev, "Name descending");

    let by_size = search(&engine, "item", SortKey::Size, Direction::Ascending, 0, 100);
    let sizes: Vec<u64> = by_size
        .ids
        .iter()
        .map(|&id| idx.size(EntryId(id)))
        .collect();
    assert_eq!(sizes.len(), 25);
    assert!(
        sizes.windows(2).all(|w| w[0] <= w[1]),
        "Size ascending: {sizes:?}"
    );

    let by_size_desc = search(
        &engine,
        "item",
        SortKey::Size,
        Direction::Descending,
        0,
        100,
    );
    let sizes: Vec<u64> = by_size_desc
        .ids
        .iter()
        .map(|&id| idx.size(EntryId(id)))
        .collect();
    assert!(
        sizes.windows(2).all(|w| w[0] >= w[1]),
        "Size descending: {sizes:?}"
    );
    engine.shutdown();
}

#[test]
fn windows_past_the_end_or_empty_keep_the_total() {
    let tree = paging_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = started(&tree, &source);
    let past = search(&engine, "item", SortKey::Name, Direction::Ascending, 25, 10);
    assert_eq!(past.total, 25);
    assert!(past.ids.is_empty());
    let far = search(
        &engine,
        "item",
        SortKey::Path,
        Direction::Descending,
        1_000,
        10,
    );
    assert_eq!(far.total, 25);
    assert!(far.ids.is_empty());
    let none = search(&engine, "item", SortKey::Size, Direction::Ascending, 0, 0);
    assert_eq!(none.total, 25);
    assert!(none.ids.is_empty());
    let huge = search(
        &engine,
        "item",
        SortKey::Modified,
        Direction::Ascending,
        20,
        usize::MAX,
    );
    assert_eq!(huge.total, 25);
    assert_eq!(huge.ids.len(), 5);
    let nothing = search(
        &engine,
        "zzz-no-match",
        SortKey::Name,
        Direction::Ascending,
        0,
        10,
    );
    assert_eq!(nothing.total, 0);
    assert!(nothing.ids.is_empty());
    engine.shutdown();
}

#[test]
fn repeated_and_refined_searches_stay_correct_across_batches() {
    // Exercises the refinement cache: "item" then "item0" refines, and a new
    // generation must not serve stale cached hits.
    let tree = paging_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = started(&tree, &source);
    let all = search(&engine, "item", SortKey::Name, Direction::Ascending, 0, 100);
    assert_eq!(all.total, 25);
    let refined = search(
        &engine,
        "item0",
        SortKey::Name,
        Direction::Ascending,
        0,
        100,
    );
    assert_eq!(refined.total, 10, "Item00..Item09");
    let again = search(&engine, "item", SortKey::Name, Direction::Ascending, 0, 100);
    assert_eq!(again.ids, all.ids);

    tree.touch("/vol/b/item99.txt", 1);
    tree.remove("/vol/a");
    source.push(vec![check("/vol/b/item99.txt"), check("/vol/a")]);
    wait_for_tree(&engine, &tree, "batch applied");
    let snap = engine.snapshot();
    let want = expected(&snap, SortKey::Name, Direction::Ascending);
    let after = search(&engine, "item", SortKey::Name, Direction::Ascending, 0, 100);
    assert_eq!(after.generation, snap.generation());
    assert_eq!(after.ids, want);
    assert!(names(&snap, &after.ids).contains(&"item99.txt".to_string()));
    let refined = search(
        &engine,
        "item0",
        SortKey::Name,
        Direction::Ascending,
        0,
        100,
    );
    let refined_names = names(&snap, &refined.ids);
    assert!(
        refined_names
            .iter()
            .all(|n| n.to_lowercase().contains("item0"))
    );
    assert_eq!(refined.total, refined_names.len());
    engine.shutdown();
}

#[test]
fn search_is_callable_from_many_threads() {
    let tree = paging_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = started(&tree, &source);
    thread::scope(|s| {
        for t in 0..4 {
            let engine = &engine;
            s.spawn(move || {
                for i in 0..20 {
                    let q = if (t + i) % 2 == 0 { "item" } else { "item1" };
                    let hits = search(engine, q, SortKey::Name, Direction::Ascending, 0, 100);
                    let want = if q == "item" { 25 } else { 10 };
                    assert_eq!(hits.total, want, "{q}");
                }
            });
        }
    });
    engine.shutdown();
}

// ---------------------------------------------------------------------------
// Positions of known files (to keep the selection on the same files)
// ---------------------------------------------------------------------------
//
// `Engine::positions(query, sort, direction, paths: &[&str])` returns a
// `fx_engine::Positions` with `generation` (the snapshot searched) and
// `rows`: for each path given, in order, the offset at which `search` would
// return that entry, or `None` when no live entry has the path or it does
// not match. Paths are index paths, as `fx_index::Snapshot::path` gives
// them (`vol/a/Item03.dat`). `EngineSnapshot::find_path(path)` returns the
// live entry ID with that path.

/// Every match of `query`, read `page` rows at a time.
fn paged_ids(engine: &Engine, query: &str, key: SortKey, dir: Direction, page: usize) -> Hits {
    let mut all = Hits {
        generation: 0,
        total: 0,
        ids: Vec::new(),
    };
    loop {
        let hits = search(engine, query, key, dir, all.ids.len(), page);
        if all.ids.is_empty() {
            all.generation = hits.generation;
            all.total = hits.total;
        }
        assert_eq!(hits.generation, all.generation, "one generation throughout");
        if hits.ids.is_empty() {
            return all;
        }
        all.ids.extend(hits.ids);
    }
}

/// Asserts that `positions` gives, for every live path (asked in reverse
/// ID order) and some missing ones, the row that paging finds it at.
fn assert_positions_match_paging(engine: &Engine, query: &str) {
    let snap = engine.snapshot();
    let idx: &fx_index::Snapshot = snap.index();
    let mut asked: Vec<String> = idx.live_ids().map(|id| idx.path(id)).collect();
    asked.reverse();
    asked.extend(
        [
            "vol/nope.dat",
            "vol/a/missing/Item01.dat",
            "VOL",
            "",
            "vol/",
        ]
        .map(String::from),
    );
    let refs: Vec<&str> = asked.iter().map(String::as_str).collect();
    for key in ENGINE_KEYS {
        for dir in [Direction::Ascending, Direction::Descending] {
            let paged = paged_ids(engine, query, key, dir, 7);
            assert_eq!(paged.generation, snap.generation());
            assert_eq!(paged.ids.len(), paged.total);
            let found = engine
                .positions(query, fx_engine::QueryOptions::default(), key, dir, &refs)
                .expect("not cancelled");
            assert_eq!(found.generation.get(), snap.generation());
            assert_eq!(found.rows.len(), asked.len());
            for (k, path) in asked.iter().enumerate() {
                let want = paged
                    .ids
                    .iter()
                    .position(|&id| idx.path(EntryId(id)) == *path);
                assert_eq!(found.rows[k], want, "{query:?} {key:?} {dir:?} {path:?}");
            }
        }
    }
}

#[test]
fn find_path_finds_every_live_entry_and_nothing_else() {
    let tree = paging_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = started(&tree, &source);
    let snap = engine.snapshot();
    let idx: &fx_index::Snapshot = snap.index();
    let live: Vec<EntryId> = idx.live_ids().collect();
    assert!(live.len() > 30);
    for id in live {
        let path = idx.path(id);
        assert_eq!(snap.find_path(&path), Some(id.0), "{path}");
    }
    for missing in [
        "",
        "vol/",
        "VOL",
        "vol/A",
        "vol/a/item00.dat",
        "vol/a/Item00.dat/x",
        "vol/b/deep/nope",
        "other/a",
    ] {
        assert_eq!(snap.find_path(missing), None, "{missing:?}");
    }
    engine.shutdown();
}

#[test]
fn positions_match_reading_the_results_page_by_page() {
    let tree = paging_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = started(&tree, &source);
    for query in ["item", "", "1", "other", "zzz-no-match"] {
        assert_positions_match_paging(&engine, query);
    }
    engine.shutdown();
}

#[test]
fn positions_follow_creates_renames_and_deletes() {
    let tree = paging_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = started(&tree, &source);
    let before = engine.snapshot();
    let moved = ["vol/a", "vol/b", "vol/b/deep"]
        .iter()
        .find_map(|dir| before.find_path(&format!("{dir}/Item00.dat")))
        .expect("Item00.dat exists");
    let old_path = before.index().path(EntryId(moved));

    tree.touch("/vol/a/Item-new.dat", 3);
    tree.mkdir("/vol/c");
    tree.rename(&format!("/{old_path}"), "/vol/c/Item00.dat");
    tree.remove("/vol/b/deep");
    source.push(vec![
        check("/vol/a/Item-new.dat"),
        check("/vol/c"),
        check(&format!("/{old_path}")),
        check("/vol/c/Item00.dat"),
        check("/vol/b/deep"),
    ]);
    let after = wait_for_tree(&engine, &tree, "changes applied");
    assert!(after.generation() > before.generation());

    let found = engine
        .positions(
            "item",
            fx_engine::QueryOptions::default(),
            SortKey::Name,
            Direction::Ascending,
            &[old_path.as_str(), "vol/c/Item00.dat", "vol/a/Item-new.dat"],
        )
        .expect("not cancelled");
    assert_eq!(found.rows[0], None, "the old path is gone");
    assert!(found.rows[1].is_some(), "the new path is found");
    assert!(found.rows[2].is_some(), "the created file is found");
    for query in ["item", "", "dat"] {
        assert_positions_match_paging(&engine, query);
    }
    engine.shutdown();
}

#[test]
fn positions_of_no_paths_is_empty() {
    let tree = paging_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = started(&tree, &source);
    let found = engine
        .positions(
            "item",
            fx_engine::QueryOptions::default(),
            SortKey::Path,
            Direction::Descending,
            &[],
        )
        .expect("not cancelled");
    assert!(found.rows.is_empty());
    assert_eq!(found.generation.get(), engine.snapshot().generation());
    engine.shutdown();
}

#[test]
fn hiding_the_window_drops_the_cached_result_sets() {
    // Run 2.17: nobody searches while no window shows the results, so the
    // result sets kept for refining a query (up to 8, each as large as its
    // matches) are dropped instead of held for hours.
    let tree = sample_tree();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = started(&tree, &source);
    let _ = search(&engine, "notes", SortKey::Name, Direction::Ascending, 0, 10);
    let _ = search(&engine, "photo", SortKey::Name, Direction::Ascending, 0, 10);
    assert!(
        engine.cached_searches() > 0,
        "searches are cached while shown"
    );
    engine.set_foreground(false);
    assert_eq!(engine.cached_searches(), 0, "dropped once hidden");
    engine.set_foreground(true);
    let _ = search(&engine, "notes", SortKey::Name, Direction::Ascending, 0, 10);
    assert!(engine.cached_searches() > 0, "cached again once shown");
    engine.shutdown();
}
