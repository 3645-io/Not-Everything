//! The resident menu-bar side of the product window: the status
//! item and its menu, the global summon hotkey, Launch at Login and the
//! Full Disk Access onboarding.
//!
//! All of it runs on the main thread from [`WinController`] methods. The
//! menu is rendered from [`crate::menu::build_menu`]; each action item's tag
//! is the index of its [`MenuAction`] in [`MenuAction::ALL`], and its first
//! line (the build's name and version) is a disabled item. The onboarding
//! state machine ([`crate::onboarding`]) decides what to show; this file
//! applies its output. A grant after a limited launch opens the full index
//! and switches the window to it ([`FullIndexSwitch`]). Nothing
//! here deletes, renames or moves a file.

use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use fx_fs::AccessState;
use fx_fs_macos::fda::FdaProbe;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSControlStateValueOff, NSControlStateValueOn, NSEventModifierFlags, NSImage,
    NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{NSString, NSTimer};

use super::classes::WinController;
use super::settings_window::SettingsUi;
use crate::APP_NAME;
use crate::actions::{HideMode, hide_mode};
use crate::appkit::login_item::{self, LoginItemStatus};
use crate::appkit::summon::{GlobalRegistrar, SummonHotkey, poll_pressed};
use crate::build_info::BuildInfo;
use crate::hotkey::{HotkeySpec, Key, Refusal};
use crate::hotkey_slot::HotkeySlot;
use crate::menu::{MenuAction, MenuItem, MenuState, build_menu};
use crate::onboarding::{IndexScope, Onboarding, OnboardingEvent, OnboardingOutput, ScopeNote};
use crate::settings::Settings;
use crate::shortcut_recorder::{Input, Recorder};
use crate::source::ResultsSource;
use crate::switchable::SwitchableSource;

/// How often the summon hotkey's event receiver is polled.
const HOTKEY_POLL: Duration = Duration::from_millis(100);

/// What the app needs to run resident, prepared by `app::launch`.
pub struct ResidentConfig {
    /// The loaded settings.
    pub settings: Settings,
    /// Where the settings are saved; `None` when `HOME` is unset.
    pub settings_path: Option<PathBuf>,
    /// The onboarding state machine after its `Launch` event (the scan's
    /// exclusions were chosen from it).
    pub onboarding: Onboarding,
    /// The output of that `Launch` event.
    pub launch_output: OnboardingOutput,
    /// Whether to register the global hotkey.
    pub hotkey: bool,
    /// How to index the protected folders once access is granted; `Some`
    /// when the launch built a limited index.
    pub full_index: Option<FullIndexSwitch>,
}

/// Opens the full-access index: an engine over the root with no exclusions
/// and the full-scope store. Runs on the switch thread.
pub type OpenFullIndex = Box<dyn FnOnce() -> Result<Arc<dyn ResultsSource>, String> + Send>;

/// How a limited launch moves to the full index without a relaunch:
/// when onboarding reports the grant, `open` runs on a background thread and
/// `source`, the window's results source, switches to the new index once it
/// has its first snapshot; the limited engine then shuts down.
pub struct FullIndexSwitch {
    /// The window's results source.
    pub source: Arc<SwitchableSource>,
    /// Opens the full index.
    pub open: OpenFullIndex,
}

/// The resident state the window controller owns.
pub(crate) struct ResidentState {
    pub(super) settings: Settings,
    settings_path: Option<PathBuf>,
    onboarding: Onboarding,
    launch_output: OnboardingOutput,
    want_hotkey: bool,
    probe: FdaProbe,
    /// The registered summon hotkey; `None` when the app doesn't register
    /// one or `global-hotkey`'s manager could not start.
    pub(super) hotkey: Option<SummonHotkey>,
    /// Why the summon hotkey isn't the one last picked, or isn't working:
    /// a refused change, or a failed registration at launch. The Settings
    /// row shows it until a change succeeds.
    pub(super) hotkey_error: Option<Refusal>,
    /// The Settings row's shortcut recorder.
    pub(super) recorder: Recorder,
    status_item: Option<Retained<NSStatusItem>>,
    pub(super) login: LoginItemStatus,
    /// Why the last change of Launch at Login was refused, until the next
    /// change or a new status; the Settings switch shows it.
    pub(super) login_error: Option<String>,
    recheck_scheduled: bool,
    /// Whether the launch built a limited (non-FDA) index. Set once at
    /// launch; with `switchable` it gives the scope of the index the window
    /// searches ([`ResidentState::index_scope`]), shown by
    /// [`ResidentState::scope_note`] separately from `note`.
    scan_limited: bool,
    /// The window's results source after a limited launch, which switches
    /// to the full index.
    switchable: Option<Arc<SwitchableSource>>,
    /// Opens the full index; taken when the switch starts.
    open_full: Option<OpenFullIndex>,
    /// The switch thread, until its result has been looked at.
    switch_thread: Option<JoinHandle<Result<(), String>>>,
    /// A note for the status bar's middle slot (Launch at Login needs
    /// approval), until replaced. Never carries the scope status.
    pub(super) note: Option<String>,
    /// An error for the status bar's index slot (Launch at Login failed,
    /// the protected folders can't be indexed), until replaced. Kept apart
    /// from `note`, since errors replace "Up to date".
    pub(super) error: Option<String>,
    /// The Settings window, once it has been shown.
    pub(super) settings_ui: Option<SettingsUi>,
    /// Whether the controller observes `NSApp.effectiveAppearance`.
    pub(super) observing_appearance: bool,
    /// Whether the app was already active when the search window was last
    /// summoned (the user was in Not-Everything, not another app). Decides
    /// how hiding hands back the focus ([`crate::actions::hide_mode`]).
    active_at_summon: bool,
}

impl ResidentState {
    pub(crate) fn new(config: ResidentConfig) -> ResidentState {
        let (switchable, open_full) = match config.full_index {
            Some(full) => (Some(full.source), Some(full.open)),
            None => (None, None),
        };
        ResidentState {
            settings: config.settings,
            settings_path: config.settings_path,
            onboarding: config.onboarding,
            launch_output: config.launch_output,
            want_hotkey: config.hotkey,
            probe: FdaProbe::new(),
            hotkey: None,
            hotkey_error: None,
            recorder: Recorder::default(),
            status_item: None,
            login: LoginItemStatus::Unavailable,
            login_error: None,
            recheck_scheduled: false,
            scan_limited: config.launch_output.limited_mode,
            switchable,
            open_full,
            switch_thread: None,
            note: None,
            error: None,
            settings_ui: None,
            observing_appearance: false,
            active_at_summon: false,
        }
    }

    /// The scope of the index the window searches: limited until the switch
    /// to the full index starts, upgrading while it opens and scans, full
    /// once the window searches it (or when the launch had access).
    pub(crate) fn index_scope(&self) -> IndexScope {
        if !self.scan_limited {
            return IndexScope::Full;
        }
        match &self.switchable {
            // Ask about a running switch first: one that ends between the
            // two questions then reads as done, never as not started.
            Some(s) if s.is_switching() => IndexScope::Upgrading,
            Some(s) if s.epoch() > 0 => IndexScope::Full,
            _ => IndexScope::Limited,
        }
    }

    /// Looks at a finished switch thread: a switch that failed is logged and
    /// noted in the status bar (the limited index stays).
    pub(crate) fn poll_full_index(&mut self) {
        if !self.switch_thread.as_ref().is_some_and(|t| t.is_finished()) {
            return;
        }
        let Some(thread) = self.switch_thread.take() else {
            return;
        };
        let failed = match thread.join() {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(e),
            Err(_) => Some("the switch thread panicked".to_string()),
        };
        if let Some(e) = failed {
            eprintln!("{APP_NAME}: cannot index the protected folders: {e}");
            self.error = Some(format!("Cannot index protected folders: {e}"));
        }
    }

    /// What the status bar notes about the scope of the index the window
    /// searches ([`ScopeNote::of`]).
    pub(crate) fn scope_note(&self) -> Option<ScopeNote> {
        ScopeNote::of(self.index_scope(), self.onboarding.limited_mode())
    }

    /// The Full Disk Access state as last probed.
    pub(super) fn access(&self) -> AccessState {
        self.onboarding.access()
    }

    pub(super) fn save_settings(&self) {
        if let Some(path) = &self.settings_path {
            if let Err(e) = self.settings.save(path) {
                eprintln!(
                    "{APP_NAME}: cannot save settings to {}: {e}",
                    path.display()
                );
            }
        }
    }
}

impl WinController {
    /// Starts the resident parts: the saved appearance and Dock icon, login
    /// item status, status item, hotkey and the first onboarding output.
    pub(crate) fn start_resident(&self) {
        let mtm = MainThreadMarker::from(self);
        let Some(want_hotkey) = self.with_resident(|r| {
            r.login = login_item::status();
            r.want_hotkey
        }) else {
            return;
        };
        self.start_look();

        if want_hotkey {
            self.start_hotkey();
        }

        let status_bar = NSStatusBar::systemStatusBar();
        let item = status_bar.statusItemWithLength(NSVariableStatusItemLength);
        if let Some(button) = item.button(mtm) {
            let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                &NSString::from_str("magnifyingglass"),
                Some(&NSString::from_str(&format!("{APP_NAME} search"))),
            );
            match image {
                Some(image) => {
                    image.setTemplate(true);
                    button.setImage(Some(&image));
                }
                None => button.setTitle(&NSString::from_str("fx")),
            }
        }
        self.with_resident(|r| r.status_item = Some(item));
        self.rebuild_menu();

        if let Some(out) = self.with_resident(|r| r.launch_output) {
            self.apply_onboarding(out);
        }
    }

    /// Registers the saved summon hotkey and starts polling for it. A
    /// shortcut macOS refuses stays saved and is shown in Settings with the
    /// reason; the poll runs either way, so a shortcut picked in Settings
    /// later works without a relaunch.
    fn start_hotkey(&self) {
        let Some(spec) = self.with_resident(|r| r.settings.hotkey) else {
            return;
        };
        let registrar = match GlobalRegistrar::new() {
            Ok(registrar) => registrar,
            Err(e) => {
                eprintln!("{APP_NAME}: global hotkey not available: {e}");
                self.with_resident(|r| r.hotkey_error = Some(Refusal::NotAcceptedAtLaunch(spec)));
                return;
            }
        };
        let mut slot = HotkeySlot::new(registrar);
        // A file edited by hand is not held to the Settings rules: whatever
        // parses is registered, as before the recorder.
        let failed = slot.replace(spec).err();
        self.with_resident(|r| {
            r.hotkey = Some(slot);
            if let Some(e) = &failed {
                eprintln!("{APP_NAME}: global hotkey not available: {e}");
                r.hotkey_error = Some(Refusal::NotAcceptedAtLaunch(spec));
            }
        });
        let target: &AnyObject = self.as_ref();
        // SAFETY: `hotkeyTick:` is implemented as
        // `-(void)hotkeyTick:(NSTimer *)timer`; the run loop retains the
        // repeating timer, which retains its target.
        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                HOTKEY_POLL.as_secs_f64(),
                target,
                sel!(hotkeyTick:),
                None,
                true,
            )
        };
        timer.setTolerance(HOTKEY_POLL.as_secs_f64() / 2.0);
    }

    /// Runs `f` on the resident state, if the app runs resident.
    pub(crate) fn with_resident<T>(&self, f: impl FnOnce(&mut ResidentState) -> T) -> Option<T> {
        self.ivars().resident.borrow_mut().as_mut().map(f)
    }

    pub(crate) fn is_resident(&self) -> bool {
        self.ivars().resident.borrow().is_some()
    }

    /// Rebuilds the status item's menu from the current state.
    pub(crate) fn rebuild_menu(&self) {
        let mtm = MainThreadMarker::from(self);
        let Some((state, item)) = self.with_resident(|r| {
            let state = MenuState {
                build: BuildInfo::CURRENT,
                hotkey: r.settings.hotkey,
                launch_at_login: r.login.is_on(),
                access: r.onboarding.access(),
            };
            (state, r.status_item.clone())
        }) else {
            return;
        };
        let Some(item) = item else { return };
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);
        let target: &AnyObject = self.as_ref();
        for entry in build_menu(&state) {
            let entry = match entry {
                MenuItem::Info(title) => {
                    // SAFETY: no action, so the item never sends anything.
                    let info = unsafe {
                        NSMenuItem::initWithTitle_action_keyEquivalent(
                            NSMenuItem::alloc(mtm),
                            &NSString::from_str(&title),
                            None,
                            &NSString::from_str(""),
                        )
                    };
                    info.setEnabled(false);
                    menu.addItem(&info);
                    continue;
                }
                MenuItem::Separator => {
                    menu.addItem(&NSMenuItem::separatorItem(mtm));
                    continue;
                }
                MenuItem::Action(entry) => entry,
            };
            let (key, mask) = entry.hotkey.map_or((String::new(), None), key_equivalent);
            // SAFETY: `menuAction:` is implemented by WinController as
            // `-(void)menuAction:(NSMenuItem *)sender`.
            let ns_item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str(entry.title),
                    Some(sel!(menuAction:)),
                    &NSString::from_str(&key),
                )
            };
            if let Some(mask) = mask {
                ns_item.setKeyEquivalentModifierMask(mask);
            }
            // SAFETY: the controller lives as long as the app.
            unsafe { ns_item.setTarget(Some(target)) };
            let tag = MenuAction::ALL
                .into_iter()
                .position(|a| a == entry.action)
                .unwrap_or(usize::MAX);
            ns_item.setTag(isize::try_from(tag).unwrap_or(-1));
            ns_item.setState(if entry.checked {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
            if entry.action == MenuAction::ToggleLaunchAtLogin {
                let status = self.with_resident(|r| r.login);
                if status == Some(LoginItemStatus::Unavailable) {
                    ns_item.setEnabled(false);
                    ns_item.setToolTip(Some(&NSString::from_str(
                        "Available when run from the .app bundle",
                    )));
                }
            }
            menu.addItem(&ns_item);
        }
        item.setMenu(Some(&menu));
    }

    /// Dispatches the status menu item with `tag`.
    pub(crate) fn menu_action(&self, tag: isize) {
        let Some(action) = usize::try_from(tag)
            .ok()
            .and_then(|i| MenuAction::ALL.into_iter().nth(i))
        else {
            return;
        };
        match action {
            MenuAction::ShowSearchWindow => self.show_window(),
            MenuAction::ShowSettings => self.show_settings(),
            MenuAction::ToggleLaunchAtLogin => self.toggle_launch_at_login(),
            MenuAction::GrantFullDiskAccess => self.onboarding_event(OnboardingEvent::GrantClicked),
            MenuAction::Quit => {
                NSApplication::sharedApplication(MainThreadMarker::from(self)).terminate(None)
            }
        }
    }

    /// The summon hotkey: hide the window when it is key in the active app,
    /// show and focus it otherwise.
    pub(crate) fn toggle_window(&self) {
        let mtm = MainThreadMarker::from(self);
        let Some(ui) = self.ui() else { return };
        let app = NSApplication::sharedApplication(mtm);
        if app.isActive() && ui.window.isKeyWindow() {
            self.hide_window();
        } else {
            self.show_window();
        }
    }

    /// Hides the search window without quitting: the summon hotkey, and
    /// Escape with the search field empty (see
    /// [`crate::actions::escape_action`]). As [`hide_mode`] decides, it
    /// hides the app, so macOS activates the app the user was using before,
    /// or, when the user came from the Settings window, orders out only the
    /// search window and leaves Settings in front.
    pub(crate) fn hide_window(&self) {
        let Some(ui) = self.ui() else { return };
        let (active_at_summon, settings_visible) = self
            .with_resident(|r| {
                let settings = r.settings_ui.as_ref().is_some_and(|s| s.is_visible());
                (r.active_at_summon, settings)
            })
            .unwrap_or((false, false));
        match hide_mode(active_at_summon, settings_visible) {
            HideMode::HideApp => {
                NSApplication::sharedApplication(MainThreadMarker::from(self)).hide(None)
            }
            HideMode::WindowOnly => ui.window.orderOut(None),
        }
    }

    /// Shows the window, activates the app and focuses the search field.
    pub(crate) fn show_window(&self) {
        let mtm = MainThreadMarker::from(self);
        let Some(ui) = self.ui() else { return };
        let app = NSApplication::sharedApplication(mtm);
        let active = app.isActive();
        self.with_resident(|r| r.active_at_summon = active);
        activate(&app);
        ui.window.makeKeyAndOrderFront(None);
        // Making the search field first responder selects its text, so
        // typing replaces the last query.
        ui.window.makeFirstResponder(Some(&ui.search));
    }

    /// Whether the menu-bar status item is showing.
    pub(crate) fn has_status_item(&self) -> bool {
        self.with_resident(|r| r.status_item.is_some())
            .unwrap_or(false)
    }

    /// Removes the menu-bar status item (when the app quits). Removing it,
    /// rather than setting `visible` to false, leaves no hidden-item
    /// preference behind for the next launch.
    pub(crate) fn remove_status_item(&self) {
        if let Some(item) = self.with_resident(|r| r.status_item.take()).flatten() {
            NSStatusBar::systemStatusBar().removeStatusItem(&item);
        }
    }

    /// Orders out every window (search window, Settings) and commits that
    /// to the window server at once, so they are gone before anything slow
    /// runs on the main thread (when the app quits).
    pub(crate) fn hide_windows(&self) {
        let app = NSApplication::sharedApplication(MainThreadMarker::from(self));
        for window in app.windows().to_vec() {
            window.orderOut(None);
        }
        // Window changes are committed with the run loop's Core Animation
        // transaction, which the quit would not reach; flush it now.
        if let Some(transaction) = AnyClass::get(c"CATransaction") {
            // SAFETY: `+[CATransaction flush]` takes no arguments and
            // returns void; it is called on the main thread.
            let _: () = unsafe { msg_send![transaction, flush] };
        }
    }

    /// The summon hotkey's timer: a press toggles the search window, or,
    /// while Settings records a new shortcut, ends recording with no change
    /// (macOS hands the registered shortcut to the app, not the window).
    pub(crate) fn poll_hotkey(&self) {
        let Some((pressed, recording)) = self.with_resident(|r| {
            let pressed = r.hotkey.as_ref().is_some_and(poll_pressed);
            (pressed, r.recorder.is_recording())
        }) else {
            return;
        };
        if !pressed {
            return;
        }
        if recording {
            self.shortcut_input(Input::SummonPressed);
        } else {
            self.toggle_window();
        }
    }

    /// Turns Launch at Login over, for the menu-bar menu's item and the
    /// Settings switch, and shows the outcome in both.
    pub(super) fn toggle_launch_at_login(&self) {
        let Some(on) = self.with_resident(|r| !r.login.is_on()) else {
            return;
        };
        let result = login_item::set_enabled(on);
        self.with_resident(|r| {
            match result {
                Ok(status) => {
                    r.login = status;
                    r.login_error = None;
                    r.note = (status == LoginItemStatus::RequiresApproval)
                        .then(|| format!("Launch at Login {status}"));
                    r.error = None;
                }
                Err(e) => {
                    eprintln!("{APP_NAME}: cannot change Launch at Login: {e}");
                    r.login = login_item::status();
                    r.note = None;
                    r.error = Some(format!("Launch at Login: {e}"));
                    r.login_error = Some(e);
                }
            }
            r.settings.launch_at_login = r.login.is_on();
            r.save_settings();
        });
        self.rebuild_menu();
        self.update_status();
        self.refresh_settings();
    }

    /// Feeds `event` to the onboarding state machine and applies its output.
    pub(crate) fn onboarding_event(&self, event: OnboardingEvent) {
        let Some(out) = self.with_resident(|r| {
            let out = r.onboarding.handle(event, &r.probe);
            if event == OnboardingEvent::Dismissed {
                r.settings.fda_onboarding_dismissed = true;
                r.save_settings();
            }
            out
        }) else {
            return;
        };
        self.apply_onboarding(out);
    }

    fn apply_onboarding(&self, out: OnboardingOutput) {
        if out.rescan_excluded {
            self.start_full_index();
        }
        if let Some(callout) = self.ui().and_then(|ui| ui.callout.as_ref()) {
            if callout.is_shown() != out.show_banner {
                callout.set_shown(out.show_banner);
                self.layout_window();
            }
        }
        self.rebuild_menu();
        self.update_status();
        self.refresh_settings();

        let active = NSApplication::sharedApplication(MainThreadMarker::from(self)).isActive();
        let schedule = self
            .with_resident(|r| {
                let go = out.recheck_after.is_some() && active && !r.recheck_scheduled;
                if go {
                    r.recheck_scheduled = true;
                }
                go
            })
            .unwrap_or(false);
        if let (true, Some(after)) = (schedule, out.recheck_after) {
            let target: &AnyObject = self.as_ref();
            // SAFETY: `onboardingTick:` is implemented as
            // `-(void)onboardingTick:(NSTimer *)timer`; the run loop retains
            // the one-shot timer, which retains its target.
            let _timer = unsafe {
                NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                    after.as_secs_f64(),
                    target,
                    sel!(onboardingTick:),
                    None,
                    false,
                )
            };
        }
    }

    /// Access was granted after a limited launch: opens the full index on a
    /// background thread beside the limited one. Once it has its first
    /// snapshot the window's source switches to it and tells the window with
    /// a new generation, so the current search re-runs through the normal
    /// refresh path; the limited engine then shuts down. Meanwhile the
    /// status bar says the protected folders are being indexed.
    fn start_full_index(&self) {
        let Some(Some((source, open))) = self.with_resident(|r| {
            let source = Arc::clone(r.switchable.as_ref()?);
            Some((source, r.open_full.take()?))
        }) else {
            return;
        };
        match source.switch_when_ready(open) {
            Ok(thread) => {
                self.with_resident(|r| r.switch_thread = Some(thread));
            }
            Err(e) => {
                eprintln!("{APP_NAME}: cannot index the protected folders: {e}");
                self.with_resident(|r| {
                    r.error = Some(format!("Cannot index protected folders: {e}"));
                });
            }
        }
    }

    /// The one-shot recheck timer fired.
    pub(crate) fn onboarding_tick(&self) {
        self.with_resident(|r| r.recheck_scheduled = false);
        self.onboarding_event(OnboardingEvent::RecheckTick);
    }
}

/// Activates the app, first bringing back its windows if hiding the search
/// window hid the app ([`HideMode::HideApp`]).
pub(super) fn activate(app: &NSApplication) {
    if app.isHidden() {
        app.unhideWithoutActivation();
    }
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
}

/// The menu key equivalent (key string, modifier mask) showing `spec`.
fn key_equivalent(spec: HotkeySpec) -> (String, Option<NSEventModifierFlags>) {
    let key = match spec.key {
        Key::Space => " ".to_string(),
        k if (Key::F1 as u8..=Key::F12 as u8).contains(&(k as u8)) => {
            // NSF1FunctionKey is U+F704; F1..F12 are consecutive.
            let n = k as u32 - Key::F1 as u32;
            char::from_u32(0xF704 + n).map_or_else(String::new, String::from)
        }
        k => k.name().to_string(),
    };
    let m = spec.modifiers;
    let mut mask = NSEventModifierFlags::empty();
    for (on, flag) in [
        (m.cmd, NSEventModifierFlags::Command),
        (m.shift, NSEventModifierFlags::Shift),
        (m.option, NSEventModifierFlags::Option),
        (m.control, NSEventModifierFlags::Control),
    ] {
        if on {
            mask |= flag;
        }
    }
    (key, Some(mask))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_equivalents() {
        let (k, m) = key_equivalent(HotkeySpec::parse("option+space").unwrap());
        assert_eq!(k, " ");
        assert_eq!(m, Some(NSEventModifierFlags::Option));
        let (k, _) = key_equivalent(HotkeySpec::parse("cmd+f2").unwrap());
        assert_eq!(k, "\u{F705}");
        let (k, m) = key_equivalent(HotkeySpec::parse("cmd+shift+k").unwrap());
        assert_eq!(k, "k");
        assert_eq!(
            m,
            Some(NSEventModifierFlags::Command | NSEventModifierFlags::Shift)
        );
    }
}
