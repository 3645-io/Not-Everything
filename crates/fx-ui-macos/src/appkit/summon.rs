//! The global summon hotkey, registered through the `global-hotkey` crate
//! (Carbon `RegisterEventHotKey`, so no Accessibility prompt).
//!
//! [`SummonHotkey::register`] maps a [`HotkeySpec`] onto `global-hotkey`'s
//! key code and modifiers and registers it. The crate hands each event to a
//! handler the window controller installs, which wakes the main thread; the
//! controller then asks [`SummonHotkey::is_press`] whether it was this
//! hotkey going down. Until 0.3.1 a timer polled for events 10 times a
//! second, which kept waking the app. The manager must be created on the
//! main thread and live as long as the hotkey should; it unregisters when
//! dropped.

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

use crate::hotkey::{HotkeySpec, Key};

/// A registered summon hotkey.
pub struct SummonHotkey {
    manager: GlobalHotKeyManager,
    hotkey: HotKey,
}

impl SummonHotkey {
    /// Registers `spec` as a global hotkey.
    ///
    /// # Errors
    ///
    /// A readable message when the manager cannot start or the combination
    /// cannot be registered (for example because another app holds it).
    pub fn register(spec: &HotkeySpec) -> Result<SummonHotkey, String> {
        let manager = GlobalHotKeyManager::new().map_err(|e| e.to_string())?;
        let hotkey = to_global_hotkey(spec);
        manager
            .register(hotkey)
            .map_err(|e| format!("cannot register {spec}: {e}"))?;
        Ok(SummonHotkey { manager, hotkey })
    }

    /// Whether `event` is this hotkey going down.
    pub fn is_press(&self, event: &GlobalHotKeyEvent) -> bool {
        event.id() == self.hotkey.id() && event.state() == HotKeyState::Pressed
    }
}

impl Drop for SummonHotkey {
    fn drop(&mut self) {
        let _ = self.manager.unregister(self.hotkey);
    }
}

/// The `global-hotkey` form of `spec`.
pub fn to_global_hotkey(spec: &HotkeySpec) -> HotKey {
    let m = spec.modifiers;
    let mut mods = Modifiers::empty();
    if m.cmd {
        // keyboard-types 0.8 names the Command key META (SUPER is legacy).
        mods |= Modifiers::META;
    }
    if m.shift {
        mods |= Modifiers::SHIFT;
    }
    if m.option {
        mods |= Modifiers::ALT;
    }
    if m.control {
        mods |= Modifiers::CONTROL;
    }
    HotKey::new(Some(mods), key_code(spec.key))
}

/// The `global-hotkey` key code of `key` (physical key positions, like the
/// `kVK_*` codes of [`Key::carbon_code`]).
pub fn key_code(key: Key) -> Code {
    match key {
        Key::A => Code::KeyA,
        Key::B => Code::KeyB,
        Key::C => Code::KeyC,
        Key::D => Code::KeyD,
        Key::E => Code::KeyE,
        Key::F => Code::KeyF,
        Key::G => Code::KeyG,
        Key::H => Code::KeyH,
        Key::I => Code::KeyI,
        Key::J => Code::KeyJ,
        Key::K => Code::KeyK,
        Key::L => Code::KeyL,
        Key::M => Code::KeyM,
        Key::N => Code::KeyN,
        Key::O => Code::KeyO,
        Key::P => Code::KeyP,
        Key::Q => Code::KeyQ,
        Key::R => Code::KeyR,
        Key::S => Code::KeyS,
        Key::T => Code::KeyT,
        Key::U => Code::KeyU,
        Key::V => Code::KeyV,
        Key::W => Code::KeyW,
        Key::X => Code::KeyX,
        Key::Y => Code::KeyY,
        Key::Z => Code::KeyZ,
        Key::Digit0 => Code::Digit0,
        Key::Digit1 => Code::Digit1,
        Key::Digit2 => Code::Digit2,
        Key::Digit3 => Code::Digit3,
        Key::Digit4 => Code::Digit4,
        Key::Digit5 => Code::Digit5,
        Key::Digit6 => Code::Digit6,
        Key::Digit7 => Code::Digit7,
        Key::Digit8 => Code::Digit8,
        Key::Digit9 => Code::Digit9,
        Key::Space => Code::Space,
        Key::F1 => Code::F1,
        Key::F2 => Code::F2,
        Key::F3 => Code::F3,
        Key::F4 => Code::F4,
        Key::F5 => Code::F5,
        Key::F6 => Code::F6,
        Key::F7 => Code::F7,
        Key::F8 => Code::F8,
        Key::F9 => Code::F9,
        Key::F10 => Code::F10,
        Key::F11 => Code::F11,
        Key::F12 => Code::F12,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_maps_to_a_distinct_code() {
        let mut codes: Vec<String> = Key::all().map(|k| format!("{:?}", key_code(k))).collect();
        let total = codes.len();
        codes.sort();
        codes.dedup();
        assert_eq!(codes.len(), total);
    }

    #[test]
    fn default_hotkeys_map_to_global_hotkeys() {
        let hk = to_global_hotkey(&HotkeySpec::default_for(fx_channel::Channel::Release));
        assert_eq!(hk, HotKey::new(Some(Modifiers::ALT), Code::Space));
        let hk = to_global_hotkey(&HotkeySpec::default_for(fx_channel::Channel::Rc));
        assert_eq!(
            hk,
            HotKey::new(Some(Modifiers::ALT | Modifiers::SHIFT), Code::Space)
        );
        let cmd_shift_k = HotkeySpec::parse("cmd+shift+k").unwrap();
        assert_eq!(
            to_global_hotkey(&cmd_shift_k),
            HotKey::new(Some(Modifiers::META | Modifiers::SHIFT), Code::KeyK)
        );
    }
}
