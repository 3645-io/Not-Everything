//! What the app does when it quits, as data, so the AppKit layer
//! (`applicationWillTerminate:`) only performs the steps.
//!
//! Cmd+Q quits at once: the windows and the menu-bar item disappear
//! first, so the quit looks instant whatever follows; then the index is
//! saved only when the next launch could not load it otherwise.
//! That decision is the engine's (`fx_engine::Engine::checkpoint_for_quit`,
//! through [`ResultsSource::save_for_quit`](crate::source::ResultsSource::save_for_quit)),
//! because it has to be taken under the engine's save lock.

#![forbid(unsafe_code)]

/// One step of quitting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum QuitStep {
    /// Remove the menu-bar status item.
    RemoveStatusItem,
    /// Order out every window and show that on screen at once.
    HideWindows,
    /// Save the index if the next launch needs it
    /// ([`ResultsSource::save_for_quit`](crate::source::ResultsSource::save_for_quit)).
    SaveIndex,
}

/// The steps of a quit, in order: the menu-bar item (when `status_item`,
/// as in the resident app) and the windows go first, the index save last.
pub fn quit_steps(status_item: bool) -> Vec<QuitStep> {
    let mut steps = Vec::with_capacity(3);
    if status_item {
        steps.push(QuitStep::RemoveStatusItem);
    }
    steps.push(QuitStep::HideWindows);
    steps.push(QuitStep::SaveIndex);
    steps
}
