//! The search syntax: query tree ([`ast`]), tokens ([`lexer`]), the
//! lenient operator parser ([`parser`]), wildcard patterns ([`wildcard`])
//! and search modifiers ([`modifiers`]).
//!
//! Operators, in our own words after voidtools' public Search Syntax page
//! and docs/PLAN.md: whitespace between terms is AND, `|` is OR, a `!`
//! prefix is NOT, `<` and `>` group, and double quotes make whitespace and
//! operator characters literal. NOT binds tightest, then OR, then AND.
//! A term with `*` or `?` is a wildcard that must match the whole name (or
//! path); see [`wildcard`]. A `name:` prefix such as `case:` or `folder:`
//! changes how the term, phrase or group after it is matched; see
//! [`modifiers`]. Search functions such as `size:` and `dm:` parse into a
//! [`Filter`]; see [`functions`], with size values in [`size`] and date
//! values in [`date`]. Type macros such as `pic:` expand to an `ext:`
//! filter; see [`macros`].

pub mod ast;
pub mod date;
pub mod functions;
pub mod lexer;
pub mod macros;
pub mod modifiers;
pub mod parser;
pub mod size;
pub mod wildcard;

pub use ast::{
    Diagnostic, DiagnosticKind, Node, Parsed, Pattern, SearchOptions, Term, TermKind, TermOptions,
};
pub use functions::Filter;
pub use modifiers::Modifier;
pub use parser::{MAX_GROUP_DEPTH, parse};
pub use wildcard::WildcardPattern;
