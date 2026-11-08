//! Tests for `fx_fs_macos::firmlinks` (firmlink mapping).
//!
//! Contract pinned here:
//! - The module is pure Rust and compiles on every host (CI also runs on
//!   Ubuntu); only the agreement test with `fsevents::stream` is macOS-only.
//! - `FirmlinkMap` derives `Debug + Clone + PartialEq + Eq` and has:
//!   - `FirmlinkMap::parse(text: &str) -> FirmlinkMap`: one
//!     `<path on />\t<path relative to the Data volume>` entry per line.
//!     Surrounding whitespace on each field is trimmed. Blank lines and
//!     malformed lines (no tab, a first field that is not absolute, an empty
//!     second field) are ignored.
//!   - `FirmlinkMap::fallback() -> FirmlinkMap`: a built-in list that covers
//!     at least `/Users`, `/Applications`, `/Library`, `/private`, `/opt`
//!     `/usr/local` and `/Volumes`.
//!   - `FirmlinkMap::load_from(path: &Path) -> FirmlinkMap`: parses the file,
//!     or returns `fallback()` if it is missing, unreadable or yields no
//!     entries. `FirmlinkMap::load()` does the same for
//!     `/usr/share/firmlinks`.
//!   - `len()` / `is_empty()`: number of parsed entries.
//!   - `to_root(&self, path: &Path) -> PathBuf`: maps
//!     `/System/Volumes/Data/<data-relative>/<tail>` to `<path on />/<tail>`,
//!     matching whole path components and preferring the longest
//!     data-relative path. Paths outside `/System/Volumes/Data` are returned
//!     unchanged.
//! - `is_data_volume_alias(path: &Path) -> bool` is true for
//!   `/System/Volumes/Data` itself and anything below it (component-wise),
//!   so the walker never descends into it from `/`.
//! - On macOS, `fsevents::stream::normalise_firmlinks` gives the same answer
//!   as `FirmlinkMap::load().to_root`.

use std::path::{Path, PathBuf};

use fx_fs_macos::firmlinks::{FirmlinkMap, is_data_volume_alias};

/// An excerpt in the real `/usr/share/firmlinks` format.
const SAMPLE: &str = "/AppleInternal\tAppleInternal\n\
/Applications\tApplications\n\
/Library\tLibrary\n\
/System/Library/Caches\tSystem/Library/Caches\n\
/Users\tUsers\n\
/Volumes\tVolumes\n\
/opt\topt\n\
/private\tprivate\n\
/usr/local\tusr/local\n";

fn root(map: &FirmlinkMap, p: &str) -> PathBuf {
    map.to_root(Path::new(p))
}

// --------------------------------------------------------------------- parser

#[test]
fn parses_every_well_formed_line() {
    let map = FirmlinkMap::parse(SAMPLE);
    assert_eq!(map.len(), 9);
    assert!(!map.is_empty());
}

#[test]
fn empty_input_gives_an_empty_map() {
    let map = FirmlinkMap::parse("");
    assert!(map.is_empty());
    assert_eq!(map.len(), 0);
}

#[test]
fn blank_and_malformed_lines_are_ignored() {
    let text = "\n\
   \n\
/Users\tUsers\n\
no tab on this line\n\
relative\tfirst/field\n\
/Empty\t\n\
/Empty2\t   \n\
\t\n\
/Applications\tApplications";
    let map = FirmlinkMap::parse(text);
    assert_eq!(map.len(), 2, "{map:?}");
    assert_eq!(
        root(&map, "/System/Volumes/Data/Users/x"),
        PathBuf::from("/Users/x")
    );
    // The last line has no trailing newline and still counts.
    assert_eq!(
        root(&map, "/System/Volumes/Data/Applications/Safari.app"),
        PathBuf::from("/Applications/Safari.app")
    );
}

#[test]
fn fields_are_trimmed() {
    let map = FirmlinkMap::parse("  /Users \t Users  \r\n");
    assert_eq!(map.len(), 1);
    assert_eq!(
        root(&map, "/System/Volumes/Data/Users/a/b"),
        PathBuf::from("/Users/a/b")
    );
}

#[test]
fn parse_is_deterministic() {
    assert_eq!(FirmlinkMap::parse(SAMPLE), FirmlinkMap::parse(SAMPLE));
}

// -------------------------------------------------------------------- mapping

#[test]
fn data_paths_fold_onto_root_paths() {
    let map = FirmlinkMap::parse(SAMPLE);
    assert_eq!(
        root(&map, "/System/Volumes/Data/Users/x"),
        PathBuf::from("/Users/x")
    );
    assert_eq!(
        root(&map, "/System/Volumes/Data/Users/x/Documents/report.pdf"),
        PathBuf::from("/Users/x/Documents/report.pdf")
    );
    assert_eq!(
        root(&map, "/System/Volumes/Data/private/var/log"),
        PathBuf::from("/private/var/log")
    );
    assert_eq!(
        root(&map, "/System/Volumes/Data/usr/local/bin/tool"),
        PathBuf::from("/usr/local/bin/tool")
    );
    assert_eq!(
        root(
            &map,
            "/System/Volumes/Data/System/Library/Caches/com.example"
        ),
        PathBuf::from("/System/Library/Caches/com.example")
    );
}

#[test]
fn firmlinked_directory_itself_maps_to_its_root_path() {
    let map = FirmlinkMap::parse(SAMPLE);
    assert_eq!(
        root(&map, "/System/Volumes/Data/Users"),
        PathBuf::from("/Users")
    );
    assert_eq!(
        root(&map, "/System/Volumes/Data/usr/local"),
        PathBuf::from("/usr/local")
    );
}

#[test]
fn matching_is_by_whole_component() {
    let map = FirmlinkMap::parse(SAMPLE);
    let p = "/System/Volumes/Data/UsersBackup/x";
    assert_ne!(root(&map, p), PathBuf::from("/UsersBackup/x"));
    assert_ne!(root(&map, p), PathBuf::from("/Users/Backup/x"));
    let q = "/System/Volumes/Data/usr/localextra";
    assert_ne!(root(&map, q), PathBuf::from("/usr/localextra"));
}

#[test]
fn longest_data_relative_path_wins() {
    let map = FirmlinkMap::parse("/short\tshared\n/long\tshared/inner\n");
    assert_eq!(
        root(&map, "/System/Volumes/Data/shared/inner/file"),
        PathBuf::from("/long/file")
    );
    assert_eq!(
        root(&map, "/System/Volumes/Data/shared/other"),
        PathBuf::from("/short/other")
    );
    // Order in the file does not matter.
    let rev = FirmlinkMap::parse("/long\tshared/inner\n/short\tshared\n");
    assert_eq!(
        root(&rev, "/System/Volumes/Data/shared/inner/file"),
        PathBuf::from("/long/file")
    );
}

#[test]
fn paths_outside_the_data_volume_are_unchanged() {
    let map = FirmlinkMap::parse(SAMPLE);
    for p in [
        "/",
        "/Users/x",
        "/Applications/Safari.app",
        "/System/Library/CoreServices",
        "/System/Volumes",
        "/System/Volumes/DataX/Users/x",
        "/Volumes/USB/Users/x",
        "relative/Users/x",
    ] {
        assert_eq!(root(&map, p), PathBuf::from(p), "{p}");
    }
}

#[test]
fn an_empty_map_changes_nothing() {
    let map = FirmlinkMap::parse("");
    assert_eq!(
        root(&map, "/System/Volumes/Data/Users/x"),
        PathBuf::from("/System/Volumes/Data/Users/x")
    );
}

// ------------------------------------------------------------ alias predicate

#[test]
fn data_volume_and_its_children_are_aliases() {
    for p in [
        "/System/Volumes/Data",
        "/System/Volumes/Data/",
        "/System/Volumes/Data/Users",
        "/System/Volumes/Data/Users/x/file.txt",
        "/System/Volumes/Data/private/var",
    ] {
        assert!(is_data_volume_alias(Path::new(p)), "{p}");
    }
}

#[test]
fn other_paths_are_not_aliases() {
    for p in [
        "/",
        "/System",
        "/System/Volumes",
        "/System/Volumes/DataX",
        "/System/Volumes/Preboot",
        "/System/Volumes/VM",
        "/Users",
        "/Users/x/System/Volumes/Data",
        "/Volumes/Data",
    ] {
        assert!(!is_data_volume_alias(Path::new(p)), "{p}");
    }
}

#[test]
fn mapped_paths_are_never_aliases() {
    let map = FirmlinkMap::parse(SAMPLE);
    for p in [
        "/System/Volumes/Data/Users/x",
        "/System/Volumes/Data/Applications",
        "/System/Volumes/Data/private/etc/hosts",
        "/System/Volumes/Data/usr/local/lib",
    ] {
        let mapped = map.to_root(Path::new(p));
        assert!(!is_data_volume_alias(&mapped), "{p} -> {mapped:?}");
    }
}

// ------------------------------------------------------- fallback and loading

#[test]
fn fallback_covers_the_common_firmlinks() {
    let map = FirmlinkMap::fallback();
    assert!(!map.is_empty());
    for (data, want) in [
        ("/System/Volumes/Data/Users/x", "/Users/x"),
        (
            "/System/Volumes/Data/Applications/A.app",
            "/Applications/A.app",
        ),
        ("/System/Volumes/Data/Library/Fonts", "/Library/Fonts"),
        ("/System/Volumes/Data/private/var", "/private/var"),
        ("/System/Volumes/Data/opt/homebrew", "/opt/homebrew"),
        ("/System/Volumes/Data/usr/local/bin", "/usr/local/bin"),
        ("/System/Volumes/Data/Volumes/USB", "/Volumes/USB"),
    ] {
        assert_eq!(root(&map, data), PathBuf::from(want), "{data}");
    }
}

#[test]
fn load_from_missing_file_uses_the_fallback() {
    let missing = std::env::temp_dir().join(format!(
        "fx-firmlinks-missing-{}-does-not-exist",
        std::process::id()
    ));
    assert!(!missing.exists());
    assert_eq!(FirmlinkMap::load_from(&missing), FirmlinkMap::fallback());
}

#[test]
fn load_from_reads_the_file() {
    let dir = std::env::temp_dir().join(format!("fx-firmlinks-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("firmlinks");
    std::fs::write(&file, "/Custom\tcustom/dir\n").unwrap();
    let map = FirmlinkMap::load_from(&file);
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(map, FirmlinkMap::parse("/Custom\tcustom/dir\n"));
    assert_eq!(
        root(&map, "/System/Volumes/Data/custom/dir/x"),
        PathBuf::from("/Custom/x")
    );
}

#[test]
fn load_from_a_file_without_entries_uses_the_fallback() {
    let dir = std::env::temp_dir().join(format!("fx-firmlinks-empty-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("firmlinks");
    std::fs::write(&file, "\n\ngarbage\n").unwrap();
    let map = FirmlinkMap::load_from(&file);
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(map, FirmlinkMap::fallback());
}

#[test]
fn load_always_maps_users() {
    // Real file on macOS, fallback elsewhere: both firmlink /Users.
    let map = FirmlinkMap::load();
    assert!(!map.is_empty());
    assert_eq!(
        root(&map, "/System/Volumes/Data/Users/x"),
        PathBuf::from("/Users/x")
    );
}

// --------------------------------------- agreement with fsevents::stream

#[cfg(target_os = "macos")]
#[test]
fn normalise_firmlinks_agrees_with_the_map() {
    use fx_fs_macos::fsevents::stream::normalise_firmlinks;

    let map = FirmlinkMap::load();
    for p in [
        "/System/Volumes/Data/Users/x",
        "/System/Volumes/Data/Users",
        "/System/Volumes/Data/Applications/Safari.app",
        "/System/Volumes/Data/private/var/log/system.log",
        "/System/Volumes/Data/usr/local/bin",
        "/System/Volumes/Data/UsersBackup/x",
        "/System/Volumes/Data",
        "/Users/x",
        "/Volumes/USB/file",
        "/",
    ] {
        let p = Path::new(p);
        assert_eq!(normalise_firmlinks(p), map.to_root(p), "{p:?}");
    }
}
