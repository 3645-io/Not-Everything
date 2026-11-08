//! Full Disk Access probe and the non-FDA exclusion set.
//!
//! Contract pinned here:
//! - `fx_fs_macos::fda` is a public module. Its pure parts compile on every
//!   host: `FDA_SETTINGS_URL`, `consent_prompt_paths(home)` and
//!   `consent_prompt_exclusions(home)`.
//! - `FDA_SETTINGS_URL` is Apple's documented deep link to the Full Disk
//!   Access pane; `open_settings()` hands it to `/usr/bin/open`. Tests only
//!   check the constant and never launch anything.
//! - `consent_prompt_paths(home)` lists the folders a scan must skip when
//!   access is not `Granted`, so the non-FDA mode triggers no TCC consent
//!   prompt: Desktop, Documents, Downloads, iCloud (Mobile Documents),
//!   CloudStorage, the Photos library, Mail, Messages, Safari, `/Volumes`
//!   and the TCC folders.
//! - `consent_prompt_exclusions(home)` returns an `fx_fs::ExclusionConfig`
//!   whose `excluded_folders` holds that list (nothing else switched on),
//!   ready for `fx_fs::Exclusions::new` and `ScanOptions::exclusions`.
//! - `FdaProbe` (macOS) implements `fx_fs::PermissionProbe`.
//!   `FdaProbe::with_paths(paths)` probes the given files in order with a
//!   read-only, no-follow `open` (contents are never read): `Granted` if any
//!   open succeeds, `Denied` on EPERM/EACCES, `Unknown` when none exist or
//!   the error is anything else (a symlink, `O_NOFOLLOW`, is not a success).
//!   `FdaProbe::new()` (also `Default`) probes the user and system
//!   `TCC.db`, user first, exposed through `paths()`, and never a
//!   consent-prompt location.

use std::path::{Path, PathBuf};

use fx_fs::{ExclusionConfig, Exclusions, Verdict};
use fx_fs_macos::fda::{FDA_SETTINGS_URL, consent_prompt_exclusions, consent_prompt_paths};

fn fake_home() -> PathBuf {
    PathBuf::from("/Users/fake-fx-user")
}

// ---------------------------------------------------------------------------
// Settings deep link
// ---------------------------------------------------------------------------

#[test]
fn settings_url_is_the_full_disk_access_deep_link() {
    let url: &str = FDA_SETTINGS_URL;
    assert_eq!(
        url,
        "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles"
    );
}

// ---------------------------------------------------------------------------
// Consent-prompt exclusion list
// ---------------------------------------------------------------------------

#[test]
fn consent_paths_cover_the_prompting_home_folders() {
    let home = fake_home();
    let paths = consent_prompt_paths(&home);
    let expected = [
        "Desktop",
        "Documents",
        "Downloads",
        "Library/Mobile Documents",
        "Library/CloudStorage",
        "Pictures/Photos Library.photoslibrary",
        "Library/Mail",
        "Library/Messages",
        "Library/Safari",
        "Library/Application Support/com.apple.TCC",
    ];
    for rel in expected {
        let want = home.join(rel);
        assert!(
            paths.contains(&want),
            "consent_prompt_paths is missing {}: {paths:?}",
            want.display()
        );
    }
}

#[test]
fn consent_paths_cover_volumes_and_system_tcc() {
    let paths = consent_prompt_paths(&fake_home());
    for want in ["/Volumes", "/Library/Application Support/com.apple.TCC"] {
        assert!(
            paths.contains(&PathBuf::from(want)),
            "consent_prompt_paths is missing {want}: {paths:?}"
        );
    }
}

#[test]
fn consent_paths_follow_the_given_home() {
    let a = consent_prompt_paths(Path::new("/Users/alice"));
    let b = consent_prompt_paths(Path::new("/Users/bob"));
    assert!(a.contains(&PathBuf::from("/Users/alice/Desktop")));
    assert!(b.contains(&PathBuf::from("/Users/bob/Desktop")));
    assert!(!b.iter().any(|p| p.starts_with("/Users/alice")));
}

#[test]
fn consent_paths_are_absolute_and_do_not_cover_the_whole_home() {
    let home = fake_home();
    let paths = consent_prompt_paths(&home);
    assert!(!paths.is_empty());
    for p in &paths {
        assert!(p.is_absolute(), "not absolute: {}", p.display());
        assert_ne!(p, &home, "the whole home must stay indexable");
        assert_ne!(p, Path::new("/"), "the whole disk must stay indexable");
        assert_ne!(p, &home.join("Library"), "~/Library must stay indexable");
    }
}

#[test]
fn consent_exclusions_hold_exactly_the_consent_paths() {
    let home = fake_home();
    let cfg: ExclusionConfig = consent_prompt_exclusions(&home);
    assert_eq!(cfg.excluded_folders, consent_prompt_paths(&home));
    assert!(!cfg.exclude_hidden);
    assert!(!cfg.exclude_system);
    assert!(cfg.include_only.is_empty());
    assert!(cfg.wildcard_patterns.is_empty());
    assert!(cfg.regex_patterns.is_empty());
}

#[test]
fn consent_exclusions_skip_prompting_subtrees_and_keep_the_rest() {
    let home = fake_home();
    let ex = Exclusions::new(consent_prompt_exclusions(&home)).expect("compiles");

    let dir = |p: &Path| {
        let name = p.file_name().unwrap().to_str().unwrap();
        ex.check(p, name, true, false, false)
    };

    for rel in [
        "Desktop",
        "Documents",
        "Downloads",
        "Library/Mobile Documents",
        "Library/CloudStorage",
        "Pictures/Photos Library.photoslibrary",
        "Library/Mail",
        "Library/Messages",
        "Library/Safari",
    ] {
        assert_eq!(
            dir(&home.join(rel)),
            Verdict::SkipSubtree,
            "{rel} must be skipped without FDA"
        );
    }
    assert_eq!(dir(Path::new("/Volumes")), Verdict::SkipSubtree);
    assert_eq!(
        dir(Path::new("/Library/Application Support/com.apple.TCC")),
        Verdict::SkipSubtree
    );

    // Ordinary folders stay in the index.
    for p in [
        home.join("Projects"),
        home.join("Library"),
        home.join("Pictures"),
        PathBuf::from("/Applications"),
        PathBuf::from("/usr"),
    ] {
        assert_eq!(
            dir(&p),
            Verdict::Include,
            "{} must stay indexed",
            p.display()
        );
    }
}

// ---------------------------------------------------------------------------
// The probe itself (macOS)
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod probe {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use fx_fs::{AccessState, PermissionProbe};
    use fx_fs_macos::fda::FdaProbe;

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(tag: &str) -> TempRoot {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let p =
                std::env::temp_dir().join(format!("fx-fda-{tag}-{}-{nanos}", std::process::id()));
            fs::create_dir_all(&p).expect("create temp root");
            TempRoot(fs::canonicalize(&p).expect("canonicalize"))
        }

        fn readable(&self, name: &str) -> PathBuf {
            let p = self.0.join(name);
            fs::write(&p, b"probe").expect("write");
            p
        }

        /// A chmod-000 file, or `None` when the process can still open it
        /// (running as root), in which case the caller skips.
        fn unreadable(&self, name: &str) -> Option<PathBuf> {
            let p = self.0.join(name);
            fs::write(&p, b"secret").expect("write");
            fs::set_permissions(&p, fs::Permissions::from_mode(0o000)).expect("chmod 000");
            if fs::File::open(&p).is_ok() {
                eprintln!("skipping: chmod-000 file is readable (running as root?)");
                return None;
            }
            Some(p)
        }

        fn missing(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            if let Ok(rd) = fs::read_dir(&self.0) {
                for e in rd.flatten() {
                    let _ = fs::set_permissions(e.path(), fs::Permissions::from_mode(0o644));
                }
            }
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn probe(paths: Vec<PathBuf>) -> AccessState {
        FdaProbe::with_paths(paths).full_access()
    }

    #[test]
    fn readable_file_means_granted() {
        let t = TempRoot::new("granted");
        let f = t.readable("TCC.db");
        assert_eq!(probe(vec![f]), AccessState::Granted);
    }

    #[test]
    fn probing_does_not_modify_the_file() {
        let t = TempRoot::new("nomodify");
        let f = t.readable("TCC.db");
        assert_eq!(probe(vec![f.clone()]), AccessState::Granted);
        assert_eq!(fs::read(&f).expect("read back"), b"probe");
    }

    #[test]
    fn unreadable_file_means_denied() {
        let t = TempRoot::new("denied");
        let Some(f) = t.unreadable("TCC.db") else {
            return;
        };
        assert_eq!(probe(vec![f]), AccessState::Denied);
    }

    #[test]
    fn missing_files_mean_unknown() {
        let t = TempRoot::new("missing");
        let a = t.missing("a/TCC.db");
        let b = t.missing("TCC.db");
        assert_eq!(probe(vec![a, b]), AccessState::Unknown);
    }

    #[test]
    fn no_paths_means_unknown() {
        assert_eq!(probe(Vec::new()), AccessState::Unknown);
    }

    #[test]
    fn other_errors_mean_unknown() {
        // ENOTDIR: a path component is a regular file.
        let t = TempRoot::new("enotdir");
        let f = t.readable("plain");
        assert_eq!(probe(vec![f.join("TCC.db")]), AccessState::Unknown);
    }

    #[test]
    fn symlinks_are_not_followed() {
        let t = TempRoot::new("nofollow");
        let target = t.readable("real.db");
        let link = t.0.join("TCC.db");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        assert_ne!(
            probe(vec![link]),
            AccessState::Granted,
            "O_NOFOLLOW: a symlink must not count as an FDA-only file"
        );
    }

    #[test]
    fn first_success_wins_after_missing() {
        let t = TempRoot::new("missing-then-ok");
        let miss = t.missing("TCC.db");
        let ok = t.readable("system-TCC.db");
        assert_eq!(probe(vec![miss, ok]), AccessState::Granted);
    }

    #[test]
    fn success_wins_over_earlier_denial() {
        let t = TempRoot::new("denied-then-ok");
        let Some(denied) = t.unreadable("user-TCC.db") else {
            return;
        };
        let ok = t.readable("system-TCC.db");
        assert_eq!(probe(vec![denied, ok]), AccessState::Granted);
    }

    #[test]
    fn success_wins_over_later_denial() {
        let t = TempRoot::new("ok-then-denied");
        let ok = t.readable("user-TCC.db");
        let Some(denied) = t.unreadable("system-TCC.db") else {
            return;
        };
        assert_eq!(probe(vec![ok, denied]), AccessState::Granted);
    }

    #[test]
    fn denial_beats_missing_in_either_order() {
        let t = TempRoot::new("denied-missing");
        let Some(denied) = t.unreadable("TCC.db") else {
            return;
        };
        let miss = t.missing("nope.db");
        assert_eq!(
            probe(vec![miss.clone(), denied.clone()]),
            AccessState::Denied
        );
        assert_eq!(probe(vec![denied, miss]), AccessState::Denied);
    }

    #[test]
    fn probe_is_usable_as_a_trait_object() {
        let t = TempRoot::new("dyn");
        let f = t.readable("TCC.db");
        let p: Box<dyn PermissionProbe> = Box::new(FdaProbe::with_paths(vec![f]));
        assert_eq!(p.full_access(), AccessState::Granted);
        // Repeated probes give the same answer (onboarding rechecks).
        assert_eq!(p.full_access(), AccessState::Granted);
    }

    #[test]
    fn default_probe_targets_the_tcc_databases_user_first() {
        let probe = FdaProbe::new();
        let paths = probe.paths();
        assert_eq!(paths.len(), 2, "{paths:?}");
        assert!(
            paths[0].ends_with("Library/Application Support/com.apple.TCC/TCC.db"),
            "{paths:?}"
        );
        assert!(paths[0].is_absolute());
        assert_ne!(
            paths[0],
            Path::new("/Library/Application Support/com.apple.TCC/TCC.db")
        );
        assert_eq!(
            paths[1],
            Path::new("/Library/Application Support/com.apple.TCC/TCC.db")
        );
        assert_eq!(FdaProbe::default().paths(), paths);
    }

    #[test]
    fn default_probe_never_touches_consent_prompt_locations() {
        let probe = FdaProbe::new();
        let home = std::env::var_os("HOME").map(PathBuf::from).expect("HOME");
        let prompting = [
            home.join("Desktop"),
            home.join("Documents"),
            home.join("Downloads"),
            home.join("Library/Mobile Documents"),
            home.join("Library/CloudStorage"),
            home.join("Pictures"),
            PathBuf::from("/Volumes"),
        ];
        for p in probe.paths() {
            for bad in &prompting {
                assert!(
                    !p.starts_with(bad),
                    "probe path {} lies under consent-prompt location {}",
                    p.display(),
                    bad.display()
                );
            }
        }
    }
}
