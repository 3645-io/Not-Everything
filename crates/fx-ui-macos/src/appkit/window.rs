//! The product window: an `NSSearchField`, a view-based `NSTableView`
//! (Name, Path, Size, Date Modified; fixed row height) in an `NSScrollView`
//! and a status bar, over a
//! [`ResultsController`](crate::controller::ResultsController).
//!
//! The window is custom-drawn in the 3645 design: a transparent title bar
//! over a custom top bar (`chrome.rs`) with the search field and the Aa /
//! Path / .* chips, the results card (`card.rs`), the Full Disk Access
//! callout (`callout.rs`) and the status bar (`status.rs`), built from
//! shared surfaces (`views.rs`, `surfaces.rs`), in Light and Dark following
//! the appearance.
//!
//! Threads:
//!
//! - Main thread: all AppKit work and the controller.
//! - One search worker: receives
//!   [`SearchRequest`](crate::controller::SearchRequest)s, skips to the
//!   newest, runs it on the source and posts the page to the inbox. A new
//!   query or sort cancels the search in flight
//!   (`ResultsController::set_query` and `set_sort` call the source's
//!   `cancel`).
//! - One generation listener: forwards the source's new snapshot
//!   generations to the inbox, so the first scan shows results
//!   progressively.
//!
//! Both background threads wake the main thread with
//! `performSelectorOnMainThread:withObject:waitUntilDone:` on a small
//! `Waker` object (`classes.rs`); the main thread drains the inbox, applies
//! pages (the controller drops stale ones) and reloads the table. Sorting is the
//! engine's: header clicks go through sort descriptors to `set_sort`, and
//! AppKit never sorts.
//!
//! Actions are read-only (see [`crate::actions`]): Enter opens, Cmd+Enter
//! reveals in Finder, Cmd+C copies full paths, double-click opens, and
//! dragging rows out offers file URLs with copy and link operations only.
//! In the resident app Escape clears the search, then hides the window and
//! hands the focus back to the previous app, as the summon hotkey does.
//!
//! The app runs resident when [`WindowConfig::resident`] is set: a status
//! item and menu, the global summon hotkey, Launch at Login and the Full
//! Disk Access banner (see `resident.rs`). Closing the window only hides
//! it. It is a regular app with a Dock icon and its own menu bar
//! ([`crate::main_menu`]), plus a Settings window for the appearance and the
//! Dock icon (see `settings_window.rs`).

mod callout;
mod card;
mod chrome;
mod classes;
#[cfg(debug_assertions)]
mod debug_script;
pub mod paint;
mod resident;
mod ribbon;
mod settings_controls;
mod settings_window;
mod status;
pub mod surfaces;
mod views;

pub use resident::{FullIndexSwitch, OpenFullIndex, ResidentConfig};

use std::ffi::CString;
#[cfg(feature = "measure")]
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
#[cfg(feature = "measure")]
use objc2::sel;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAboutPanelOptionApplicationVersion, NSAboutPanelOptionCredits, NSAboutPanelOptionVersion,
    NSApplication, NSApplicationActivationPolicy, NSColor, NSDragOperation, NSEventModifierFlags,
    NSFont, NSFontAttributeName, NSForegroundColorAttributeName, NSMenu, NSMenuItem, NSScreen,
};
#[cfg(feature = "measure")]
use objc2_foundation::NSTimer;
use objc2_foundation::{NSAttributedString, NSDictionary, NSPoint, NSRect, NSSize, NSString};

use crate::APP_NAME;
use crate::actions::DRAG_OPERATION_MASK;
use crate::build_info::BuildInfo;
use crate::main_layout::initial_size;
use crate::main_menu::{Item, MenuKind, Shortcut, main_menu};
use crate::notices::{THIRD_PARTY_NOTICES, about_credits};
use crate::source::ResultsSource;
use crate::theme::metrics;
use callout::Callout;
use card::ResultsCard;
use chrome::{RootView, SearchWindow, TopBar};
use classes::{WinController, WinUi};
use status::StatusBar;

/// Fixed table row height in points: the design's 40-point rows; the
/// table adds the 2-point gap between them.
pub const ROW_HEIGHT: f64 = metrics::ROW_HEIGHT;
/// Frames scrolled in `--autoscroll` mode.
#[cfg(feature = "measure")]
pub const AUTOSCROLL_FRAMES: u64 = 600;
/// Rows scrolled per autoscroll frame (as in the spike).
#[cfg(feature = "measure")]
pub const AUTOSCROLL_ROWS_PER_FRAME: usize = crate::spike_args::DEFAULT_ROWS_PER_FRAME;
/// Most items Enter opens at once, so a large selection can't launch
/// thousands of applications.
pub const MAX_OPEN: usize = 20;

/// The app is a regular app: a Dock icon and its own menu bar, resident or
/// not. An accessory app would have neither.
pub(crate) const ACTIVATION_POLICY: NSApplicationActivationPolicy =
    NSApplicationActivationPolicy::Regular;

/// What the window runs on.
pub struct WindowConfig {
    /// Where the results come from.
    pub source: Arc<dyn ResultsSource>,
    /// The window title ([`crate::build_info::window_title`]).
    pub title: String,
    /// List your files first (the saved setting).
    pub your_files_first: bool,
    /// Scroll `AUTOSCROLL_FRAMES` frames, print frame stats and exit.
    #[cfg(feature = "measure")]
    pub autoscroll: bool,
    /// Print the time to the first painted results and exit.
    #[cfg(feature = "measure")]
    pub launch_probe: bool,
    /// Run the Phase 1 gate probe (`crate::gate_probe::ProbeRun`), write its
    /// report to this new file and exit.
    #[cfg(feature = "measure")]
    pub gate_probe: Option<PathBuf>,
    /// Recorded as early as possible in `main`; the measurement modes time
    /// the launch from it.
    pub process_start: Instant,
    /// Run as a resident menu-bar app; `None` runs a plain window app that
    /// quits when its window closes (measurement modes).
    pub resident: Option<ResidentConfig>,
}

/// Why the window could not open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowError {
    /// [`run`] was called off the main thread.
    NotMainThread,
}

impl std::fmt::Display for WindowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WindowError::NotMainThread => f.write_str("the window must run on the main thread"),
        }
    }
}

impl std::error::Error for WindowError {}

/// The drag operations offered for rows dragged out of the table:
/// [`DRAG_OPERATION_MASK`] (copy and link only) as an `NSDragOperation`.
pub fn drag_operation() -> NSDragOperation {
    NSDragOperation(DRAG_OPERATION_MASK as usize)
}

/// Builds the window and runs the application until it stops. In
/// measurement modes the app prints its report and terminates the process.
pub fn run(config: WindowConfig) -> Result<(), WindowError> {
    let mtm = MainThreadMarker::new().ok_or(WindowError::NotMainThread)?;
    let app = NSApplication::sharedApplication(mtm);
    let resident = config.resident.is_some();
    app.setActivationPolicy(ACTIVATION_POLICY);
    // Only the resident app has settings to show.
    install_main_menu(mtm, &app, resident);

    #[cfg(feature = "measure")]
    let measuring = config.autoscroll || config.launch_probe || config.gate_probe.is_some();
    let title = config.title.clone();
    let controller = WinController::new(mtm, config);
    let ui = build_window(mtm, &controller, &title, resident);
    controller.set_ui(ui);
    let Some(ui) = controller.ui() else {
        unreachable!("the window was just built")
    };
    ui.window.center();
    ui.window.makeKeyAndOrderFront(None);
    ui.window.makeFirstResponder(Some(&ui.search));
    controller.start();

    app.setDelegate(Some(ProtocolObject::from_ref(&*controller)));
    controller.start_resident();

    #[cfg(feature = "measure")]
    if measuring {
        let fps = NSScreen::mainScreen(mtm)
            .map(|s| s.maximumFramesPerSecond())
            .filter(|&f| f > 0)
            .unwrap_or(60);
        let target: &AnyObject = controller.as_ref();
        // SAFETY: `measureTick:` is implemented by WinController as
        // `-(void)measureTick:(NSTimer *)timer`; the run loop retains the
        // timer and the timer retains its target.
        let _timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                1.0 / fps as f64,
                target,
                sel!(measureTick:),
                None,
                true,
            )
        };
    }

    app.run();
    drop(controller);
    Ok(())
}

/// Renders [`main_menu`] as the app's menu bar and makes its Window menu
/// the windows menu, which AppKit fills with the open windows. Items send
/// their action to the first responder that answers it (Settings… reaches
/// the window controller as the app delegate); AppKit disables the rest.
fn install_main_menu(mtm: MainThreadMarker, app: &NSApplication, settings: bool) {
    let menu_bar = NSMenu::new(mtm);
    for submenu in main_menu(APP_NAME, settings) {
        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(&submenu.title));
        for item in &submenu.items {
            let (title, command) = match item {
                Item::Separator => {
                    menu.addItem(&NSMenuItem::separatorItem(mtm));
                    continue;
                }
                Item::Command { title, command } => (title, *command),
            };
            let action = CString::new(command.selector())
                .map(|name| Sel::register(&name))
                .expect("selector names have no NUL");
            let (key, mask) = command
                .shortcut()
                .map_or(("", None), |s| (s.key, Some(modifier_mask(s))));
            // SAFETY: every action is a standard responder-chain action or
            // the controller's `showSettings:`, all `-(void)name:(id)sender`;
            // a nil target sends it to the first responder that implements it.
            let entry = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str(title),
                    Some(action),
                    &NSString::from_str(key),
                )
            };
            if let Some(mask) = mask {
                entry.setKeyEquivalentModifierMask(mask);
            }
            menu.addItem(&entry);
        }
        let item = NSMenuItem::new(mtm);
        item.setSubmenu(Some(&menu));
        menu_bar.addItem(&item);
        if submenu.kind == MenuKind::Window {
            app.setWindowsMenu(Some(&menu));
        }
    }
    app.setMainMenu(Some(&menu_bar));
}

/// Shows the standard About panel with this build's version label and
/// build number ([`BuildInfo::about`]), "Version 0.2.0-RC-1 (200)", rather
/// than the bundle's bare version, and the third-party notices as its
/// scrolling credits, which the open-source licences of the crates it is
/// built with ask for.
pub(crate) fn show_about_panel(mtm: MainThreadMarker) {
    let about = BuildInfo::CURRENT.about();
    let application_version = NSString::from_str(&about.application_version);
    let version = NSString::from_str(&about.build);
    let credits = about_credits_text();
    let values: [&AnyObject; 3] = [
        application_version.as_ref(),
        version.as_ref(),
        credits.as_ref(),
    ];
    // SAFETY: the option keys are immutable framework constants.
    let keys = unsafe {
        [
            NSAboutPanelOptionApplicationVersion,
            NSAboutPanelOptionVersion,
            NSAboutPanelOptionCredits,
        ]
    };
    let options = NSDictionary::<NSString, AnyObject>::from_slices(&keys, &values);
    // SAFETY: the version options take an NSString and the credits an
    // NSAttributedString, as given.
    unsafe {
        NSApplication::sharedApplication(mtm).orderFrontStandardAboutPanelWithOptions(&options);
    }
}

/// The third-party notices as plain text ([`about_credits`]) in the small
/// system font and the label colour, which follows light and dark.
fn about_credits_text() -> Retained<NSAttributedString> {
    let text = NSString::from_str(&about_credits(THIRD_PARTY_NOTICES));
    let font = NSFont::systemFontOfSize(NSFont::smallSystemFontSize());
    let color = NSColor::labelColor();
    let values: [&AnyObject; 2] = [font.as_ref(), color.as_ref()];
    // SAFETY: the attribute names are immutable framework constants.
    let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    let attributes = NSDictionary::<NSString, AnyObject>::from_slices(&keys, &values);
    // SAFETY: the attributes hold the classes AppKit expects (NSFont,
    // NSColor).
    unsafe { NSAttributedString::new_with_attributes(&text, &attributes) }
}

/// The modifier mask of `shortcut`: Command, plus Shift and Option when set.
fn modifier_mask(shortcut: Shortcut) -> NSEventModifierFlags {
    let mut mask = NSEventModifierFlags::Command;
    if shortcut.shift {
        mask |= NSEventModifierFlags::Shift;
    }
    if shortcut.option {
        mask |= NSEventModifierFlags::Option;
    }
    mask
}

/// Builds the window: the root view on the `ground`, then the results
/// card, the access callout (resident app only), the status bar and, on
/// top, the top bar, wired to `controller`. The parts are laid out by the
/// controller from the window's size.
fn build_window(
    mtm: MainThreadMarker,
    controller: &WinController,
    title: &str,
    resident: bool,
) -> WinUi {
    let screen = NSScreen::mainScreen(mtm).map(|s| s.visibleFrame().size);
    let (w, h) = screen.map_or((metrics::WINDOW_WIDTH, metrics::WINDOW_HEIGHT), |s| {
        initial_size(s.width, s.height)
    });
    let window = SearchWindow::new(mtm, rect(0.0, 0.0, w, h));
    // The title stays for the Window menu; the top bar shows the name.
    window.setTitle(&NSString::from_str(title));
    window.setContentMinSize(NSSize::new(
        metrics::WINDOW_MIN_WIDTH,
        metrics::WINDOW_MIN_HEIGHT,
    ));
    let root = RootView::new(mtm);
    window.setContentView(Some(&root));

    let target: &AnyObject = controller.as_ref();
    let card = ResultsCard::new(mtm, controller);
    card.attach(&root);
    let callout = resident.then(|| {
        let callout = Callout::new(mtm, target);
        callout.attach(&root);
        callout
    });
    let status = StatusBar::new(mtm);
    status.attach(&root);
    let top = TopBar::new(mtm, controller, controller.options());
    top.attach(&root);

    WinUi {
        search: top.search.clone(),
        table: card.table.clone(),
        window,
        root,
        top,
        card,
        status,
        callout,
    }
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drag_offers_copy_and_link_only() {
        let op = drag_operation();
        assert_eq!(op, NSDragOperation::Copy | NSDragOperation::Link);
        for forbidden in [
            NSDragOperation::Move,
            NSDragOperation::Delete,
            NSDragOperation::Generic,
        ] {
            assert!(!op.intersects(forbidden), "{forbidden:?} must stay clear");
        }
    }

    /// An `Accessory` app has no Dock icon and macOS never shows its menu
    /// bar.
    #[test]
    fn the_app_shows_in_the_dock_with_its_menu_bar() {
        assert_eq!(ACTIVATION_POLICY, NSApplicationActivationPolicy::Regular);
    }

    #[cfg(feature = "measure")]
    #[test]
    fn run_off_the_main_thread_is_an_error() {
        if MainThreadMarker::new().is_some() {
            return;
        }
        let config = WindowConfig {
            source: Arc::new(crate::source::SyntheticSource::new(10, 1)),
            title: String::new(),
            your_files_first: true,
            autoscroll: false,
            launch_probe: false,
            gate_probe: None,
            process_start: Instant::now(),
            resident: None,
        };
        assert_eq!(run(config).err(), Some(WindowError::NotMainThread));
    }
}
