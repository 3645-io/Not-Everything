//! Command-line options of the `table_spike` example.
//!
//! [`SpikeArgs::parse`] takes the arguments after the program name. Bad
//! input is a [`SpikeArgsError`] (the example prints it with [`USAGE`] and
//! exits with code 2), never a panic.

#![forbid(unsafe_code)]

use std::fmt;

/// Usage text for the spike example.
pub const USAGE: &str = "\
usage: table_spike [--rows N] [--seed S] [--autoscroll FRAMES]
                   [--rows-per-frame K] [--launch-probe] [--json]

  --rows N            virtual rows in the table (default 2000000)
  --seed S            seed of the synthetic names (default 1)
  --autoscroll FRAMES scroll FRAMES frames, print frame stats, exit
                      (default 0: interactive)
  --rows-per-frame K  rows scrolled per autoscroll frame (default 5)
  --launch-probe      print the time to the first painted table, then exit
  --json              print results as JSON";

/// Default number of virtual rows.
pub const DEFAULT_ROWS: usize = 2_000_000;
/// Default seed of the synthetic tree.
pub const DEFAULT_SEED: u64 = 1;
/// Default rows scrolled per autoscroll frame.
pub const DEFAULT_ROWS_PER_FRAME: usize = 5;

/// Parsed options of the spike example.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpikeArgs {
    /// Number of virtual rows (`--rows`), at least 1 and below `u32::MAX`.
    pub rows: usize,
    /// Seed of the synthetic tree (`--seed`).
    pub seed: u64,
    /// Frames to autoscroll before printing stats and exiting
    /// (`--autoscroll`); 0 means interactive.
    pub autoscroll: u64,
    /// Rows scrolled per autoscroll frame (`--rows-per-frame`), at least 1.
    pub rows_per_frame: usize,
    /// Print the time to the first painted table, then exit
    /// (`--launch-probe`).
    pub launch_probe: bool,
    /// Print results as JSON (`--json`).
    pub json: bool,
}

impl Default for SpikeArgs {
    fn default() -> Self {
        SpikeArgs {
            rows: DEFAULT_ROWS,
            seed: DEFAULT_SEED,
            autoscroll: 0,
            rows_per_frame: DEFAULT_ROWS_PER_FRAME,
            launch_probe: false,
            json: false,
        }
    }
}

/// A bad command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpikeArgsError {
    message: String,
}

impl SpikeArgsError {
    fn new(message: impl Into<String>) -> SpikeArgsError {
        SpikeArgsError {
            message: message.into(),
        }
    }
}

impl fmt::Display for SpikeArgsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SpikeArgsError {}

impl SpikeArgs {
    /// Parses the arguments after the program name, e.g.
    /// `SpikeArgs::parse(std::env::args().skip(1))`. Options may come in
    /// any order; a repeated option keeps its last value.
    pub fn parse<I, S>(args: I) -> Result<SpikeArgs, SpikeArgsError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut out = SpikeArgs::default();
        let mut it = args.into_iter().map(Into::into);
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--rows" => {
                    let n = number(&arg, it.next())?;
                    if n == 0 {
                        return Err(SpikeArgsError::new("--rows must be at least 1"));
                    }
                    if n >= u64::from(u32::MAX) {
                        return Err(SpikeArgsError::new(format!(
                            "--rows must be below {}",
                            u32::MAX
                        )));
                    }
                    out.rows = usize::try_from(n)
                        .map_err(|_| SpikeArgsError::new("--rows is too large"))?;
                }
                "--seed" => out.seed = number(&arg, it.next())?,
                "--autoscroll" => out.autoscroll = number(&arg, it.next())?,
                "--rows-per-frame" => {
                    let k = number(&arg, it.next())?;
                    if k == 0 {
                        return Err(SpikeArgsError::new("--rows-per-frame must be at least 1"));
                    }
                    out.rows_per_frame = usize::try_from(k)
                        .map_err(|_| SpikeArgsError::new("--rows-per-frame is too large"))?;
                }
                "--launch-probe" => out.launch_probe = true,
                "--json" => out.json = true,
                other if other.starts_with('-') => {
                    return Err(SpikeArgsError::new(format!("unknown option {other:?}")));
                }
                other => {
                    return Err(SpikeArgsError::new(format!(
                        "unexpected argument {other:?}"
                    )));
                }
            }
        }
        Ok(out)
    }
}

/// Parses the value of option `opt` as a non-negative decimal integer.
fn number(opt: &str, value: Option<String>) -> Result<u64, SpikeArgsError> {
    let value = value.ok_or_else(|| SpikeArgsError::new(format!("{opt} needs a value")))?;
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(SpikeArgsError::new(format!(
            "{opt} needs a non-negative whole number, got {value:?}"
        )));
    }
    value
        .parse::<u64>()
        .map_err(|_| SpikeArgsError::new(format!("{opt} value {value:?} is out of range")))
}
