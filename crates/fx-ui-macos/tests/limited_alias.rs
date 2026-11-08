//! Limited (non-FDA) mode hardening.
//!
//! - `fx_fs_macos::fda::user_home()` is the home folder limited mode and the
//!   store path are built from: `$HOME` when it is set and absolute, else the
//!   password database's home folder for the real user.
//! - The Data volume alias: every consent-prompt folder is also reachable as
//!   `/System/Volumes/Data/<path>` (through the `/Users`, `/Volumes` and
//!   `/Library` firmlinks). `consent_prompt_paths` lists those variants too,
//!   so a root given as a Data volume alias (or a change event spelled that
//!   way) is kept away from the protected folders just like the `/` spelling.
//!
//! `fx-fs-macos` is a macOS-only dependency of this crate, so the whole file
//! is macOS-only.
#![cfg(target_os = "macos")]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fx_fs::{ChangeSource, Exclusions, Verdict};
use fx_fs_macos::fda::{consent_prompt_exclusions, consent_prompt_paths, user_home};
use fx_fs_macos::firmlinks::{DATA_VOLUME, FirmlinkMap};
use fx_model::{Change, ResumeCursor, StartOutcome};
use fx_ui_macos::limited::{ExcludingChangeSource, is_excluded};

const HOME: &str = "/Users/tester";

fn home() -> PathBuf {
    PathBuf::from(HOME)
}

/// `path` (absolute) spelled through the Data volume mount point.
fn data_alias(path: &Path) -> PathBuf {
    Path::new(DATA_VOLUME).join(path.strip_prefix("/").expect("absolute path"))
}

// ---------------------------------------------------------------------------
// user_home()
// ---------------------------------------------------------------------------

/// Restores `HOME` when dropped, even if an assertion fails.
struct HomeGuard(Option<OsString>);

impl Drop for HomeGuard {
    fn drop(&mut self) {
        // SAFETY: only this test in this binary reads or writes `HOME`.
        unsafe {
            match &self.0 {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}

/// The only test in this binary that touches `HOME` (environment changes
/// are process-wide, so every case runs here, sequentially).
#[test]
fn user_home_uses_absolute_home_then_the_passwd_entry() {
    let _guard = HomeGuard(std::env::var_os("HOME"));

    // No HOME: the password database's home folder.
    // SAFETY: see `HomeGuard`.
    unsafe { std::env::remove_var("HOME") };
    let passwd = user_home().expect("the current user has a passwd home folder");
    assert!(
        passwd.is_absolute(),
        "passwd home must be absolute: {}",
        passwd.display()
    );

    // An absolute HOME wins over the passwd entry.
    let custom = std::env::temp_dir().join(format!("fx-limited-alias-home-{}", std::process::id()));
    std::fs::create_dir_all(&custom).expect("create custom home");
    // SAFETY: see `HomeGuard`.
    unsafe { std::env::set_var("HOME", &custom) };
    assert_eq!(user_home(), Some(custom.clone()), "absolute HOME is used");

    // An empty HOME falls back to the passwd entry.
    // SAFETY: see `HomeGuard`.
    unsafe { std::env::set_var("HOME", "") };
    assert_eq!(
        user_home(),
        Some(passwd.clone()),
        "empty HOME falls back to passwd"
    );

    // A relative HOME falls back to the passwd entry.
    // SAFETY: see `HomeGuard`.
    unsafe { std::env::set_var("HOME", "relative/home") };
    assert_eq!(
        user_home(),
        Some(passwd.clone()),
        "relative HOME falls back to passwd"
    );

    let _ = std::fs::remove_dir(&custom);
}

// ---------------------------------------------------------------------------
// Data volume aliases of the consent-prompt folders
// ---------------------------------------------------------------------------

#[test]
fn consent_paths_include_the_data_volume_variants() {
    let paths = consent_prompt_paths(&home());
    let wanted = [
        home().join("Desktop"),
        home().join("Documents"),
        home().join("Downloads"),
        home().join("Library/Mobile Documents"),
        home().join("Library/CloudStorage"),
        home().join("Library/Application Support/com.apple.TCC"),
        PathBuf::from("/Volumes"),
        PathBuf::from("/Library/Application Support/com.apple.TCC"),
    ];
    for path in &wanted {
        assert!(
            paths.contains(path),
            "missing {}: {paths:?}",
            path.display()
        );
        let alias = data_alias(path);
        assert!(
            paths.contains(&alias),
            "missing the Data volume variant {}: {paths:?}",
            alias.display()
        );
    }
}

#[test]
fn data_volume_variants_map_back_to_listed_paths_and_spare_the_home() {
    let paths = consent_prompt_paths(&home());
    let map = FirmlinkMap::fallback();
    let data = Path::new(DATA_VOLUME);
    for p in paths.iter().filter(|p| p.starts_with(data)) {
        // No over-broad alias: the Data volume, the aliased home and the
        // aliased ~/Library must stay indexable.
        assert_ne!(p, data, "the whole Data volume must stay indexable");
        assert_ne!(
            p,
            &data_alias(&home()),
            "the aliased home must stay indexable"
        );
        assert_ne!(
            p,
            &data_alias(&home().join("Library")),
            "the aliased ~/Library must stay indexable"
        );
        // Every alias is the Data volume spelling of a listed `/` path.
        let canonical = map.to_root(p);
        assert!(
            paths.contains(&canonical),
            "{} is not the alias of a listed path ({}): {paths:?}",
            p.display(),
            canonical.display()
        );
    }
}

#[test]
fn alias_root_inside_a_protected_folder_is_excluded() {
    // A root (or any path) given through /System/Volumes/Data must be
    // recognised as inside a consent-prompt folder, so limited mode refuses
    // to open it instead of raising a TCC prompt.
    let paths = consent_prompt_paths(&home());
    for rel in ["Documents", "Desktop/Project", "Downloads/a.dmg"] {
        let alias = data_alias(&home().join(rel));
        assert!(
            is_excluded(&paths, &alias),
            "alias root {} must be excluded",
            alias.display()
        );
    }
    assert!(is_excluded(
        &paths,
        &data_alias(&home().join("Library/Mobile Documents/com~apple~CloudDocs"))
    ));
    assert!(is_excluded(
        &paths,
        Path::new("/System/Volumes/Data/Volumes/USB")
    ));

    // Siblings stay indexable under the alias too.
    for rel in ["Music", "Documents2/notes.txt", "Library/Preferences"] {
        let alias = data_alias(&home().join(rel));
        assert!(
            !is_excluded(&paths, &alias),
            "{} must stay indexable",
            alias.display()
        );
    }
    assert!(!is_excluded(&paths, &data_alias(&home())));
}

#[test]
fn scanner_exclusions_skip_protected_folders_under_an_alias_root() {
    // Scanning from an alias root such as /System/Volumes/Data/Users/tester
    // visits Data volume spellings; the scanner's exclusions must prune the
    // protected folders there as well.
    let ex = Exclusions::new(consent_prompt_exclusions(&home())).expect("compile exclusions");
    let alias_home = data_alias(&home());
    for name in ["Desktop", "Documents", "Downloads"] {
        let dir = alias_home.join(name);
        assert_eq!(
            ex.check(&dir, name, true, false, false),
            Verdict::SkipSubtree,
            "{} must be skipped",
            dir.display()
        );
    }
    let icloud = alias_home.join("Library/Mobile Documents");
    assert_eq!(
        ex.check(&icloud, "Mobile Documents", true, false, false),
        Verdict::SkipSubtree
    );
    let volumes = Path::new("/System/Volumes/Data/Volumes");
    assert_eq!(
        ex.check(volumes, "Volumes", true, false, false),
        Verdict::SkipSubtree
    );

    let music = alias_home.join("Music");
    assert_eq!(
        ex.check(&music, "Music", true, false, false),
        Verdict::Include
    );
    let library = alias_home.join("Library");
    assert_eq!(
        ex.check(&library, "Library", true, false, false),
        Verdict::Include
    );
}

// ---------------------------------------------------------------------------
// Change events spelled through the Data volume
// ---------------------------------------------------------------------------

struct OneBatch(Option<Vec<Change>>);

impl ChangeSource for OneBatch {
    fn start(&mut self, _from: Option<&ResumeCursor>) -> fx_fs::Result<StartOutcome> {
        Ok(StartOutcome::Resumed)
    }

    fn next_batch(&mut self, _deadline: Instant) -> fx_fs::Result<Vec<Change>> {
        Ok(self.0.take().unwrap_or_default())
    }

    fn cursor(&self) -> ResumeCursor {
        ResumeCursor::FsEvents {
            event_id: 1,
            db_uuid: [0; 16],
        }
    }
}

#[test]
fn alias_change_events_inside_protected_folders_are_dropped() {
    let keep = vec![
        Change::CheckPath(data_alias(&home().join("Music/song.mp3"))),
        Change::CheckPath(home().join("notes.txt")),
    ];
    let mut batch = vec![
        Change::CheckPath(data_alias(&home().join("Documents/secret.txt"))),
        Change::RescanSubtree(data_alias(&home().join("Desktop/a"))),
        Change::CheckPath(PathBuf::from("/System/Volumes/Data/Volumes/USB/x")),
    ];
    batch.extend(keep.clone());
    let mut source =
        ExcludingChangeSource::new(OneBatch(Some(batch)), consent_prompt_paths(&home()));
    source.start(None).expect("start");
    let out = source
        .next_batch(Instant::now() + Duration::from_millis(10))
        .expect("batch");
    assert_eq!(out, keep);
}
