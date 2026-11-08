//! The lenient operator parser.
//!
//! Grammar, loosest binding first:
//!
//! ```text
//! query   = and
//! and     = or { or }                 (operands separated by whitespace)
//! or      = unary { "|" unary }
//! unary   = { "!" } primary
//! primary = [ modifiers ] word | [ modifiers ] "<" and [ ">" ]
//! ```
//!
//! So NOT binds tightest, then OR, then AND: `a b|c` is `a AND (b OR c)`.
//! A run of `!` keeps only its parity (`!!a` is `a`), and nested ANDs and
//! ORs of the same kind are flattened, so `<a b> c` is `and(a, b, c)`.
//!
//! Search modifiers (see [`modifiers`](super::modifiers)) are read from the
//! start of a word. If they leave text (or a quoted phrase), they apply to
//! that term; if the word is nothing but modifiers and a `<` follows with no
//! space, they apply to every term of the group, under any modifiers of the
//! terms themselves. Otherwise they are dropped, except that a bare `file:`
//! or `folder:` still filters by kind: it becomes an empty term matching
//! every file (or every folder), so `folder:` alone lists all folders.
//!
//! A word whose unquoted `name:` is a search function (`size:`, `ext:`,
//! ...; see [`functions`](super::functions)) becomes a [`Node::Function`]
//! instead of a term, under the same operators as terms. `count:N` sets
//! [`Parsed::count`] and adds no node. A function with a missing or
//! malformed argument is dropped with a diagnostic.
//!
//! A type macro (`pic:`, `doc:`, ...; see [`macros`](super::macros)) is
//! its `ext:` filter when bare, and that filter AND the argument as an
//! ordinary term when one follows (`pic:holiday`).
//!
//! Parsing never fails. Problems are recorded as diagnostics and recovered:
//! an unclosed group closes at the end of input, a stray `>` is ignored, an
//! empty `|` operand, a dangling `!`, an empty `<>` group and a modifier
//! with nothing to apply to (other than bare `file:`/`folder:`) are dropped.

use std::ops::Range;

use super::ast::{
    Diagnostic, DiagnosticKind, Node, Parsed, Pattern, SearchOptions, Term, TermKind, TermOptions,
};
use super::functions::{ArgError, FunctionWord, parse_function};
use super::lexer::{Token, TokenKind, Word, lex};
use super::macros::macro_filter;
use super::modifiers::split_modifiers;

/// Deepest `<` nesting the parser follows. A `<` beyond this depth is
/// ignored with a [`DiagnosticKind::TooDeep`] diagnostic, which keeps the
/// recursion bounded on hostile input.
pub const MAX_GROUP_DEPTH: usize = 128;

/// Parses a query. Never fails: see the module docs for how malformed input
/// is recovered. An empty or whitespace-only query gives [`Node::MatchAll`].
pub fn parse(input: &str, opts: &SearchOptions) -> Parsed {
    let (tokens, diagnostics) = lex(input);
    let mut parser = Parser {
        tokens,
        pos: 0,
        base: TermOptions::from_search(opts),
        diagnostics,
        count: None,
    };
    let children = parser.parse_and(0);
    let query = combine_and(children).unwrap_or(Node::MatchAll);
    let mut diagnostics = parser.diagnostics;
    diagnostics.sort_by_key(|d| (d.span.start, d.span.end));
    Parsed {
        query,
        diagnostics,
        count: parser.count,
    }
}

/// Builds the term for one word token whose modifier prefix (`prefix_len`
/// bytes) has already been resolved into `options`. The term's text and
/// span leave the prefix out.
fn make_term(word: Word, span: Range<usize>, prefix_len: usize, options: TermOptions) -> Term {
    let Word { mut text, .. } = word;
    text.drain(..prefix_len);
    Term {
        pattern: Pattern::for_text(&text, &options),
        text,
        options,
        span: span.start + prefix_len..span.end,
    }
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    /// Options for terms at the current position: the global options plus
    /// the modifiers of every enclosing group.
    base: TermOptions,
    diagnostics: Vec<Diagnostic>,
    /// The limit from the last `count:` word.
    count: Option<usize>,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn diag(&mut self, kind: DiagnosticKind, span: Range<usize>) {
        self.diagnostics.push(Diagnostic { kind, span });
    }

    /// Parses AND operands until the end of input, or until a `>` when
    /// inside a group (`depth > 0`). The `>` is left for the caller.
    fn parse_and(&mut self, depth: usize) -> Vec<Node> {
        let mut children = Vec::new();
        while let Some(tok) = self.peek() {
            if tok.kind == TokenKind::Close {
                if depth > 0 {
                    break;
                }
                let span = tok.span.clone();
                self.pos += 1;
                self.diag(DiagnosticKind::StrayClose, span);
                continue;
            }
            // The next token is a word, `|`, `!` or `<`, so `parse_or`
            // always consumes at least one token.
            if let Some(node) = self.parse_or(depth) {
                children.push(node);
            }
        }
        children
    }

    /// Parses `unary { "|" unary }`, dropping empty operands.
    fn parse_or(&mut self, depth: usize) -> Option<Node> {
        let mut operands = Vec::new();
        let mut last_bar: Option<Range<usize>> = None;
        loop {
            let operand = self.parse_unary(depth);
            let bar = match self.peek() {
                Some(tok) if tok.kind == TokenKind::Or => Some(tok.span.clone()),
                _ => None,
            };
            let missing = operand.is_none();
            operands.extend(operand);
            match bar {
                Some(span) => {
                    if missing {
                        self.diag(DiagnosticKind::EmptyOrOperand, span.clone());
                    }
                    self.pos += 1;
                    last_bar = Some(span);
                }
                None => {
                    if missing && let Some(span) = last_bar {
                        self.diag(DiagnosticKind::EmptyOrOperand, span);
                    }
                    break;
                }
            }
        }
        combine_or(operands)
    }

    /// Parses `{ "!" } primary`. Returns `None` (consuming nothing) at the
    /// end of input or before `|` or `>`.
    fn parse_unary(&mut self, depth: usize) -> Option<Node> {
        let mut nots: Vec<Range<usize>> = Vec::new();
        while let Some(tok) = self.peek() {
            if tok.kind != TokenKind::Not {
                break;
            }
            nots.push(tok.span.clone());
            self.pos += 1;
        }
        match self.parse_primary(depth) {
            // `!!x` is `x`: only the parity of the `!` run matters, which
            // also keeps the tree shallow on long `!` runs.
            Some(node) if nots.len() % 2 == 1 => Some(Node::Not(Box::new(node))),
            Some(node) => Some(node),
            None => {
                if let Some(first) = nots.first() {
                    self.diag(DiagnosticKind::DanglingNot, first.clone());
                }
                None
            }
        }
    }

    fn parse_primary(&mut self, depth: usize) -> Option<Node> {
        loop {
            let tok = self.peek()?;
            let span = tok.span.clone();
            match &tok.kind {
                TokenKind::Word(word) => {
                    let word = word.clone();
                    self.pos += 1;
                    let prefix = split_modifiers(&word.text, word.quote_start);
                    let options = prefix.apply_to(self.base);
                    if let Some(node) = self.function(&word, &span, prefix.len, options) {
                        return node;
                    }
                    if prefix.len < word.text.len() || word.quote_start.is_some() {
                        let term = make_term(word, span, prefix.len, options);
                        return Some(Node::Term(term));
                    }
                    // Only modifiers: they cover a `<` group glued to them,
                    // or else nothing.
                    let open = match self.peek() {
                        Some(tok) if tok.kind == TokenKind::Open && tok.span.start == span.end => {
                            tok.span.clone()
                        }
                        _ if options.kind != TermKind::Any => {
                            // A bare `file:` or `folder:` still filters by
                            // kind: it becomes an empty term that matches
                            // every entry of that kind.
                            let only_kind = TermOptions {
                                kind: options.kind,
                                ..TermOptions::default()
                            };
                            let term = make_term(word, span, prefix.len, only_kind);
                            return Some(Node::Term(term));
                        }
                        _ => {
                            self.diag(DiagnosticKind::BareModifier, span);
                            return None;
                        }
                    };
                    self.pos += 1;
                    if depth >= MAX_GROUP_DEPTH {
                        // Ignore the `<` and the modifiers with it.
                        self.diag(DiagnosticKind::TooDeep, open);
                        continue;
                    }
                    let outer = std::mem::replace(&mut self.base, options);
                    let node = self.parse_group(open, depth);
                    self.base = outer;
                    return node;
                }
                TokenKind::Open => {
                    self.pos += 1;
                    if depth >= MAX_GROUP_DEPTH {
                        // Ignore the `<`; its `>` will be reported as stray
                        // or will close an outer group early.
                        self.diag(DiagnosticKind::TooDeep, span);
                        continue;
                    }
                    return self.parse_group(span, depth);
                }
                TokenKind::Or | TokenKind::Close | TokenKind::Not => return None,
            }
        }
    }

    /// Reads a word (after its `prefix_len` bytes of modifiers) as a search
    /// function. Returns `None` if it is not one, so the word stays a term;
    /// otherwise the node to use, which is `None` for `count:` and for a
    /// function whose argument is missing or malformed (with a diagnostic).
    /// A `file:` or `folder:` modifier on a function still limits the kind.
    fn function(
        &mut self,
        word: &Word,
        span: &Range<usize>,
        prefix_len: usize,
        options: TermOptions,
    ) -> Option<Option<Node>> {
        let rest = &word.text[prefix_len..];
        let colon = rest.find(':')?;
        let name_end = prefix_len + colon;
        // The name must be typed outside quotes: `"size:1kb"` is text.
        if word.quote_start.is_some_and(|q| q <= name_end) {
            return None;
        }
        let (name, arg) = (&rest[..colon], &rest[colon + 1..]);
        // Text before the first quote maps byte for byte onto the input, so
        // offsets up to just past the `:` are valid input offsets.
        let fn_span = span.start + prefix_len..span.end;
        let arg_span = span.start + name_end + 1..span.end;
        if let Some(filter) = macro_filter(name) {
            // A bare macro is its `ext:` filter; with an argument it is the
            // filter AND the argument as an ordinary term, which carries the
            // word's modifiers (kind included).
            let node = Node::Function(filter);
            if arg.is_empty() {
                return Some(Some(with_kind(node, options, &fn_span)));
            }
            let term = make_term(word.clone(), span.clone(), name_end + 1, options);
            return Some(Some(Node::And(vec![node, Node::Term(term)])));
        }
        let result = parse_function(name, arg)?;
        Some(match result {
            Ok(FunctionWord::Filter(filter)) => {
                Some(with_kind(Node::Function(filter), options, &fn_span))
            }
            Ok(FunctionWord::Count(n)) => {
                self.count = Some(n);
                None
            }
            Err(ArgError::Missing) => {
                self.diag(DiagnosticKind::MissingArgument, fn_span);
                None
            }
            Err(ArgError::Invalid) => {
                self.diag(DiagnosticKind::InvalidArgument, arg_span);
                None
            }
        })
    }

    /// Parses the inside of a group whose `<` (at `open`) was just consumed,
    /// plus its closing `>` if present.
    fn parse_group(&mut self, open: Range<usize>, depth: usize) -> Option<Node> {
        let children = self.parse_and(depth + 1);
        match self.peek() {
            Some(tok) if tok.kind == TokenKind::Close => self.pos += 1,
            _ => self.diag(DiagnosticKind::UnclosedGroup, open.clone()),
        }
        let node = combine_and(children);
        if node.is_none() {
            self.diag(DiagnosticKind::EmptyGroup, open);
        }
        node
    }
}

/// Wraps a function node so that a `file:` or `folder:` modifier on its
/// word still limits the kind: `and(kind-only empty term, node)`. Returns
/// the node itself when the kind is [`TermKind::Any`].
fn with_kind(node: Node, options: TermOptions, fn_span: &Range<usize>) -> Node {
    if options.kind == TermKind::Any {
        return node;
    }
    let kind = TermOptions {
        kind: options.kind,
        ..TermOptions::default()
    };
    let term = Term {
        text: String::new(),
        pattern: Pattern::Literal(String::new()),
        options: kind,
        span: fn_span.start..fn_span.start,
    };
    Node::And(vec![Node::Term(term), node])
}

/// Combines AND operands, flattening nested ANDs. `None` if there are none.
fn combine_and(children: Vec<Node>) -> Option<Node> {
    combine(children, true)
}

/// Combines OR operands, flattening nested ORs. `None` if there are none.
fn combine_or(children: Vec<Node>) -> Option<Node> {
    combine(children, false)
}

fn combine(children: Vec<Node>, and: bool) -> Option<Node> {
    let mut flat = Vec::with_capacity(children.len());
    for child in children {
        match child {
            Node::And(inner) if and => flat.extend(inner),
            Node::Or(inner) if !and => flat.extend(inner),
            other => flat.push(other),
        }
    }
    match flat.len() {
        0 => None,
        1 => flat.pop(),
        _ if and => Some(Node::And(flat)),
        _ => Some(Node::Or(flat)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_nesting_is_bounded_and_reported() {
        let input = format!("{}a{}", "<".repeat(10_000), ">".repeat(10_000));
        let parsed = parse(&input, &SearchOptions::default());
        assert!(matches!(parsed.query, Node::Term(_)));
        assert!(
            parsed
                .diagnostics
                .iter()
                .any(|d| d.kind == DiagnosticKind::TooDeep)
        );
    }

    #[test]
    fn deep_modifier_groups_are_bounded() {
        let input = format!("{}a", "case:<".repeat(10_000));
        let parsed = parse(&input, &SearchOptions::default());
        match parsed.query {
            Node::Term(t) => assert!(t.options.match_case),
            other => panic!("expected a term, got {other:?}"),
        }
        assert!(
            parsed
                .diagnostics
                .iter()
                .any(|d| d.kind == DiagnosticKind::TooDeep)
        );
    }

    #[test]
    fn long_not_chain_does_not_recurse() {
        let input = format!("{}a", "!".repeat(100_001));
        let parsed = parse(&input, &SearchOptions::default());
        assert!(parsed.diagnostics.is_empty());
        match parsed.query {
            Node::Not(inner) => assert!(matches!(*inner, Node::Term(_))),
            other => panic!("expected not(a), got {other:?}"),
        }
        let parsed = parse("!!a", &SearchOptions::default());
        assert!(matches!(parsed.query, Node::Term(_)));
    }

    #[test]
    fn bare_kind_modifier_is_a_clean_kind_filter() {
        let parsed = parse("folder:", &SearchOptions::default());
        assert!(parsed.diagnostics.is_empty());
        match parsed.query {
            Node::Term(t) => {
                assert_eq!(t.options.kind, TermKind::FoldersOnly);
                assert_eq!(t.pattern, Pattern::Literal(String::new()));
                assert_eq!(t.span, 7..7);
            }
            other => panic!("expected a term, got {other:?}"),
        }
        // Global regex does not turn the kind filter into a regex term.
        let opts = SearchOptions {
            regex: true,
            ..SearchOptions::default()
        };
        match parse("file:", &opts).query {
            Node::Term(t) => assert_eq!(t.pattern, Pattern::Literal(String::new())),
            other => panic!("expected a term, got {other:?}"),
        }
    }

    #[test]
    fn functions_and_their_diagnostics() {
        use crate::syntax::functions::Filter;
        let opts = SearchOptions::default();
        // A kind modifier on a function still limits the kind.
        match parse("folder:empty:", &opts).query {
            Node::And(children) => {
                assert!(
                    matches!(&children[0], Node::Term(t) if t.options.kind == TermKind::FoldersOnly)
                );
                assert_eq!(children[1], Node::Function(Filter::Empty));
            }
            other => panic!("expected and(kind, empty), got {other:?}"),
        }
        // A quoted name is text.
        assert!(matches!(parse("\"ext:txt\"", &opts).query, Node::Term(_)));
        // Invalid arguments point at the argument, missing ones at the word.
        let parsed = parse("a size:abc", &opts);
        assert_eq!(parsed.diagnostics.len(), 1);
        assert_eq!(parsed.diagnostics[0].kind, DiagnosticKind::InvalidArgument);
        assert_eq!(parsed.diagnostics[0].span, 7..10);
        let parsed = parse("depth:", &opts);
        assert_eq!(parsed.diagnostics[0].kind, DiagnosticKind::MissingArgument);
        assert_eq!(parsed.diagnostics[0].span, 0..6);
        assert_eq!(parsed.query, Node::MatchAll);
        // The last `count:` wins.
        assert_eq!(parse("count:2 count:7", &opts).count, Some(7));
    }

    #[test]
    fn empty_group_is_dropped_with_a_diagnostic() {
        let parsed = parse("a <>", &SearchOptions::default());
        assert!(matches!(parsed.query, Node::Term(_)));
        assert_eq!(parsed.diagnostics.len(), 1);
        assert_eq!(parsed.diagnostics[0].kind, DiagnosticKind::EmptyGroup);
        assert_eq!(parsed.diagnostics[0].span, 2..3);
    }
}
