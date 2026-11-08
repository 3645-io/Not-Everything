//! Platform-agnostic exclusion engine.
//!
//! An [`ExclusionConfig`] lists what to leave out of the index; it is
//! compiled once into [`Exclusions`], which a walker asks about every entry
//! with [`Exclusions::check`] *before* descending into a directory.
//!
//! Clean-room: written from docs/PLAN.md (the "Exclusions (hidden, system,
//! folders, wildcards, regex) and include-only folders" feature row) and
//! voidtools' public Indexes support page only.
//!
//! # Rules
//!
//! An entry is excluded when any of these holds:
//!
//! - it is hidden and [`ExclusionConfig::exclude_hidden`] is set;
//! - it is a system entry and [`ExclusionConfig::exclude_system`] is set;
//! - its path equals, or lies under, one of
//!   [`ExclusionConfig::excluded_folders`];
//! - one of [`ExclusionConfig::wildcard_patterns`] matches it;
//! - one of [`ExclusionConfig::regex_patterns`] matches its name.
//!
//! When [`ExclusionConfig::include_only`] is not empty, only entries equal to
//! or under one of its roots are indexed. Directories that are ancestors of a
//! root are traversed (so the walker can reach the root) but not emitted:
//! [`Verdict::TraverseOnly`]. Everything else is skipped. Exclusions always
//! win over traverse-only, and they also apply inside include-only roots.
//!
//! An excluded directory gives [`Verdict::SkipSubtree`]; an excluded file
//! gives [`Verdict::SkipEntry`].
//!
//! # Path matching
//!
//! Excluded folders and include-only roots match by path component, so
//! `/a/b` covers `/a/b` and `/a/b/c` but not `/a/bc`. Because they compare
//! prefixes, they also give the right answer for a single path checked
//! out of walk order (for example a path reported by a change source).
//! Config paths should be absolute and free of `..`; `.` components and
//! trailing or repeated separators are ignored.
//!
//! Wildcard and regex patterns test the entry itself only, not its
//! ancestors: a walker never descends into an excluded directory, so its
//! descendants are pruned with it.
//!
//! # Wildcards
//!
//! A wildcard pattern must match the whole name: `*` matches any run of
//! characters and `?` exactly one; every other character is literal. A
//! pattern containing `/` is matched against the full path instead of the
//! name. In such a pattern `*` and `?` do not cross `/`, while `**` does:
//! `**/node_modules` matches a `node_modules` entry at any depth,
//! `/data/**/cache` matches `/data/cache` and `/data/x/y/cache`, and
//! `/scratch/**` matches everything under `/scratch`.
//!
//! # Regex
//!
//! Regex patterns use the Rust `regex` dialect (no look-around or
//! back-references) and search the entry name, unanchored: use `^` and `$`
//! to anchor.
//!
//! # Case
//!
//! Matching is case-insensitive by default because APFS volumes are
//! case-insensitive by default; set [`ExclusionConfig::case_sensitive`] for
//! case-sensitive volumes.
//!
//! Matching folds case but does not normalise Unicode. APFS is
//! normalisation-insensitive but preserves the spelling a name was created
//! with, so the same name can be stored, or typed into a rule, as NFC
//! (`Café` with a precomposed `é`, U+00E9) or NFD (`e` followed by the
//! combining acute accent U+0301). Those two spellings don't match each
//! other in [`excluded_folders`](ExclusionConfig::excluded_folders),
//! [`include_only`](ExclusionConfig::include_only),
//! [wildcard](ExclusionConfig::wildcard_patterns) or
//! [regex](ExclusionConfig::regex_patterns) rules: a rule only matches
//! names spelled in the same normalisation form as the rule.

use std::borrow::Cow;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use regex::{RegexBuilder, RegexSet, RegexSetBuilder};

/// What to leave out of the index. [`Default`] excludes nothing.
///
/// Matching folds case (unless [`case_sensitive`](Self::case_sensitive) is
/// set) but does not normalise Unicode. APFS is normalisation-insensitive,
/// so the NFC and NFD spellings of the same name (e.g. `Café` with a
/// precomposed `é` versus `e` plus a combining accent) are the same file on
/// disk, yet they won't match each other in `excluded_folders`,
/// `include_only`, wildcard or regex rules. See the module documentation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExclusionConfig {
    /// Skip entries flagged hidden (and, for directories, their subtrees).
    pub exclude_hidden: bool,
    /// Skip entries flagged as system entries (and their subtrees).
    pub exclude_system: bool,
    /// Folders whose whole subtree, the folder included, is skipped.
    pub excluded_folders: Vec<PathBuf>,
    /// When not empty, only these folders and their contents are indexed.
    /// Empty means everything.
    pub include_only: Vec<PathBuf>,
    /// Wildcard patterns (`*`, `?`, and `**` in patterns containing `/`).
    /// See the module documentation.
    pub wildcard_patterns: Vec<String>,
    /// Regex patterns searched in the entry name.
    pub regex_patterns: Vec<String>,
    /// Match names and paths case-sensitively. Off by default, as APFS is
    /// case-insensitive by default.
    ///
    /// Case-insensitive matching folds case only; it does not normalise
    /// Unicode, so NFC and NFD spellings of the same name (e.g. `Café`)
    /// still don't match each other, although APFS treats them as the same
    /// name.
    pub case_sensitive: bool,
}

/// The answer for one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Verdict {
    /// Emit the entry, and descend into it if it is a directory.
    Include,
    /// Don't emit this entry (a file, or anything that is not descended
    /// into).
    SkipEntry,
    /// Don't emit this directory and don't descend into it.
    SkipSubtree,
    /// A directory that is an ancestor of an include-only root: descend into
    /// it but don't emit it.
    TraverseOnly,
}

impl Verdict {
    /// Whether the entry itself should be emitted.
    pub fn emits(self) -> bool {
        self == Verdict::Include
    }

    /// Whether a walker should descend into the entry (if it is a directory).
    pub fn descends(self) -> bool {
        matches!(self, Verdict::Include | Verdict::TraverseOnly)
    }
}

/// Why an [`ExclusionConfig`] could not be compiled.
#[derive(Debug)]
pub enum ExclusionError {
    /// A regex pattern failed to compile.
    InvalidRegex {
        /// The offending pattern.
        pattern: String,
        /// The parser's error.
        source: regex::Error,
    },
    /// A wildcard pattern is invalid.
    InvalidWildcard {
        /// The offending pattern.
        pattern: String,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// A folder list contains an empty path, which would match everything.
    EmptyFolder {
        /// The config field holding the empty path.
        field: &'static str,
    },
    /// The patterns are valid one by one but too large to compile together.
    TooComplex {
        /// Which pattern list is too large.
        field: &'static str,
        /// The compiler's error.
        source: regex::Error,
    },
}

impl fmt::Display for ExclusionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExclusionError::InvalidRegex { pattern, source } => {
                write!(f, "invalid regex exclusion {pattern:?}: {source}")
            }
            ExclusionError::InvalidWildcard { pattern, reason } => {
                write!(f, "invalid wildcard exclusion {pattern:?}: {reason}")
            }
            ExclusionError::EmptyFolder { field } => {
                write!(f, "empty path in {field}")
            }
            ExclusionError::TooComplex { field, source } => {
                write!(f, "{field} too large to compile: {source}")
            }
        }
    }
}

impl std::error::Error for ExclusionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ExclusionError::InvalidRegex { source, .. }
            | ExclusionError::TooComplex { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// A compiled [`ExclusionConfig`]. Cheap to query, `Send + Sync`, so one
/// instance can serve parallel walkers.
#[derive(Debug, Clone)]
pub struct Exclusions {
    exclude_hidden: bool,
    exclude_system: bool,
    case_sensitive: bool,
    /// Excluded folders as component lists (folded when case-insensitive).
    excluded_folders: Vec<Vec<String>>,
    /// Include-only roots as component lists (folded when case-insensitive).
    include_only: Vec<Vec<String>>,
    /// Wildcards without `/`, matched against the name.
    name_wildcards: Option<RegexSet>,
    /// Wildcards with `/`, matched against the full path.
    path_wildcards: Option<RegexSet>,
    /// User regexes, searched in the name.
    name_regexes: Option<RegexSet>,
}

/// How a path relates to a folder rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Relation {
    /// The path is the folder or lies under it.
    Inside,
    /// The path is a strict ancestor of the folder.
    Ancestor,
    /// Neither.
    Unrelated,
}

impl Exclusions {
    /// Compiles `cfg`. An invalid regex or wildcard, or an empty folder
    /// path, is an error.
    pub fn new(cfg: ExclusionConfig) -> Result<Exclusions, ExclusionError> {
        let cs = cfg.case_sensitive;
        let excluded_folders = folder_rules(&cfg.excluded_folders, cs, "excluded_folders")?;
        let include_only = folder_rules(&cfg.include_only, cs, "include_only")?;

        let mut name_wc = Vec::new();
        let mut path_wc = Vec::new();
        for pattern in &cfg.wildcard_patterns {
            if pattern.is_empty() {
                return Err(ExclusionError::InvalidWildcard {
                    pattern: pattern.clone(),
                    reason: "empty pattern",
                });
            }
            if pattern.contains('/') {
                path_wc.push(wildcard_to_regex(pattern, true));
            } else {
                name_wc.push(wildcard_to_regex(pattern, false));
            }
        }

        // Validate each user regex on its own so the error names it.
        for pattern in &cfg.regex_patterns {
            RegexBuilder::new(pattern)
                .case_insensitive(!cs)
                .build()
                .map_err(|source| ExclusionError::InvalidRegex {
                    pattern: pattern.clone(),
                    source,
                })?;
        }

        Ok(Exclusions {
            exclude_hidden: cfg.exclude_hidden,
            exclude_system: cfg.exclude_system,
            case_sensitive: cs,
            excluded_folders,
            include_only,
            name_wildcards: build_set(&name_wc, cs, true, "wildcard_patterns")?,
            path_wildcards: build_set(&path_wc, cs, true, "wildcard_patterns")?,
            name_regexes: build_set(&cfg.regex_patterns, cs, false, "regex_patterns")?,
        })
    }

    /// Decides what to do with one entry.
    ///
    /// `path` is the full path of the entry, its own `name` included;
    /// `hidden` and `system` are the entry's platform flags.
    pub fn check(
        &self,
        path: &Path,
        name: &str,
        is_dir: bool,
        hidden: bool,
        system: bool,
    ) -> Verdict {
        let skip = if is_dir {
            Verdict::SkipSubtree
        } else {
            Verdict::SkipEntry
        };

        if (self.exclude_hidden && hidden) || (self.exclude_system && system) {
            return skip;
        }

        // Include-only first: outside every root is a skip either way.
        let mut traverse_only = false;
        if !self.include_only.is_empty() {
            let mut ancestor = false;
            let mut inside = false;
            for root in &self.include_only {
                match self.relation(path, root) {
                    Relation::Inside => {
                        inside = true;
                        break;
                    }
                    Relation::Ancestor => ancestor = true,
                    Relation::Unrelated => {}
                }
            }
            if !inside {
                if ancestor && is_dir {
                    traverse_only = true;
                } else {
                    return skip;
                }
            }
        }

        if self
            .excluded_folders
            .iter()
            .any(|f| self.relation(path, f) == Relation::Inside)
        {
            return skip;
        }

        if let Some(set) = &self.name_wildcards {
            if set.is_match(name) {
                return skip;
            }
        }
        if let Some(set) = &self.name_regexes {
            if set.is_match(name) {
                return skip;
            }
        }
        if let Some(set) = &self.path_wildcards {
            if set.is_match(&path.to_string_lossy()) {
                return skip;
            }
        }

        if traverse_only {
            Verdict::TraverseOnly
        } else {
            Verdict::Include
        }
    }

    /// How `path` relates to the folder rule `rule` (a component list).
    fn relation(&self, path: &Path, rule: &[String]) -> Relation {
        let mut comps = significant_components(path);
        for want in rule {
            match comps.next() {
                None => return Relation::Ancestor,
                Some(c) if self.component_eq(c, want) => {}
                Some(_) => return Relation::Unrelated,
            }
        }
        Relation::Inside
    }

    /// Whether path component `comp` equals rule component `want`, which is
    /// already folded when matching is case-insensitive.
    fn component_eq(&self, comp: &OsStr, want: &str) -> bool {
        let s = comp.to_string_lossy();
        if self.case_sensitive {
            s == want
        } else if s.is_ascii() && want.is_ascii() {
            s.eq_ignore_ascii_case(want)
        } else {
            fold(&s) == want
        }
    }
}

/// The components of `path` that matter for matching (`.` dropped).
fn significant_components(path: &Path) -> impl Iterator<Item = &OsStr> {
    path.components()
        .filter(|c| !matches!(c, Component::CurDir))
        .map(Component::as_os_str)
}

/// Case-folds `s` for case-insensitive comparison.
fn fold(s: &str) -> Cow<'_, str> {
    if s.chars().any(char::is_uppercase) {
        Cow::Owned(s.to_lowercase())
    } else {
        Cow::Borrowed(s)
    }
}

/// Turns folder paths into component lists, folded when case-insensitive.
fn folder_rules(
    folders: &[PathBuf],
    case_sensitive: bool,
    field: &'static str,
) -> Result<Vec<Vec<String>>, ExclusionError> {
    folders
        .iter()
        .map(|p| {
            let comps: Vec<String> = significant_components(p)
                .map(|c| {
                    let s = c.to_string_lossy();
                    if case_sensitive {
                        s.into_owned()
                    } else {
                        fold(&s).into_owned()
                    }
                })
                .collect();
            if comps.is_empty() {
                Err(ExclusionError::EmptyFolder { field })
            } else {
                Ok(comps)
            }
        })
        .collect()
}

/// Translates a wildcard pattern into an anchored regex.
///
/// `*` becomes `[^/]*` and `?` becomes `[^/]`. With `crosses_slash` (the
/// pattern contains `/`), `**` matches across `/`: a `**/` at the start or
/// after `/` matches zero or more whole directories, any other `**` matches
/// any run of characters. Without it, `**` is the same as `*`.
fn wildcard_to_regex(pattern: &str, crosses_slash: bool) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::with_capacity(pattern.len() * 2 + 2);
    out.push('^');
    let mut i = 0;
    let mut buf = [0u8; 4];
    while i < chars.len() {
        match chars[i] {
            '*' => {
                let start = i;
                while i < chars.len() && chars[i] == '*' {
                    i += 1;
                }
                let double = i - start >= 2;
                if double && crosses_slash {
                    let after_sep = start == 0 || chars[start - 1] == '/';
                    if after_sep && chars.get(i) == Some(&'/') {
                        // `**/`: zero or more whole directories.
                        out.push_str("(?:.*/)?");
                        i += 1;
                    } else {
                        out.push_str(".*");
                    }
                } else {
                    out.push_str("[^/]*");
                }
                continue;
            }
            '?' => out.push_str("[^/]"),
            c => out.push_str(&regex::escape(c.encode_utf8(&mut buf))),
        }
        i += 1;
    }
    out.push('$');
    out
}

/// Compiles `patterns` into one set, or `None` when there are none.
fn build_set<S: AsRef<str>>(
    patterns: &[S],
    case_sensitive: bool,
    dot_all: bool,
    field: &'static str,
) -> Result<Option<RegexSet>, ExclusionError> {
    if patterns.is_empty() {
        return Ok(None);
    }
    RegexSetBuilder::new(patterns.iter().map(AsRef::as_ref))
        .case_insensitive(!case_sensitive)
        .dot_matches_new_line(dot_all)
        .build()
        .map(Some)
        .map_err(|source| ExclusionError::TooComplex { field, source })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_translation() {
        assert_eq!(wildcard_to_regex("*.tmp", false), r"^[^/]*\.tmp$");
        assert_eq!(wildcard_to_regex("a?c", false), "^a[^/]c$");
        assert_eq!(wildcard_to_regex("a**b", false), "^a[^/]*b$");
        assert_eq!(
            wildcard_to_regex("**/node_modules", true),
            "^(?:.*/)?node_modules$"
        );
        assert_eq!(
            wildcard_to_regex("/data/**/cache", true),
            "^/data/(?:.*/)?cache$"
        );
        assert_eq!(wildcard_to_regex("/scratch/**", true), "^/scratch/.*$");
        assert_eq!(wildcard_to_regex("/a/x**", true), "^/a/x.*$");
    }

    #[test]
    fn double_star_matches_zero_directories() {
        let ex = Exclusions::new(ExclusionConfig {
            wildcard_patterns: vec!["/data/**/cache".into()],
            ..Default::default()
        })
        .unwrap();
        let p = Path::new("/data/cache");
        assert_eq!(
            ex.check(p, "cache", true, false, false),
            Verdict::SkipSubtree
        );
    }

    #[test]
    fn folder_matching_is_case_insensitive_by_default() {
        let ex = Exclusions::new(ExclusionConfig {
            excluded_folders: vec!["/Users/Me/Ünïcode".into()],
            ..Default::default()
        })
        .unwrap();
        let p = Path::new("/users/me/üNÏcode/x");
        assert_eq!(ex.check(p, "x", false, false, false), Verdict::SkipEntry);

        let ex = Exclusions::new(ExclusionConfig {
            excluded_folders: vec!["/Users/Me".into()],
            case_sensitive: true,
            ..Default::default()
        })
        .unwrap();
        let p = Path::new("/Users/me/x");
        assert_eq!(ex.check(p, "x", false, false, false), Verdict::Include);
    }

    #[test]
    fn empty_inputs_are_errors() {
        let empty_folder = Exclusions::new(ExclusionConfig {
            excluded_folders: vec![PathBuf::new()],
            ..Default::default()
        });
        assert!(matches!(
            empty_folder,
            Err(ExclusionError::EmptyFolder { .. })
        ));
        let empty_root = Exclusions::new(ExclusionConfig {
            include_only: vec![PathBuf::from(".")],
            ..Default::default()
        });
        assert!(matches!(
            empty_root,
            Err(ExclusionError::EmptyFolder { .. })
        ));
        let empty_wildcard = Exclusions::new(ExclusionConfig {
            wildcard_patterns: vec![String::new()],
            ..Default::default()
        });
        assert!(matches!(
            empty_wildcard,
            Err(ExclusionError::InvalidWildcard { .. })
        ));
    }

    #[test]
    fn literal_regex_metacharacters_in_wildcards() {
        let ex = Exclusions::new(ExclusionConfig {
            wildcard_patterns: vec!["a+(b).[c]".into()],
            ..Default::default()
        })
        .unwrap();
        let p = Path::new("/x/a+(b).[c]");
        assert_eq!(
            ex.check(p, "a+(b).[c]", false, false, false),
            Verdict::SkipEntry
        );
        let p = Path::new("/x/aa(b)x[c]");
        assert_eq!(
            ex.check(p, "aa(b)x[c]", false, false, false),
            Verdict::Include
        );
    }

    #[test]
    fn include_only_root_at_filesystem_root_includes_everything() {
        let ex = Exclusions::new(ExclusionConfig {
            include_only: vec!["/".into()],
            ..Default::default()
        })
        .unwrap();
        let p = Path::new("/a/b");
        assert_eq!(ex.check(p, "b", false, false, false), Verdict::Include);
        assert_eq!(
            ex.check(Path::new("/"), "", true, false, false),
            Verdict::Include
        );
    }
}
