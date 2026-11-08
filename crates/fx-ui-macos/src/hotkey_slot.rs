//! The registered summon hotkey, and how a shortcut picked in Settings
//! replaces it without a relaunch.
//!
//! A [`HotkeySlot`] holds the one registered shortcut. Replacing it
//! registers the new shortcut first and only then unregisters the old one,
//! so when macOS refuses the new one the old one keeps working.
//! [`apply`] adds the Settings rules ([`HotkeySpec::check`]) in front, so a
//! refused shortcut never reaches macOS, and reads the shortcut already
//! registered as no change.
//!
//! The registering itself is a [`Registrar`]: the AppKit layer's is the
//! `global-hotkey` manager (`appkit::summon`); tests use a fake. Nothing
//! here depends on AppKit.

#![forbid(unsafe_code)]

use crate::hotkey::{HotkeySpec, Refusal};

/// Registers and unregisters global hotkeys.
pub trait Registrar {
    /// What unregistering needs back.
    type Handle;

    /// Registers `spec` as a global hotkey.
    ///
    /// # Errors
    ///
    /// A readable message when macOS refuses it.
    fn register(&mut self, spec: &HotkeySpec) -> Result<Self::Handle, String>;

    /// Unregisters a hotkey [`Registrar::register`] registered.
    fn unregister(&mut self, handle: Self::Handle);
}

/// The registered summon hotkey, if any.
pub struct HotkeySlot<R: Registrar> {
    registrar: R,
    active: Option<(HotkeySpec, R::Handle)>,
}

impl<R: Registrar> HotkeySlot<R> {
    /// A slot with nothing registered.
    pub fn new(registrar: R) -> HotkeySlot<R> {
        HotkeySlot {
            registrar,
            active: None,
        }
    }

    /// The registered shortcut.
    pub fn active(&self) -> Option<HotkeySpec> {
        self.active.as_ref().map(|(spec, _)| *spec)
    }

    /// The registered shortcut's handle.
    pub fn handle(&self) -> Option<&R::Handle> {
        self.active.as_ref().map(|(_, handle)| handle)
    }

    /// The registrar.
    pub fn registrar(&self) -> &R {
        &self.registrar
    }

    /// The registrar, mutably.
    pub fn registrar_mut(&mut self) -> &mut R {
        &mut self.registrar
    }

    /// Registers `spec` in place of the registered shortcut: the new one
    /// first, then the old one goes. The registered shortcut again does
    /// nothing.
    ///
    /// # Errors
    ///
    /// macOS's refusal; the old shortcut stays registered.
    pub fn replace(&mut self, spec: HotkeySpec) -> Result<(), String> {
        if self.active() == Some(spec) {
            return Ok(());
        }
        // Register before unregistering: a refusal must leave the old
        // shortcut working.
        let handle = self.registrar.register(&spec)?;
        if let Some((_, old)) = self.active.replace((spec, handle)) {
            self.registrar.unregister(old);
        }
        Ok(())
    }
}

impl<R: Registrar> Drop for HotkeySlot<R> {
    /// Unregisters the shortcut: it works only while the slot lives.
    fn drop(&mut self) {
        if let Some((_, handle)) = self.active.take() {
            self.registrar.unregister(handle);
        }
    }
}

/// What [`apply`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applied {
    /// The new shortcut is registered and the old one is gone: save it.
    Changed,
    /// It was already the registered shortcut.
    Unchanged,
}

/// Applies a shortcut picked in Settings.
///
/// # Errors
///
/// The [`Refusal`]: the rules' ([`HotkeySpec::check`], before macOS is
/// asked) or macOS's ([`Refusal::NotAccepted`]). Either way the registered
/// shortcut stays.
pub fn apply<R: Registrar>(slot: &mut HotkeySlot<R>, spec: HotkeySpec) -> Result<Applied, Refusal> {
    // The registered shortcut is kept even if the rules would refuse it
    // now: a file edited by hand may hold one, and it works.
    if slot.active() == Some(spec) {
        return Ok(Applied::Unchanged);
    }
    spec.check()?;
    slot.replace(spec).map_err(|_| Refusal::NotAccepted(spec))?;
    Ok(Applied::Changed)
}
