//! The Settings row's shortcut recorder: what each click, key press and
//! focus change does while the user picks a new summon shortcut.
//!
//! A [`Recorder`] starts on a click on the key caps (or Space or Return
//! with the keyboard focus on them) and shows the modifiers held so far.
//! The first key pressed with a modifier ends it with that shortcut
//! ([`Output::Picked`]); applying it is the caller's job, which checks the
//! rules ([`HotkeySpec::check`]) and asks macOS, so the old shortcut keeps
//! working until the new one is registered. Escape, or the focus leaving
//! the caps, cancels. A key with no modifier beeps and keeps recording, so
//! a stray letter loses nothing.
//!
//! The current shortcut never reaches the window while it is registered:
//! macOS hands it to the app as a hotkey press, which the caller passes in
//! as [`Input::SummonPressed`], and recording ends with no change.

#![forbid(unsafe_code)]

use crate::hotkey::{HotkeySpec, KVK_ESCAPE, Key, Modifiers, Refusal};

/// What happened to the recorder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// A click on the caps, or Space or Return with the focus on them.
    Start,
    /// The modifier keys held changed (`flagsChanged:`).
    Modifiers(Modifiers),
    /// A key went down (`keyDown:`, or `performKeyEquivalent:` for a
    /// combination the menus would otherwise take).
    Key {
        /// The macOS virtual key code (`NSEvent.keyCode`).
        code: u32,
        /// The modifiers held with it.
        modifiers: Modifiers,
        /// Whether it is an auto-repeat of a key held down.
        repeat: bool,
    },
    /// The current summon shortcut was pressed.
    SummonPressed,
    /// The focus left the caps, the window closed or lost key status, or
    /// another control was used.
    Cancel,
}

/// What the caller does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    /// Nothing changes.
    Nothing,
    /// Recording started: show the recording box.
    Started,
    /// Show these held modifiers in the recording box.
    Held(Modifiers),
    /// Beep (`NSBeep`); recording goes on.
    Beep,
    /// Recording ended with no change.
    Cancelled,
    /// Recording ended because the current shortcut was pressed.
    Unchanged,
    /// Recording ended with this shortcut; apply it.
    Picked(HotkeySpec),
    /// Recording ended with a key no shortcut can use.
    Refused(Refusal),
}

/// The recorder's state: whether it records, and the modifiers held.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Recorder {
    recording: bool,
    held: Modifiers,
}

impl Recorder {
    /// Whether the recorder is recording.
    pub fn is_recording(&self) -> bool {
        self.recording
    }

    /// The modifiers held so far while recording.
    pub fn held(&self) -> Modifiers {
        self.held
    }

    /// Handles `input`; see [`Output`] for what to do next.
    pub fn handle(&mut self, input: Input) -> Output {
        if !self.recording {
            if input == Input::Start {
                self.recording = true;
                self.held = Modifiers::default();
                return Output::Started;
            }
            return Output::Nothing;
        }
        match input {
            Input::Start => Output::Nothing,
            Input::Modifiers(m) => {
                self.held = m;
                Output::Held(m)
            }
            Input::Key { repeat: true, .. } => Output::Nothing,
            Input::Key { code, .. } if code == KVK_ESCAPE => self.stop(Output::Cancelled),
            Input::Key { modifiers, .. } if modifiers.is_empty() => Output::Beep,
            Input::Key {
                code, modifiers, ..
            } => match Key::from_carbon_code(code) {
                Some(key) => self.stop(Output::Picked(HotkeySpec { modifiers, key })),
                None => self.stop(Output::Refused(Refusal::UnsupportedKey)),
            },
            Input::SummonPressed => self.stop(Output::Unchanged),
            Input::Cancel => self.stop(Output::Cancelled),
        }
    }

    fn stop(&mut self, output: Output) -> Output {
        *self = Recorder::default();
        output
    }
}
