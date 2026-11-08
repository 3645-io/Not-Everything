//! Tests for `fx_ui_macos::settings`: the app settings file.
//!
//! Contract exercised here:
//!
//! - `Settings { hotkey: HotkeySpec, launch_at_login: bool,
//!   fda_onboarding_dismissed: bool, appearance: AppearanceMode, app_icon:
//!   AppIcon, your_files_first: bool, sort: SortState }` with public
//!   fields; `Default` is the default summon hotkey (option+space), not
//!   launching at login, the onboarding not dismissed, the system
//!   appearance, the Match Row icon, "Your files first" on and Name
//!   ascending. The default hotkey is the build channel's:
//!   `Settings::defaults_for(channel)` gives shift+option+space for the
//!   Preview app (release candidates and betas), and `Default` is this
//!   build's channel's.
//! - A line-based `key=value` UTF-8 text format with the keys `hotkey`,
//!   `launch_at_login`, `fda_onboarding_dismissed`, `appearance` (`system`,
//!   `light` or `dark`), `app_icon` (an `AppIcon` value such as
//!   `lit-slot`), `your_files_first` and `sort` (the
//!   results column and direction, such as `size:descending`); booleans are
//!   `true` / `false`. `Settings::parse(&str) -> Settings` never fails:
//!   unknown keys and lines without `=` are ignored, whitespace around keys and
//!   values is trimmed, and a bad value falls back to that field's default.
//!   `Settings::to_text(&self) -> String` writes the format back.
//! - `Settings::load(&Path) -> io::Result<Settings>` returns the defaults
//!   when the file does not exist. `Settings::save(&self, &Path) ->
//!   io::Result<()>` creates missing parent directories and replaces the file
//!   atomically (temp file in the same directory, then rename), leaving no
//!   temp file behind. The file is the owner's only (mode 0600) and so is its
//!   folder (0700), also when they were open before the save.
//! - `settings::settings_path(home)` is
//!   `<home>/Library/Application Support/Not-Everything/settings` in a
//!   release build, and in the `Not-Everything Preview` folder in a Preview
//!   build.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fx_channel::Channel;
use fx_query::QueryOptions;
use fx_sort::Direction;
use fx_ui_macos::app_icon::AppIcon;
use fx_ui_macos::appearance::AppearanceMode;
use fx_ui_macos::columns::{Column, SortState};
use fx_ui_macos::hotkey::HotkeySpec;
use fx_ui_macos::settings::{Settings, settings_path};

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> TempDir {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "fx-ui-macos-settings-{tag}-{}-{}-{nanos}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        TempDir { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn hk(s: &str) -> HotkeySpec {
    HotkeySpec::parse(s).unwrap()
}

fn custom() -> Settings {
    Settings {
        hotkey: hk("cmd+shift+space"),
        launch_at_login: true,
        fda_onboarding_dismissed: true,
        appearance: AppearanceMode::Dark,
        app_icon: AppIcon::LitSlot,
        your_files_first: false,
        sort: SortState {
            column: Column::Size,
            direction: Direction::Descending,
        },
        search_options: QueryOptions {
            match_case: true,
            match_path: false,
            regex: true,
        },
        index_size: Some(2_014_377),
    }
}

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn defaults() {
    let s = Settings::default();
    assert_eq!(s, Settings::defaults_for(Channel::CURRENT));
    assert_eq!(s.hotkey, HotkeySpec::default());
    assert!(!s.launch_at_login);
    assert!(!s.fda_onboarding_dismissed);
    assert_eq!(s.appearance, AppearanceMode::System);
    assert_eq!(s.app_icon, AppIcon::MatchRow);
    assert_eq!(s.sort, SortState::default());
    assert_eq!(s.sort.column, Column::Name);
    assert_eq!(s.sort.direction, Direction::Ascending);
    assert!(s.your_files_first, "Your files first is on by default");
    assert_eq!(s.search_options, QueryOptions::default(), "every chip off");
    assert_eq!(s.index_size, None);
}

/// The release app and the Preview app (release candidates and
/// betas) differ only in the default hotkey, so both can run side by side.
/// Neither starts at login unless the user turns it on.
#[test]
fn defaults_per_channel() {
    let release = Settings::defaults_for(Channel::Release);
    assert_eq!(release.hotkey, hk("option+space"));
    for channel in [Channel::Rc, Channel::Beta] {
        let preview = Settings::defaults_for(channel);
        assert_eq!(preview.hotkey, hk("shift+option+space"), "{channel:?}");
        assert_eq!(
            Settings {
                hotkey: release.hotkey,
                ..preview
            },
            release,
            "{channel:?}"
        );
    }
    for channel in Channel::ALL {
        let s = Settings::defaults_for(channel);
        assert!(!s.launch_at_login, "{channel:?}");
        assert!(!s.fda_onboarding_dismissed, "{channel:?}");
        assert!(s.your_files_first, "{channel:?}");
    }
}

#[test]
fn text_round_trip() {
    for s in [Settings::default(), custom()] {
        assert_eq!(Settings::parse(&s.to_text()), s);
    }
}

#[test]
fn text_is_line_based_key_value() {
    let text = custom().to_text();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(lines.contains(&"hotkey=cmd+shift+space"), "{text}");
    assert!(lines.contains(&"launch_at_login=true"), "{text}");
    assert!(lines.contains(&"fda_onboarding_dismissed=true"), "{text}");
    assert!(lines.contains(&"appearance=dark"), "{text}");
    assert!(lines.contains(&"app_icon=lit-slot"), "{text}");
    assert!(lines.contains(&"sort=size:descending"), "{text}");
    assert!(lines.contains(&"your_files_first=false"), "{text}");
    assert!(lines.contains(&"match_case=true"), "{text}");
    assert!(lines.contains(&"match_path=false"), "{text}");
    assert!(lines.contains(&"regex=true"), "{text}");
    assert!(lines.contains(&"index_size=2014377"), "{text}");
    let text = Settings::default().to_text();
    assert!(text.lines().any(|l| l == "your_files_first=true"), "{text}");
    assert!(text.lines().any(|l| l == "regex=false"), "{text}");
    assert!(
        !text.lines().any(|l| l.starts_with("index_size")),
        "no size before a first index: {text}"
    );
}

/// The search window's Aa, Path and .* chips are remembered
/// between launches, one line each, off by default and when unreadable.
#[test]
fn the_search_chips_round_trip_and_fall_back_to_off() {
    for bits in 0..8u8 {
        let options = QueryOptions {
            match_case: bits & 1 != 0,
            match_path: bits & 2 != 0,
            regex: bits & 4 != 0,
        };
        let s = Settings {
            search_options: options,
            ..Settings::default()
        };
        assert_eq!(Settings::parse(&s.to_text()), s, "{options:?}");
    }
    let s = Settings::parse("match_case=true\nmatch_path=yes\n regex = true \n");
    assert_eq!(
        s.search_options,
        QueryOptions {
            match_case: true,
            match_path: false,
            regex: true,
        }
    );
    assert_eq!(
        Settings::parse("hotkey=cmd+k\n").search_options,
        QueryOptions::default(),
        "files written before the chips existed start with every chip off"
    );
}

/// The size of the last complete index, for "Indexing… N of about
/// M" while the next scan runs.
#[test]
fn the_last_index_size_round_trips() {
    for size in [None, Some(1), Some(460_513), Some(2_014_377)] {
        let s = Settings {
            index_size: size,
            ..Settings::default()
        };
        assert_eq!(Settings::parse(&s.to_text()), s, "{size:?}");
    }
    assert_eq!(Settings::parse("index_size=1,000\n").index_size, None);
    assert_eq!(Settings::parse("index_size=-4\n").index_size, None);
    assert_eq!(Settings::parse("index_size=0\n").index_size, None);
    assert_eq!(Settings::parse("index_size= 42 \n").index_size, Some(42));
}

#[test]
fn your_files_first_round_trips_and_falls_back_to_on() {
    for on in [true, false] {
        let s = Settings {
            your_files_first: on,
            ..Settings::default()
        };
        assert_eq!(Settings::parse(&s.to_text()), s);
    }
    assert!(!Settings::parse("your_files_first=false\n").your_files_first);
    assert!(Settings::parse("your_files_first=true\n").your_files_first);
    assert!(
        Settings::parse("your_files_first=maybe\n").your_files_first,
        "a bad value falls back to on"
    );
    assert!(!Settings::parse(" your_files_first = false \n").your_files_first);
}

#[test]
fn every_appearance_and_icon_round_trips() {
    for appearance in AppearanceMode::ALL {
        for app_icon in AppIcon::ALL {
            let s = Settings {
                appearance,
                app_icon,
                ..Settings::default()
            };
            assert_eq!(Settings::parse(&s.to_text()), s);
        }
    }
}

#[test]
fn parses_appearance_and_icon() {
    let s = Settings::parse("appearance=light\napp_icon=front-stack\n");
    assert_eq!(s.appearance, AppearanceMode::Light);
    assert_eq!(s.app_icon, AppIcon::FrontStack);
    assert_eq!(s.hotkey, HotkeySpec::default());
}

#[test]
fn bad_appearance_and_icon_fall_back_to_defaults() {
    let s = Settings::parse("appearance=sepia\napp_icon=rocket\nlaunch_at_login=true\n");
    assert_eq!(s.appearance, AppearanceMode::System);
    assert_eq!(s.app_icon, AppIcon::MatchRow);
    assert!(s.launch_at_login, "good values are kept");
}

#[test]
fn every_sort_column_and_direction_round_trips() {
    for column in Column::ALL.iter().copied() {
        for direction in [Direction::Ascending, Direction::Descending] {
            let s = Settings {
                sort: SortState { column, direction },
                ..Settings::default()
            };
            assert_eq!(Settings::parse(&s.to_text()), s, "{column:?} {direction:?}");
        }
    }
}

#[test]
fn parses_the_sort() {
    let s = Settings::parse("sort=modified:descending\n");
    assert_eq!(s.sort.column, Column::Modified);
    assert_eq!(s.sort.direction, Direction::Descending);
    let s = Settings::parse(" sort = path:ascending \n");
    assert_eq!(s.sort.column, Column::Path);
    assert_eq!(s.sort.direction, Direction::Ascending);
    assert_eq!(s.hotkey, HotkeySpec::default());
}

#[test]
fn bad_sort_values_fall_back_to_name_ascending() {
    for value in [
        "",
        "size",
        "size:",
        ":descending",
        "size:sideways",
        "colour:descending",
        "Size:Descending",
        "size:descending:extra",
        "size descending",
    ] {
        let s = Settings::parse(&format!("sort={value}\nlaunch_at_login=true\n"));
        assert_eq!(s.sort, SortState::default(), "sort={value}");
        assert!(s.launch_at_login, "good values are kept (sort={value})");
    }
}

#[test]
fn files_without_a_sort_line_start_at_name_ascending() {
    // Files written before the sort was remembered.
    let s = Settings::parse(
        "hotkey=cmd+shift+space\nlaunch_at_login=true\nfda_onboarding_dismissed=true\n\
         appearance=dark\napp_icon=lit-slot\n",
    );
    assert_eq!(s.appearance, AppearanceMode::Dark);
    assert_eq!(s.sort, SortState::default());
}

#[test]
fn a_header_click_is_saved_as_the_sort_line() {
    // What the window does on a header click: take the controller's new
    // sort, put it in the settings and save them.
    let dir = TempDir::new("sort");
    let path = dir.path.join("settings");
    let mut settings = custom();
    settings.sort = SortState::default();
    settings.save(&path).unwrap();

    let mut sort = Settings::load(&path).unwrap().sort;
    sort.click(Column::Size);
    sort.click(Column::Size);
    settings.sort = sort;
    settings.save(&path).unwrap();

    let text = fs::read_to_string(&path).unwrap();
    assert!(text.lines().any(|l| l == "sort=size:descending"), "{text}");
    let loaded = Settings::load(&path).unwrap();
    assert_eq!(loaded.sort, sort);
    assert_eq!(loaded, settings, "the other settings are kept");
}

#[test]
fn first_release_files_load_with_the_new_defaults() {
    // A settings file from the first release has only these three lines.
    let s = Settings::parse(
        "hotkey=cmd+shift+space\nlaunch_at_login=true\nfda_onboarding_dismissed=true\n",
    );
    assert_eq!(s.hotkey, hk("cmd+shift+space"));
    assert!(s.launch_at_login);
    assert!(s.fda_onboarding_dismissed);
    assert_eq!(s.appearance, AppearanceMode::System);
    assert_eq!(s.app_icon, AppIcon::MatchRow);
    assert!(
        s.your_files_first,
        "files from before the setting existed get the new default"
    );
}

#[test]
fn parses_known_keys() {
    let s = Settings::parse("hotkey=cmd+k\nlaunch_at_login=true\nfda_onboarding_dismissed=false\n");
    assert_eq!(s.hotkey, hk("cmd+k"));
    assert!(s.launch_at_login);
    assert!(!s.fda_onboarding_dismissed);
}

#[test]
fn missing_keys_use_defaults() {
    let s = Settings::parse("launch_at_login=true\n");
    assert!(s.launch_at_login);
    assert_eq!(s.hotkey, HotkeySpec::default());
    assert!(!s.fda_onboarding_dismissed);
    assert_eq!(Settings::parse(""), Settings::default());
}

#[test]
fn unknown_keys_and_junk_lines_are_ignored() {
    let text = "theme=dark\n\
                not a key value line\n\
                \n\
                hotkey=control+option+f\n\
                future_option=42\n\
                fda_onboarding_dismissed=true\n";
    let s = Settings::parse(text);
    assert_eq!(s.hotkey, hk("control+option+f"));
    assert!(s.fda_onboarding_dismissed);
    assert!(!s.launch_at_login);
}

#[test]
fn whitespace_around_keys_and_values_is_trimmed() {
    let s = Settings::parse("  hotkey = cmd+shift+space  \n launch_at_login =true\n");
    assert_eq!(s.hotkey, hk("cmd+shift+space"));
    assert!(s.launch_at_login);
}

#[test]
fn bad_values_fall_back_to_defaults_per_field() {
    let s = Settings::parse("hotkey=space\nlaunch_at_login=maybe\nfda_onboarding_dismissed=true\n");
    assert_eq!(s.hotkey, HotkeySpec::default(), "no-modifier hotkey is bad");
    assert!(!s.launch_at_login, "unparsable bool falls back to false");
    assert!(s.fda_onboarding_dismissed, "good values are kept");

    let s =
        Settings::parse("hotkey=cmd+banana\nlaunch_at_login=true\nfda_onboarding_dismissed=1x\n");
    assert_eq!(s.hotkey, HotkeySpec::default());
    assert!(s.launch_at_login);
    assert!(!s.fda_onboarding_dismissed);
}

#[test]
fn load_missing_file_gives_defaults() {
    let dir = TempDir::new("missing");
    let s = Settings::load(&dir.path.join("nope").join("settings")).unwrap();
    assert_eq!(s, Settings::default());
}

#[test]
fn save_then_load_round_trips_and_creates_parents() {
    let dir = TempDir::new("roundtrip");
    let path = dir
        .path
        .join("Library/Application Support/Not-Everything/settings");
    custom().save(&path).unwrap();
    assert!(path.is_file());
    assert_eq!(Settings::load(&path).unwrap(), custom());
    let text = fs::read(&path).unwrap();
    assert!(String::from_utf8(text).is_ok(), "settings file is UTF-8");
}

#[test]
fn save_replaces_existing_file_and_leaves_no_temp_files() {
    let dir = TempDir::new("atomic");
    let path = dir.path.join("settings");
    fs::write(&path, "hotkey=cmd+k\nsomething=else\n").unwrap();

    custom().save(&path).unwrap();
    assert_eq!(Settings::load(&path).unwrap(), custom());
    Settings::default().save(&path).unwrap();
    assert_eq!(Settings::load(&path).unwrap(), Settings::default());

    assert_eq!(entries(&dir.path), vec!["settings".to_string()]);
}

/// The permission bits of `path`.
#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Only the owner may read the settings or open their folder, the data
/// folder that also holds the index.
#[cfg(unix)]
#[test]
fn save_writes_an_owner_only_file_in_an_owner_only_folder() {
    let dir = TempDir::new("private");
    let folder = dir.path.join("Library/Application Support/Not-Everything");
    let path = folder.join("settings");
    custom().save(&path).unwrap();
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(&folder), 0o700);
}

/// A folder and file an earlier version left readable by others are
/// tightened on the next save.
#[cfg(unix)]
#[test]
fn save_tightens_an_open_folder_and_file() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("tighten");
    let folder = dir.path.join("Not-Everything");
    let path = folder.join("settings");
    fs::create_dir_all(&folder).unwrap();
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(&path, "hotkey=cmd+k\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    custom().save(&path).unwrap();
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(&folder), 0o700);
    assert_eq!(Settings::load(&path).unwrap(), custom());
}

#[test]
fn load_tolerates_hand_edited_file() {
    let dir = TempDir::new("handedit");
    let path = dir.path.join("settings");
    fs::write(&path, "launch_at_login=true\nunknown=1\nhotkey=nonsense\n").unwrap();
    let s = Settings::load(&path).unwrap();
    assert!(s.launch_at_login);
    assert_eq!(s.hotkey, HotkeySpec::default());
}

#[test]
fn settings_path_is_in_the_app_folder() {
    let folder = match Channel::CURRENT {
        Channel::Release => "Not-Everything",
        Channel::Rc | Channel::Beta => "Not-Everything Preview",
    };
    assert_eq!(
        settings_path(Path::new("/Users/someone")),
        Path::new("/Users/someone/Library/Application Support")
            .join(folder)
            .join("settings")
    );
}

// ------------------------------------------------------- shortcut recorder

/// Every shortcut the Settings recorder can pick (any of the 49 keys with
/// any of the 15 modifier sets) is saved as the `hotkey=` line and read
/// back unchanged, in a file with the other settings around it.
#[test]
fn every_recordable_shortcut_round_trips_through_the_file() {
    use fx_ui_macos::hotkey::{Key, Modifiers};
    let dir = TempDir::new("recorded");
    let path = dir.path.join("settings");
    let mut count = 0;
    for bits in 1u8..16 {
        let modifiers = Modifiers {
            cmd: bits & 1 != 0,
            shift: bits & 2 != 0,
            option: bits & 4 != 0,
            control: bits & 8 != 0,
        };
        for key in Key::all() {
            let spec = HotkeySpec { modifiers, key };
            let mut settings = custom();
            settings.hotkey = spec;
            let loaded = Settings::parse(&settings.to_text());
            assert_eq!(loaded, settings, "{spec}");
            count += 1;
        }
    }
    assert_eq!(count, 15 * 49);
    // And through a real save and load.
    let mut settings = custom();
    settings.hotkey = hk("control+option+k");
    settings.save(&path).unwrap();
    assert_eq!(
        Settings::load(&path).unwrap().hotkey,
        hk("control+option+k")
    );
    let text = fs::read_to_string(&path).unwrap();
    assert!(
        text.lines().any(|l| l == "hotkey=option+control+k"),
        "{text}"
    );
}

/// A settings file as 0.3.0 wrote it, for the Preview app and for the
/// release app with a shortcut edited by hand, keeps its shortcut.
#[test]
fn a_0_3_0_file_keeps_its_shortcut() {
    let preview = "hotkey=shift+option+space\nlaunch_at_login=false\n\
                   fda_onboarding_dismissed=true\nappearance=system\napp_icon=match-row\n\
                   your_files_first=true\nsort=name:ascending\nmatch_case=false\n\
                   match_path=false\nregex=false\nindex_size=1920000\n";
    assert_eq!(Settings::parse(preview).hotkey, hk("shift+option+space"));
    let edited = preview.replace("hotkey=shift+option+space", "hotkey=Ctrl+Alt+K");
    assert_eq!(Settings::parse(&edited).hotkey, hk("control+option+k"));
}

/// The release app and the Preview app keep separate files, so a shortcut
/// picked in one never changes the other's.
#[test]
fn each_app_s_shortcut_is_its_own() {
    let home = PathBuf::from("/Users/me");
    let path = settings_path(&home);
    let other = if path.to_string_lossy().contains("Not-Everything Preview") {
        home.join("Library/Application Support/Not-Everything/settings")
    } else {
        home.join("Library/Application Support/Not-Everything Preview/settings")
    };
    assert_ne!(path, other);
    assert_ne!(
        Settings::defaults_for(Channel::Release).hotkey,
        Settings::defaults_for(Channel::Rc).hotkey
    );
}
