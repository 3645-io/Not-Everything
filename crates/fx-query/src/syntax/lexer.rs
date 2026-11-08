//! Splits a query string into tokens.
//!
//! Outside quotes, whitespace separates words, `|` is OR, `<` and `>` open
//! and close a group, and `!` at the start of a token is NOT. A `!` inside a
//! word is literal. Double quotes may appear anywhere in a word; between them
//! whitespace and operator characters are literal, and the quote characters
//! themselves are dropped, so `foo" bar"` is the single word `foo bar`. An
//! unclosed quote runs to the end of input and yields a diagnostic.
//!
//! One exception keeps comparisons in one word: right after the `:` of a
//! search function that takes comparisons (`size:`, `depth:`, `len:`,
//! `dm:`, ...), `<` and `>` are part of the word, so `size:<1kb` and
//! `size:>=1mb` are single words. Once the value has started, `>` closes a
//! group again, so `<size:>1mb>` is a group holding `size:>1mb`.

use std::ops::Range;

use super::ast::{Diagnostic, DiagnosticKind};
use super::functions::takes_comparison;

/// A word token: the text of one term with quotes removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    /// The text with quote characters removed.
    pub text: String,
    /// Byte offset in `text` where the first quoted part starts, or `None`
    /// if the word has no quotes. Text before this offset was typed outside
    /// quotes (useful to tell `case:foo` from `"case:foo"`).
    pub quote_start: Option<usize>,
}

/// What a token is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    /// A word (term text).
    Word(Word),
    /// `|`
    Or,
    /// `!` at the start of a token.
    Not,
    /// `<`
    Open,
    /// `>`
    Close,
}

/// One token with its byte range in the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// What the token is.
    pub kind: TokenKind,
    /// Byte range in the input (quotes included for words).
    pub span: Range<usize>,
}

/// Whether `c` ends an unquoted word.
fn ends_word(c: char) -> bool {
    c.is_whitespace() || matches!(c, '|' | '<' | '>')
}

/// Whether the unquoted word text so far ends where a comparison operator
/// may stand: right after the `:` of a function that takes comparisons
/// (see [`takes_comparison`]), possibly after other operator characters
/// (`size:<`, `size:>=`, `file:depth:!`). There, `<` and `>` belong to the
/// word.
fn at_comparison_operator(text: &str) -> bool {
    let Some(head) = text
        .trim_end_matches(['<', '>', '=', '!'])
        .strip_suffix(':')
    else {
        return false;
    };
    let name = head.rsplit(':').next().unwrap_or(head);
    takes_comparison(name)
}

/// Splits `input` into tokens. Never fails; an unclosed quote is reported in
/// the returned diagnostics.
pub fn lex(input: &str) -> (Vec<Token>, Vec<Diagnostic>) {
    let mut tokens = Vec::new();
    let mut diagnostics = Vec::new();
    let mut chars = input.char_indices().peekable();

    while let Some(&(start, c)) = chars.peek() {
        let single = |kind| Token {
            kind,
            span: start..start + c.len_utf8(),
        };
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '|' => {
                chars.next();
                tokens.push(single(TokenKind::Or));
            }
            '!' => {
                chars.next();
                tokens.push(single(TokenKind::Not));
            }
            '<' => {
                chars.next();
                tokens.push(single(TokenKind::Open));
            }
            '>' => {
                chars.next();
                tokens.push(single(TokenKind::Close));
            }
            _ => {
                let mut text = String::new();
                let mut quote_start = None;
                // Byte offset of the currently open quote, if any.
                let mut open_quote: Option<usize> = None;
                let mut end = start;
                while let Some(&(i, ch)) = chars.peek() {
                    if open_quote.is_some() {
                        if ch == '"' {
                            open_quote = None;
                        } else {
                            text.push(ch);
                        }
                    } else if matches!(ch, '<' | '>')
                        && quote_start.is_none()
                        && at_comparison_operator(&text)
                    {
                        // `size:<1kb`: a comparison operator, not a group.
                        text.push(ch);
                    } else if ends_word(ch) {
                        break;
                    } else if ch == '"' {
                        open_quote = Some(i);
                        quote_start.get_or_insert(text.len());
                    } else {
                        text.push(ch);
                    }
                    chars.next();
                    end = i + ch.len_utf8();
                }
                if let Some(q) = open_quote {
                    diagnostics.push(Diagnostic {
                        kind: DiagnosticKind::UnclosedQuote,
                        span: q..q + 1,
                    });
                }
                tokens.push(Token {
                    kind: TokenKind::Word(Word { text, quote_start }),
                    span: start..end,
                });
            }
        }
    }
    (tokens, diagnostics)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(input: &str) -> Vec<String> {
        lex(input)
            .0
            .into_iter()
            .map(|t| match t.kind {
                TokenKind::Word(w) => w.text,
                TokenKind::Or => "|".into(),
                TokenKind::Not => "!".into(),
                TokenKind::Open => "<".into(),
                TokenKind::Close => ">".into(),
            })
            .collect()
    }

    #[test]
    fn splits_operators_and_words() {
        assert_eq!(words("a b|!<c>"), ["a", "b", "|", "!", "<", "c", ">"]);
    }

    #[test]
    fn comparison_operators_stay_in_function_words() {
        assert_eq!(words("size:<1kb"), ["size:<1kb"]);
        assert_eq!(words("SIZE:>=1kb>"), ["SIZE:>=1kb", ">"]);
        assert_eq!(words("<size:>1mb>"), ["<", "size:>1mb", ">"]);
        assert_eq!(words("file:depth:<2"), ["file:depth:<2"]);
        assert_eq!(words("ext:<a>"), ["ext:", "<", "a", ">"]);
        assert_eq!(words("\"size:\"<1"), ["size:", "<", "1"]);
    }

    #[test]
    fn bang_inside_a_word_is_literal() {
        assert_eq!(words("a!b"), ["a!b"]);
    }

    #[test]
    fn quotes_join_and_record_their_start() {
        let (toks, diags) = lex("ab\"c d\"e");
        assert!(diags.is_empty());
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].span, 0..8);
        match &toks[0].kind {
            TokenKind::Word(w) => {
                assert_eq!(w.text, "abc de");
                assert_eq!(w.quote_start, Some(2));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn unclosed_quote_is_reported() {
        let (toks, diags) = lex("x \"a b");
        assert_eq!(toks.len(), 2);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].span, 2..3);
    }
}
