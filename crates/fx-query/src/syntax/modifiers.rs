//! Search modifiers: `name:` prefixes that change how a term is matched.
//!
//! Rules, in our own words after voidtools' public Search Modifiers and
//! Search Syntax pages and docs/PLAN.md:
//!
//! - A modifier is a known name followed by `:`, written in front of a term,
//!   a quoted phrase or a `< >` group. It covers everything it prefixes and
//!   nothing else, and overrides the global [`SearchOptions`] there.
//! - Names are matched case-insensitively. The supported names, with the
//!   long aliases the public page lists, are: `case:`/`nocase:`,
//!   `path:`/`nopath:`, `file:` (`files:`), `folder:` (`folders:`),
//!   `ww:` (`wholeword:`)/`noww:`, `wfn:` (`wholefilename:`)/`nowfn:`,
//!   `regex:`/`noregex:`, `diacritics:`/`nodiacritics:`, `start-with:` and
//!   `end-with:`.
//! - Modifiers stack (`case:ww:foo`). They are applied left to right, so the
//!   one closest to the term wins (`case:nocase:Foo` ignores case), and a
//!   modifier on a term beats one on an enclosing group.
//! - `regex:` takes the rest of the term raw: nothing after it is read as a
//!   modifier, and the text is not treated as a wildcard.
//! - Only the part of a word typed outside quotes can hold modifiers, so
//!   `"case:foo"` is the literal text `case:foo`.
//! - A `word:` prefix that is not a known modifier stays part of the term's
//!   text, which leaves room for search functions (`ext:`, `size:`) later.
//!
//! [`SearchOptions`]: super::SearchOptions

use super::ast::{TermKind, TermOptions};

/// One search modifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Modifier {
    /// `case:` (`true`) or `nocase:` (`false`).
    Case(bool),
    /// `path:` (`true`) or `nopath:` (`false`).
    Path(bool),
    /// `file:` / `files:`: match files only.
    Files,
    /// `folder:` / `folders:`: match folders only.
    Folders,
    /// `ww:` / `wholeword:` (`true`) or `noww:` (`false`).
    WholeWord(bool),
    /// `wfn:` / `wholefilename:` (`true`) or `nowfn:` (`false`).
    WholeFilename(bool),
    /// `regex:` (`true`) or `noregex:` (`false`).
    Regex(bool),
    /// `diacritics:` (`true`) or `nodiacritics:` (`false`).
    Diacritics(bool),
    /// `start-with:`: the name (or path) must start with the term.
    StartWith,
    /// `end-with:`: the name (or path) must end with the term.
    EndWith,
}

/// Every modifier name (lowercase, without the `:`) and what it means.
const NAMES: &[(&str, Modifier)] = &[
    ("case", Modifier::Case(true)),
    ("nocase", Modifier::Case(false)),
    ("path", Modifier::Path(true)),
    ("nopath", Modifier::Path(false)),
    ("file", Modifier::Files),
    ("files", Modifier::Files),
    ("folder", Modifier::Folders),
    ("folders", Modifier::Folders),
    ("ww", Modifier::WholeWord(true)),
    ("wholeword", Modifier::WholeWord(true)),
    ("noww", Modifier::WholeWord(false)),
    ("wfn", Modifier::WholeFilename(true)),
    ("wholefilename", Modifier::WholeFilename(true)),
    ("nowfn", Modifier::WholeFilename(false)),
    ("regex", Modifier::Regex(true)),
    ("noregex", Modifier::Regex(false)),
    ("diacritics", Modifier::Diacritics(true)),
    ("nodiacritics", Modifier::Diacritics(false)),
    ("start-with", Modifier::StartWith),
    ("end-with", Modifier::EndWith),
];

impl Modifier {
    /// The modifier called `name` (without the `:`), matched
    /// case-insensitively, or `None` if `name` is not a modifier.
    pub fn from_name(name: &str) -> Option<Self> {
        NAMES
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|&(_, m)| m)
    }

    /// Applies the modifier to `options`, overriding what was there.
    pub fn apply(self, options: &mut TermOptions) {
        match self {
            Modifier::Case(on) => options.match_case = on,
            Modifier::Path(on) => options.match_path = on,
            Modifier::Files => options.kind = TermKind::FilesOnly,
            Modifier::Folders => options.kind = TermKind::FoldersOnly,
            Modifier::WholeWord(on) => options.whole_word = on,
            Modifier::WholeFilename(on) => options.whole_filename = on,
            Modifier::Regex(on) => options.regex = on,
            Modifier::Diacritics(on) => options.match_diacritics = on,
            Modifier::StartWith => options.start_with = true,
            Modifier::EndWith => options.end_with = true,
        }
    }
}

/// The modifiers at the start of a word, as split by [`split_modifiers`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModifierPrefix {
    /// The modifiers, in input order (apply them in this order).
    pub modifiers: Vec<Modifier>,
    /// Byte length of the modifier prefix, `:`s included. The prefix lies
    /// outside quotes, so this is also its length in the input.
    pub len: usize,
}

impl ModifierPrefix {
    /// Applies every modifier, in order, on top of `base`.
    pub fn apply_to(&self, base: TermOptions) -> TermOptions {
        let mut options = base;
        for m in &self.modifiers {
            m.apply(&mut options);
        }
        options
    }
}

/// Splits the leading modifiers off a word's `text`. `quote_start` is the
/// byte offset where the first quoted part begins (see
/// [`Word::quote_start`](super::lexer::Word::quote_start)); modifiers are
/// only read from the text before it. Reading stops at the first prefix
/// that is not a known modifier name, and right after `regex:`.
pub fn split_modifiers(text: &str, quote_start: Option<usize>) -> ModifierPrefix {
    let unquoted = &text[..quote_start.unwrap_or(text.len()).min(text.len())];
    let mut prefix = ModifierPrefix::default();
    while let Some(colon) = unquoted[prefix.len..].find(':') {
        let name = &unquoted[prefix.len..prefix.len + colon];
        let Some(modifier) = Modifier::from_name(name) else {
            break;
        };
        prefix.modifiers.push(modifier);
        prefix.len += colon + 1;
        if modifier == Modifier::Regex(true) {
            break;
        }
    }
    prefix
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_case_insensitive() {
        assert_eq!(Modifier::from_name("CaSe"), Some(Modifier::Case(true)));
        assert_eq!(Modifier::from_name("START-WITH"), Some(Modifier::StartWith));
        assert_eq!(Modifier::from_name("ext"), None);
        assert_eq!(Modifier::from_name(""), None);
    }

    #[test]
    fn splits_stacked_prefixes() {
        let p = split_modifiers("case:ww:foo", None);
        assert_eq!(
            p.modifiers,
            [Modifier::Case(true), Modifier::WholeWord(true)]
        );
        assert_eq!(p.len, 8);
    }

    #[test]
    fn stops_at_unknown_prefix_and_quotes() {
        let p = split_modifiers("case:abc:def", None);
        assert_eq!(p.modifiers, [Modifier::Case(true)]);
        assert_eq!(p.len, 5);
        assert_eq!(split_modifiers("case:foo", Some(0)).len, 0);
        assert_eq!(split_modifiers("case:foo", Some(5)).len, 5);
        assert_eq!(split_modifiers("::", None).len, 0);
    }

    #[test]
    fn regex_takes_the_rest_raw() {
        let p = split_modifiers("regex:case:x", None);
        assert_eq!(p.modifiers, [Modifier::Regex(true)]);
        assert_eq!(p.len, 6);
    }
}
