//! The right-click menu's AppKit side (the rules are in
//! [`crate::context_menu`]): its `NSMenuItem`s, the targets' kinds on disk,
//! the Open With submenu's apps, and what the items that aren't key actions
//! do: open with an app, App Store…, Other…, Share… and New Terminal at
//! Folder. Copy's pasteboard writer is here too.
//!
//! The menu is a native `NSMenu`: macOS draws its panel, highlight, rows
//! and font, so the boards' `.ne-menu` styling can't be applied; it follows
//! the app's Light or Dark appearance. Each item shows the SF Symbol
//! closest to the boards' glyph as a template image, and each Open With
//! app its own icon, as in Finder; on macOS 27, which hides menu images by
//! default, each item asks for its image to show.
//!
//! Main thread only, except the pasteboard functions.

use std::cell::RefCell;
use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{
    AnyThread, ClassType, Encode, Encoding, MainThreadMarker, MainThreadOnly, msg_send, sel,
};
use objc2_app_kit::{
    NSEventModifierFlags, NSImage, NSMenuItem, NSModalResponseOK, NSOpenPanel, NSPasteboard,
    NSPasteboardWriting, NSSharingServicePicker, NSView, NSWorkspace, NSWorkspaceOpenConfiguration,
};
use objc2_foundation::{
    NSArray, NSBundle, NSFileManager, NSRect, NSRectEdge, NSSize, NSString, NSURL,
};

use crate::context_menu::{
    AppChoice, ContextItem, OpenWithEntry, TargetKind, common_apps, common_default,
    is_application_bundle,
};

/// Terminal's bundle identifier.
const TERMINAL: &str = "com.apple.Terminal";
/// The Mac App Store's bundle identifier.
const APP_STORE: &str = "com.apple.AppStore";
/// The type Other… offers: app bundles (`UTType.applicationBundle`).
const APPLICATION_BUNDLE_TYPE: &str = "com.apple.application-bundle";
/// The size the Open With apps' icons are drawn at, as in Finder's menu.
const APP_ICON_SIZE: f64 = 16.0;

thread_local! {
    /// The share picker on screen, kept until the next Share….
    static PICKER: RefCell<Option<Retained<NSSharingServicePicker>>> = const { RefCell::new(None) };
}

/// A file URL for `path`.
pub(super) fn file_url(path: &str) -> Retained<NSURL> {
    NSURL::fileURLWithPath(&NSString::from_str(path))
}

fn file_urls(paths: &[String]) -> Retained<NSArray<NSURL>> {
    let urls: Vec<Retained<NSURL>> = paths.iter().map(|p| file_url(p)).collect();
    NSArray::from_retained_slice(&urls)
}

// ------------------------------------------------------------ pasteboard

/// The pasteboard Copy and Copy Full Path write to: the general one. A
/// debug build's in-app checks may name another (`NE_DEBUG_PASTEBOARD`), so
/// they never touch the user's clipboard.
pub(super) fn copy_pasteboard() -> Retained<NSPasteboard> {
    #[cfg(debug_assertions)]
    if let Some(name) = std::env::var("NE_DEBUG_PASTEBOARD")
        .ok()
        .filter(|n| !n.is_empty())
    {
        return NSPasteboard::pasteboardWithName(&NSString::from_str(&name));
    }
    NSPasteboard::generalPasteboard()
}

/// Puts the files at `paths` on `pasteboard` as file URLs, as Finder's
/// Copy does, so pasting in Finder copies them. Returns whether the
/// pasteboard took them.
pub(super) fn write_files(pasteboard: &NSPasteboard, paths: &[String]) -> bool {
    pasteboard.clearContents();
    let objects: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = paths
        .iter()
        .map(|p| ProtocolObject::from_retained(file_url(p)))
        .collect();
    pasteboard.writeObjects(&NSArray::from_retained_slice(&objects))
}

/// The file paths on `pasteboard`, in order (the debug script's
/// `pasteboard` step and the tests).
#[cfg(any(test, debug_assertions))]
pub(super) fn read_files(pasteboard: &NSPasteboard) -> Vec<String> {
    let classes = NSArray::from_slice(&[NSURL::class()]);
    // SAFETY: the class list holds NSURL, a class that reads from a
    // pasteboard; no options are passed.
    let objects = unsafe { pasteboard.readObjectsForClasses_options(&classes, None) };
    objects
        .map(|a| {
            a.iter()
                .filter_map(|o| o.downcast::<NSURL>().ok())
                .filter(|u| u.isFileURL())
                .filter_map(|u| u.path().map(|p| p.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// The types on `pasteboard` (the debug script's `pasteboard` step).
#[cfg(debug_assertions)]
pub(super) fn pasteboard_types(pasteboard: &NSPasteboard) -> Vec<String> {
    pasteboard
        .types()
        .map(|t| t.iter().map(|s| s.to_string()).collect())
        .unwrap_or_default()
}

// ----------------------------------------------------------------- kinds

/// What `path` is on disk: a folder, a package, an app or a file. A path
/// that can't be read counts as a file. Reads the folder's metadata only,
/// never a file's contents, so nothing is downloaded from the cloud.
pub(super) fn target_kind(workspace: &NSWorkspace, path: &str) -> TargetKind {
    let is_dir = std::fs::metadata(path).is_ok_and(|m| m.is_dir());
    if !is_dir {
        return TargetKind::File;
    }
    if !workspace.isFilePackageAtPath(&NSString::from_str(path)) {
        TargetKind::Folder
    } else if is_application_bundle(path) {
        TargetKind::Application
    } else {
        TargetKind::Package
    }
}

// ----------------------------------------------------------------- items

/// The SF Symbol `name` as a template image, if this macOS has it.
fn symbol(name: &str) -> Option<Retained<NSImage>> {
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(name),
        None,
    )?;
    image.setTemplate(true);
    Some(image)
}

/// `NSMenuItemImageVisibilityVisible` (macOS 27's `NSMenuItem.h`).
const IMAGE_VISIBLE: isize = 1;

/// Shows `image` on `item`. From macOS 27 AppKit hides menu item images
/// unless asked to show them (`preferredImageVisibility`); the boards give
/// every item its glyph, so they are asked for. Earlier systems always
/// show them.
fn set_image(item: &NSMenuItem, image: Option<&NSImage>) {
    item.setImage(image);
    if image.is_some() && item.respondsToSelector(sel!(setPreferredImageVisibility:)) {
        // SAFETY: `setPreferredImageVisibility:` (macOS 27) takes an
        // NSInteger, here `NSMenuItemImageVisibilityVisible`; the check
        // above keeps it from older systems.
        let _: () = unsafe { msg_send![item, setPreferredImageVisibility: IMAGE_VISIBLE] };
    }
}

/// A menu item titled `title` that sends `action` to `target`.
fn menu_item(
    mtm: MainThreadMarker,
    title: &str,
    action: Sel,
    target: &AnyObject,
) -> Retained<NSMenuItem> {
    // SAFETY: every action given is one of the window controller's
    // `-(void)name:(id)sender` methods, and the target is the controller.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(action),
            &NSString::from_str(""),
        )
    };
    // SAFETY: the target is the window controller, which lives until the
    // app quits and implements `action`.
    unsafe { item.setTarget(Some(target)) };
    item
}

/// The `NSMenuItem` for `item`: its title, its shortcut shown as Finder
/// shows it, its tag and its glyph, sending `action` to `target`.
pub(super) fn context_item(
    mtm: MainThreadMarker,
    item: ContextItem,
    action: Sel,
    target: &AnyObject,
) -> Retained<NSMenuItem> {
    let entry = menu_item(mtm, item.title(), action, target);
    entry.setTag(item.tag());
    if let Some(shortcut) = item.shortcut() {
        entry.setKeyEquivalent(&NSString::from_str(shortcut.key_equivalent()));
        let mut mask = NSEventModifierFlags::empty();
        if shortcut.mods.command {
            mask |= NSEventModifierFlags::Command;
        }
        if shortcut.mods.option {
            mask |= NSEventModifierFlags::Option;
        }
        entry.setKeyEquivalentModifierMask(mask);
    }
    set_image(&entry, symbol(item.symbol()).as_deref());
    entry
}

/// The Open With submenu's `entry`: an app (its icon, the path as the
/// represented object) sends `app_action`, App Store… `store_action`,
/// Other… `other_action`.
pub(super) fn open_with_item(
    mtm: MainThreadMarker,
    entry: &OpenWithEntry,
    target: &AnyObject,
    [app_action, store_action, other_action]: [Sel; 3],
) -> Retained<NSMenuItem> {
    match entry {
        OpenWithEntry::App { title, path, .. } => {
            let item = menu_item(mtm, title, app_action, target);
            let path = NSString::from_str(path);
            let icon = NSWorkspace::sharedWorkspace().iconForFile(&path);
            icon.setSize(NSSize::new(APP_ICON_SIZE, APP_ICON_SIZE));
            set_image(&item, Some(&icon));
            // SAFETY: the represented object is an NSString, which the
            // action reads back as one.
            unsafe { item.setRepresentedObject(Some(&path)) };
            item
        }
        OpenWithEntry::Separator => NSMenuItem::separatorItem(mtm),
        OpenWithEntry::AppStore => menu_item(mtm, "App Store\u{2026}", store_action, target),
        OpenWithEntry::Other => menu_item(mtm, "Other\u{2026}", other_action, target),
    }
}

// ------------------------------------------------------------- Open With

/// The apps that can open every one of `paths`, and their shared default
/// app, for [`crate::context_menu::open_with_entries`]. Names are the ones
/// Finder shows, without ".app"; versions are read only for apps sharing a
/// name.
pub(super) fn open_with_apps(paths: &[String]) -> (Vec<AppChoice>, Option<String>) {
    let workspace = NSWorkspace::sharedWorkspace();
    let mut lists = Vec::with_capacity(paths.len());
    let mut defaults = Vec::with_capacity(paths.len());
    for path in paths {
        let url = file_url(path);
        let apps: Vec<String> = workspace
            .URLsForApplicationsToOpenURL(&url)
            .iter()
            .filter_map(|u| u.path().map(|p| p.to_string()))
            .collect();
        lists.push(apps);
        defaults.push(
            workspace
                .URLForApplicationToOpenURL(&url)
                .and_then(|u| u.path())
                .map(|p| p.to_string()),
        );
    }
    let files = NSFileManager::defaultManager();
    let mut apps: Vec<AppChoice> = common_apps(&lists)
        .into_iter()
        .map(|path| {
            let shown = files
                .displayNameAtPath(&NSString::from_str(&path))
                .to_string();
            let name = shown
                .strip_suffix(".app")
                .map_or(shown.clone(), str::to_owned);
            AppChoice {
                name,
                version: None,
                path,
            }
        })
        .collect();
    let names: Vec<String> = apps.iter().map(|a| a.name.to_lowercase()).collect();
    for (i, app) in apps.iter_mut().enumerate() {
        let shared = names
            .iter()
            .enumerate()
            .any(|(j, n)| j != i && *n == names[i]);
        if shared {
            app.version = app_version(&app.path);
        }
    }
    (apps, common_default(&defaults))
}

/// The app at `path`'s version (`CFBundleShortVersionString`).
fn app_version(path: &str) -> Option<String> {
    let bundle = NSBundle::bundleWithURL(&file_url(path))?;
    let value =
        bundle.objectForInfoDictionaryKey(&NSString::from_str("CFBundleShortVersionString"))?;
    Some(value.downcast::<NSString>().ok()?.to_string())
}

/// A nil completion handler, for the `NSWorkspace` methods that take an
/// optional block: no block crate is needed to pass none.
#[repr(transparent)]
struct NoCompletion(*const c_void);

// SAFETY: a block pointer has the encoding `@?`; this one is always null,
// which the methods it is passed to accept (their handler is nullable).
unsafe impl Encode for NoCompletion {
    const ENCODING: Encoding = Encoding::Block;
}

/// Opens the files at `paths` with the app at `app`, once (no "Always Open
/// With", which would change a system setting).
pub(super) fn open_with(paths: &[String], app: &NSURL) {
    open_with_prompting(paths, app, true);
}

/// [`open_with`], letting macOS ask the user (a first launch of a
/// downloaded app, say) only when `prompt` is set.
fn open_with_prompting(paths: &[String], app: &NSURL, prompt: bool) {
    if paths.is_empty() {
        return;
    }
    let workspace = NSWorkspace::sharedWorkspace();
    let urls = file_urls(paths);
    let configuration = NSWorkspaceOpenConfiguration::configuration();
    configuration.setPromptsUserIfNeeded(prompt);
    // SAFETY: `openURLs:withApplicationAtURL:configuration:completionHandler:`
    // takes an array of file URLs, an app URL, a configuration and a
    // nullable block, here nil; it returns void.
    let _: () = unsafe {
        msg_send![
            &*workspace,
            openURLs: &*urls,
            withApplicationAtURL: app,
            configuration: &*configuration,
            completionHandler: NoCompletion(std::ptr::null())
        ]
    };
}

/// The installed app with `bundle_id`, if any.
fn app_with_id(bundle_id: &str) -> Option<Retained<NSURL>> {
    NSWorkspace::sharedWorkspace()
        .URLForApplicationWithBundleIdentifier(&NSString::from_str(bundle_id))
}

/// App Store…: opens the Mac App Store. macOS has no public way to open
/// it on Finder's search for apps that open a file type.
pub(super) fn open_app_store() {
    if let Some(store) = app_with_id(APP_STORE) {
        NSWorkspace::sharedWorkspace().openURL(&store);
    }
}

/// Other…: a panel in /Applications offering apps only; the chosen app
/// opens the files at `paths`, once.
pub(super) fn choose_other_app(mtm: MainThreadMarker, paths: &[String]) {
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseFiles(true);
    panel.setCanChooseDirectories(false);
    panel.setAllowsMultipleSelection(false);
    panel.setTreatsFilePackagesAsDirectories(false);
    panel.setDirectoryURL(Some(&NSURL::fileURLWithPath(&NSString::from_str(
        "/Applications",
    ))));
    panel.setPrompt(Some(&NSString::from_str("Open")));
    let what = match paths {
        [one] => format!(
            "Choose an application to open the document \u{201c}{}\u{201d}.",
            one.rsplit('/').next().unwrap_or(one)
        ),
        _ => format!(
            "Choose an application to open the {} documents.",
            paths.len()
        ),
    };
    panel.setMessage(Some(&NSString::from_str(&what)));
    allow_only_apps(&panel);
    if panel.runModal() != NSModalResponseOK {
        return;
    }
    if let Some(app) = panel.URL() {
        open_with(paths, &app);
    }
}

/// Lets `panel` choose app bundles only (`setAllowedContentTypes:` with
/// `UTType.applicationBundle`). `UTType` comes from UniformTypeIdentifiers,
/// which AppKit loads, so it is found by name and no binding crate is
/// needed; without it the panel offers every file.
fn allow_only_apps(panel: &NSOpenPanel) {
    let Some(class) = AnyClass::get(c"UTType") else {
        return;
    };
    let id = NSString::from_str(APPLICATION_BUNDLE_TYPE);
    // SAFETY: `+[UTType typeWithIdentifier:]` takes an identifier string
    // and returns a type or nil.
    let kind: Option<Retained<AnyObject>> = unsafe { msg_send![class, typeWithIdentifier: &*id] };
    let Some(kind) = kind else { return };
    let kinds = NSArray::from_retained_slice(&[kind]);
    // SAFETY: `setAllowedContentTypes:` takes an array of UTType, as built.
    let _: () = unsafe { msg_send![panel, setAllowedContentTypes: &*kinds] };
}

// ---------------------------------------------------- Share and Terminal

/// Share…: the system's share picker for the files at `paths`, shown from
/// `rect` in `view`.
pub(super) fn share(paths: &[String], view: &NSView, rect: NSRect) {
    if paths.is_empty() {
        return;
    }
    let urls: Vec<Retained<NSURL>> = paths.iter().map(|p| file_url(p)).collect();
    let items: Vec<&AnyObject> = urls.iter().map(|u| u.as_ref()).collect();
    let items = NSArray::from_slice(&items);
    // SAFETY: the items are NSURLs, which the picker shares as files.
    let picker =
        unsafe { NSSharingServicePicker::initWithItems(NSSharingServicePicker::alloc(), &items) };
    picker.showRelativeToRect_ofView_preferredEdge(rect, view, NSRectEdge::MaxY);
    PICKER.with(|p| *p.borrow_mut() = Some(picker));
}

/// New Terminal at Folder: a Terminal window at each folder in `paths`.
pub(super) fn new_terminal(paths: &[String]) {
    if let Some(terminal) = app_with_id(TERMINAL) {
        open_with(paths, &terminal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder in the temporary folder for one test.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ne-context-menu-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch folder");
        dir
    }

    fn path(p: &std::path::Path) -> String {
        p.to_str().expect("a UTF-8 path").to_owned()
    }

    /// Copy puts the files on the pasteboard as file URLs, which Finder
    /// pastes as copies. Checked on a uniquely named pasteboard, never the
    /// user's.
    #[test]
    fn copy_writes_file_urls() {
        let dir = scratch("copy");
        let file = dir.join("report one.txt");
        std::fs::write(&file, b"x").expect("a file");
        let folder = dir.join("Folder");
        std::fs::create_dir(&folder).expect("a folder");
        let paths = vec![path(&file), path(&folder)];

        let pasteboard = NSPasteboard::pasteboardWithUniqueName();
        assert!(write_files(&pasteboard, &paths));
        assert_eq!(read_files(&pasteboard), paths);
        let types: Vec<String> = pasteboard
            .types()
            .map(|t| t.iter().map(|s| s.to_string()).collect())
            .unwrap_or_default();
        assert!(
            types.iter().any(|t| t == "public.file-url"),
            "types {types:?}"
        );
        // A second copy replaces the first.
        assert!(write_files(&pasteboard, &paths[..1]));
        assert_eq!(read_files(&pasteboard), paths[..1]);
        // SAFETY: `releaseGlobally` takes nothing and returns void; the
        // pasteboard is this test's own.
        let _: () = unsafe { msg_send![&*pasteboard, releaseGlobally] };
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Open With and New Terminal at Folder go by what the targets are on
    /// disk: a package (here an `.rtfd`) is a file, an `.app` an app.
    #[test]
    fn kinds_come_from_the_disk() {
        let dir = scratch("kinds");
        let file = dir.join("notes.txt");
        std::fs::write(&file, b"x").expect("a file");
        let folder = dir.join("Projects");
        let package = dir.join("Letter.rtfd");
        let app = dir.join("Tool.app");
        for d in [&folder, &package, &app] {
            std::fs::create_dir(d).expect("a folder");
        }
        let workspace = NSWorkspace::sharedWorkspace();
        assert_eq!(target_kind(&workspace, &path(&file)), TargetKind::File);
        assert_eq!(target_kind(&workspace, &path(&folder)), TargetKind::Folder);
        assert_eq!(
            target_kind(&workspace, &path(&package)),
            TargetKind::Package
        );
        assert_eq!(
            target_kind(&workspace, &path(&app)),
            TargetKind::Application
        );
        assert_eq!(
            target_kind(&workspace, &path(&dir.join("gone"))),
            TargetKind::File
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// New Terminal at Folder and App Store… find their apps by bundle
    /// identifier.
    #[test]
    fn terminal_and_the_app_store_are_found() {
        assert!(app_with_id(TERMINAL).is_some(), "{TERMINAL}");
        assert!(app_with_id(APP_STORE).is_some(), "{APP_STORE}");
    }

    /// Open With passes a nil completion block: the call goes through
    /// (debug builds check every argument's type), and an app that isn't
    /// there opens nothing.
    #[test]
    fn open_with_a_missing_app_opens_nothing() {
        let dir = scratch("open-with");
        let file = dir.join("notes.txt");
        std::fs::write(&file, b"x").expect("a file");
        let missing = file_url(&path(&dir.join("No Such App.app")));
        open_with_prompting(&[path(&file)], &missing, false);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
