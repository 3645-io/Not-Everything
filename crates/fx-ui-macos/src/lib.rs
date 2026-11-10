//! Native user interface for macOS.
//!
//! The results logic and the interface's values are platform-agnostic and
//! build and test on any host:
//!
//! - [`source`]: the [`source::ResultsSource`] trait the window reads from,
//!   implemented for `fx_engine::Engine` and for [`source::EngineSource`]
//!   (the engine plus its root, for absolute paths).
//! - [`columns`]: the four columns (Name, Path, Size, Date Modified) and
//!   the header-click sort state.
//! - [`controller`]: [`controller::ResultsController`], the synchronous
//!   state machine for the query, sort, stale-result dropping, throttled
//!   re-runs on new snapshot generations, cached row windows and the status
//!   text.
//! - [`actions`]: the read-only actions (open, reveal in Finder, copy path),
//!   the key map and the drag-out operation mask (copy and link only).
//! - [`app_args`]: command-line options of the `fx-ui-macos` app binary and
//!   the per-root engine store path.
//! - [`data_folder`]: the data folder's owner-only folders and files.
//! - [`bundle`]: the `.app` bundle writer and signer behind the
//!   `make_bundle` example, and a directory-size helper.
//!
//! The resident app:
//!
//! - [`hotkey`]: the global summon hotkey spec (default option+space, or
//!   shift+option+space in a Preview build), its string form and its Carbon
//!   key code and modifier mask.
//! - [`settings`]: the line-based `key=value` settings file under
//!   `~/Library/Application Support/Not-Everything/`, saved atomically.
//! - [`onboarding`]: the Full Disk Access onboarding state machine (banner,
//!   limited non-FDA mode, switch to the full index after a grant, recheck
//!   schedule).
//! - [`limited`]: [`limited::ExcludingChangeSource`], which in limited mode
//!   drops change events at or below the consent-prompt folders before they
//!   reach the engine (whose writer would stat them).
//! - [`switchable`]: [`switchable::SwitchableSource`], the results source of
//!   a limited launch, which moves the window to the full-access index once
//!   Full Disk Access is granted, without a relaunch.
//! - [`your_files`]: the folders the **Your files first** setting lists
//!   first.
//! - [`menu`] and [`main_menu`]: the menu-bar item's menu and the main menu
//!   bar as data; read-only, no destructive items.
//! - [`appearance`]: the System / Light / Dark appearance choice.
//! - [`app_icon`]: the six approved app icon designs and their embedded
//!   light and dark images.
//! - [`settings_view`]: the Settings window's copy and contents.
//! - [`shortcut_recorder`]: the Settings recorder that picks a new summon
//!   hotkey; [`hotkey_slot`]: the registered hotkey and how a picked one
//!   replaces it, the new one registered before the old one goes.
//! - [`quit`]: the steps of quitting, in order (menu-bar item and windows
//!   first, then the index save, which usually writes nothing).
//! - [`notices`]: the third-party notices the About panel shows.
//!
//! Release candidates and betas are a separate app, Not-Everything Preview,
//! chosen at compile time by `fx_channel::Channel::CURRENT`, so testing
//! never touches the daily app: [`APP_NAME`], [`APP_BUNDLE_ID`], the data
//! folder ([`app_args::APP_FOLDER`]) and the default hotkey follow the
//! channel. Each build shows its channel and version:
//!
//! - [`build_info`]: the window title, the menu-bar menu's first line and
//!   the About panel's version label and build number.
//! - [`dock_ribbon`]: the corner ribbon ("RC" or "BETA") a Preview build
//!   draws on its Dock icon: text, colours and geometry.
//!
//! The windows are custom-drawn in the 3645 design system, after the
//! owner's app mockups, rather than built from standard AppKit controls.
//! Its values and rules are platform-agnostic:
//!
//! - [`theme`]: the 3645 tokens for Light and Dark (colours, shadows, type,
//!   spacing, radii) and the mockups' component metrics.
//! - [`formats`]: sizes, local dates, "~" folders, counts, search times.
//! - [`file_glyph`]: the glyph a row shows and the glyphs' paths.
//! - [`status_view`]: the status bar's slots; [`key_caps`]: key caps and
//!   the result hints.
//! - [`chips`]: the Aa, Path and .* search options.
//! - [`main_layout`] and [`window_view`]: where the search window's parts
//!   go and what it shows in each state.
//! - [`column_sizes`]: the column widths the dividers between the column
//!   titles set (drag, arrow keys, the default layout).
//! - [`ui_text`]: the search window's copy.
//! - [`surface`]: the surfaces and glyphs rendered into pixels, once per
//!   theme and size class, for `appkit::window::surfaces` to share.
//!
//! The measurement tools are built only with the `measure` feature, which
//! release builds leave off and the gate scripts turn on: `model` (a
//! virtual row model over synthetic names from `fx-bench`), `frame_stats`
//! (per-frame scroll timings and the frame-time gate), `spike_args` (the
//! command line of the `table_spike` example), `gate_probe` (the Phase 1
//! gate probe behind `--gate-probe <out>`), `summon_probe` and `sort_probe`
//! (the shortcut-to-ready-window and re-sort-to-repaint probes behind
//! `--summon-probe <out>` and `--sort-probe <out>`), `latency_probe` (their
//! report) and `source::SyntheticSource` (millions of synthetic rows, for
//! measuring the window without an index).
//!
//! On macOS, `appkit` (built only for `target_os = "macos"`) is the AppKit
//! layer: the product window (`appkit::window`: search field, sortable
//! table and status bar over a [`controller::ResultsController`], with the
//! read-only actions and drag-out), `appkit::app`, which the `fx-ui-macos`
//! binary runs over an engine, and with `measure` the Phase 0 spike window
//! of the `table_spike` example.
//!
//! The UI is read-only: Enter opens, Cmd+Enter reveals in Finder,
//! Option+Cmd+C copies full paths, and dragging a result out offers copy
//! and link only.
//! There is no delete, rename, move or trash action. Quick Look on Space is
//! not built yet.

pub mod actions;
pub mod app_args;
pub mod app_icon;
pub mod appearance;
#[cfg(target_os = "macos")]
pub mod appkit;
pub mod build_info;
pub mod bundle;
pub mod chips;
pub mod column_sizes;
pub mod columns;
pub mod controller;
pub mod daily_save;
pub mod data_folder;
pub mod dock_ribbon;
pub mod file_glyph;
pub mod formats;
#[cfg(feature = "measure")]
pub mod frame_stats;
#[cfg(feature = "measure")]
pub mod gate_probe;
pub mod hotkey;
pub mod hotkey_slot;
pub mod key_caps;
#[cfg(feature = "measure")]
pub mod latency_probe;
pub mod limited;
pub mod main_layout;
pub mod main_menu;
pub mod memory_relief;
pub mod menu;
#[cfg(feature = "measure")]
pub mod model;
pub mod notices;
pub mod onboarding;
pub mod quit;
pub mod settings;
pub mod settings_view;
pub mod shortcut_recorder;
#[cfg(feature = "measure")]
pub mod sort_probe;
pub mod source;
#[cfg(feature = "measure")]
pub mod spike_args;
pub mod status_view;
#[cfg(feature = "measure")]
pub mod summon_probe;
pub mod surface;
pub mod switchable;
pub mod theme;
pub mod ui_text;
pub mod window_view;
pub mod your_files;

/// The app's name in its bundle, menus, windows and Settings window: this
/// build channel's, Not-Everything for a release and Not-Everything Preview
/// for a release candidate or beta. Crates keep the `fx-` prefix.
pub const APP_NAME: &str = fx_channel::Channel::CURRENT.app_name();

/// The app's bundle identifier: `io.3645.not-everything`, or
/// `io.3645.not-everything.preview` for a Preview build.
pub const APP_BUNDLE_ID: &str = fx_channel::Channel::CURRENT.bundle_id();

/// A fixed list of values in a defined order, such as every
/// [`columns::Column`] or every [`actions::Action`]. Iterates like an array:
/// `list.iter()` borrows, `for x in list` yields values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FixedList<T: 'static, const N: usize>(pub [T; N]);

impl<T: 'static, const N: usize> FixedList<T, N> {
    /// Iterates over references to the values in order.
    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.0.iter()
    }

    /// The values as a slice.
    pub const fn as_slice(&self) -> &[T] {
        &self.0
    }

    /// Number of values.
    pub const fn len(&self) -> usize {
        N
    }

    /// Whether the list is empty.
    pub const fn is_empty(&self) -> bool {
        N == 0
    }

    /// Whether `value` is in the list.
    pub fn contains(&self, value: &T) -> bool
    where
        T: PartialEq,
    {
        self.0.contains(value)
    }
}

impl<T: 'static, const N: usize> IntoIterator for FixedList<T, N> {
    type Item = T;
    type IntoIter = std::array::IntoIter<T, N>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a, T: 'static, const N: usize> IntoIterator for &'a FixedList<T, N> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}
