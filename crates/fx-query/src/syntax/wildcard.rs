//! Wildcard patterns: `*`, `?` and `**`.
//!
//! Rules, in our own words after voidtools' public Searching and Search
//! Syntax pages and docs/PLAN.md:
//!
//! - `*` matches any run of characters, possibly empty, but never a `/`.
//! - `?` matches exactly one character (one Unicode scalar value, not a
//!   byte or a UTF-16 unit), but never a `/`.
//! - `**` (or a longer run of `*`) matches any run of characters, `/`
//!   included. Names never contain `/`, so on names `*` and `**` are the
//!   same; the difference only shows when a term is matched against a path.
//! - A wildcard pattern must match the WHOLE string (name or path), not a
//!   part of it: `*.txt` matches `notes.txt` but not `notes.txt.bak`.
//! - Quotes do not switch wildcards off: the public pages only say quotes
//!   make spaces and operator characters literal. There is no escape
//!   character, so every `*` and `?` in a term's text is a wildcard.
//!
//! Matching is a linear simulation over the set of reachable pattern
//! positions (`O(text × pattern)` time, `O(pattern)` space), so patterns
//! such as `*a*a*a*a*b` cannot blow up the way naive backtracking does.

/// One compiled piece of a [`WildcardPattern`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Piece {
    /// Text that must appear as is (subject to case folding). Never empty;
    /// adjacent literal characters are merged into one piece.
    Literal(String),
    /// `?`: exactly one character other than `/`.
    AnyOne,
    /// `*`: any run of characters without `/`, possibly empty.
    AnySeq,
    /// `**`: any run of characters, `/` included, possibly empty.
    AnySeqCrossing,
}

/// A compiled wildcard pattern that must match a whole string.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WildcardPattern {
    pieces: Vec<Piece>,
}

/// Whether `text` contains a wildcard character (`*` or `?`), which makes a
/// term a wildcard term.
pub fn has_wildcard(text: &str) -> bool {
    text.contains(['*', '?'])
}

/// One pattern step at `char` granularity, used while matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Char(char),
    One,
    Seq,
    Cross,
}

impl WildcardPattern {
    /// Compiles `text`. Every `*` and `?` is a wildcard; a run of two or
    /// more `*` is one [`Piece::AnySeqCrossing`]. Text without wildcards
    /// compiles to a single literal (an exact whole-string match).
    pub fn new(text: &str) -> Self {
        let mut pieces = Vec::new();
        let mut literal = String::new();
        let mut chars = text.chars().peekable();
        let flush = |literal: &mut String, pieces: &mut Vec<Piece>| {
            if !literal.is_empty() {
                pieces.push(Piece::Literal(std::mem::take(literal)));
            }
        };
        while let Some(c) = chars.next() {
            match c {
                '*' => {
                    flush(&mut literal, &mut pieces);
                    let mut crossing = false;
                    while chars.peek() == Some(&'*') {
                        chars.next();
                        crossing = true;
                    }
                    pieces.push(if crossing {
                        Piece::AnySeqCrossing
                    } else {
                        Piece::AnySeq
                    });
                }
                '?' => {
                    flush(&mut literal, &mut pieces);
                    pieces.push(Piece::AnyOne);
                }
                c => literal.push(c),
            }
        }
        flush(&mut literal, &mut pieces);
        WildcardPattern { pieces }
    }

    /// The compiled pieces, in order.
    pub fn pieces(&self) -> &[Piece] {
        &self.pieces
    }

    /// `Some("txt")` when the pattern is exactly `*.txt`: one `*`, a dot,
    /// then a non-empty extension with no further dot, slash or wildcard.
    /// Lets the compiled matcher route the term to the extension column.
    /// The hint describes name matching; with path matching `*` does not
    /// cross `/`, so callers must only use it for name-only terms.
    pub fn extension_hint(&self) -> Option<&str> {
        match self.pieces.as_slice() {
            [Piece::AnySeq, Piece::Literal(lit)] => {
                let ext = lit.strip_prefix('.')?;
                if ext.is_empty() || ext.contains(['.', '/']) {
                    None
                } else {
                    Some(ext)
                }
            }
            _ => None,
        }
    }

    /// Whether the pattern matches the whole of `text`. Without
    /// `case_sensitive`, both sides are lowercased one `char` at a time with
    /// std's Unicode lowercase mapping, as the reference evaluator does for
    /// literal terms.
    pub fn matches(&self, text: &str, case_sensitive: bool) -> bool {
        let steps = self.steps(case_sensitive);
        if case_sensitive {
            run(&steps, text.chars())
        } else {
            run(&steps, text.chars().flat_map(char::to_lowercase))
        }
    }

    fn steps(&self, case_sensitive: bool) -> Vec<Step> {
        let mut steps = Vec::new();
        for piece in &self.pieces {
            match piece {
                Piece::Literal(s) if case_sensitive => steps.extend(s.chars().map(Step::Char)),
                Piece::Literal(s) => {
                    steps.extend(s.chars().flat_map(char::to_lowercase).map(Step::Char))
                }
                Piece::AnyOne => steps.push(Step::One),
                Piece::AnySeq => steps.push(Step::Seq),
                Piece::AnySeqCrossing => steps.push(Step::Cross),
            }
        }
        steps
    }
}

/// Simulates the pattern over `text`, tracking every reachable position.
fn run(steps: &[Step], text: impl Iterator<Item = char>) -> bool {
    let m = steps.len();
    let mut current = vec![false; m + 1];
    let mut next = vec![false; m + 1];
    current[0] = true;
    close(steps, &mut current);
    for c in text {
        next.fill(false);
        let mut any = false;
        for (j, step) in steps.iter().enumerate() {
            if !current[j] {
                continue;
            }
            let target = match *step {
                Step::Char(p) if p == c => j + 1,
                Step::One if c != '/' => j + 1,
                Step::Seq if c != '/' => j,
                Step::Cross => j,
                _ => continue,
            };
            next[target] = true;
            any = true;
        }
        if !any {
            return false;
        }
        close(steps, &mut next);
        std::mem::swap(&mut current, &mut next);
    }
    current[m]
}

/// Adds the positions reachable without consuming a character: a `*` or
/// `**` may match the empty string, so the step after it is reachable too.
fn close(steps: &[Step], set: &mut [bool]) {
    for (j, step) in steps.iter().enumerate() {
        if set[j] && matches!(step, Step::Seq | Step::Cross) {
            set[j + 1] = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_pieces() {
        let w = WildcardPattern::new("a*b?c**d***");
        assert_eq!(
            w.pieces(),
            [
                Piece::Literal("a".into()),
                Piece::AnySeq,
                Piece::Literal("b".into()),
                Piece::AnyOne,
                Piece::Literal("c".into()),
                Piece::AnySeqCrossing,
                Piece::Literal("d".into()),
                Piece::AnySeqCrossing,
            ]
        );
    }

    #[test]
    fn literal_only_is_exact() {
        let w = WildcardPattern::new("abc");
        assert!(w.matches("abc", true));
        assert!(w.matches("ABC", false));
        assert!(!w.matches("abcd", false));
        assert!(WildcardPattern::new("").matches("", true));
        assert!(!WildcardPattern::new("").matches("a", true));
    }

    #[test]
    fn has_wildcard_detects_star_and_question_mark() {
        assert!(has_wildcard("*.txt"));
        assert!(has_wildcard("a?"));
        assert!(!has_wildcard("notes.txt"));
    }
}
