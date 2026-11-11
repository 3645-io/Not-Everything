//! The global summon hotkey: a platform-agnostic spec, its string form and
//! its mapping to the macOS virtual key code and Carbon modifier mask.
//!
//! A [`HotkeySpec`] is one or more modifiers plus exactly one [`Key`]. Its
//! string form is `+`-separated, for example `"cmd+shift+space"` or
//! `"option+space"`. [`HotkeySpec::parse`] is case-insensitive and takes the
//! parts in any order; it rejects a spec with no modifier, an unknown key or
//! modifier, a duplicate modifier, no key or more than one key. The
//! [`Display`](fmt::Display) form is canonical: lower case, modifiers in the
//! order cmd, shift, option, control, then the key.
//!
//! Modifier names are `cmd`, `shift`, `option` and `control`; the aliases
//! `command`, `opt`, `alt` and `ctrl` are accepted when parsing. Keys are the
//! letters `a`–`z`, the digits `0`–`9`, `space` and `f1`–`f12`.
//!
//! # Default summon hotkey
//!
//! The default is **option+space** for the release app and
//! **shift+option+space** for the Preview app (release candidates and
//! betas), so both can run at once ([`HotkeySpec::default_for`]; this
//! build's is [`HotkeySpec::default`], spelled [`DEFAULT_SUMMON_HOTKEY`]).
//! Cmd+Space is Spotlight's and Ctrl+Space switches input sources, so
//! neither is taken by default; the user picks another combination in
//! Settings (or the settings file).
//!
//! # Recording in Settings
//!
//! The Settings recorder reads a key press as a spec:
//! [`Key::from_carbon_code`] maps `NSEvent.keyCode` back to a key and
//! [`Modifiers::from_event_flags`] reads `NSEvent.modifierFlags`
//! ([`EVENT_SHIFT`], [`EVENT_CONTROL`], [`EVENT_OPTION`],
//! [`EVENT_COMMAND`]). Before macOS is asked, [`HotkeySpec::check`] refuses
//! a shortcut that would break typing or every app's own commands, or that
//! macOS uses itself; [`Refusal`] says why, in the words the Settings row
//! shows. A file edited by hand is not checked: whatever parses is
//! registered.
//!
//! # Carbon mapping
//!
//! [`HotkeySpec::to_carbon`] returns plain integers, the virtual key code
//! (`kVK_*` from HIToolbox `Events.h`, ANSI layout positions) and the Carbon
//! modifier mask (`cmdKey` 0x100, `shiftKey` 0x200, `optionKey` 0x800,
//! `controlKey` 0x1000), so this module stays free of `global-hotkey` types
//! and builds on any OS. The AppKit layer maps them into `global-hotkey`.

#![forbid(unsafe_code)]

use std::fmt;

use fx_channel::Channel;

/// The documented default summon hotkey of this build's channel, in its
/// string form: `option+space`, or `shift+option+space` for a Preview build.
pub const DEFAULT_SUMMON_HOTKEY: &str = Channel::CURRENT.default_hotkey();

/// Carbon `cmdKey` modifier bit.
pub const CARBON_CMD_KEY: u32 = 1 << 8;
/// Carbon `shiftKey` modifier bit.
pub const CARBON_SHIFT_KEY: u32 = 1 << 9;
/// Carbon `optionKey` modifier bit.
pub const CARBON_OPTION_KEY: u32 = 1 << 11;
/// Carbon `controlKey` modifier bit.
pub const CARBON_CONTROL_KEY: u32 = 1 << 12;

/// `NSEventModifierFlagShift` in `NSEvent.modifierFlags`.
pub const EVENT_SHIFT: u64 = 1 << 17;
/// `NSEventModifierFlagControl`.
pub const EVENT_CONTROL: u64 = 1 << 18;
/// `NSEventModifierFlagOption`.
pub const EVENT_OPTION: u64 = 1 << 19;
/// `NSEventModifierFlagCommand`.
pub const EVENT_COMMAND: u64 = 1 << 20;

/// `kVK_Escape`: cancels recording in Settings.
pub const KVK_ESCAPE: u32 = 0x35;

/// The modifier keys of a hotkey.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    /// Command (⌘).
    pub cmd: bool,
    /// Shift (⇧).
    pub shift: bool,
    /// Option (⌥).
    pub option: bool,
    /// Control (⌃).
    pub control: bool,
}

impl Modifiers {
    /// Whether no modifier is set.
    pub const fn is_empty(&self) -> bool {
        !(self.cmd || self.shift || self.option || self.control)
    }

    /// The modifiers held in `NSEvent.modifierFlags`. Caps Lock, the
    /// numeric pad, Help, fn (which F1 to F12 report) and the left or right
    /// key bits are not modifiers of a shortcut, so they are ignored.
    pub const fn from_event_flags(flags: u64) -> Modifiers {
        Modifiers {
            cmd: flags & EVENT_COMMAND != 0,
            shift: flags & EVENT_SHIFT != 0,
            option: flags & EVENT_OPTION != 0,
            control: flags & EVENT_CONTROL != 0,
        }
    }

    /// Whether only `cmd`, `shift`, `option` and `control` (those given as
    /// `true`) are set.
    const fn exactly(&self, cmd: bool, shift: bool, option: bool, control: bool) -> bool {
        self.cmd == cmd && self.shift == shift && self.option == option && self.control == control
    }

    /// The Carbon modifier mask (`cmdKey`, `shiftKey`, `optionKey`,
    /// `controlKey`).
    pub const fn to_carbon(&self) -> u32 {
        let mut mask = 0;
        if self.cmd {
            mask |= CARBON_CMD_KEY;
        }
        if self.shift {
            mask |= CARBON_SHIFT_KEY;
        }
        if self.option {
            mask |= CARBON_OPTION_KEY;
        }
        if self.control {
            mask |= CARBON_CONTROL_KEY;
        }
        mask
    }

    /// The menu-style symbols in Apple's order (⌃⌥⇧⌘), for display.
    pub fn symbols(&self) -> String {
        let mut s = String::new();
        if self.control {
            s.push('⌃');
        }
        if self.option {
            s.push('⌥');
        }
        if self.shift {
            s.push('⇧');
        }
        if self.cmd {
            s.push('⌘');
        }
        s
    }
}

/// A non-modifier key a hotkey can use.
#[allow(missing_docs)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    /// The space bar.
    Space,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
}

/// Every key with its canonical name and macOS virtual key code (`kVK_*`).
const KEYS: [(Key, &str, u32); 49] = [
    (Key::A, "a", 0x00),
    (Key::B, "b", 0x0B),
    (Key::C, "c", 0x08),
    (Key::D, "d", 0x02),
    (Key::E, "e", 0x0E),
    (Key::F, "f", 0x03),
    (Key::G, "g", 0x05),
    (Key::H, "h", 0x04),
    (Key::I, "i", 0x22),
    (Key::J, "j", 0x26),
    (Key::K, "k", 0x28),
    (Key::L, "l", 0x25),
    (Key::M, "m", 0x2E),
    (Key::N, "n", 0x2D),
    (Key::O, "o", 0x1F),
    (Key::P, "p", 0x23),
    (Key::Q, "q", 0x0C),
    (Key::R, "r", 0x0F),
    (Key::S, "s", 0x01),
    (Key::T, "t", 0x11),
    (Key::U, "u", 0x20),
    (Key::V, "v", 0x09),
    (Key::W, "w", 0x0D),
    (Key::X, "x", 0x07),
    (Key::Y, "y", 0x10),
    (Key::Z, "z", 0x06),
    (Key::Digit0, "0", 0x1D),
    (Key::Digit1, "1", 0x12),
    (Key::Digit2, "2", 0x13),
    (Key::Digit3, "3", 0x14),
    (Key::Digit4, "4", 0x15),
    (Key::Digit5, "5", 0x17),
    (Key::Digit6, "6", 0x16),
    (Key::Digit7, "7", 0x1A),
    (Key::Digit8, "8", 0x1C),
    (Key::Digit9, "9", 0x19),
    (Key::Space, "space", 0x31),
    (Key::F1, "f1", 0x7A),
    (Key::F2, "f2", 0x78),
    (Key::F3, "f3", 0x63),
    (Key::F4, "f4", 0x76),
    (Key::F5, "f5", 0x60),
    (Key::F6, "f6", 0x61),
    (Key::F7, "f7", 0x62),
    (Key::F8, "f8", 0x64),
    (Key::F9, "f9", 0x65),
    (Key::F10, "f10", 0x6D),
    (Key::F11, "f11", 0x67),
    (Key::F12, "f12", 0x6F),
];

impl Key {
    fn entry(self) -> &'static (Key, &'static str, u32) {
        KEYS.iter()
            .find(|(k, _, _)| *k == self)
            .expect("every key is in the table")
    }

    /// The canonical lower-case name (`"a"`, `"1"`, `"space"`, `"f1"`).
    pub fn name(self) -> &'static str {
        self.entry().1
    }

    /// The macOS virtual key code (`kVK_*`, ANSI layout position).
    pub fn carbon_code(self) -> u32 {
        self.entry().2
    }

    /// Looks up a key by name, case-insensitively.
    pub fn from_name(name: &str) -> Option<Key> {
        KEYS.iter()
            .find(|(_, n, _)| n.eq_ignore_ascii_case(name))
            .map(|(k, _, _)| *k)
    }

    /// The key at macOS virtual key code `code` (`NSEvent.keyCode`), or
    /// `None` for a key a shortcut can't use (Return, Tab, the arrows,
    /// punctuation, the keypad, F13 and up).
    pub fn from_carbon_code(code: u32) -> Option<Key> {
        KEYS.iter().find(|(_, _, c)| *c == code).map(|(k, _, _)| *k)
    }

    /// Every key, in table order.
    pub fn all() -> impl Iterator<Item = Key> {
        KEYS.iter().map(|(k, _, _)| *k)
    }

    /// Whether the key types a character: a letter or a digit.
    fn types_a_character(self) -> bool {
        !matches!(
            self,
            Key::Space
                | Key::F1
                | Key::F2
                | Key::F3
                | Key::F4
                | Key::F5
                | Key::F6
                | Key::F7
                | Key::F8
                | Key::F9
                | Key::F10
                | Key::F11
                | Key::F12
        )
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A global hotkey: modifiers plus one key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HotkeySpec {
    /// The modifiers; at least one is set in a parsed spec.
    pub modifiers: Modifiers,
    /// The key.
    pub key: Key,
}

impl Default for HotkeySpec {
    /// This build's default summon hotkey ([`DEFAULT_SUMMON_HOTKEY`]):
    /// [`HotkeySpec::default_for`] the current channel.
    fn default() -> Self {
        HotkeySpec::default_for(Channel::CURRENT)
    }
}

/// Why a hotkey string could not be parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HotkeyError {
    /// The input was empty or only whitespace.
    Empty,
    /// A `+`-separated part was empty (`"cmd+"`, `"cmd++space"`).
    EmptyPart,
    /// A part names no known modifier or key.
    Unknown(String),
    /// A modifier appears more than once.
    DuplicateModifier(&'static str),
    /// No modifier was given.
    NoModifier,
    /// No key was given.
    NoKey,
    /// More than one key was given.
    MultipleKeys,
}

impl fmt::Display for HotkeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HotkeyError::Empty => f.write_str("empty hotkey"),
            HotkeyError::EmptyPart => f.write_str("empty part in hotkey (check the '+' signs)"),
            HotkeyError::Unknown(part) => write!(f, "unknown key or modifier {part:?}"),
            HotkeyError::DuplicateModifier(m) => write!(f, "modifier {m:?} given more than once"),
            HotkeyError::NoModifier => {
                f.write_str("hotkey needs at least one modifier (cmd, shift, option, control)")
            }
            HotkeyError::NoKey => f.write_str("hotkey has no key"),
            HotkeyError::MultipleKeys => f.write_str("hotkey has more than one key"),
        }
    }
}

impl std::error::Error for HotkeyError {}

/// macOS's own shortcuts that Settings refuses ([`Refusal::System`]), in
/// canonical form, with the feature each one opens: macOS's defaults for
/// Spotlight, input sources, emoji, screenshots, the Apple menu's Lock
/// Screen and Log Out, the Dock and full screen. macOS would let the app
/// register most of them and then never hand them over.
const SYSTEM_SHORTCUTS: [(&str, &str); 13] = [
    ("cmd+space", "Spotlight"),
    ("cmd+option+space", "Finder search"),
    ("control+space", "input sources"),
    ("option+control+space", "input sources"),
    ("cmd+control+space", "emoji and symbols"),
    ("cmd+shift+3", "screenshots"),
    ("cmd+shift+4", "screenshots"),
    ("cmd+shift+5", "screenshots"),
    ("cmd+shift+6", "screenshots"),
    ("cmd+control+q", "Lock Screen"),
    ("cmd+shift+q", "Log Out"),
    ("cmd+option+d", "hiding the Dock"),
    ("cmd+control+f", "full screen"),
];

/// Why a shortcut picked in Settings was refused; its
/// [`Display`](fmt::Display) form is the reason the Settings row shows in
/// place of its caption. The shortcut appears in its menu form
/// ([`HotkeySpec::symbols`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Command with only a letter or digit: every app's own commands
    /// (⌘Q, ⌘W, ⌘C…).
    UsedByApps(HotkeySpec),
    /// Shift or Option (or both) with a letter or digit, or Shift with
    /// Space: in every text field it types a character.
    TypesCharacter(HotkeySpec),
    /// One of macOS's own shortcuts, with the feature it opens.
    System(HotkeySpec, &'static str),
    /// A key a shortcut can't use (see [`Key::from_carbon_code`]).
    UnsupportedKey,
    /// macOS didn't register it, usually because another app holds it.
    NotAccepted(HotkeySpec),
    /// The saved shortcut failed to register when the app opened.
    NotAcceptedAtLaunch(HotkeySpec),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::UsedByApps(spec) => write!(
                f,
                "{} is used by every app. Try another shortcut.",
                spec.symbols()
            ),
            Refusal::TypesCharacter(spec) => write!(
                f,
                "{} types a character. Try another shortcut.",
                spec.symbols()
            ),
            Refusal::System(spec, feature) => write!(
                f,
                "macOS uses {} for {feature}. Try another shortcut.",
                spec.symbols()
            ),
            Refusal::UnsupportedKey => f.write_str(
                "That key can\u{2019}t be used. Use a letter, digit, Space or F1\u{2013}F12.",
            ),
            Refusal::NotAccepted(spec) => write!(
                f,
                "macOS didn\u{2019}t accept {}. Another app may be using it.",
                spec.symbols()
            ),
            Refusal::NotAcceptedAtLaunch(spec) => write!(
                f,
                "macOS didn\u{2019}t accept {} when the app opened. Click the keys to pick \
                 another.",
                spec.symbols()
            ),
        }
    }
}

impl std::error::Error for Refusal {}

impl HotkeySpec {
    /// The default summon hotkey of the app built for `channel`
    /// ([`Channel::default_hotkey`]): option+space for the release app,
    /// shift+option+space for the Preview app.
    pub fn default_for(channel: Channel) -> HotkeySpec {
        HotkeySpec::parse(channel.default_hotkey())
            .expect("every channel's default hotkey is a valid spec")
    }

    /// Parses a `+`-separated hotkey such as `"cmd+shift+space"`.
    ///
    /// Case-insensitive; parts may come in any order and surrounding
    /// whitespace is ignored.
    pub fn parse(s: &str) -> Result<HotkeySpec, HotkeyError> {
        if s.trim().is_empty() {
            return Err(HotkeyError::Empty);
        }
        let mut modifiers = Modifiers::default();
        let mut key = None;
        for part in s.split('+') {
            let part = part.trim();
            if part.is_empty() {
                return Err(HotkeyError::EmptyPart);
            }
            let lower = part.to_ascii_lowercase();
            let modifier = match lower.as_str() {
                "cmd" | "command" => Some((&mut modifiers.cmd, "cmd")),
                "shift" => Some((&mut modifiers.shift, "shift")),
                "option" | "opt" | "alt" => Some((&mut modifiers.option, "option")),
                "control" | "ctrl" => Some((&mut modifiers.control, "control")),
                _ => None,
            };
            match modifier {
                Some((flag, name)) => {
                    if *flag {
                        return Err(HotkeyError::DuplicateModifier(name));
                    }
                    *flag = true;
                }
                None => {
                    let k = Key::from_name(&lower)
                        .ok_or_else(|| HotkeyError::Unknown(part.to_string()))?;
                    if key.is_some() {
                        return Err(HotkeyError::MultipleKeys);
                    }
                    key = Some(k);
                }
            }
        }
        let key = key.ok_or(HotkeyError::NoKey)?;
        if modifiers.is_empty() {
            return Err(HotkeyError::NoModifier);
        }
        Ok(HotkeySpec { modifiers, key })
    }

    /// Whether Settings may take this shortcut, before macOS is asked.
    ///
    /// Every shortcut needs Command or Control, except Space and F1 to
    /// F12, which may use Option, Shift or both (so both apps' defaults,
    /// ⌥Space and ⌥⇧Space, pass). Refused: macOS's own shortcuts
    /// ([`Refusal::System`]); Command alone with a letter or digit
    /// ([`Refusal::UsedByApps`]); Shift alone with a letter, digit or
    /// Space, and Option with or without Shift on a letter or digit
    /// ([`Refusal::TypesCharacter`]).
    ///
    /// # Errors
    ///
    /// The [`Refusal`] saying why.
    pub fn check(&self) -> Result<(), Refusal> {
        let canonical = self.to_string();
        if let Some((_, feature)) = SYSTEM_SHORTCUTS.iter().find(|(s, _)| *s == canonical) {
            return Err(Refusal::System(*self, feature));
        }
        let m = &self.modifiers;
        let types = self.key.types_a_character();
        if types && m.exactly(true, false, false, false) {
            return Err(Refusal::UsedByApps(*self));
        }
        let shift_alone = m.exactly(false, true, false, false);
        if (shift_alone && (types || self.key == Key::Space)) || (types && !m.cmd && !m.control) {
            return Err(Refusal::TypesCharacter(*self));
        }
        Ok(())
    }

    /// The macOS virtual key code and Carbon modifier mask, for registering
    /// the hotkey (the AppKit layer feeds these into `global-hotkey`).
    pub fn to_carbon(&self) -> (u32, u32) {
        (self.key.carbon_code(), self.modifiers.to_carbon())
    }

    /// A short display form for menus and help, such as `"⌥Space"` or
    /// `"⇧⌘K"`.
    pub fn symbols(&self) -> String {
        let key = match self.key {
            Key::Space => "Space".to_string(),
            k => k.name().to_ascii_uppercase(),
        };
        format!("{}{key}", self.modifiers.symbols())
    }
}

impl std::str::FromStr for HotkeySpec {
    type Err = HotkeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        HotkeySpec::parse(s)
    }
}

impl fmt::Display for HotkeySpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = &self.modifiers;
        for (on, name) in [
            (m.cmd, "cmd"),
            (m.shift, "shift"),
            (m.option, "option"),
            (m.control, "control"),
        ] {
            if on {
                write!(f, "{name}+")?;
            }
        }
        f.write_str(self.key.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_names_and_codes_are_unique() {
        let mut names: Vec<&str> = KEYS.iter().map(|e| e.1).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), KEYS.len());
        let mut codes: Vec<u32> = KEYS.iter().map(|e| e.2).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), KEYS.len());
    }

    #[test]
    fn default_matches_documented_string() {
        assert_eq!(
            HotkeySpec::parse(DEFAULT_SUMMON_HOTKEY).unwrap(),
            HotkeySpec::default()
        );
    }

    #[test]
    fn symbols() {
        assert_eq!(
            HotkeySpec::default_for(Channel::Release).symbols(),
            "⌥Space"
        );
        assert_eq!(HotkeySpec::default_for(Channel::Rc).symbols(), "⌥⇧Space");
        assert_eq!(HotkeySpec::parse("cmd+shift+k").unwrap().symbols(), "⇧⌘K");
    }
}
