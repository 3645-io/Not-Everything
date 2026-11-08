//! Compiled regular-expression terms.
//!
//! A [`Pattern::Regex`](crate::Pattern::Regex) term (from `regex:` or the
//! global regex toggle in [`SearchOptions`](crate::SearchOptions)) is
//! compiled once into a [`RegexTerm`] with the [`regex`] crate, then tested
//! against each entry's name, or its full path in path mode.
//!
//! Rules, in our own words after voidtools' public Search Syntax and Search
//! Modifiers pages and docs/PLAN.md:
//!
//! - **Unanchored.** The expression may match anywhere in the haystack; the
//!   user writes `^` and `$` to anchor it to the start or end.
//! - **Case.** Case-insensitive (Unicode simple case folding, so `É`
//!   matches `é`) unless the term matches case (`case:` or the global
//!   toggle). An inline `(?-i)` or `(?i)` in the expression still works.
//! - **Diacritics do not apply.** The diacritics option is ignored: the
//!   expression sees accented letters as stored, so `regex:resume` does not
//!   match `Résumé.pdf`. Both the expression and the haystack are put in
//!   NFC, so a name stored decomposed (NFD, as APFS may do) and a composed
//!   query still compare equal, and `.` matches one accented letter.
//! - **Haystack.** The full path (`parent/name`) when the term matches
//!   paths, otherwise the name only. A `/` inside the expression does not
//!   switch to the path, unlike literal terms: `/` is ordinary regex text.
//! - **Modifiers after `regex:`.** The parser keeps everything after
//!   `regex:` raw, so `regex:case:^FOO` stores the pattern `case:^FOO`.
//!   [`RegexTerm::compile`] peels known modifier names off the front of the
//!   pattern and applies them, so that query is case-sensitive and anchored
//!   as written. Only a leading run of known names is peeled; `regex:` and
//!   `noregex:` in that run are dropped (the term stays a regex).
//! - **Dialect.** The `regex` crate's syntax, not PCRE: there is no
//!   look-around (`(?=`, `(?!`, `(?<=`, `(?<!`) and no back-references
//!   (`\1`). Such patterns fail to compile.
//! - **Invalid patterns** give a [`RegexError`] from [`RegexTerm::compile`].
//!   The reference evaluator treats an invalid regex term as matching
//!   nothing; it never panics.

use std::error::Error;
use std::fmt;

use regex::{Regex, RegexBuilder};

use crate::fold::{FoldOptions, fold_str};
use crate::syntax::{Modifier, TermOptions};

/// Upper bound on the compiled program size, so a pathological pattern
/// fails to compile instead of using unbounded memory.
const SIZE_LIMIT: usize = 32 * (1 << 20);

/// A regular expression that failed to compile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegexError {
    /// The pattern as written (before modifier peeling).
    pub pattern: String,
    /// The `regex` crate's description of the problem.
    pub message: String,
}

impl fmt::Display for RegexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid regular expression {:?}: {}",
            self.pattern, self.message
        )
    }
}

impl Error for RegexError {}

/// A compiled regex term: the expression plus the options it was resolved
/// under (see the [module docs](self)).
#[derive(Debug, Clone)]
pub struct RegexTerm {
    regex: Regex,
    options: TermOptions,
}

impl RegexTerm {
    /// Compiles `pattern` under the term's `options`.
    ///
    /// Leading modifier names in `pattern` (`case:`, `path:`, ...) are
    /// applied on top of `options` first; [`RegexTerm::options`] returns the
    /// result. The expression is case-insensitive unless the resolved
    /// options match case, and unanchored.
    ///
    /// ```
    /// use fx_query::TermOptions;
    /// use fx_query::regex_term::RegexTerm;
    ///
    /// let t = RegexTerm::compile("^foo.*bar$", &TermOptions::default()).unwrap();
    /// assert!(t.is_match("FOO-bar"));
    /// assert!(!t.is_match("foobar.txt"));
    /// assert!(RegexTerm::compile("(", &TermOptions::default()).is_err());
    /// ```
    pub fn compile(pattern: &str, options: &TermOptions) -> Result<Self, RegexError> {
        let (body, mut options) = peel_modifiers(pattern, *options);
        options.regex = true;
        let body = fold_str(body, FoldOptions::default());
        let regex = RegexBuilder::new(&body)
            .case_insensitive(!options.match_case)
            .size_limit(SIZE_LIMIT)
            .build()
            .map_err(|e| RegexError {
                pattern: pattern.to_owned(),
                message: e.to_string(),
            })?;
        Ok(RegexTerm { regex, options })
    }

    /// The resolved options: the ones passed to [`RegexTerm::compile`] with
    /// any modifiers peeled off the pattern applied. `match_path` picks the
    /// haystack and `kind` limits the term to files or folders; the
    /// diacritics, whole-word, whole-name, start and end flags do not apply
    /// to regex terms.
    pub fn options(&self) -> &TermOptions {
        &self.options
    }

    /// Whether the haystack is the full path rather than the name.
    pub fn matches_path(&self) -> bool {
        self.options.match_path
    }

    /// The compiled expression.
    pub fn as_regex(&self) -> &Regex {
        &self.regex
    }

    /// Whether the expression matches anywhere in `haystack` (a name, or a
    /// full path in path mode). The haystack is put in NFC first; ASCII
    /// text is used as is.
    pub fn is_match(&self, haystack: &str) -> bool {
        self.regex
            .is_match(&fold_str(haystack, FoldOptions::default()))
    }
}

/// Splits known modifier names off the front of `pattern` and applies them
/// to `options`. `regex:` and `noregex:` are consumed without effect.
fn peel_modifiers(pattern: &str, mut options: TermOptions) -> (&str, TermOptions) {
    let mut rest = pattern;
    while let Some(colon) = rest.find(':') {
        let Some(modifier) = Modifier::from_name(&rest[..colon]) else {
            break;
        };
        if !matches!(modifier, Modifier::Regex(_)) {
            modifier.apply(&mut options);
        }
        rest = &rest[colon + 1..];
    }
    (rest, options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TermKind;

    #[test]
    fn peels_leading_modifiers_only() {
        let (body, opts) = peel_modifiers("case:path:^a:b", TermOptions::default());
        assert_eq!(body, "^a:b");
        assert!(opts.match_case);
        assert!(opts.match_path);

        let (body, opts) = peel_modifiers("folder:regex:x", TermOptions::default());
        assert_eq!(body, "x");
        assert_eq!(opts.kind, TermKind::FoldersOnly);

        let (body, _) = peel_modifiers("(?i:a):b", TermOptions::default());
        assert_eq!(body, "(?i:a):b");
    }

    #[test]
    fn nfd_haystack_matches_nfc_pattern() {
        let t = RegexTerm::compile("^résumé$", &TermOptions::default()).unwrap();
        assert!(t.is_match("Re\u{301}sume\u{301}"));
        let t = RegexTerm::compile("^r.sum.$", &TermOptions::default()).unwrap();
        assert!(t.is_match("re\u{301}sume\u{301}"));
    }

    #[test]
    fn error_carries_message() {
        let err = RegexTerm::compile("(", &TermOptions::default()).unwrap_err();
        assert_eq!(err.pattern, "(");
        assert!(!err.message.is_empty());
    }
}
