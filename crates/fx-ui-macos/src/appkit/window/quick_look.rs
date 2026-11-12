//! The system Quick Look panel (`QLPreviewPanel`, from the QuickLookUI
//! framework): Space in the results shows the selected files in it, as in
//! Finder (the rules are in [`crate::quick_look`]).
//!
//! The panel is shared by the whole app and finds its controller in the
//! responder chain: the search window accepts it (`SearchWindow`'s
//! `acceptsPreviewPanelControl:` in `chrome.rs`) and hands it to the window
//! controller, which becomes its data source (the selected files' URLs) and
//! delegate (key presses go to the results, so the arrow keys move the
//! selection; the panel zooms from the selected row).
//!
//! QuickLookUI is a system framework, loaded the first time Quick Look
//! opens, so a launch, and a session that never uses Quick Look, don't pay
//! for it; `objc2` then finds the class by name, so no binding crate is
//! needed. Until it is loaded the panel can't exist, and asking whether it
//! is open loads nothing. Main thread only.

use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2_app_kit::NSWindow;
use objc2_foundation::NSString;

/// Where QuickLookUI is: its own framework, or inside Quartz (the
/// umbrella framework that also loads it) on systems without the first.
const FRAMEWORKS: [&str; 2] = [
    "/System/Library/Frameworks/QuickLookUI.framework",
    "/System/Library/Frameworks/Quartz.framework",
];

/// The `QLPreviewPanel` class if QuickLookUI is loaded.
fn loaded_class() -> Option<&'static AnyClass> {
    AnyClass::get(c"QLPreviewPanel")
}

/// The `QLPreviewPanel` class, loading QuickLookUI first if needed.
fn class() -> Option<&'static AnyClass> {
    if let Some(class) = loaded_class() {
        return Some(class);
    }
    let bundles = AnyClass::get(c"NSBundle")?;
    for path in FRAMEWORKS {
        let path = NSString::from_str(path);
        // SAFETY: `+bundleWithPath:` takes an NSString and returns a bundle
        // or nil; `-load` takes no arguments and returns BOOL. Loading a
        // system framework runs only Apple's code, as linking it would.
        let loaded = unsafe {
            let bundle: Option<Retained<AnyObject>> = msg_send![bundles, bundleWithPath: &*path];
            bundle.is_some_and(|b| {
                let ok: bool = msg_send![&*b, load];
                ok
            })
        };
        if loaded && loaded_class().is_some() {
            break;
        }
    }
    loaded_class()
}

/// The shared panel, made on first use.
fn shared() -> Option<Retained<NSWindow>> {
    let class = class()?;
    // SAFETY: `+sharedPreviewPanel` takes no arguments and returns the
    // shared QLPreviewPanel, an NSPanel and so an NSWindow; called on the
    // main thread, as every caller of this module is.
    unsafe { msg_send![class, sharedPreviewPanel] }
}

/// The shared panel if it was made already; asking for it otherwise would
/// make it.
pub(super) fn existing() -> Option<Retained<NSWindow>> {
    let class = loaded_class()?;
    // SAFETY: `+sharedPreviewPanelExists` takes no arguments and returns
    // BOOL.
    let exists: bool = unsafe { msg_send![class, sharedPreviewPanelExists] };
    if !exists {
        return None;
    }
    // SAFETY: as in `shared`; the panel exists, so nothing is made.
    unsafe { msg_send![class, sharedPreviewPanel] }
}

/// Whether the panel is on screen.
pub(super) fn is_open() -> bool {
    existing().is_some_and(|panel| panel.isVisible())
}

/// Shows the panel; it then asks the responder chain for its controller.
pub(super) fn open() {
    if let Some(panel) = shared() {
        panel.makeKeyAndOrderFront(None);
    }
}

/// Closes the panel if it is on screen.
pub(super) fn close() {
    if let Some(panel) = existing().filter(|p| p.isVisible()) {
        panel.orderOut(None);
    }
}

/// Makes the panel ask its data source again.
pub(super) fn reload(panel: &NSWindow) {
    // SAFETY: `-reloadData` takes no arguments and returns void; `panel` is
    // the shared QLPreviewPanel.
    let _: () = unsafe { msg_send![panel, reloadData] };
}

/// Makes `controller` (or nothing) the panel's data source and delegate.
pub(super) fn set_controller(panel: &NSWindow, controller: Option<&AnyObject>) {
    // SAFETY: `-setDataSource:` and `-setDelegate:` take an object or nil
    // and return void. The window controller answers the data source's two
    // required methods and the delegate's optional ones, and lives until
    // the app quits; `endPreviewPanelControl:` sets nil before control
    // passes elsewhere.
    unsafe {
        let _: () = msg_send![panel, setDataSource: controller];
        let _: () = msg_send![panel, setDelegate: controller];
    }
}
