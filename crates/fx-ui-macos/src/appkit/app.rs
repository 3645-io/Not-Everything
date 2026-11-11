//! What the `fx-ui-macos` binary runs: picks the results source from
//! [`AppArgs`] (an engine over the root or, in a `measure` build, synthetic
//! rows), arms the measurement watchdog and opens the window.
//!
//! A resident launch without Full Disk Access runs the limited engine behind
//! a [`SwitchableSource`]: when access is granted, the window moves to a
//! full-access engine over the same root without a relaunch (see
//! `open_full_index`).
//!
//! The stores and the settings live in the data folder
//! ([`AppArgs::data_folder`]): the app's folder under Application Support,
//! or with `--data-dir` (a `measure` build) the measurement's own folder.
//! Such a run registers no global hotkey and leaves the login item alone.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(feature = "measure")]
use std::time::Duration;
use std::time::Instant;

use fx_engine::{Engine, EngineConfig};
use fx_fs::{ExclusionConfig, Exclusions};
use fx_fs_macos::enumerator::MacScanner;
use fx_fs_macos::fda::{FdaProbe, consent_prompt_paths, user_home};
use fx_fs_macos::fsevents::source::FsEventsSource;
use fx_fs_macos::fsevents::stream::StreamConfig;
use fx_fs_macos::fsevents::volumes_source::VolumesSource;
use fx_fs_macos::path_stat::MacPathStat;
use fx_fs_macos::scan::ScanOptions;
use fx_fs_macos::volumes::ExternalVolumes;

use super::window::{self, FullIndexSwitch, ResidentConfig, WindowConfig};
use crate::APP_NAME;
use crate::app_args::{AppArgs, StoreScope, app_data_folder, store_path_in};
use crate::build_info::{BuildInfo, Showing, window_title};
use crate::data_folder::make_store_folders_private;
use crate::limited::ExcludingChangeSource;
use crate::onboarding::{Onboarding, OnboardingEvent};
use crate::settings::{Settings, settings_path_in, user_settings_path};
use crate::source::{EngineSource, ResultsSource};
use crate::switchable::SwitchableSource;
use crate::your_files::your_folders;

/// Seed of the synthetic rows in `--synthetic` mode.
#[cfg(feature = "measure")]
const SYNTHETIC_SEED: u64 = 1;

/// Why the app could not start.
#[derive(Debug)]
pub enum AppError {
    /// The engine could not be opened over the root.
    Engine(PathBuf, std::io::Error),
    /// The window could not be opened.
    Window(window::WindowError),
    /// Without Full Disk Access the root lies inside a folder that would
    /// raise a consent prompt (the folder is given).
    NeedsFullDiskAccess(PathBuf),
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::NeedsFullDiskAccess(folder) => write!(
                f,
                "indexing inside {} needs Full Disk Access (System Settings \u{203a} \
                 Privacy & Security \u{203a} Full Disk Access); without it that folder \
                 is skipped so no consent prompt appears",
                folder.display()
            ),
            AppError::Engine(root, e) => {
                write!(f, "cannot open the index for {}: {e}", root.display())
            }
            AppError::Window(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for AppError {}

/// Opens the source `args` asks for and runs the window until it closes
/// (or, in measurement modes, until the report is printed).
pub fn launch(args: AppArgs, process_start: Instant) -> Result<(), AppError> {
    let measuring = args.measuring();
    #[cfg(feature = "measure")]
    arm_measure_watchdog(&args);
    #[cfg(feature = "measure")]
    if args.has_data_dir() {
        // Before the resident side reads the login item's status.
        super::login_item::keep_off();
    }

    // Where the stores go: the --data-dir folder, else the app's data
    // folder under the home folder (none without one: no store is kept).
    let data_folder = args
        .data_dir()
        .map(Path::to_path_buf)
        .or_else(|| user_home().map(|home| app_data_folder(&home)));

    // Settings and the Full Disk Access probe come before the scanner, so
    // limited mode can exclude the folders that would raise a consent
    // prompt.
    let settings_path = match args.data_dir() {
        Some(dir) => Some(settings_path_in(dir)),
        None => user_settings_path(),
    };
    let settings = match settings_path.as_deref().map(Settings::load) {
        Some(Ok(s)) => s,
        Some(Err(e)) => {
            eprintln!("{APP_NAME}: cannot read settings, using defaults: {e}");
            Settings::default()
        }
        None => Settings::default(),
    };
    let mut onboarding = Onboarding::new(settings.fda_onboarding_dismissed);
    let launch_output = onboarding.handle(OnboardingEvent::Launch, &FdaProbe::new());

    // Set when the engine starts limited: how to switch it to the full one.
    let mut full_index = None;
    #[cfg(feature = "measure")]
    let synthetic = synthetic_source(&args);
    #[cfg(not(feature = "measure"))]
    let synthetic = None;
    let (source, title): (Arc<dyn ResultsSource>, String) = match synthetic {
        Some(shown) => shown,
        None => {
            // FSEvents and the store key want the absolute, resolved root.
            let root = std::fs::canonicalize(&args.root)
                .map_err(|e| AppError::Engine(args.root.clone(), e))?;
            // Empty when access is granted. In limited mode both the scanner
            // and the change stream skip these folders.
            let excluded = limited_exclusions(launch_output.limited_mode);
            let scan = scan_options(&excluded, &root)?;
            // Each access scope has its own store, so the first launch with
            // access granted after a limited one scans fresh instead of
            // replaying the limited store's snapshot and cursor.
            let scope = StoreScope::for_access(launch_output.access);
            debug_assert_eq!(launch_output.limited_mode, scope == StoreScope::Limited);
            let engine = open_engine(&root, scan, excluded, scope, data_folder.as_deref())
                .map_err(|e| AppError::Engine(root.clone(), e))?;
            let title = window_title(BuildInfo::CURRENT, Showing::Root(&root), measuring);
            let engine: Arc<dyn ResultsSource> =
                Arc::new(EngineSource::new(Arc::new(engine), &root));
            // Only a resident app sees access granted later; measurement
            // runs keep the bare engine.
            if launch_output.limited_mode && !measuring {
                let switchable = Arc::new(SwitchableSource::new(engine));
                full_index = Some(FullIndexSwitch {
                    source: Arc::clone(&switchable),
                    open: Box::new(move || open_full_index(&root, data_folder.as_deref())),
                });
                (switchable, title)
            } else {
                (engine, title)
            }
        }
    };

    window::run(WindowConfig {
        source,
        title,
        your_files_first: settings.your_files_first,
        #[cfg(feature = "measure")]
        autoscroll: args.autoscroll,
        #[cfg(feature = "measure")]
        launch_probe: args.launch_probe,
        #[cfg(feature = "measure")]
        gate_probe: args.gate_probe.clone(),
        #[cfg(feature = "measure")]
        summon_probe: args.summon_probe.clone(),
        #[cfg(feature = "measure")]
        sort_probe: args.sort_probe.clone(),
        #[cfg(feature = "measure")]
        window_start: args.window.clone(),
        process_start,
        // Measurement runs stay plain window apps: no status item, hotkey
        // or onboarding timers.
        resident: (!measuring).then_some(ResidentConfig {
            settings,
            settings_path,
            onboarding,
            launch_output,
            // A run with its own data folder must not take the daily app's
            // shortcut.
            hotkey: !args.has_data_dir(),
            full_index,
        }),
    })
    .map_err(AppError::Window)
}

/// The synthetic rows `--synthetic` asks for, with their window title, or
/// `None` for an engine over the root.
#[cfg(feature = "measure")]
fn synthetic_source(args: &AppArgs) -> Option<(Arc<dyn ResultsSource>, String)> {
    let rows = args.synthetic?;
    let source = crate::source::SyntheticSource::new(rows, SYNTHETIC_SEED)
        .with_page_cost(Duration::from_millis(args.page_cost_ms.unwrap_or(0)));
    let title = window_title(
        BuildInfo::CURRENT,
        Showing::Synthetic(rows),
        args.measuring(),
    );
    Some((Arc::new(source), title))
}

/// Opens the full-access index over `root` after a limited launch, once
/// Full Disk Access is granted: an engine with no exclusions over the
/// full-scope store, which it loads when there is one and otherwise fills
/// with a first scan. Runs on the switch thread, beside the limited engine
/// (see [`SwitchableSource::switch_when_ready`]).
fn open_full_index(root: &Path, data: Option<&Path>) -> Result<Arc<dyn ResultsSource>, String> {
    let excluded = limited_exclusions(false);
    let scan = scan_options(&excluded, root).map_err(|e| e.to_string())?;
    let engine = open_engine(root, scan, excluded, StoreScope::Full, data)
        .map_err(|e| AppError::Engine(root.to_path_buf(), e).to_string())?;
    Ok(Arc::new(EngineSource::new(Arc::new(engine), root)))
}

/// The folders limited (non-FDA) mode keeps away from: the
/// [`consent_prompt_paths`] under [`user_home`] (`$HOME`, else the passwd
/// entry) and, when it differs, under the resolved home (as the root and
/// FSEvents paths are canonical). Both lists include the Data volume
/// spellings. Empty when `limited` is false, that is when Full Disk Access
/// is granted.
fn limited_exclusions(limited: bool) -> Vec<PathBuf> {
    if !limited {
        return Vec::new();
    }
    let home = user_home().unwrap_or_else(|| PathBuf::from("/var/empty"));
    let resolved = std::fs::canonicalize(&home).unwrap_or_else(|_| home.clone());
    let mut paths = consent_prompt_paths(&home);
    if resolved != home {
        for path in consent_prompt_paths(&resolved) {
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    paths
}

/// The scan options for `root`: the `excluded` folders (in limited mode,
/// [`limited_exclusions`]: those that would raise a TCC consent prompt) are
/// skipped. No exclusions when `excluded` is empty.
///
/// # Errors
///
/// [`AppError::NeedsFullDiskAccess`] when `root` itself is inside one of
/// those folders, since opening it would raise the prompt.
fn scan_options(excluded: &[PathBuf], root: &Path) -> Result<ScanOptions, AppError> {
    let mut opts = ScanOptions::default();
    if excluded.is_empty() {
        return Ok(opts);
    }
    if let Some(blocked) = excluded.iter().find(|p| root.starts_with(p)) {
        return Err(AppError::NeedsFullDiskAccess(blocked.clone()));
    }
    let config = ExclusionConfig {
        excluded_folders: excluded.to_vec(),
        ..ExclusionConfig::default()
    };
    opts.exclusions = Exclusions::new(config).map_err(|e| {
        AppError::Engine(
            root.to_path_buf(),
            std::io::Error::other(format!("limited-mode exclusions: {e}")),
        )
    })?;
    Ok(opts)
}

/// Opens an engine over `root` with the macOS scanner, FSEvents source and
/// `lstat` path stat, loading the root's store for `scope` in the data
/// folder `data` when there is one (and falling back to a first scan when
/// there is not). A store built in the other scope is never opened: see
/// [`store_path_in`]. Without a data folder (no home folder) no store is
/// kept.
///
/// The engine's priority folders are your folders under the resolved home
/// folder ([`your_folders`]), whatever the "Your files first" setting, so
/// turning it on needs no rebuild.
///
/// The FSEvents source is wrapped in an [`ExcludingChangeSource`] over
/// `excluded`, so in limited mode no change event (live or replayed from the
/// saved cursor) at or below a consent-prompt folder reaches the engine's
/// writer, which would stat it. With access granted `excluded` is empty and
/// the wrapper passes everything.
///
/// External volumes (run 2.4, [`external_volumes_for`]): with access
/// granted and `/Volumes` below the root, the scanner enters the external
/// volumes, a [`VolumesSource`] watches each one and reports mounts and
/// unmounts, and the engine keeps an unmounted volume's entries offline.
fn open_engine(
    root: &Path,
    mut scan: ScanOptions,
    excluded: Vec<PathBuf>,
    scope: StoreScope,
    data: Option<&Path>,
) -> std::io::Result<Engine> {
    let external = external_volumes_for(root, &excluded);
    scan.external_volumes.clone_from(&external);
    let mut config = EngineConfig::new(root);
    if let Some(external) = &external {
        config = config.with_volumes_folder(&external.folder);
    }
    if let Some(home) = user_home() {
        // The index holds resolved paths, as the root is canonical.
        let resolved = std::fs::canonicalize(&home).unwrap_or_else(|_| home.clone());
        let yours = your_folders(&resolved);
        config = config.with_priority_folders(yours.include, yours.exclude);
    }
    if let Some(data) = data {
        let store = store_path_in(data, root, scope);
        // The data folder and its stores folder are the owner's only: the
        // index holds the names of files in protected folders. They are
        // created or tightened here; the engine's saves only create a
        // missing folder. A missing store folder only means no
        // checkpoints; the engine still runs from a first scan.
        let _ = make_store_folders_private(&store);
        config = config.with_store_path(store);
    }
    let mut stream = StreamConfig::new(root.to_path_buf());
    stream.path_filter = Some(root.to_path_buf());
    let fsevents = FsEventsSource::new(stream);
    match external {
        Some(external) => Engine::open(
            config,
            ExcludingChangeSource::new(VolumesSource::new(fsevents, root, external), excluded),
            MacPathStat::new(),
            MacScanner::new(scan),
        ),
        None => Engine::open(
            config,
            ExcludingChangeSource::new(fsevents, excluded),
            MacPathStat::new(),
            MacScanner::new(scan),
        ),
    }
}

/// The external volumes an engine over `root` indexes (run 2.4): the
/// browsable APFS, HFS+, FAT and exFAT volumes in `/Volumes`
/// ([`ExternalVolumes::system`]), when `/Volumes` lies strictly below the
/// root and nothing is `excluded`. In limited mode `/Volumes` is one of the
/// consent-prompt folders (reading a removable volume asks for consent),
/// so there are none.
fn external_volumes_for(root: &Path, excluded: &[PathBuf]) -> Option<ExternalVolumes> {
    let external = ExternalVolumes::system();
    (excluded.is_empty() && external.folder != root && external.folder.starts_with(root))
        .then_some(external)
}

/// Arms the watchdog of the measurement `args` asks for, if any. A
/// `--window` run is not a measurement and gets none: it runs until it is
/// quit or gets SIGTERM.
#[cfg(feature = "measure")]
fn arm_measure_watchdog(args: &AppArgs) {
    if args.gate_probe.is_some() {
        // The gate probe waits up to a minute for the first result and up
        // to `STEP_TIMEOUT` per scripted step before it gives up on them.
        arm_watchdog(Duration::from_secs(900));
    } else if args.summon_probe.is_some() || args.sort_probe.is_some() {
        // Up to `LIST_TIMEOUT` (300 s) for a steady list, then at most 50
        // cycles of `HIDE_TIMEOUT` and `READY_TIMEOUT` (8 s), or 48 clicks
        // of `STEP_TIMEOUT` (10 s).
        arm_watchdog(Duration::from_secs(1200));
    } else if args.measuring() {
        arm_watchdog(Duration::from_secs(120));
    }
}

/// Exits with code 1 if a measurement has not reported within `limit`
/// (for example without a window server), so it never hangs.
#[cfg(feature = "measure")]
fn arm_watchdog(limit: Duration) {
    std::thread::spawn(move || {
        std::thread::sleep(limit);
        eprintln!(
            "{APP_NAME}: no report after {} s (is a window server available?)",
            limit.as_secs()
        );
        std::process::exit(1);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limited_mode_refuses_an_alias_root_inside_a_protected_folder() {
        let excluded = consent_prompt_paths(Path::new("/Users/tester"));
        for root in [
            "/System/Volumes/Data/Users/tester/Documents",
            "/System/Volumes/Data/Users/tester/Desktop/Project",
            "/System/Volumes/Data/Volumes/USB",
        ] {
            assert!(
                matches!(
                    scan_options(&excluded, Path::new(root)),
                    Err(AppError::NeedsFullDiskAccess(_))
                ),
                "{root} must need Full Disk Access"
            );
        }
        for root in [
            "/System/Volumes/Data/Users/tester",
            "/System/Volumes/Data/Users/tester/Music",
        ] {
            assert!(scan_options(&excluded, Path::new(root)).is_ok(), "{root}");
        }
    }

    #[test]
    fn granted_access_has_no_exclusions() {
        assert!(limited_exclusions(false).is_empty());
    }

    #[test]
    fn external_volumes_are_indexed_from_the_whole_disk_with_access_only() {
        let whole = external_volumes_for(Path::new("/"), &[]);
        assert_eq!(whole, Some(ExternalVolumes::system()));
        let limited = consent_prompt_paths(Path::new("/Users/tester"));
        assert_eq!(external_volumes_for(Path::new("/"), &limited), None);
        for root in ["/Users/tester", "/Volumes", "/Volumes/Drive", "/tmp"] {
            assert_eq!(external_volumes_for(Path::new(root), &[]), None, "{root}");
        }
    }
}
