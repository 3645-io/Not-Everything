//! Process exit codes of the `not-everything` binary.
//!
//! The numbering follows the contract of the `es` command-line tool so that
//! scripts written against it keep working:
//!
//! | code | meaning |
//! |------|---------|
//! | 0 | success |
//! | 1, 2 | reserved, never returned (window-class codes have no macOS meaning) |
//! | 3 | out of memory (reserved) |
//! | 4 | an option is missing its argument, or the argument is invalid (also `-0` with csv or json output) |
//! | 5 | the export output file can't be created |
//! | 6 | unknown option |
//! | 7 | the query failed (parse error, or the store rejected it) |
//! | 8 | no index found (the app has never saved one) |
//! | 9 | no results, and `-no-result-error` was given |

use std::fmt;

/// A documented exit code. [`ExitCode::code`] gives the numeric value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExitCode {
    /// 0: the search ran (results, a count, help or the version were printed).
    Success,
    /// 3: out of memory. Reserved; Rust aborts on allocation failure.
    OutOfMemory,
    /// 4: an option is missing its argument, or the argument is invalid
    /// (also `-0` with csv or json output).
    BadArgument,
    /// 5: the export output file can't be created or written.
    ExportFailed,
    /// 6: an unknown option was given.
    UnknownOption,
    /// 7: the query failed to parse, or the store rejected it.
    QueryFailed,
    /// 8: no index was found at the store path.
    NoIndex,
    /// 9: the search found nothing and `-no-result-error` was given.
    NoResults,
}

impl ExitCode {
    /// The numeric process exit status.
    pub fn code(self) -> u8 {
        match self {
            ExitCode::Success => 0,
            ExitCode::OutOfMemory => 3,
            ExitCode::BadArgument => 4,
            ExitCode::ExportFailed => 5,
            ExitCode::UnknownOption => 6,
            ExitCode::QueryFailed => 7,
            ExitCode::NoIndex => 8,
            ExitCode::NoResults => 9,
        }
    }

    /// A short description of the code, as documented.
    pub fn description(self) -> &'static str {
        match self {
            ExitCode::Success => "success",
            ExitCode::OutOfMemory => "out of memory",
            ExitCode::BadArgument => "an option is missing its argument or the argument is invalid",
            ExitCode::ExportFailed => "the export output file can't be created",
            ExitCode::UnknownOption => "unknown option",
            ExitCode::QueryFailed => "the query failed",
            ExitCode::NoIndex => "no index found",
            ExitCode::NoResults => "no results",
        }
    }
}

impl fmt::Display for ExitCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.code(), self.description())
    }
}

impl From<ExitCode> for std::process::ExitCode {
    fn from(code: ExitCode) -> Self {
        std::process::ExitCode::from(code.code())
    }
}
