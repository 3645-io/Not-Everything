//! The app settings file.
//!
//! A line-based `key=value` UTF-8 text file with the keys `hotkey` (a
//! [`HotkeySpec`] string such as `option+space`), `launch_at_login` and
//! `fda_onboarding_dismissed` (`true` / `false`), `appearance`
//! ([`AppearanceMode`]: `system`, `light` or `dark`), `app_icon`
//! ([`AppIcon`], such as `match-row`), `your_files_first` (`true` /
//! `false`, default `true`), `sort` (the results table's [`SortState`], such
//! as `size:descending`), the search chips `match_case`, `match_path` and
//! `regex` (`true` / `false`, default `false`; [`crate::chips`]) and
//! `index_size` (the entry count of the last complete index, for
//! "Indexing… N of about M"; absent until there is one). Reading never fails
//! on content: unknown keys, lines without `=` and `#` comment lines are
//! ignored, whitespace around keys and values is trimmed, and a bad value
//! falls back to that field's default, so files written by older versions
//! load with the newer keys at their defaults. Saving writes a temp file in
//! the same directory and renames it over the old file, so a crash never
//! leaves a half-written settings file. The file is its owner's only, as is
//! its folder ([`crate::data_folder`]).
//!
//! The file lives at `~/Library/Application Support/Not-Everything/settings`
//! ([`settings_path`]), or in the `Not-Everything Preview` folder for a
//! Preview build. The defaults follow the build channel
//! ([`Settings::defaults_for`]): only the hotkey differs, so the release and
//! Preview apps can run side by side.

#![forbid(unsafe_code)]

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::app_icon::AppIcon;
use crate::appearance::AppearanceMode;
use crate::chips::{Chip, QueryOptions};
use crate::columns::SortState;
use crate::data_folder::{create_private_file, make_dir_private};
use crate::hotkey::HotkeySpec;
use fx_channel::Channel;

/// Name of the app's folder under `~/Library/Application Support`: the
/// app's name, shared with the index stores.
pub const APP_DIR_NAME: &str = crate::app_args::APP_FOLDER;

/// File name of the settings file.
pub const SETTINGS_FILE_NAME: &str = "settings";

const KEY_HOTKEY: &str = "hotkey";
const KEY_LAUNCH_AT_LOGIN: &str = "launch_at_login";
const KEY_FDA_ONBOARDING_DISMISSED: &str = "fda_onboarding_dismissed";
const KEY_APPEARANCE: &str = "appearance";
const KEY_APP_ICON: &str = "app_icon";
const KEY_YOUR_FILES_FIRST: &str = "your_files_first";
const KEY_SORT: &str = "sort";
const KEY_INDEX_SIZE: &str = "index_size";

/// User settings. [`Default`] is this build channel's defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    /// The global summon hotkey; defaults to the channel's
    /// ([`HotkeySpec::default_for`]): option+space, or shift+option+space
    /// for the Preview app.
    pub hotkey: HotkeySpec,
    /// Whether the app registers itself as a login item; off by default.
    pub launch_at_login: bool,
    /// Whether the user dismissed the Full Disk Access banner.
    pub fda_onboarding_dismissed: bool,
    /// The app's light or dark appearance; defaults to following the
    /// system.
    pub appearance: AppearanceMode,
    /// The Dock icon; defaults to the icon inside the `.app` (Match Row).
    pub app_icon: AppIcon,
    /// Whether results in your folders come first under every sort
    /// ([`crate::your_files`]); on by default.
    pub your_files_first: bool,
    /// The results sort the window starts with, saved on every header
    /// click; defaults to Name ascending.
    pub sort: SortState,
    /// The search window's Aa, Path and .* chips, saved on every click; all
    /// off by default.
    pub search_options: QueryOptions,
    /// How many entries the last complete index held, saved when a scan
    /// finishes; `None` until one has.
    pub index_size: Option<usize>,
}

impl Default for Settings {
    /// [`Settings::defaults_for`] this build's channel.
    fn default() -> Self {
        Settings::defaults_for(Channel::CURRENT)
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

impl Settings {
    /// The defaults of the app built for `channel`: its default hotkey, not
    /// launching at login, the onboarding not dismissed, the system
    /// appearance, the Match Row icon, your files first, Name ascending,
    /// every search chip off and no index size yet.
    pub fn defaults_for(channel: Channel) -> Settings {
        Settings {
            hotkey: HotkeySpec::default_for(channel),
            launch_at_login: false,
            fda_onboarding_dismissed: false,
            appearance: AppearanceMode::default(),
            app_icon: AppIcon::default(),
            your_files_first: true,
            sort: SortState::default(),
            search_options: QueryOptions::default(),
            index_size: None,
        }
    }

    /// Parses the settings text. Never fails: see the module documentation
    /// for how unknown keys and bad values are handled.
    pub fn parse(text: &str) -> Settings {
        let defaults = Settings::default();
        let mut s = defaults;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                KEY_HOTKEY => {
                    s.hotkey = HotkeySpec::parse(value).unwrap_or(defaults.hotkey);
                }
                KEY_LAUNCH_AT_LOGIN => {
                    s.launch_at_login = parse_bool(value).unwrap_or(defaults.launch_at_login);
                }
                KEY_FDA_ONBOARDING_DISMISSED => {
                    s.fda_onboarding_dismissed =
                        parse_bool(value).unwrap_or(defaults.fda_onboarding_dismissed);
                }
                KEY_APPEARANCE => {
                    s.appearance = AppearanceMode::parse(value).unwrap_or(defaults.appearance);
                }
                KEY_APP_ICON => {
                    s.app_icon = AppIcon::parse(value).unwrap_or(defaults.app_icon);
                }
                KEY_YOUR_FILES_FIRST => {
                    s.your_files_first = parse_bool(value).unwrap_or(defaults.your_files_first);
                }
                KEY_SORT => {
                    s.sort = SortState::parse(value).unwrap_or(defaults.sort);
                }
                KEY_INDEX_SIZE => {
                    s.index_size = value.parse().ok().filter(|&n| n > 0);
                }
                key => {
                    if let Some(chip) = Chip::ALL.into_iter().find(|c| c.settings_key() == key) {
                        let on = parse_bool(value).unwrap_or(chip.is_on(defaults.search_options));
                        if chip.is_on(s.search_options) != on {
                            s.search_options = chip.toggle(s.search_options);
                        }
                    }
                }
            }
        }
        s
    }

    /// The settings as file text.
    pub fn to_text(&self) -> String {
        let mut text = format!(
            "{KEY_HOTKEY}={}\n{KEY_LAUNCH_AT_LOGIN}={}\n{KEY_FDA_ONBOARDING_DISMISSED}={}\n\
             {KEY_APPEARANCE}={}\n{KEY_APP_ICON}={}\n{KEY_YOUR_FILES_FIRST}={}\n\
             {KEY_SORT}={}\n",
            self.hotkey,
            self.launch_at_login,
            self.fda_onboarding_dismissed,
            self.appearance,
            self.app_icon,
            self.your_files_first,
            self.sort
        );
        for chip in Chip::ALL {
            text.push_str(&format!(
                "{}={}\n",
                chip.settings_key(),
                chip.is_on(self.search_options)
            ));
        }
        if let Some(size) = self.index_size {
            text.push_str(&format!("{KEY_INDEX_SIZE}={size}\n"));
        }
        text
    }

    /// Loads the settings file at `path`, or the defaults when it does not
    /// exist. Invalid UTF-8 is read lossily.
    pub fn load(path: &Path) -> io::Result<Settings> {
        match fs::read(path) {
            Ok(bytes) => Ok(Settings::parse(&String::from_utf8_lossy(&bytes))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(e),
        }
    }

    /// Saves the settings to `path` atomically: creates missing parent
    /// directories, writes a temp file in the same directory, syncs it and
    /// renames it over `path`. The file gets mode 0600 and its folder 0700
    /// ([`crate::data_folder`]), also when they existed with wider modes.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let dir = match path.parent() {
            Some(d) if !d.as_os_str().is_empty() => {
                make_dir_private(d)?;
                d
            }
            // A bare file name: the current directory is not the app's to
            // change.
            _ => Path::new("."),
        };
        let file_name = path
            .file_name()
            .ok_or_else(|| io::Error::other("settings path has no file name"))?;
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let tmp = dir.join(format!(
            ".{}.tmp-{}-{}",
            file_name.to_string_lossy(),
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| {
            let mut f = create_private_file(&tmp)?;
            f.write_all(self.to_text().as_bytes())?;
            f.sync_all()?;
            drop(f);
            fs::rename(&tmp, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }
}

/// The settings file path for the home directory `home`:
/// `<home>/Library/Application Support/Not-Everything/settings`.
pub fn settings_path(home: &Path) -> PathBuf {
    home.join("Library")
        .join("Application Support")
        .join(APP_DIR_NAME)
        .join(SETTINGS_FILE_NAME)
}

/// The settings file path for the current user, from `$HOME`; `None` when
/// `HOME` is unset or empty.
pub fn user_settings_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    Some(settings_path(Path::new(&home)))
}
