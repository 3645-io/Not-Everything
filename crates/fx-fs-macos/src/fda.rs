//! Full Disk Access (FDA) probe and the non-FDA exclusion set.
//!
//! # The probe
//!
//! [`FdaProbe`] (macOS only) implements [`fx_fs::PermissionProbe`]. It
//! answers "does this process have Full Disk Access?" without ever showing a
//! consent prompt: it tries a read-only, no-follow `open(2)` of files that
//! only an FDA-holding process may open, by default the user and the system
//! TCC databases:
//!
//! 1. `~/Library/Application Support/com.apple.TCC/TCC.db`
//! 2. `/Library/Application Support/com.apple.TCC/TCC.db`
//!
//! Each file is opened with `O_RDONLY | O_NOFOLLOW | O_NONBLOCK |
//! O_CLOEXEC` and closed at once; its contents are never read and nothing
//! is ever written. The answer is
//!
//! * [`Granted`](fx_fs::AccessState::Granted) if any open succeeds (the
//!   first success wins);
//! * [`Denied`](fx_fs::AccessState::Denied) if none succeeds and at least
//!   one fails with `EPERM` or `EACCES`;
//! * [`Unknown`](fx_fs::AccessState::Unknown) otherwise: no file exists,
//!   or the errors are something else (`ENOTDIR`, `ELOOP` for a symlink,
//!   ...).
//!
//! The probe never touches a consent-prompt location (Desktop, Documents,
//! Downloads, iCloud Drive, removable or network volumes): opening anything
//! there without access would make macOS show a TCC prompt, which is the
//! very thing the probe exists to avoid. The TCC folders are FDA-class
//! locations: without access they fail with `EPERM` silently.
//!
//! # Opening System Settings
//!
//! [`PermissionProbe::open_settings`](fx_fs::PermissionProbe::open_settings)
//! hands [`FDA_SETTINGS_URL`], Apple's documented deep link to Privacy &
//! Security › Full Disk Access, to `/usr/bin/open` (see
//! [`open_settings_command`]). The constant and the command builder can be
//! tested without launching anything.
//!
//! # Non-FDA mode
//!
//! When access is not `Granted`, a scan must skip the folders that make
//! macOS ask for consent. [`consent_prompt_paths`] lists them for a given
//! home folder and [`consent_prompt_exclusions`] wraps them in the
//! [`fx_fs::ExclusionConfig`] that the scanner applies through
//! `ScanOptions::exclusions` (after `fx_fs::Exclusions::new`). The list
//! holds both the `/` and the `/System/Volumes/Data` spelling of every
//! folder, so an alias root is pruned too. These parts are pure and compile
//! on every host.
//!
//! [`user_home`] is the home folder to build them from: `$HOME` when it is
//! set and absolute, else the password database's entry.

use std::path::{Path, PathBuf};

use fx_fs::ExclusionConfig;

use crate::firmlinks::{FirmlinkMap, is_data_volume_alias};

/// Apple's documented deep link to System Settings › Privacy & Security ›
/// Full Disk Access.
pub const FDA_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

/// The program that opens [`FDA_SETTINGS_URL`].
pub const OPEN_PROGRAM: &str = "/usr/bin/open";

/// The TCC folder, relative to a home folder or to `/`.
const TCC_DIR: &str = "Library/Application Support/com.apple.TCC";

/// The system-wide TCC database (read by the macOS probe only).
#[cfg(target_os = "macos")]
const SYSTEM_TCC_DB: &str = "/Library/Application Support/com.apple.TCC/TCC.db";

/// Home-relative folders whose listing can show a TCC consent prompt (or
/// that are FDA-class and pointless to try without access).
const HOME_CONSENT_FOLDERS: &[&str] = &[
    "Desktop",
    "Documents",
    "Downloads",
    // iCloud Drive.
    "Library/Mobile Documents",
    // File Provider cloud storage (Dropbox, OneDrive, Google Drive, ...).
    "Library/CloudStorage",
    "Pictures/Photos Library.photoslibrary",
    "Library/Mail",
    "Library/Messages",
    "Library/Safari",
    // Other apps' sandbox data: macOS 14+ may ask "would like to access
    // data from other apps" when another app's container is read.
    "Library/Containers",
    "Library/Group Containers",
    TCC_DIR,
];

/// The folders a scan must skip when Full Disk Access is not granted, so
/// that the non-FDA mode triggers no TCC consent prompt.
///
/// Under `home`: Desktop, Documents, Downloads, iCloud Drive
/// (`Library/Mobile Documents`), `Library/CloudStorage`, the Photos library,
/// Mail, Messages, Safari, the app containers and the user TCC folder. Plus
/// `/Volumes` (removable and network volumes) and the system TCC folder.
/// Every path is absolute when `home` is; neither `home` itself nor
/// `~/Library` is listed, so the rest of the home folder stays indexable.
///
/// Each folder is also reachable through the Data volume
/// (`/System/Volumes/Data/Users/...`, `/System/Volumes/Data/Volumes`, ...)
/// via the standard firmlinks, so the other spelling of every path is
/// listed too, after all the given spellings: a root or a change event
/// spelled through the Data volume is kept away from the protected folders
/// just like the `/` spelling. (A `home` given as a Data volume path gets
/// its `/` spelling added the same way.) The mapping uses the built-in
/// firmlink table ([`FirmlinkMap::fallback`]), so this stays pure.
#[must_use]
pub fn consent_prompt_paths(home: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = HOME_CONSENT_FOLDERS
        .iter()
        .map(|rel| home.join(rel))
        .collect();
    paths.push(PathBuf::from("/Volumes"));
    paths.push(Path::new("/").join(TCC_DIR));

    let map = FirmlinkMap::fallback();
    let aliases: Vec<PathBuf> = paths
        .iter()
        .filter_map(|p| {
            if is_data_volume_alias(p) {
                let root = map.to_root(p);
                (root != *p).then_some(root)
            } else {
                map.to_data(p)
            }
        })
        .collect();
    for alias in aliases {
        if !paths.contains(&alias) {
            paths.push(alias);
        }
    }
    paths
}

/// The current user's home folder: `$HOME` when it is set and absolute,
/// else the password database's home folder for the real user (macOS only;
/// `None` elsewhere when `$HOME` is unusable).
#[must_use]
pub fn user_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
        && home.is_absolute()
    {
        return Some(home);
    }
    passwd_home()
}

#[cfg(target_os = "macos")]
fn passwd_home() -> Option<PathBuf> {
    use std::ffi::{CStr, OsStr};
    use std::os::unix::ffi::OsStrExt;

    // SAFETY: `libc::passwd` is plain old data; all-zero is valid.
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let mut buf = vec![0 as libc::c_char; 4096];
    // SAFETY: every pointer is valid for the duration of the call and
    // `buf.len()` is the real buffer size.
    let rc = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &raw mut pwd,
            buf.as_mut_ptr(),
            buf.len(),
            &raw mut result,
        )
    };
    if rc != 0 || result.is_null() || pwd.pw_dir.is_null() {
        return None;
    }
    // SAFETY: on success `pw_dir` points to a NUL-terminated string inside
    // `buf`, which is still alive.
    let dir = unsafe { CStr::from_ptr(pwd.pw_dir) };
    let home = PathBuf::from(OsStr::from_bytes(dir.to_bytes()));
    home.is_absolute().then_some(home)
}

#[cfg(not(target_os = "macos"))]
fn passwd_home() -> Option<PathBuf> {
    None
}

/// [`consent_prompt_paths`] as an [`ExclusionConfig`]: only
/// `excluded_folders` is set, everything else is the default. Compile it
/// with `fx_fs::Exclusions::new` and pass it as `ScanOptions::exclusions`.
#[must_use]
pub fn consent_prompt_exclusions(home: &Path) -> ExclusionConfig {
    ExclusionConfig {
        excluded_folders: consent_prompt_paths(home),
        ..ExclusionConfig::default()
    }
}

/// The `/usr/bin/open` command that shows the Full Disk Access pane. Not
/// spawned; [`PermissionProbe::open_settings`](fx_fs::PermissionProbe)
/// runs it.
#[must_use]
pub fn open_settings_command() -> std::process::Command {
    let mut cmd = std::process::Command::new(OPEN_PROGRAM);
    cmd.arg(FDA_SETTINGS_URL);
    cmd
}

#[cfg(target_os = "macos")]
pub use probe::FdaProbe;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_command_opens_the_deep_link_without_running() {
        let cmd = open_settings_command();
        assert_eq!(cmd.get_program(), OPEN_PROGRAM);
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, [FDA_SETTINGS_URL]);
    }

    #[test]
    fn consent_paths_list_both_spellings_of_each_folder() {
        let home = Path::new("/Users/tester");
        let paths = consent_prompt_paths(home);
        for p in [
            "/Users/tester/Documents",
            "/System/Volumes/Data/Users/tester/Documents",
            "/Volumes",
            "/System/Volumes/Data/Volumes",
            "/Library/Application Support/com.apple.TCC",
            "/System/Volumes/Data/Library/Application Support/com.apple.TCC",
        ] {
            assert!(paths.contains(&PathBuf::from(p)), "missing {p}: {paths:?}");
        }
        // The `/` spellings come first, then their aliases, no duplicates.
        assert_eq!(paths[0], home.join("Desktop"));
        let mut dedup = paths.clone();
        dedup.sort();
        dedup.dedup();
        assert_eq!(dedup.len(), paths.len());
    }

    #[test]
    fn alias_home_gets_the_root_spelling_too() {
        let home = Path::new("/System/Volumes/Data/Users/tester");
        let paths = consent_prompt_paths(home);
        assert!(paths.contains(&home.join("Desktop")));
        assert!(paths.contains(&PathBuf::from("/Users/tester/Desktop")));
    }

    #[test]
    fn home_outside_a_firmlink_gets_no_alias() {
        let paths = consent_prompt_paths(Path::new("/var/empty"));
        assert!(paths.contains(&PathBuf::from("/var/empty/Desktop")));
        assert!(
            !paths
                .iter()
                .any(|p| p.starts_with("/System/Volumes/Data/var"))
        );
    }
}

#[cfg(target_os = "macos")]
mod probe {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};

    use fx_fs::{AccessState, PermissionProbe};

    use super::{SYSTEM_TCC_DB, TCC_DIR, open_settings_command, user_home};

    /// The outcome of probing one file.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Probe {
        Opened,
        Denied,
        Other,
    }

    /// Full Disk Access probe. See the [module docs](super).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct FdaProbe {
        paths: Vec<PathBuf>,
    }

    impl FdaProbe {
        /// Probes the user TCC database, then the system one.
        #[must_use]
        pub fn new() -> FdaProbe {
            let mut paths = Vec::with_capacity(2);
            if let Some(home) = user_home() {
                paths.push(home.join(TCC_DIR).join("TCC.db"));
            }
            paths.push(PathBuf::from(SYSTEM_TCC_DB));
            FdaProbe { paths }
        }

        /// Probes `paths` in order instead of the TCC databases (for tests).
        /// The paths must not lie in consent-prompt locations.
        #[must_use]
        pub fn with_paths(paths: Vec<PathBuf>) -> FdaProbe {
            FdaProbe { paths }
        }

        /// The files this probe opens, in order.
        #[must_use]
        pub fn paths(&self) -> &[PathBuf] {
            &self.paths
        }
    }

    impl Default for FdaProbe {
        fn default() -> FdaProbe {
            FdaProbe::new()
        }
    }

    impl PermissionProbe for FdaProbe {
        fn full_access(&self) -> AccessState {
            let mut denied = false;
            for path in &self.paths {
                match probe_file(path) {
                    Probe::Opened => return AccessState::Granted,
                    Probe::Denied => denied = true,
                    Probe::Other => {}
                }
            }
            if denied {
                AccessState::Denied
            } else {
                AccessState::Unknown
            }
        }

        fn open_settings(&self) {
            // Wait on a helper thread so the caller (the UI thread) never
            // blocks and the child is reaped.
            let spawned = std::thread::Builder::new()
                .name("fx-open-fda-settings".into())
                .spawn(|| {
                    let _ = open_settings_command().status();
                });
            drop(spawned);
        }
    }

    /// Opens `path` read-only without following a final symlink, then
    /// closes it. Never reads or writes the file.
    fn probe_file(path: &Path) -> Probe {
        let Ok(c) = CString::new(path.as_os_str().as_bytes()) else {
            return Probe::Other;
        };
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;
        // SAFETY: `c` is a valid NUL-terminated string; no mode is needed
        // without `O_CREAT`.
        let fd = unsafe { libc::open(c.as_ptr(), flags) };
        if fd >= 0 {
            // SAFETY: `fd` was just returned by `open` and is owned here.
            unsafe { libc::close(fd) };
            return Probe::Opened;
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EPERM | libc::EACCES) => Probe::Denied,
            _ => Probe::Other,
        }
    }
}
