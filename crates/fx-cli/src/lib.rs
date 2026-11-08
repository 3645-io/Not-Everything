//! Library half of the command-line interface (CLI).
//!
//! The `not-everything` binary searches the index the app saved,
//! read-only. This library holds the parts that need no index, so they can
//! be tested on their own:
//!
//! - [`args`]: the hand-written argument parser, with the option names of
//!   the `es` command-line tool where it has them.
//! - [`exit`]: the documented process exit codes 0–9.
//! - [`output`]: the txt, csv and json formatters over plain rows.
//! - [`store_path`]: where the app keeps the store for a root.
//!
//! There is deliberately no option that deletes, renames, moves or trashes
//! a file: the MVP is read-only.

#![forbid(unsafe_code)]

pub mod args;
pub mod exit;
pub mod output;
pub mod store_path;

/// The command's name, in its usage text, messages and version line:
/// `not-everything`, or `not-everything-preview` for a Preview build (a
/// release candidate or beta), whose binary the release script renames to
/// match.
pub const PROGRAM: &str = fx_channel::Channel::CURRENT.command_name();
