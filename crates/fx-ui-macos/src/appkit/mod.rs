//! The macOS AppKit layer (macOS only).
//!
//! - [`window`]: the product window, a search field, results table and
//!   status bar over a
//!   [`ResultsController`](crate::controller::ResultsController), with the
//!   read-only actions and drag-out, the menu-bar item, the Full Disk Access
//!   onboarding and the Settings window.
//! - [`app`]: what the `fx-ui-macos` binary runs: an engine over a root.
//! - [`summon`]: registers the global summon hotkey; [`login_item`]: wraps
//!   `SMAppService` for Launch at Login.
//! - [`time_zone`]: the local time zone for the results table's dates.
//! - `spike` (`measure` builds only): the Phase 0 UI spike window the
//!   `table_spike` example measures.

pub mod app;
pub mod login_item;
#[cfg(feature = "measure")]
pub mod spike;
pub mod summon;
pub mod time_zone;
pub mod window;
