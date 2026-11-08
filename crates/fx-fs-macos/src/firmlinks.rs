//! Firmlink mapping between the Data volume and `/`.
//!
//! On macOS 10.15 and later the writable user data lives on the Data volume,
//! mounted at `/System/Volumes/Data`, and is reached from `/` through
//! firmlinks such as `/Users`, `/Applications` or `/private`. The table of
//! firmlinks is the plain-text file `/usr/share/firmlinks`: one
//! `<path on />\t<path relative to the Data volume>` entry per line.
//!
//! [`FirmlinkMap`] parses that table and maps a path under
//! `/System/Volumes/Data` back to its canonical path on `/`, so the index
//! never holds the same file twice. [`is_data_volume_alias`] tells the
//! walker never to descend into `/System/Volumes/Data` from `/`.
//!
//! The module is pure Rust and compiles on every host; only
//! [`FirmlinkMap::load`] touches the file system.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Mount point of the Data volume on macOS 10.15 and later.
pub const DATA_VOLUME: &str = "/System/Volumes/Data";

/// Location of the system firmlink table.
pub const FIRMLINKS_FILE: &str = "/usr/share/firmlinks";

/// Standard firmlinks: the path on `/` and the path relative to the Data
/// volume root. Used only if the firmlink file is missing, unreadable or
/// empty.
const FALLBACK_FIRMLINKS: &[(&str, &str)] = &[
    ("/AppleInternal", "AppleInternal"),
    ("/Applications", "Applications"),
    ("/Library", "Library"),
    ("/System/Library/Caches", "System/Library/Caches"),
    ("/System/Library/Assets", "System/Library/Assets"),
    (
        "/System/Library/PreinstalledAssets",
        "System/Library/PreinstalledAssets",
    ),
    ("/System/Library/AssetsV2", "System/Library/AssetsV2"),
    (
        "/System/Library/PreinstalledAssetsV2",
        "System/Library/PreinstalledAssetsV2",
    ),
    ("/System/Library/Speech", "System/Library/Speech"),
    ("/Users", "Users"),
    ("/Volumes", "Volumes"),
    ("/cores", "cores"),
    ("/opt", "opt"),
    ("/private", "private"),
    ("/usr/local", "usr/local"),
    ("/usr/libexec/cups", "usr/libexec/cups"),
    ("/usr/share/snmp", "usr/share/snmp"),
];

/// A parsed firmlink table: `(path on /, path relative to the Data
/// volume)` pairs in file order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FirmlinkMap {
    entries: Vec<(PathBuf, PathBuf)>,
}

impl FirmlinkMap {
    /// Parses the `/usr/share/firmlinks` format. Each field is trimmed;
    /// blank lines and malformed lines (no tab, a first field that is not
    /// absolute, an empty second field) are ignored.
    #[must_use]
    pub fn parse(text: &str) -> FirmlinkMap {
        let entries = text
            .lines()
            .filter_map(|line| {
                let (root, rel) = line.split_once('\t')?;
                let root = root.trim();
                let rel = rel.trim().trim_start_matches('/');
                if !root.starts_with('/') || rel.is_empty() {
                    return None;
                }
                Some((PathBuf::from(root), PathBuf::from(rel)))
            })
            .collect();
        FirmlinkMap { entries }
    }

    /// The built-in copy of the standard firmlinks.
    #[must_use]
    pub fn fallback() -> FirmlinkMap {
        FirmlinkMap {
            entries: FALLBACK_FIRMLINKS
                .iter()
                .map(|(root, rel)| (PathBuf::from(root), PathBuf::from(rel)))
                .collect(),
        }
    }

    /// Parses the file at `path`, or returns [`FirmlinkMap::fallback`] if
    /// it is missing, unreadable or yields no entries.
    #[must_use]
    pub fn load_from(path: &Path) -> FirmlinkMap {
        std::fs::read(path)
            .ok()
            .map(|bytes| FirmlinkMap::parse(&String::from_utf8_lossy(&bytes)))
            .filter(|map| !map.is_empty())
            .unwrap_or_else(FirmlinkMap::fallback)
    }

    /// [`FirmlinkMap::load_from`] for `/usr/share/firmlinks`.
    #[must_use]
    pub fn load() -> FirmlinkMap {
        FirmlinkMap::load_from(Path::new(FIRMLINKS_FILE))
    }

    /// The system table, loaded once per process with
    /// [`FirmlinkMap::load`].
    #[must_use]
    pub fn system() -> &'static FirmlinkMap {
        static MAP: OnceLock<FirmlinkMap> = OnceLock::new();
        MAP.get_or_init(FirmlinkMap::load)
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if the table has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The entries in file order: `(path on /, path relative to the Data
    /// volume)`.
    #[must_use]
    pub fn entries(&self) -> &[(PathBuf, PathBuf)] {
        &self.entries
    }

    /// Maps `/System/Volumes/Data/<data-relative>/<tail>` to
    /// `<path on />/<tail>`, matching whole path components and preferring
    /// the longest data-relative path (the first one in file order on a
    /// tie). Other paths, including Data volume paths that are not below a
    /// firmlink, are returned unchanged.
    #[must_use]
    pub fn to_root(&self, path: &Path) -> PathBuf {
        let Ok(rel) = path.strip_prefix(DATA_VOLUME) else {
            return path.to_path_buf();
        };
        let mut best: Option<(usize, &Path, &Path)> = None;
        for (root, data_rel) in &self.entries {
            if let Ok(tail) = rel.strip_prefix(data_rel) {
                let depth = data_rel.components().count();
                if best.is_none_or(|(d, _, _)| depth > d) {
                    best = Some((depth, root, tail));
                }
            }
        }
        match best {
            Some((_, root, tail)) if tail.as_os_str().is_empty() => root.to_path_buf(),
            Some((_, root, tail)) => root.join(tail),
            None => path.to_path_buf(),
        }
    }

    /// The inverse of [`FirmlinkMap::to_root`]: maps `<path on />/<tail>` to
    /// `/System/Volumes/Data/<data-relative>/<tail>`, matching whole path
    /// components and preferring the longest path on `/` (the first one in
    /// file order on a tie). `None` when `path` is not at or below a
    /// firmlink, or is already a Data volume path.
    #[must_use]
    pub fn to_data(&self, path: &Path) -> Option<PathBuf> {
        if is_data_volume_alias(path) {
            return None;
        }
        let mut best: Option<(usize, &Path, &Path)> = None;
        for (root, data_rel) in &self.entries {
            if let Ok(tail) = path.strip_prefix(root) {
                let depth = root.components().count();
                if best.is_none_or(|(d, _, _)| depth > d) {
                    best = Some((depth, data_rel, tail));
                }
            }
        }
        best.map(|(_, data_rel, tail)| {
            let base = Path::new(DATA_VOLUME).join(data_rel);
            if tail.as_os_str().is_empty() {
                base
            } else {
                base.join(tail)
            }
        })
    }
}

/// True for `/System/Volumes/Data` itself and anything below it (compared
/// component by component). The walker never descends into such a path
/// from `/`: everything it holds is reached through the firmlinks.
#[must_use]
pub fn is_data_volume_alias(path: &Path) -> bool {
    path.starts_with(DATA_VOLUME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_data_inverts_to_root() {
        let map = FirmlinkMap::fallback();
        let alias = map
            .to_data(Path::new("/Users/tester/Documents"))
            .expect("under /Users");
        assert_eq!(
            alias,
            PathBuf::from("/System/Volumes/Data/Users/tester/Documents")
        );
        assert_eq!(
            map.to_root(&alias),
            PathBuf::from("/Users/tester/Documents")
        );
        assert_eq!(
            map.to_data(Path::new("/Volumes")),
            Some(PathBuf::from("/System/Volumes/Data/Volumes"))
        );
        // Not below a firmlink, or already a Data volume path.
        assert_eq!(map.to_data(Path::new("/bin/ls")), None);
        assert_eq!(map.to_data(Path::new("/Users2/x")), None);
        assert_eq!(map.to_data(&alias), None);
    }
}
