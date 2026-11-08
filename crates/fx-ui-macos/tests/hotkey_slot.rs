//! Tests for `fx_ui_macos::hotkey_slot`: the registered summon hotkey and
//! how a shortcut picked in Settings replaces it (run 2.10).
//!
//! Contract exercised here:
//!
//! - `Registrar`: registers a `HotkeySpec` (giving a handle, or the reason
//!   macOS refused it) and unregisters a handle. The AppKit layer's is
//!   `global-hotkey`'s manager; these tests use a fake that records every
//!   call.
//! - `HotkeySlot::new(registrar)` holds no shortcut; `active()` is the
//!   registered one.
//! - `HotkeySlot::replace(spec)` registers `spec` first and only then
//!   unregisters the old one, so a refusal leaves the old one working.
//!   Replacing with the active shortcut does nothing.
//! - `apply(&mut slot, spec)`: the active shortcut again is `Unchanged`; one
//!   the rules refuse (`HotkeySpec::check`) is refused without asking
//!   macOS; one macOS refuses is `Refusal::NotAccepted`; else `Changed`.

use fx_ui_macos::hotkey::{HotkeySpec, Refusal};
use fx_ui_macos::hotkey_slot::{Applied, HotkeySlot, Registrar, apply};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    Register(String),
    Unregister(String),
}

/// A registrar that refuses the shortcuts in `refuse` and records calls.
#[derive(Default)]
struct Fake {
    calls: Vec<Call>,
    refuse: Vec<HotkeySpec>,
}

impl Registrar for Fake {
    type Handle = HotkeySpec;

    fn register(&mut self, spec: &HotkeySpec) -> Result<HotkeySpec, String> {
        self.calls.push(Call::Register(spec.to_string()));
        if self.refuse.contains(spec) {
            Err(format!("cannot register {spec}"))
        } else {
            Ok(*spec)
        }
    }

    fn unregister(&mut self, handle: HotkeySpec) {
        self.calls.push(Call::Unregister(handle.to_string()));
    }
}

fn hk(s: &str) -> HotkeySpec {
    HotkeySpec::parse(s).expect("a valid hotkey")
}

fn reg(s: &str) -> Call {
    Call::Register(s.to_string())
}

fn unreg(s: &str) -> Call {
    Call::Unregister(s.to_string())
}

fn slot_with(active: &str, refuse: &[&str]) -> HotkeySlot<Fake> {
    let mut slot = HotkeySlot::new(Fake {
        calls: Vec::new(),
        refuse: refuse.iter().map(|s| hk(s)).collect(),
    });
    slot.replace(hk(active))
        .expect("the first shortcut registers");
    slot.registrar_mut().calls.clear();
    slot
}

#[test]
fn a_new_slot_holds_nothing() {
    let slot = HotkeySlot::new(Fake::default());
    assert_eq!(slot.active(), None);
    assert!(slot.registrar().calls.is_empty());
}

#[test]
fn the_first_shortcut_registers() {
    let mut slot = HotkeySlot::new(Fake::default());
    assert_eq!(slot.replace(hk("option+space")), Ok(()));
    assert_eq!(slot.active(), Some(hk("option+space")));
    assert_eq!(slot.registrar().calls, [reg("option+space")]);
}

#[test]
fn a_refused_first_shortcut_leaves_none() {
    let mut slot = HotkeySlot::new(Fake {
        calls: Vec::new(),
        refuse: vec![hk("option+space")],
    });
    assert!(slot.replace(hk("option+space")).is_err());
    assert_eq!(slot.active(), None);
}

#[test]
fn the_new_shortcut_registers_before_the_old_one_goes() {
    let mut slot = slot_with("option+space", &[]);
    assert_eq!(slot.replace(hk("control+option+k")), Ok(()));
    assert_eq!(slot.active(), Some(hk("control+option+k")));
    assert_eq!(
        slot.registrar().calls,
        [reg("option+control+k"), unreg("option+space")]
    );
}

#[test]
fn a_refused_shortcut_leaves_the_old_one_registered() {
    let mut slot = slot_with("option+space", &["control+option+k"]);
    let err = slot.replace(hk("control+option+k")).unwrap_err();
    assert!(err.contains("option+control+k"), "{err}");
    assert_eq!(slot.active(), Some(hk("option+space")));
    assert_eq!(slot.registrar().calls, [reg("option+control+k")]);
}

#[test]
fn replacing_with_the_active_shortcut_does_nothing() {
    let mut slot = slot_with("option+space", &[]);
    assert_eq!(slot.replace(hk("option+space")), Ok(()));
    assert!(slot.registrar().calls.is_empty());
}

#[test]
fn applying_a_new_shortcut_changes_it() {
    let mut slot = slot_with("option+space", &[]);
    assert_eq!(
        apply(&mut slot, hk("control+option+k")),
        Ok(Applied::Changed)
    );
    assert_eq!(slot.active(), Some(hk("control+option+k")));
}

#[test]
fn applying_the_active_shortcut_is_unchanged() {
    let mut slot = slot_with("option+space", &[]);
    assert_eq!(apply(&mut slot, hk("option+space")), Ok(Applied::Unchanged));
    assert!(slot.registrar().calls.is_empty());
}

#[test]
fn the_active_shortcut_is_unchanged_even_if_the_rules_refuse_it() {
    // A settings file edited by hand can hold ⌘K; picking it again keeps
    // it rather than refusing the shortcut that already works.
    let mut slot = slot_with("cmd+k", &[]);
    assert_eq!(apply(&mut slot, hk("cmd+k")), Ok(Applied::Unchanged));
}

#[test]
fn the_rules_refuse_before_macos_is_asked() {
    let mut slot = slot_with("option+space", &[]);
    let quit = hk("cmd+q");
    assert_eq!(apply(&mut slot, quit), Err(Refusal::UsedByApps(quit)));
    assert_eq!(slot.active(), Some(hk("option+space")));
    assert!(slot.registrar().calls.is_empty());
}

#[test]
fn a_shortcut_macos_refuses_keeps_the_old_one() {
    let mut slot = slot_with("option+space", &["control+option+k"]);
    let new = hk("control+option+k");
    assert_eq!(apply(&mut slot, new), Err(Refusal::NotAccepted(new)));
    assert_eq!(slot.active(), Some(hk("option+space")));
}

#[test]
fn after_a_failed_launch_the_saved_shortcut_can_be_tried_again() {
    // Nothing registered at launch: picking the saved shortcut asks macOS
    // again instead of reading as unchanged.
    let mut slot = HotkeySlot::new(Fake::default());
    assert_eq!(apply(&mut slot, hk("option+space")), Ok(Applied::Changed));
    assert_eq!(slot.active(), Some(hk("option+space")));
}
