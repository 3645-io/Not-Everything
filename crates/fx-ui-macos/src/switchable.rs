//! A results source that can swap the source behind it.
//!
//! The app builds its index in limited (non-FDA) mode when it launches
//! without Full Disk Access. When access is granted later, it opens a second,
//! full-access engine beside the limited one; [`SwitchableSource`] lets the
//! window move to it without a relaunch, and the limited engine then shuts
//! down.
//!
//! Generations carry the switch count (the epoch) in their top bits, so the
//! [`ResultsController`](crate::controller::ResultsController), which drops
//! pages older than the one shown and re-runs on newer generations, sees a
//! switch as a newer generation, whatever the two sources' own numbers:
//!
//! - every generation after a switch is newer than every one before it, so
//!   the window re-runs its search through the normal refresh path and never
//!   takes a page from the old source for a newer one;
//! - rows asked for with a generation from before the switch are `None`: an
//!   entry ID of the old index read in the new one would show another file;
//! - every call is forwarded to the current source, the grouped search and
//!   position lookups ("Your files first") and the quit save included;
//!   positions are asked by path and answered with a tagged generation, so
//!   a lookup from after a switch reads as newer than the page shown;
//! - subscribers are told about the switch with the new source's current
//!   generation and then receive only the new source's generations.
//!
//! Before any switch (epoch 0) generations pass through unchanged.

use std::io;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::thread::{self, JoinHandle};

use fx_engine::{Generation, SearchError};
use fx_query::Cancelled;
use fx_sort::{Direction, SortKey};

use crate::source::{ResultPage, ResultsSource, Row, RowPositions, SearchQuery, SourceStatus};

/// Bits of a generation below the epoch: a source's own generation number.
const EPOCH_SHIFT: u32 = 48;

/// The largest generation number of one source a tagged generation holds;
/// larger ones (some thousand years of batches) are clamped to it.
const INNER_MAX: u64 = (1 << EPOCH_SHIFT) - 1;

/// `g` of the source current in `epoch`, as the switchable source reports it.
fn tag(epoch: u16, g: Generation) -> Generation {
    Generation((u64::from(epoch) << EPOCH_SHIFT) | g.get().min(INNER_MAX))
}

/// The epoch and the source's own generation of a tagged generation.
fn untag(g: Generation) -> (u16, Generation) {
    let epoch = u16::try_from(g.get() >> EPOCH_SHIFT).unwrap_or(u16::MAX);
    (epoch, Generation(g.get() & INNER_MAX))
}

/// What the switchable source and its threads share.
struct Shared {
    state: Mutex<State>,
}

struct State {
    /// How many switches have completed.
    epoch: u16,
    /// The source every call goes to.
    current: Arc<dyn ResultsSource>,
    /// The window's (and tests') generation channels.
    subscribers: Vec<Sender<Generation>>,
    /// A next source is opening or waiting for its first snapshot.
    switching: bool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl State {
    fn notify(&mut self, g: Generation) {
        self.subscribers.retain(|tx| tx.send(g).is_ok());
    }
}

/// A [`ResultsSource`] over a source that can be swapped for another. See
/// the module documentation.
pub struct SwitchableSource {
    shared: Arc<Shared>,
}

impl SwitchableSource {
    /// A source that answers from `initial` until the first switch.
    pub fn new(initial: Arc<dyn ResultsSource>) -> SwitchableSource {
        let generations = initial.subscribe();
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                epoch: 0,
                current: initial,
                subscribers: Vec::new(),
                switching: false,
            }),
        });
        forward(&shared, 0, generations);
        SwitchableSource { shared }
    }

    /// How many switches have completed (0 before the first).
    pub fn epoch(&self) -> u16 {
        self.shared.lock().epoch
    }

    /// Whether a [`switch_when_ready`](Self::switch_when_ready) is opening
    /// the next source or waiting for its first snapshot.
    pub fn is_switching(&self) -> bool {
        self.shared.lock().switching
    }

    /// Makes `next` the source every call goes to and returns the previous
    /// one, which the caller shuts down. Subscribers are told at once with
    /// `next`'s current generation (newer than any before the switch) and
    /// from then on receive `next`'s generations only.
    pub fn switch_to(&self, next: Arc<dyn ResultsSource>) -> Arc<dyn ResultsSource> {
        switch(&self.shared, next)
    }

    /// Opens the next source with `open` on a background thread, waits for
    /// its first snapshot, switches to it and shuts the previous source down
    /// after saving its index. Returns at once;
    /// [`is_switching`](Self::is_switching) is true until the thread is
    /// done.
    ///
    /// The first snapshot is the first one with entries to search: a source
    /// that loaded a saved index has it as soon as it opens, one that scans
    /// publishes it when the scan is done. Until then the current source
    /// keeps answering.
    ///
    /// The thread's result is `Err` when `open` failed or the next source
    /// stopped before its first snapshot; the current source then stays.
    /// Once switched it is `Ok`: a failure to save the previous index is
    /// only logged, since that index is no longer searched.
    ///
    /// # Errors
    ///
    /// [`io::ErrorKind::AlreadyExists`] while another switch runs, or the
    /// error spawning the thread.
    pub fn switch_when_ready<F>(&self, open: F) -> io::Result<JoinHandle<Result<(), String>>>
    where
        F: FnOnce() -> Result<Arc<dyn ResultsSource>, String> + Send + 'static,
    {
        {
            let mut state = self.shared.lock();
            if state.switching {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "a switch is already running",
                ));
            }
            state.switching = true;
        }
        let shared = Arc::downgrade(&self.shared);
        let spawned = thread::Builder::new()
            .name("fx-ui-switch".into())
            .spawn(move || {
                let ready = open().and_then(|next| {
                    wait_for_first_snapshot(&*next)?;
                    Ok(next)
                });
                // The window quit meanwhile: nothing to switch.
                let Some(shared) = shared.upgrade() else {
                    return ready.map(drop);
                };
                match ready {
                    Ok(next) => {
                        let previous = switch(&shared, next);
                        drop(shared);
                        // An orderly stop: the previous index is saved as
                        // when the app quits (only what a later launch needs
                        // to load it), then its last handle here
                        // goes (a search still running on it keeps it until
                        // it returns).
                        if let Err(e) = previous.save_for_quit() {
                            eprintln!("{}: cannot save the previous index: {e}", crate::APP_NAME);
                        }
                        drop(previous);
                        Ok(())
                    }
                    Err(e) => {
                        shared.lock().switching = false;
                        Err(e)
                    }
                }
            });
        spawned.inspect_err(|_| self.shared.lock().switching = false)
    }

    /// The current epoch and source.
    fn current(&self) -> (u16, Arc<dyn ResultsSource>) {
        let state = self.shared.lock();
        (state.epoch, Arc::clone(&state.current))
    }
}

/// Waits until `next` has entries to search (see
/// [`SwitchableSource::switch_when_ready`]).
fn wait_for_first_snapshot(next: &dyn ResultsSource) -> Result<(), String> {
    // Subscribe before looking, so a snapshot published in between wakes us.
    let published = next.subscribe();
    while next.status().entries == 0 {
        published
            .recv()
            .map_err(|_| "the new index stopped before its first snapshot".to_string())?;
    }
    Ok(())
}

/// Makes `next` current under a new epoch, tells the subscribers and starts
/// forwarding its generations. Returns the previous source.
fn switch(shared: &Arc<Shared>, next: Arc<dyn ResultsSource>) -> Arc<dyn ResultsSource> {
    // Subscribed before the swap: nothing `next` publishes is missed (a
    // generation already announced may come twice; it is not newer then).
    let generations = next.subscribe();
    let (epoch, previous) = {
        let mut state = shared.lock();
        state.epoch = state.epoch.saturating_add(1);
        state.switching = false;
        let previous = std::mem::replace(&mut state.current, next);
        let g = tag(state.epoch, state.current.generation());
        state.notify(g);
        (state.epoch, previous)
    };
    forward(shared, epoch, generations);
    previous
}

/// Forwards the generations of the source current in `epoch` to the
/// subscribers, tagged, on a thread that ends at the first generation after
/// a switch (or when the source or the switchable source goes away), so the
/// old source's generations never reach a subscriber.
fn forward(shared: &Arc<Shared>, epoch: u16, generations: Receiver<Generation>) {
    let shared = Arc::downgrade(shared);
    let spawned = thread::Builder::new()
        .name("fx-ui-switch-generations".into())
        .spawn(move || forward_loop(&shared, epoch, generations));
    if let Err(e) = spawned {
        // Without the thread the window still shows results; it only misses
        // live updates.
        eprintln!("{}: cannot forward index updates: {e}", crate::APP_NAME);
    }
}

fn forward_loop(shared: &Weak<Shared>, epoch: u16, generations: Receiver<Generation>) {
    for g in generations {
        let Some(shared) = shared.upgrade() else {
            return;
        };
        let mut state = shared.lock();
        if state.epoch != epoch {
            return;
        }
        state.notify(tag(epoch, g));
    }
}

impl ResultsSource for SwitchableSource {
    fn search(
        &self,
        query: &str,
        sort: SortKey,
        dir: Direction,
        offset: usize,
        limit: usize,
    ) -> Result<ResultPage, Cancelled> {
        let (epoch, source) = self.current();
        let mut page = source.search(query, sort, dir, offset, limit)?;
        page.generation = tag(epoch, page.generation);
        Ok(page)
    }

    /// The current source's grouped search ("Your files first"), its
    /// generation tagged like [`search`](ResultsSource::search)'s. Without
    /// it the trait's default would drop the flag and list the plain sort.
    fn search_grouped(
        &self,
        query: &str,
        sort: SortKey,
        dir: Direction,
        your_files_first: bool,
        offset: usize,
        limit: usize,
    ) -> Result<ResultPage, Cancelled> {
        let (epoch, source) = self.current();
        let mut page = source.search_grouped(query, sort, dir, your_files_first, offset, limit)?;
        page.generation = tag(epoch, page.generation);
        Ok(page)
    }

    /// The current source's search under the window's options,
    /// its generation tagged like [`search`](ResultsSource::search)'s.
    /// Without it the trait's default would drop the options.
    fn search_query(
        &self,
        query: &SearchQuery<'_>,
        offset: usize,
        limit: usize,
    ) -> Result<ResultPage, SearchError> {
        let (epoch, source) = self.current();
        let mut page = source.search_query(query, offset, limit)?;
        page.generation = tag(epoch, page.generation);
        Ok(page)
    }

    /// As [`positions_grouped`](ResultsSource::positions_grouped), under the
    /// window's options.
    fn positions_query(
        &self,
        query: &SearchQuery<'_>,
        paths: &[String],
    ) -> Result<RowPositions, Cancelled> {
        let (epoch, source) = self.current();
        let mut found = source.positions_query(query, paths)?;
        found.generation = tag(epoch, found.generation);
        Ok(found)
    }

    fn cancel(&self) {
        self.current().1.cancel();
    }

    /// `None` for a generation from before the last switch: its IDs belong
    /// to the previous index.
    fn row(&self, generation: Generation, id: u32) -> Option<Row> {
        let (epoch, inner) = untag(generation);
        let (current, source) = self.current();
        if epoch != current {
            return None;
        }
        source.row(inner, id)
    }

    /// The current source's answer, its generation tagged with the epoch:
    /// files are asked about by path, so a selection made in the previous
    /// index is found in the new one, and a lookup that answers from after
    /// a switch reads as newer than any page from before it (the controller
    /// then re-runs). A row is never read by ID across the switch.
    fn positions(
        &self,
        query: &str,
        sort: SortKey,
        dir: Direction,
        paths: &[String],
    ) -> Result<RowPositions, Cancelled> {
        let (epoch, source) = self.current();
        let mut found = source.positions(query, sort, dir, paths)?;
        found.generation = tag(epoch, found.generation);
        Ok(found)
    }

    /// As [`positions`](ResultsSource::positions), in the grouped order
    /// ("Your files first"). Without it the trait's default would drop the
    /// flag and give rows of the plain order.
    fn positions_grouped(
        &self,
        query: &str,
        sort: SortKey,
        dir: Direction,
        your_files_first: bool,
        paths: &[String],
    ) -> Result<RowPositions, Cancelled> {
        let (epoch, source) = self.current();
        let mut found = source.positions_grouped(query, sort, dir, your_files_first, paths)?;
        found.generation = tag(epoch, found.generation);
        Ok(found)
    }

    fn generation(&self) -> Generation {
        let (epoch, source) = self.current();
        tag(epoch, source.generation())
    }

    fn status(&self) -> SourceStatus {
        self.current().1.status()
    }

    fn subscribe(&self) -> Receiver<Generation> {
        let (tx, rx) = mpsc::channel();
        self.shared.lock().subscribers.push(tx);
        rx
    }

    /// The current source's: after a switch, the full index (the previous
    /// one was saved when it stopped).
    fn save_for_quit(&self) -> Result<(), String> {
        self.current().1.save_for_quit()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_zero_leaves_generations_unchanged() {
        for g in [0, 1, 500, INNER_MAX] {
            assert_eq!(tag(0, Generation(g)), Generation(g));
            assert_eq!(untag(Generation(g)), (0, Generation(g)));
        }
    }

    #[test]
    fn a_later_epoch_is_newer_whatever_the_generation() {
        assert!(tag(1, Generation(0)) > tag(0, Generation(INNER_MAX)));
        assert!(tag(2, Generation(1)) > tag(1, Generation(u64::MAX)));
        assert_eq!(untag(tag(3, Generation(42))), (3, Generation(42)));
        assert_eq!(
            untag(tag(1, Generation(u64::MAX))),
            (1, Generation(INNER_MAX)),
            "a huge generation is clamped, not carried into the epoch"
        );
    }
}
