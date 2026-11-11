//! `SwitchableSource`: the window's results source when the app
//! launched in limited (non-FDA) mode. When Full Disk Access is granted it
//! switches from the limited engine to the full-access one without a
//! relaunch.
//!
//! Contract, in `fx_ui_macos::switchable`:
//!
//! - `SwitchableSource::new(Arc<dyn ResultsSource>)` searches that source.
//! - `switch_to(next) -> Arc<dyn ResultsSource>` makes `next` the source
//!   every search, row, status, save and generation goes to, and returns the
//!   previous one. Generations carry the switch count (`epoch()`), so any
//!   generation from after a switch is newer than every generation from
//!   before it, whatever the two sources' own numbers; rows asked for with a
//!   generation from before the switch are `None`, never read from `next`.
//!   The save is `save_for_quit` (the quick quit), so quitting after a
//!   switch saves the full index, not the limited one; grouped searches
//!   (`search_grouped`, "Your files first") go to the current source with
//!   the flag and their generations tagged like plain searches', and so do
//!   the selection lookups (`positions`, `positions_grouped`): a kept
//!   selection stays on the same files across the switch.
//!   Subscribers (old and new) are told about the switch with a new
//!   generation and then receive `next`'s generations, never the previous
//!   source's.
//! - `switch_when_ready(open)` opens the next source on a background thread
//!   (`is_switching()` is true meanwhile), waits for its first snapshot
//!   (entries to search: a loaded store at once, else its first published
//!   generation), switches to it and shuts the previous source down after
//!   saving its index (`save_for_quit`). A failed `open` keeps the current
//!   source.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use fx_engine::Generation;
use fx_query::Cancelled;
use fx_sort::{Direction, SortKey};
use fx_ui_macos::controller::ResultsController;
use fx_ui_macos::source::{ResultPage, ResultsSource, Row, RowPositions, SourceStatus};
use fx_ui_macos::switchable::SwitchableSource;

/// How long a test waits for a generation that must arrive.
const WAIT: Duration = Duration::from_secs(10);
/// How long a test waits to be sure a generation does not arrive.
const QUIET: Duration = Duration::from_millis(150);

// ---------------------------------------------------------------------------
// Fake source
// ---------------------------------------------------------------------------

#[derive(Default)]
struct State {
    names: Vec<String>,
    /// The names in your folders: listed first by a grouped search.
    yours: Vec<String>,
    generation: u64,
    searches: usize,
    /// The `your_files_first` flag of every `search_grouped` call.
    grouped: Vec<bool>,
    /// The `your_files_first` flag of every position lookup (`false` for
    /// plain `positions`).
    lookups: Vec<bool>,
    saves: usize,
    row_generations: Vec<Generation>,
    subscribers: Vec<Sender<Generation>>,
    /// Every `set_window_visible` call, in order.
    window_visible: Vec<bool>,
}

/// A source over a list of names; clones share one state.
#[derive(Clone, Default)]
struct Fake {
    state: Arc<Mutex<State>>,
}

impl Fake {
    fn new(names: &[&str], generation: u64) -> Fake {
        let fake = Fake::default();
        {
            let mut s = fake.lock();
            s.names = names.iter().map(|n| n.to_string()).collect();
            s.generation = generation;
        }
        fake
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Publishes a new generation to the subscribers and returns it.
    fn bump(&self) -> Generation {
        let mut s = self.lock();
        s.generation += 1;
        let g = Generation(s.generation);
        s.subscribers.retain(|tx| tx.send(g).is_ok());
        g
    }

    /// Fills an empty source (its first scan) and publishes it.
    fn fill(&self, names: &[&str]) -> Generation {
        self.lock().names = names.iter().map(|n| n.to_string()).collect();
        self.bump()
    }

    /// Marks `names` as in your folders.
    fn with_yours(self, names: &[&str]) -> Fake {
        self.lock().yours = names.iter().map(|n| n.to_string()).collect();
        self
    }

    fn searches(&self) -> usize {
        self.lock().searches
    }

    fn grouped(&self) -> Vec<bool> {
        self.lock().grouped.clone()
    }

    fn lookups(&self) -> Vec<bool> {
        self.lock().lookups.clone()
    }

    fn saves(&self) -> usize {
        self.lock().saves
    }

    fn row_generations(&self) -> Vec<Generation> {
        self.lock().row_generations.clone()
    }

    fn subscribers(&self) -> usize {
        self.lock().subscribers.len()
    }
}

impl ResultsSource for Fake {
    fn search(
        &self,
        query: &str,
        _sort: SortKey,
        _dir: Direction,
        offset: usize,
        limit: usize,
    ) -> Result<ResultPage, Cancelled> {
        let mut s = self.lock();
        s.searches += 1;
        let ids: Vec<u32> = s
            .names
            .iter()
            .enumerate()
            .filter(|(_, n)| n.contains(query))
            .map(|(i, _)| i as u32)
            .collect();
        Ok(ResultPage {
            generation: Generation(s.generation),
            total: ids.len(),
            ids: ids.into_iter().skip(offset).take(limit).collect(),
        })
    }

    /// With `your_files_first`, the matches in `yours` first, each group in
    /// list order.
    fn search_grouped(
        &self,
        query: &str,
        sort: SortKey,
        dir: Direction,
        your_files_first: bool,
        offset: usize,
        limit: usize,
    ) -> Result<ResultPage, Cancelled> {
        let mut page = self.search(query, sort, dir, 0, usize::MAX)?;
        let mut s = self.lock();
        s.grouped.push(your_files_first);
        if your_files_first {
            let names = &s.names;
            let yours = &s.yours;
            page.ids
                .sort_by_key(|&id| !yours.contains(&names[id as usize]));
        }
        page.ids = page.ids.into_iter().skip(offset).take(limit).collect();
        Ok(page)
    }

    fn cancel(&self) {}

    fn row(&self, generation: Generation, id: u32) -> Option<Row> {
        let mut s = self.lock();
        s.row_generations.push(generation);
        let name = s.names.get(id as usize)?.clone();
        Some(Row {
            full_path: format!("/{name}"),
            name,
            ..Row::default()
        })
    }

    fn positions(
        &self,
        query: &str,
        sort: SortKey,
        dir: Direction,
        paths: &[String],
    ) -> Result<RowPositions, Cancelled> {
        self.positions_grouped(query, sort, dir, false, paths)
    }

    /// The rows `search_grouped` lists the files at (full path `/name`).
    fn positions_grouped(
        &self,
        query: &str,
        sort: SortKey,
        dir: Direction,
        your_files_first: bool,
        paths: &[String],
    ) -> Result<RowPositions, Cancelled> {
        let page = self.search_grouped(query, sort, dir, your_files_first, 0, usize::MAX)?;
        let mut s = self.lock();
        s.grouped.pop();
        s.searches -= 1;
        s.lookups.push(your_files_first);
        let rows = paths
            .iter()
            .map(|p| {
                page.ids
                    .iter()
                    .position(|&id| format!("/{}", s.names[id as usize]) == *p)
            })
            .collect();
        Ok(RowPositions {
            generation: page.generation,
            rows,
        })
    }

    fn generation(&self) -> Generation {
        Generation(self.lock().generation)
    }

    fn status(&self) -> SourceStatus {
        SourceStatus {
            indexing: false,
            entries: self.lock().names.len(),
        }
    }

    fn subscribe(&self) -> Receiver<Generation> {
        let (tx, rx) = channel();
        self.lock().subscribers.push(tx);
        rx
    }

    fn save_for_quit(&self) -> Result<(), String> {
        self.lock().saves += 1;
        Ok(())
    }

    fn set_window_visible(&self, visible: bool) {
        self.lock().window_visible.push(visible);
    }
}

fn shared(fake: &Fake) -> Arc<dyn ResultsSource> {
    Arc::new(fake.clone())
}

/// Every name `source` lists for `query`, in order, with the page.
fn names(source: &SwitchableSource, query: &str) -> (Vec<String>, ResultPage) {
    let page = source
        .search(query, SortKey::Name, Direction::Ascending, 0, 100)
        .expect("not cancelled");
    let names = page
        .ids
        .iter()
        .map(|&id| source.row(page.generation, id).expect("row exists").name)
        .collect();
    (names, page)
}

/// The limited index: no protected folders. Its generation is far ahead of
/// the full index's, as after a long session.
fn limited() -> Fake {
    Fake::new(&["alpha.txt", "beta.txt"], 500)
}

/// The full index: also the protected folders, at a young generation.
fn full() -> Fake {
    Fake::new(&["alpha.txt", "beta.txt", "Documents", "secret.txt"], 1)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn before_a_switch_it_is_the_initial_source() {
    let old = limited();
    let source = SwitchableSource::new(shared(&old));
    assert_eq!(source.epoch(), 0);
    assert!(!source.is_switching());

    let (listed, page) = names(&source, "");
    assert_eq!(listed, ["alpha.txt", "beta.txt"]);
    assert_eq!(page.generation, Generation(500), "no switch, no change");
    assert_eq!(source.generation(), Generation(500));
    assert_eq!(source.status().entries, 2);
    source.save_for_quit().expect("saves");
    assert_eq!(old.saves(), 1);
}

#[test]
fn searches_rows_status_and_saves_go_to_the_new_source_after_the_switch() {
    let (old, new) = (limited(), full());
    let source = SwitchableSource::new(shared(&old));
    let _ = names(&source, "");
    let old_searches = old.searches();

    let previous = source.switch_to(shared(&new));
    assert_eq!(
        previous.status().entries,
        2,
        "the previous source is returned"
    );
    assert_eq!(source.epoch(), 1);

    let (listed, _) = names(&source, "");
    assert_eq!(
        listed,
        ["alpha.txt", "beta.txt", "Documents", "secret.txt"],
        "the protected folders are found without a relaunch"
    );
    let (listed, _) = names(&source, "secret");
    assert_eq!(listed, ["secret.txt"]);
    assert_eq!(
        old.searches(),
        old_searches,
        "the old source is not searched"
    );
    assert_eq!(source.status().entries, 4);
    source.save_for_quit().expect("saves");
    assert_eq!((old.saves(), new.saves()), (0, 1));
}

#[test]
fn generations_after_a_switch_are_newer_than_any_before_it() {
    let (old, new) = (limited(), full());
    let source = SwitchableSource::new(shared(&old));
    let (_, before) = names(&source, "");
    let generation_before = source.generation();

    source.switch_to(shared(&new));
    let (_, after) = names(&source, "");
    // The full index is at generation 1, the limited one at 500.
    assert!(after.generation > before.generation, "{after:?} {before:?}");
    assert!(source.generation() > generation_before);
    assert_eq!(source.generation(), after.generation);

    new.bump();
    let (_, later) = names(&source, "");
    assert!(later.generation > after.generation);
}

#[test]
fn rows_of_a_page_from_before_the_switch_are_never_read_from_the_new_source() {
    let (old, new) = (limited(), full());
    let source = SwitchableSource::new(shared(&old));
    let (_, before) = names(&source, "");

    source.switch_to(shared(&new));
    let rows_read = new.row_generations().len();
    for &id in &before.ids {
        assert_eq!(
            source.row(before.generation, id),
            None,
            "an old ID read from the new index would show another file"
        );
    }
    assert_eq!(
        new.row_generations().len(),
        rows_read,
        "the new source was asked for an old page's rows"
    );
}

#[test]
fn subscribers_receive_the_new_sources_generations_after_the_switch() {
    let (old, new) = (limited(), full());
    let source = SwitchableSource::new(shared(&old));
    let early = source.subscribe();

    old.bump();
    let g_old = early
        .recv_timeout(WAIT)
        .expect("the old source's generation");
    assert_eq!(g_old, Generation(501));

    source.switch_to(shared(&new));
    let g_switch = early.recv_timeout(WAIT).expect("the switch is announced");
    assert!(
        g_switch > g_old,
        "{g_switch:?} must be newer than {g_old:?}"
    );
    assert_eq!(g_switch, source.generation());

    let late = source.subscribe();
    // The old source keeps publishing until it is shut down: none of it
    // reaches the subscribers.
    old.bump();
    old.bump();
    new.bump();
    for rx in [&early, &late] {
        let g = rx.recv_timeout(WAIT).expect("the new source's generation");
        assert!(g > g_switch, "{g:?} after {g_switch:?}");
        assert_eq!(g, source.generation());
        assert!(
            rx.recv_timeout(QUIET).is_err(),
            "an old generation was forwarded"
        );
    }
    // The switch stops listening to the old source: its next publication
    // finds no one (the forwarding thread notices the switch on the first
    // generation it receives after it).
    let deadline = Instant::now() + WAIT;
    while old.subscribers() > 0 {
        assert!(
            Instant::now() < deadline,
            "still listening to the old source"
        );
        old.bump();
        std::thread::sleep(Duration::from_millis(5));
    }
    for rx in [&early, &late] {
        assert!(
            rx.recv_timeout(QUIET).is_err(),
            "an old generation was forwarded"
        );
    }
}

#[test]
fn the_controller_reruns_on_the_switch_and_never_mixes_the_two_indexes() {
    let (old, new) = (limited(), full());
    let source = Arc::new(SwitchableSource::new(shared(&old)));
    let generations = source.subscribe();
    let mut ctl = ResultsController::new(Arc::clone(&source)).with_row_window(1);
    let t0 = Instant::now();

    let req = ctl.set_query("");
    let page = ctl.run(req).expect("runs");
    let stale = page.clone();
    assert!(ctl.apply(req, page));
    assert_eq!(
        ctl.row(0).map(|r| r.name.clone()).as_deref(),
        Some("alpha.txt")
    );

    source.switch_to(shared(&new));
    let g = generations
        .recv_timeout(WAIT)
        .expect("the switch is announced");
    let rerun = ctl
        .on_generation(g, t0 + Duration::from_secs(1))
        .expect("the switch re-runs the search through the normal refresh path");

    // Until the re-run's page lands, rows of the shown page never come from
    // the full index.
    assert_eq!(ctl.row_count(), 2);
    for i in 0..ctl.row_count() {
        if let Some(row) = ctl.row(i) {
            assert!(
                ["alpha.txt", "beta.txt"].contains(&row.name.as_str()),
                "row {i} mixed in {row:?}"
            );
        }
    }

    let page = ctl.run(rerun).expect("runs");
    assert!(ctl.apply(rerun, page), "the full index's page is newer");
    assert_eq!(ctl.row_count(), 4);
    let shown: Vec<String> = (0..4)
        .map(|i| ctl.row(i).expect("row").name.clone())
        .collect();
    assert_eq!(shown, ["alpha.txt", "beta.txt", "Documents", "secret.txt"]);

    // A limited page that comes late, even for a newer request, is older
    // than the one shown and dropped.
    let late = ctl.set_query("");
    assert!(
        !ctl.apply(late, stale),
        "an old index's page replaced the new one"
    );
    assert_eq!(ctl.row_count(), 4);
}

#[test]
fn switch_when_ready_waits_for_the_first_snapshot() {
    let (old, new) = (limited(), Fake::new(&[], 0));
    let old_arc = shared(&old);
    let old_weak: Weak<dyn ResultsSource> = Arc::downgrade(&old_arc);
    let source = SwitchableSource::new(old_arc);
    let generations = source.subscribe();

    let next = new.clone();
    let switch = source
        .switch_when_ready(move || Ok(shared(&next)))
        .expect("the switch starts");
    assert!(source.is_switching());

    // The full scan is running: the limited index still answers.
    std::thread::sleep(QUIET);
    assert!(source.is_switching());
    assert_eq!(source.epoch(), 0);
    assert_eq!(names(&source, "").0, ["alpha.txt", "beta.txt"]);
    assert!(
        generations.recv_timeout(QUIET).is_err(),
        "nothing to announce before the first snapshot"
    );

    new.fill(&["alpha.txt", "beta.txt", "Documents", "secret.txt"]);
    switch
        .join()
        .expect("the switch thread")
        .expect("the switch succeeds");
    assert!(!source.is_switching());
    assert_eq!(source.epoch(), 1);
    assert_eq!(names(&source, "secret").0, ["secret.txt"]);
    let g = generations
        .recv_timeout(WAIT)
        .expect("the switch is announced");
    assert!(g > Generation(500));

    assert_eq!(old.saves(), 1, "the limited index is saved before it stops");
    drop(old);
    assert!(
        old_weak.upgrade().is_none(),
        "the limited source is shut down after the switch"
    );
}

#[test]
fn switch_when_ready_switches_at_once_to_a_loaded_index() {
    let (old, new) = (limited(), full());
    let source = SwitchableSource::new(shared(&old));
    let next = new.clone();
    source
        .switch_when_ready(move || Ok(shared(&next)))
        .expect("the switch starts")
        .join()
        .expect("the switch thread")
        .expect("a loaded store needs no new generation");
    assert_eq!(source.epoch(), 1);
    assert_eq!(names(&source, "").0.len(), 4);
}

#[test]
fn a_failed_open_keeps_the_limited_source() {
    let old = limited();
    let source = SwitchableSource::new(shared(&old));
    let result = source
        .switch_when_ready(|| Err("cannot open the full index".to_string()))
        .expect("the switch starts")
        .join()
        .expect("the switch thread");
    assert!(result.is_err());
    assert!(!source.is_switching());
    assert_eq!(source.epoch(), 0);
    assert_eq!(names(&source, "").0, ["alpha.txt", "beta.txt"]);
    assert_eq!(old.saves(), 0, "the limited source keeps running");
}

#[test]
fn only_one_switch_runs_at_a_time() {
    let (old, new) = (limited(), Fake::new(&[], 0));
    let source = SwitchableSource::new(shared(&old));
    let next = new.clone();
    let first = source
        .switch_when_ready(move || Ok(shared(&next)))
        .expect("the first switch starts");
    assert!(
        source.switch_when_ready(|| Ok(shared(&full()))).is_err(),
        "a second switch while the first waits"
    );
    new.fill(&["only.txt"]);
    first.join().expect("thread").expect("switched");
    assert_eq!(names(&source, "").0, ["only.txt"]);
}

// ---------------------------------------------------------------------------
// Forwarding of the grouped `ResultsSource` methods
// ---------------------------------------------------------------------------

/// Every name `source` lists for `query` with "Your files first" set.
fn grouped_names(source: &SwitchableSource, query: &str) -> (Vec<String>, ResultPage) {
    let page = source
        .search_grouped(query, SortKey::Name, Direction::Ascending, true, 0, 100)
        .expect("not cancelled");
    let names = page
        .ids
        .iter()
        .map(|&id| source.row(page.generation, id).expect("row exists").name)
        .collect();
    (names, page)
}

/// "Your files first" reaches the source behind the switch, before
/// and after it, with the page's generation tagged like a plain search's;
/// the trait's default would drop the flag.
#[test]
fn grouped_searches_go_to_the_current_source_with_the_flag() {
    let old = limited().with_yours(&["beta.txt"]);
    let new = full().with_yours(&["Documents", "secret.txt"]);
    let source = SwitchableSource::new(shared(&old));

    let (listed, page) = grouped_names(&source, "");
    assert_eq!(listed, ["beta.txt", "alpha.txt"], "your files first");
    assert_eq!(page.generation, Generation(500), "no switch, no change");
    assert_eq!(old.grouped(), [true]);

    source.switch_to(shared(&new));
    let old_searches = old.searches();
    let (listed, page) = grouped_names(&source, "");
    assert_eq!(
        listed,
        ["Documents", "secret.txt", "alpha.txt", "beta.txt"],
        "the full index's grouped order"
    );
    assert_eq!(
        page.generation,
        source.generation(),
        "tagged with the epoch"
    );
    assert!(page.generation > Generation(500));
    assert_eq!(new.grouped(), [true]);
    assert_eq!(
        old.searches(),
        old_searches,
        "the old source is not searched"
    );

    let plain = source
        .search_grouped("", SortKey::Name, Direction::Ascending, false, 0, 100)
        .expect("not cancelled");
    assert_eq!(new.grouped(), [true, false], "the flag is passed as given");
    assert_eq!(plain.ids, [0, 1, 2, 3]);
}

/// The controller's grouped searches go through the switchable source.
#[test]
fn the_controller_lists_your_files_first_through_the_switch() {
    let old = limited().with_yours(&["beta.txt"]);
    let new = full().with_yours(&["secret.txt"]);
    let source = Arc::new(SwitchableSource::new(shared(&old)));
    let generations = source.subscribe();
    let mut ctl = ResultsController::new(Arc::clone(&source)).with_your_files_first(true);
    let t0 = Instant::now();

    let req = ctl.set_query("");
    let page = ctl.run(req).expect("runs");
    assert!(ctl.apply(req, page));
    assert_eq!(
        ctl.row(0).map(|r| r.name.clone()).as_deref(),
        Some("beta.txt")
    );

    source.switch_to(shared(&new));
    let g = generations
        .recv_timeout(WAIT)
        .expect("the switch is announced");
    let rerun = ctl
        .on_generation(g, t0 + Duration::from_secs(1))
        .expect("the switch re-runs the search");
    let page = ctl.run(rerun).expect("runs");
    assert!(ctl.apply(rerun, page));
    let shown: Vec<String> = (0..ctl.row_count())
        .map(|i| ctl.row(i).expect("row").name.clone())
        .collect();
    assert_eq!(shown, ["secret.txt", "alpha.txt", "beta.txt", "Documents"]);
    assert!(new.grouped().iter().all(|&on| on), "{:?}", new.grouped());
}

/// Quitting after a switch saves the index the window searches
/// (`save_for_quit`), never the previous one.
#[test]
fn quitting_after_a_switch_saves_the_new_source_only() {
    let (old, new) = (limited(), full());
    let source = Arc::new(SwitchableSource::new(shared(&old)));
    // The window's source, as `applicationWillTerminate:` sees it.
    let window_source: Arc<dyn ResultsSource> = source.clone();
    window_source.save_for_quit().expect("saves");
    assert_eq!((old.saves(), new.saves()), (1, 0));

    source.switch_to(shared(&new));
    window_source.save_for_quit().expect("saves");
    assert_eq!((old.saves(), new.saves()), (1, 1));
}

/// The selection lookup (`positions`, grouped with "Your files first")
/// reaches the source behind the switch, with the flag, and its generation
/// is tagged like a page's: one answered after a switch is newer than any
/// page from before it, so the controller never mistakes it for the page
/// shown.
#[test]
fn position_lookups_go_to_the_current_source_with_the_flag() {
    let old = limited().with_yours(&["beta.txt"]);
    let new = full().with_yours(&["Documents", "secret.txt"]);
    let source = SwitchableSource::new(shared(&old));
    let asked = ["/alpha.txt", "/beta.txt", "/secret.txt"].map(String::from);
    let lookup = |grouped: bool| {
        source
            .positions_grouped("", SortKey::Name, Direction::Ascending, grouped, &asked)
            .expect("not cancelled")
    };

    let found = lookup(true);
    assert_eq!(found.rows, [Some(1), Some(0), None], "grouped rows");
    assert_eq!(found.generation, Generation(500), "no switch, no change");
    let plain = source
        .positions("", SortKey::Name, Direction::Ascending, &asked)
        .expect("not cancelled");
    assert_eq!(plain.rows, [Some(0), Some(1), None]);
    assert_eq!(old.lookups(), [true, false]);

    let (_, before) = names(&source, "");
    source.switch_to(shared(&new));
    let found = lookup(true);
    assert_eq!(
        found.rows,
        [Some(2), Some(3), Some(1)],
        "the full index's grouped rows"
    );
    assert_eq!(
        found.generation,
        source.generation(),
        "tagged with the epoch"
    );
    assert!(found.generation > before.generation);
    assert_eq!(lookup(false).rows, [Some(0), Some(1), Some(3)]);
    assert_eq!(new.lookups(), [true, false]);
    assert_eq!(old.lookups(), [true, false], "the old source is not asked");
}

/// A selection kept in the limited index stays on the same files once the
/// window moves to the full index, at their rows in the grouped list.
#[test]
fn a_kept_selection_survives_the_switch_in_the_grouped_order() {
    let old = limited().with_yours(&["beta.txt"]);
    let new = full().with_yours(&["Documents", "secret.txt"]);
    let source = Arc::new(SwitchableSource::new(shared(&old)));
    let generations = source.subscribe();
    let mut ctl = ResultsController::new(Arc::clone(&source)).with_your_files_first(true);
    let t0 = Instant::now();

    let req = ctl.set_query("");
    let page = ctl.run(req).expect("runs");
    assert!(ctl.apply(req, page));
    // beta.txt (yours in the limited index) leads; select alpha.txt below.
    assert_eq!(
        ctl.row(1).map(|r| r.name.clone()).as_deref(),
        Some("alpha.txt")
    );
    ctl.remember_selection(std::slice::from_ref(&(1..2)));

    source.switch_to(shared(&new));
    let g = generations
        .recv_timeout(WAIT)
        .expect("the switch is announced");
    let rerun = ctl
        .on_generation(g, t0 + Duration::from_secs(1))
        .expect("the switch re-runs the search");
    let page = ctl.run(rerun).expect("runs");
    assert!(ctl.apply(rerun, page));
    let r = ctl.reselect(true).expect("the selection is restored");
    assert_eq!(r.rows, [2], "alpha.txt after Documents and secret.txt");
    assert_eq!(
        ctl.row(2).map(|r| r.name.clone()).as_deref(),
        Some("alpha.txt")
    );
    assert_eq!(new.lookups(), [true]);
    assert!(old.lookups().is_empty(), "the old index is not asked");
}

#[test]
fn the_window_state_goes_to_the_current_source_and_to_the_next_at_a_switch() {
    let limited = limited();
    let full = full();
    let source = SwitchableSource::new(shared(&limited));
    source.set_window_visible(false);
    assert_eq!(limited.lock().window_visible, vec![false]);

    // The full-access engine opens while the window is hidden: it starts
    // in the background too, so it batches changes like the one before it.
    let _previous = source.switch_to(shared(&full));
    assert_eq!(full.lock().window_visible, vec![false]);
    source.set_window_visible(true);
    assert_eq!(full.lock().window_visible, vec![false, true]);
    assert_eq!(
        limited.lock().window_visible,
        vec![false],
        "the old source is left alone"
    );
}
