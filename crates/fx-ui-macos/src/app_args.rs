//! Command line of the `fx-ui-macos` app binary, and where its engine store
//! lives.
//!
//! [`AppArgs::parse`] takes the arguments after the program name. Bad input
//! is an [`AppArgsError`] (the binary prints it with [`USAGE`] and exits
//! with code 2 before opening a window), never a panic. There are no
//! options that change files: the app is read-only.
//!
//! The measurement options (`--synthetic`, `--page-cost-ms`, `--autoscroll`,
//! `--launch-probe` and `--gate-probe`) exist only in builds with the
//! `measure` feature, which the gate scripts turn on. Release builds leave it
//! off, so they reject those options as unknown and carry none of the code
//! behind them.

#![forbid(unsafe_code)]

use std::fmt;
use std::path::{Path, PathBuf};

/// Usage text for the app binary.
#[cfg(not(feature = "measure"))]
pub const USAGE: &str = "\
usage: fx-ui-macos [--root PATH]

  --root PATH         folder or volume to index (default /)";

/// Usage text for the app binary, with the measurement options.
#[cfg(feature = "measure")]
pub const USAGE: &str = "\
usage: fx-ui-macos [--root PATH] [--synthetic N] [--page-cost-ms MS] [--autoscroll] [--launch-probe]
                   [--gate-probe OUT]

  --root PATH         folder or volume to index (default /)
  --synthetic N       show N synthetic rows instead of an index (for measuring)
  --page-cost-ms MS   make every synthetic page fetch take at least MS ms
  --autoscroll        scroll the table, print frame stats, exit
  --launch-probe      print the time to the first painted results, then exit
  --gate-probe OUT    Phase 1 gate probe over the real index for --root: time to
                      the first result painted, keystroke to repaint and
                      autoscroll frames; write GATE lines to OUT, a new file,
                      then exit (not with --synthetic)";

/// The root indexed when `--root` is not given.
pub const DEFAULT_ROOT: &str = "/";

/// The application folder under `~/Library/Application Support/`: the
/// app's name, so `Not-Everything Preview` for a release candidate or beta.
/// It holds the stores and the settings file, and only its owner may open
/// it ([`crate::data_folder`]).
pub const APP_FOLDER: &str = fx_channel::Channel::CURRENT.data_folder();

/// Parsed options of the app binary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppArgs {
    /// The root to index (`--root`).
    pub root: PathBuf,
    /// Run on `SyntheticSource` with this many rows instead of an engine
    /// (`--synthetic`), at least 1 and below `u32::MAX`.
    #[cfg(feature = "measure")]
    pub synthetic: Option<usize>,
    /// Simulated cost in milliseconds of every synthetic page fetch
    /// (`--page-cost-ms`), to approximate engine paging under
    /// `--autoscroll`. Only meaningful with `--synthetic`.
    #[cfg(feature = "measure")]
    pub page_cost_ms: Option<u64>,
    /// Scroll, print frame stats and exit (`--autoscroll`).
    #[cfg(feature = "measure")]
    pub autoscroll: bool,
    /// Print the time to first paint and exit (`--launch-probe`).
    #[cfg(feature = "measure")]
    pub launch_probe: bool,
    /// Run the Phase 1 gate probe over the real engine and write its report
    /// to this new file, then exit (`--gate-probe`). Never combined with
    /// `--synthetic`.
    #[cfg(feature = "measure")]
    pub gate_probe: Option<PathBuf>,
}

impl Default for AppArgs {
    fn default() -> Self {
        AppArgs {
            root: PathBuf::from(DEFAULT_ROOT),
            #[cfg(feature = "measure")]
            synthetic: None,
            #[cfg(feature = "measure")]
            page_cost_ms: None,
            #[cfg(feature = "measure")]
            autoscroll: false,
            #[cfg(feature = "measure")]
            launch_probe: false,
            #[cfg(feature = "measure")]
            gate_probe: None,
        }
    }
}

impl AppArgs {
    /// Whether the app runs a measurement (`--autoscroll`, `--launch-probe`
    /// or `--gate-probe`) and exits, rather than running resident.
    #[cfg(feature = "measure")]
    pub fn measuring(&self) -> bool {
        self.autoscroll || self.launch_probe || self.gate_probe.is_some()
    }

    /// Whether the app runs a measurement and exits: never, in a build
    /// without the `measure` feature.
    #[cfg(not(feature = "measure"))]
    pub fn measuring(&self) -> bool {
        false
    }
}

/// A bad command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppArgsError {
    message: String,
}

impl AppArgsError {
    fn new(message: impl Into<String>) -> AppArgsError {
        AppArgsError {
            message: message.into(),
        }
    }
}

impl fmt::Display for AppArgsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AppArgsError {}

impl AppArgs {
    /// Parses the arguments after the program name, e.g.
    /// `AppArgs::parse(std::env::args().skip(1))`. Options may come in any
    /// order; a repeated option keeps its last value.
    pub fn parse<I, S>(args: I) -> Result<AppArgs, AppArgsError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut out = AppArgs::default();
        let mut it = args.into_iter().map(Into::into);
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--root" => {
                    let value = it
                        .next()
                        .ok_or_else(|| AppArgsError::new("--root needs a path"))?;
                    if value.is_empty() {
                        return Err(AppArgsError::new("--root needs a non-empty path"));
                    }
                    out.root = PathBuf::from(value);
                }
                #[cfg(feature = "measure")]
                "--synthetic" => out.synthetic = Some(row_count(&arg, it.next())?),
                #[cfg(feature = "measure")]
                "--page-cost-ms" => out.page_cost_ms = Some(millis(&arg, it.next())?),
                #[cfg(feature = "measure")]
                "--autoscroll" => out.autoscroll = true,
                #[cfg(feature = "measure")]
                "--launch-probe" => out.launch_probe = true,
                #[cfg(feature = "measure")]
                "--gate-probe" => {
                    let value = it
                        .next()
                        .ok_or_else(|| AppArgsError::new("--gate-probe needs an output path"))?;
                    if value.is_empty() {
                        return Err(AppArgsError::new(
                            "--gate-probe needs a non-empty output path",
                        ));
                    }
                    out.gate_probe = Some(PathBuf::from(value));
                }
                other if other.starts_with('-') => {
                    return Err(AppArgsError::new(format!("unknown option {other:?}")));
                }
                other => {
                    return Err(AppArgsError::new(format!("unexpected argument {other:?}")));
                }
            }
        }
        #[cfg(feature = "measure")]
        if out.gate_probe.is_some() && out.synthetic.is_some() {
            return Err(AppArgsError::new(
                "--gate-probe measures the real index and cannot be combined with --synthetic",
            ));
        }
        Ok(out)
    }
}

/// Parses the value of `opt` as a row count: a decimal integer from 1 to
/// `u32::MAX - 1`.
#[cfg(feature = "measure")]
fn row_count(opt: &str, value: Option<String>) -> Result<usize, AppArgsError> {
    let value = value.ok_or_else(|| AppArgsError::new(format!("{opt} needs a row count")))?;
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(AppArgsError::new(format!(
            "{opt} needs a positive whole number, got {value:?}"
        )));
    }
    let n: u64 = value
        .parse()
        .map_err(|_| AppArgsError::new(format!("{opt} value {value:?} is out of range")))?;
    if n == 0 {
        return Err(AppArgsError::new(format!("{opt} must be at least 1")));
    }
    if n >= u64::from(u32::MAX) {
        return Err(AppArgsError::new(format!(
            "{opt} must be below {}",
            u32::MAX
        )));
    }
    usize::try_from(n).map_err(|_| AppArgsError::new(format!("{opt} value is too large")))
}

/// Parses the value of `opt` as milliseconds: a decimal integer from 0 to
/// one hour.
#[cfg(feature = "measure")]
fn millis(opt: &str, value: Option<String>) -> Result<u64, AppArgsError> {
    const MAX_MS: u64 = 3_600_000;
    let value = value.ok_or_else(|| AppArgsError::new(format!("{opt} needs milliseconds")))?;
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(AppArgsError::new(format!(
            "{opt} needs a whole number of milliseconds, got {value:?}"
        )));
    }
    let ms: u64 = value
        .parse()
        .map_err(|_| AppArgsError::new(format!("{opt} value {value:?} is out of range")))?;
    if ms > MAX_MS {
        return Err(AppArgsError::new(format!("{opt} must be at most {MAX_MS}")));
    }
    Ok(ms)
}

/// Where the engine store for `root` lives:
/// `<home>/Library/Application Support/Not-Everything/stores/<file>`, with
/// a file name derived from the root path (a readable last component plus a
/// stable 64-bit FNV-1a hash of the whole path), so each root has its own
/// store and the same root finds its store again on the next launch.
pub fn store_path(home: &Path, root: &Path) -> PathBuf {
    let label: String = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "root".to_string())
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .take(32)
        .collect();
    let hash = fnv1a(&path_bytes(root));
    home.join("Library")
        .join("Application Support")
        .join(APP_FOLDER)
        .join("stores")
        .join(format!("{label}-{hash:016x}.fxstore"))
}

/// The access scope a checkpoint store was built in.
///
/// A store built in limited (non-FDA) mode has a snapshot without the
/// protected folders and a change-stream cursor that skipped them, so it must
/// never be replayed once Full Disk Access is granted; and a full store must
/// not be kept current by the limited change stream after access is revoked.
/// Each scope therefore has its own store file ([`store_path_for`]): switching
/// scope finds no store and starts with a fresh first scan.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StoreScope {
    /// Built without Full Disk Access: consent-prompt folders were skipped.
    Limited,
    /// Built with Full Disk Access.
    Full,
}

impl StoreScope {
    /// The scope a launch with `access` builds and loads its store in:
    /// `Granted` is [`StoreScope::Full`]; `Denied` and `Unknown` run in
    /// limited mode (as in `onboarding`) and are [`StoreScope::Limited`].
    pub fn for_access(access: fx_fs::AccessState) -> StoreScope {
        match access {
            fx_fs::AccessState::Granted => StoreScope::Full,
            _ => StoreScope::Limited,
        }
    }
}

/// Where the engine store for `root` built in `scope` lives.
///
/// [`StoreScope::Full`] is [`store_path`], the path stores had before they
/// had a scope, so those stores keep loading. [`StoreScope::Limited`] uses
/// the same file name in a sibling `stores-limited` folder, so it never
/// shares a path with any root's full store. (Limited mode arrived together
/// with this split, so no release wrote a limited store to the full path.)
pub fn store_path_for(home: &Path, root: &Path, scope: StoreScope) -> PathBuf {
    let full = store_path(home, root);
    match scope {
        StoreScope::Full => full,
        StoreScope::Limited => {
            let file = full.file_name().map(ToOwned::to_owned).unwrap_or_default();
            home.join("Library")
                .join("Application Support")
                .join(APP_FOLDER)
                .join("stores-limited")
                .join(file)
        }
    }
}

#[cfg(unix)]
fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn path_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().into_owned().into_bytes()
}

/// 64-bit FNV-1a, stable across Rust versions and launches.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}
