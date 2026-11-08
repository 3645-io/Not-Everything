//! Synthetic file names and a compact, NUL-separated name buffer.
//!
//! [`NameGen`] produces file and directory names that resemble a macOS
//! startup volume: lowercase words joined by separators, CamelCase, camera
//! and screenshot counters, hashes, dates, dotfiles and versioned libraries,
//! most with an extension from a weighted table. About 1 in 80,000 names is
//! non-ASCII (accented Latin in both NFC and NFD forms, CJK, Hangul,
//! Cyrillic). The mean length is calibrated to the plan's measured 17.3
//! bytes.
//!
//! All tables are small constants written for this project. Every random
//! decision goes through [`Rng`], so output depends only on the seed.

use std::fmt::Write as _;

use crate::rng::Rng;

/// Probability that a name is non-ASCII: 1 in this many.
const NON_ASCII_ONE_IN: u64 = 80_000;

/// Lowercase words used to build most names.
const WORDS: &[&str] = &[
    "app", "archive", "asset", "audio", "backup", "base", "bin", "blob", "book", "build", "bundle",
    "cache", "calendar", "chart", "client", "cloud", "code", "color", "common", "config", "core",
    "data", "debug", "default", "demo", "design", "dev", "device", "diff", "doc", "draft",
    "editor", "engine", "event", "export", "extra", "face", "file", "final", "font", "frame",
    "game", "graph", "guide", "handler", "header", "help", "home", "icon", "image", "import",
    "index", "info", "input", "item", "kernel", "key", "layer", "layout", "lib", "light", "link",
    "list", "local", "lock", "log", "main", "manager", "map", "media", "menu", "meta", "model",
    "module", "movie", "music", "net", "node", "note", "object", "old", "output", "package",
    "page", "panel", "parser", "patch", "photo", "plugin", "preview", "print", "profile",
    "project", "proxy", "query", "queue", "readme", "record", "release", "report", "resource",
    "result", "sample", "scene", "schema", "script", "search", "server", "service", "session",
    "setting", "shader", "shared", "sheet", "sound", "source", "split", "stack", "state", "store",
    "style", "sync", "system", "table", "task", "temp", "template", "test", "text", "theme",
    "thumb", "token", "tool", "track", "tree", "type", "update", "user", "util", "value", "vector",
    "video", "view", "voice", "watch", "web", "widget", "window", "work", "zone",
];

/// Short CamelCase prefixes seen in framework and app code.
const CAMEL_PREFIXES: &[&str] = &["NS", "UI", "CG", "CF", "AV", "MT", "FX", "WK"];

/// Prefixes of counter-style names such as camera files.
const COUNTER_PREFIXES: &[&str] = &[
    "IMG_",
    "DSC",
    "DSC_",
    "Screenshot ",
    "scan_",
    "frame",
    "page-",
    "track",
    "part",
    "chunk-",
    "tmp",
    "photo ",
    "Untitled ",
    "v",
];

/// Whole names of common dotfiles (never get an extension).
const DOTFILES: &[&str] = &[
    ".gitignore",
    ".DS_Store",
    ".env",
    ".bashrc",
    ".zshrc",
    ".editorconfig",
    ".npmrc",
    ".localized",
    ".gitkeep",
    ".gitattributes",
    ".prettierrc",
    ".swiftlint.yml",
];

/// Dot-directory names.
const DOTDIRS: &[&str] = &[
    ".git", ".cache", ".config", ".vscode", ".idea", ".npm", ".cargo", ".Trash", ".ssh", ".local",
];

/// Capitalised file stems such as build and project files.
const UPPER_FILES: &[&str] = &[
    "README",
    "LICENSE",
    "Makefile",
    "Dockerfile",
    "CHANGELOG",
    "Info",
    "PkgInfo",
    "CodeResources",
    "Podfile",
    "Gemfile",
    "AUTHORS",
    "NOTICE",
];

/// Capitalised directory names typical of macOS volumes and bundles.
const UPPER_DIRS: &[&str] = &[
    "Library",
    "Applications",
    "Documents",
    "Desktop",
    "Downloads",
    "Resources",
    "Contents",
    "MacOS",
    "Frameworks",
    "Headers",
    "Versions",
    "Current",
    "Caches",
    "Preferences",
];

/// Bundle-like directory suffixes.
const DIR_SUFFIXES: &[&str] = &[
    "app",
    "bundle",
    "framework",
    "lproj",
    "xcassets",
    "d",
    "photoslibrary",
];

/// Weighted file extensions (weights are relative).
const EXTENSIONS: &[(&str, u32)] = &[
    ("png", 90),
    ("js", 80),
    ("h", 70),
    ("json", 60),
    ("txt", 55),
    ("jpg", 55),
    ("plist", 45),
    ("rs", 35),
    ("c", 35),
    ("md", 35),
    ("py", 35),
    ("swift", 30),
    ("html", 30),
    ("ts", 30),
    ("svg", 30),
    ("xml", 30),
    ("o", 25),
    ("strings", 25),
    ("dylib", 20),
    ("css", 20),
    ("log", 20),
    ("pdf", 20),
    ("m", 20),
    ("cpp", 15),
    ("gz", 15),
    ("yml", 15),
    ("sh", 15),
    ("nib", 15),
    ("toml", 10),
    ("zip", 10),
    ("mp3", 10),
    ("a", 10),
    ("heic", 10),
    ("dat", 10),
    ("db", 8),
    ("mov", 5),
    ("car", 5),
    ("ttf", 5),
];

/// First labels of reverse-DNS identifiers.
const REVERSE_DNS_TLDS: &[(&str, u32)] = &[("com", 70), ("org", 15), ("io", 10), ("net", 5)];

/// Extensions of reverse-DNS named files.
const REVERSE_DNS_EXTENSIONS: &[(&str, u32)] =
    &[("plist", 70), ("db", 10), ("log", 10), ("json", 10)];

/// Library extensions for versioned library names.
const LIB_EXTENSIONS: &[(&str, u32)] = &[("dylib", 60), ("a", 25), ("tbd", 15)];

/// Non-ASCII words. Accented Latin words appear in both NFC (precomposed)
/// and NFD (base letter plus combining mark) forms; escapes make the form
/// explicit.
const NON_ASCII_WORDS: &[&str] = &[
    "caf\u{e9}",                                  // café, NFC
    "cafe\u{301}",                                // café, NFD
    "r\u{e9}sum\u{e9}",                           // résumé, NFC
    "re\u{301}sume\u{301}",                       // résumé, NFD
    "na\u{ef}ve",                                 // naïve, NFC
    "nai\u{308}ve",                               // naïve, NFD
    "\u{c5}ngstr\u{f6}m",                         // Ångström, NFC
    "A\u{30a}ngstro\u{308}m",                     // Ångström, NFD
    "M\u{fc}nchen",                               // München, NFC
    "Mu\u{308}nchen",                             // München, NFD
    "\u{6587}\u{66f8}",                           // CJK "document"
    "\u{5199}\u{771f}",                           // CJK "photo"
    "\u{62a5}\u{544a}",                           // CJK "report"
    "\u{d55c}\u{ae00}",                           // Hangul
    "\u{41f}\u{440}\u{438}\u{432}\u{435}\u{442}", // Cyrillic
];

/// Lowercase and uppercase hex digits.
const HEX_LOWER: &[u8; 16] = b"0123456789abcdef";
const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";

/// Separators between words, weighted. The empty separator concatenates.
const SEPARATORS: &[(&str, u32)] = &[("_", 26), ("-", 22), (" ", 18), (".", 6), ("", 8)];

/// Kinds of file name, weighted.
#[derive(Clone, Copy)]
enum FileKind {
    Words,
    ReverseDns,
    Camel,
    Counter,
    Hex,
    Date,
    Upper,
    Dotfile,
    Library,
    Numeric,
}

const FILE_KINDS: &[(FileKind, u32)] = &[
    (FileKind::Words, 40),
    (FileKind::ReverseDns, 5),
    (FileKind::Camel, 18),
    (FileKind::Counter, 12),
    (FileKind::Hex, 6),
    (FileKind::Date, 5),
    (FileKind::Upper, 5),
    (FileKind::Dotfile, 3),
    (FileKind::Library, 4),
    (FileKind::Numeric, 2),
];

/// Kinds of directory name, weighted.
#[derive(Clone, Copy)]
enum DirKind {
    Words,
    ReverseDns,
    Camel,
    Counter,
    Hex,
    Date,
    Dotdir,
    Upper,
}

const DIR_KINDS: &[(DirKind, u32)] = &[
    (DirKind::Words, 45),
    (DirKind::ReverseDns, 5),
    (DirKind::Camel, 25),
    (DirKind::Counter, 8),
    (DirKind::Hex, 6),
    (DirKind::Date, 4),
    (DirKind::Dotdir, 4),
    (DirKind::Upper, 3),
];

/// Deterministic generator of synthetic file and directory names.
///
/// The same seed always yields the same sequence of names.
#[derive(Clone, Debug)]
pub struct NameGen {
    rng: Rng,
}

impl NameGen {
    /// Creates a generator from a seed.
    pub fn new(seed: u64) -> Self {
        Self {
            rng: Rng::new(seed),
        }
    }

    /// Appends one file name to `out`.
    pub fn write_file_name(&mut self, out: &mut String) {
        let start = out.len();
        if self.rng.below(NON_ASCII_ONE_IN) == 0 {
            self.non_ascii(out, true);
        } else {
            match *self.rng.weighted(FILE_KINDS) {
                FileKind::Words => {
                    self.words(out);
                    self.maybe_ext(out, 820_000);
                }
                FileKind::ReverseDns => {
                    self.reverse_dns(out);
                    out.push('.');
                    out.push_str(self.rng.weighted(REVERSE_DNS_EXTENSIONS));
                }
                FileKind::Camel => {
                    self.camel(out);
                    self.maybe_ext(out, 860_000);
                }
                FileKind::Counter => {
                    self.counter(out);
                    self.maybe_ext(out, 950_000);
                }
                FileKind::Hex => {
                    self.hex(out, false);
                    self.maybe_ext(out, 400_000);
                }
                FileKind::Date => {
                    self.date(out);
                    self.maybe_ext(out, 700_000);
                }
                FileKind::Upper => {
                    out.push_str(self.rng.pick(UPPER_FILES));
                    self.maybe_ext(out, 300_000);
                }
                FileKind::Dotfile => out.push_str(self.rng.pick(DOTFILES)),
                FileKind::Library => self.library(out),
                FileKind::Numeric => {
                    let n = self.rng.below(1_000);
                    let _ = write!(out, "{n}");
                    self.maybe_ext(out, 500_000);
                }
            }
        }
        debug_assert!(valid_name(&out[start..]), "bad name {:?}", &out[start..]);
    }

    /// Appends one directory name to `out`.
    pub fn write_dir_name(&mut self, out: &mut String) {
        let start = out.len();
        if self.rng.below(NON_ASCII_ONE_IN) == 0 {
            self.non_ascii(out, false);
        } else {
            match *self.rng.weighted(DIR_KINDS) {
                DirKind::Words => {
                    self.words(out);
                    if self.rng.chance(80_000) {
                        out.push('.');
                        out.push_str(self.rng.pick(DIR_SUFFIXES));
                    }
                }
                DirKind::ReverseDns => self.reverse_dns(out),
                DirKind::Camel => self.camel(out),
                DirKind::Counter => self.counter(out),
                DirKind::Hex => self.hex(out, true),
                DirKind::Date => self.date(out),
                DirKind::Dotdir => out.push_str(self.rng.pick(DOTDIRS)),
                DirKind::Upper => out.push_str(self.rng.pick(UPPER_DIRS)),
            }
        }
        debug_assert!(valid_name(&out[start..]), "bad name {:?}", &out[start..]);
    }

    /// Returns the next file name as a new `String`.
    pub fn file_name(&mut self) -> String {
        let mut s = String::new();
        self.write_file_name(&mut s);
        s
    }

    /// Returns the next directory name as a new `String`.
    pub fn dir_name(&mut self) -> String {
        let mut s = String::new();
        self.write_dir_name(&mut s);
        s
    }

    /// Appends `.ext` with probability `p_per_million`.
    fn maybe_ext(&mut self, out: &mut String, p_per_million: u32) {
        if self.rng.chance(p_per_million) {
            out.push('.');
            out.push_str(self.rng.weighted(EXTENSIONS));
        }
    }

    /// Lowercase or Title Case words joined by one separator, with an
    /// optional numeric suffix.
    fn words(&mut self, out: &mut String) {
        let count = match self.rng.below(100) {
            0..12 => 1,
            12..48 => 2,
            48..84 => 3,
            _ => 4,
        };
        let sep = *self.rng.weighted(SEPARATORS);
        let title = self.rng.chance(150_000);
        for i in 0..count {
            if i > 0 {
                out.push_str(sep);
            }
            let w = *self.rng.pick(WORDS);
            if title {
                push_capitalized(out, w);
            } else {
                out.push_str(w);
            }
        }
        if self.rng.chance(220_000) {
            out.push_str(if sep.is_empty() || sep == "." {
                "_"
            } else {
                sep
            });
            self.number_suffix(out);
        }
    }

    /// A short number: one or two digits, `v` plus a digit, or a year.
    fn number_suffix(&mut self, out: &mut String) {
        let _ = match self.rng.below(4) {
            0 => write!(out, "{}", self.rng.below(10)),
            1 => write!(out, "{}", self.rng.below(100)),
            2 => write!(out, "v{}", 1 + self.rng.below(9)),
            _ => write!(out, "{}", 1995 + self.rng.below(31)),
        };
    }

    /// CamelCase from two or three words, sometimes lower camelCase or with
    /// a framework prefix.
    fn camel(&mut self, out: &mut String) {
        let count = 2 + self.rng.below(3);
        let lower_first = self.rng.chance(250_000);
        if !lower_first && self.rng.chance(120_000) {
            out.push_str(self.rng.pick(CAMEL_PREFIXES));
        }
        for i in 0..count {
            let w = *self.rng.pick(WORDS);
            if i == 0 && lower_first {
                out.push_str(w);
            } else {
                push_capitalized(out, w);
            }
        }
        if self.rng.chance(100_000) {
            let _ = write!(out, "{}", self.rng.below(100));
        }
    }

    /// Counter-style names such as `IMG_0042`.
    fn counter(&mut self, out: &mut String) {
        out.push_str(self.rng.pick(COUNTER_PREFIXES));
        let width = 2 + self.rng.below(4) as usize;
        let n = self.rng.below(10u64.pow(width as u32));
        let _ = write!(out, "{n:0width$}");
    }

    /// Hash-like names: hex strings or UUIDs. Directory hashes are often the
    /// two-character fan-out directories of caches and object stores.
    fn hex(&mut self, out: &mut String, dir: bool) {
        if dir && self.rng.chance(500_000) {
            self.hex_digits(out, 2, HEX_LOWER);
            return;
        }
        if self.rng.chance(250_000) {
            for (i, len) in [8usize, 4, 4, 4, 12].into_iter().enumerate() {
                if i > 0 {
                    out.push('-');
                }
                self.hex_digits(out, len, HEX_UPPER);
            }
            return;
        }
        let len = *self.rng.pick(&[8usize, 8, 12, 16, 16, 20, 32, 40]);
        self.hex_digits(out, len, HEX_LOWER);
    }

    fn hex_digits(&mut self, out: &mut String, len: usize, digits: &[u8; 16]) {
        for _ in 0..len {
            out.push(char::from(digits[self.rng.index(16)]));
        }
    }

    /// Date-stamped names such as `2021-08-03 notes` or `20210803_142501`.
    fn date(&mut self, out: &mut String) {
        let y = 1995 + self.rng.below(31);
        let m = 1 + self.rng.below(12);
        let d = 1 + self.rng.below(28);
        if self.rng.chance(300_000) {
            let hh = self.rng.below(24);
            let mm = self.rng.below(60);
            let ss = self.rng.below(60);
            let _ = write!(out, "{y}{m:02}{d:02}_{hh:02}{mm:02}{ss:02}");
            return;
        }
        let _ = write!(out, "{y}-{m:02}-{d:02}");
        if self.rng.chance(500_000) {
            out.push_str(if self.rng.chance(500_000) { " " } else { "_" });
            out.push_str(self.rng.pick(WORDS));
        }
    }

    /// Reverse-DNS identifiers such as `com.example.cache-tool`, common in
    /// preference, cache and container names.
    fn reverse_dns(&mut self, out: &mut String) {
        out.push_str(self.rng.weighted(REVERSE_DNS_TLDS));
        let parts = 2 + self.rng.below(2);
        for _ in 0..parts {
            out.push('.');
            out.push_str(self.rng.pick(WORDS));
            if self.rng.chance(150_000) {
                out.push('-');
                out.push_str(self.rng.pick(WORDS));
            }
        }
    }

    /// Versioned libraries such as `libcache.1.2.dylib`.
    fn library(&mut self, out: &mut String) {
        out.push_str("lib");
        out.push_str(self.rng.pick(WORDS));
        match self.rng.below(3) {
            0 => {}
            1 => {
                let _ = write!(out, ".{}", self.rng.below(10));
            }
            _ => {
                let _ = write!(out, ".{}.{}", self.rng.below(10), self.rng.below(20));
            }
        }
        out.push('.');
        out.push_str(self.rng.weighted(LIB_EXTENSIONS));
    }

    /// A non-ASCII word, optionally joined to an ASCII word, with an
    /// extension for files.
    fn non_ascii(&mut self, out: &mut String, file: bool) {
        out.push_str(self.rng.pick(NON_ASCII_WORDS));
        if self.rng.chance(500_000) {
            out.push_str(self.rng.weighted(SEPARATORS));
            out.push_str(self.rng.pick(WORDS));
        }
        if file {
            self.maybe_ext(out, 700_000);
        }
    }
}

/// Appends `word` with its first ASCII letter upper-cased.
fn push_capitalized(out: &mut String, word: &str) {
    let mut chars = word.chars();
    if let Some(first) = chars.next() {
        out.push(first.to_ascii_uppercase());
        out.push_str(chars.as_str());
    }
}

/// A name the generator may emit: non-empty, not `.` or `..`, and free of
/// `/` and NUL.
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0'])
}

/// A list of names stored in one contiguous buffer.
///
/// Each name is followed by a NUL byte; `offsets()[i]` is the start of name
/// `i` in `buffer()`. Names are UTF-8 and never contain NUL.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Names {
    buf: Vec<u8>,
    offsets: Vec<u32>,
}

impl Names {
    /// Creates an empty list.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an empty list with room for `names` names totalling
    /// `bytes` bytes (separators excluded).
    pub fn with_capacity(names: usize, bytes: usize) -> Self {
        Self {
            buf: Vec::with_capacity(bytes + names),
            offsets: Vec::with_capacity(names),
        }
    }

    /// Appends a name.
    ///
    /// # Panics
    ///
    /// Panics if `name` contains NUL or the buffer would exceed 4 GiB
    /// (offsets are `u32`).
    pub fn push(&mut self, name: &str) {
        assert!(!name.contains('\0'), "names cannot contain NUL");
        let start = u32::try_from(self.buf.len()).expect("name buffer exceeds u32 offsets");
        u32::try_from(self.buf.len() + name.len() + 1).expect("name buffer exceeds u32 offsets");
        self.offsets.push(start);
        self.buf.extend_from_slice(name.as_bytes());
        self.buf.push(0);
    }

    /// Number of names.
    pub fn len(&self) -> usize {
        self.offsets.len()
    }

    /// Whether the list is empty.
    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }

    /// Returns name `i`.
    ///
    /// # Panics
    ///
    /// Panics if `i >= len()`.
    pub fn get(&self, i: usize) -> &str {
        let start = self.offsets[i] as usize;
        let end = match self.offsets.get(i + 1) {
            Some(&next) => next as usize - 1,
            None => self.buf.len() - 1,
        };
        let bytes = &self.buf[start..end];
        debug_assert!(std::str::from_utf8(bytes).is_ok(), "name {i} is not UTF-8");
        // SAFETY: `buf` is private and only ever grows in `push`, which
        // appends the bytes of a `&str` (valid UTF-8) followed by one ASCII
        // NUL. `offsets[i]` is the start of name `i`, and `end` is the
        // position of its NUL separator, so `start..end` covers exactly the
        // bytes of one pushed `&str`, which begin and end on character
        // boundaries. No other code mutates `buf` or `offsets`.
        unsafe { std::str::from_utf8_unchecked(bytes) }
    }

    /// Iterates over the names in order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &str> + '_ {
        (0..self.len()).map(move |i| self.get(i))
    }

    /// Sum of the names' byte lengths, NUL separators excluded.
    pub fn total_bytes(&self) -> usize {
        self.buf.len() - self.offsets.len()
    }

    /// FNV-1a 64-bit checksum over `buffer()`.
    pub fn fnv1a64(&self) -> u64 {
        fnv1a64(&self.buf)
    }

    /// The NUL-separated name bytes.
    pub fn buffer(&self) -> &[u8] {
        &self.buf
    }

    /// Start offset of each name in `buffer()`.
    pub fn offsets(&self) -> &[u32] {
        &self.offsets
    }
}

/// FNV-1a 64-bit hash.
pub(crate) fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Generates `count` file names from `seed`.
///
/// Output is stable per seed on every platform. Changing it for an existing
/// seed is a breaking change: tests pin golden checksums.
pub fn generate_names(count: usize, seed: u64) -> Names {
    let mut name_gen = NameGen::new(seed);
    let mut names = Names::with_capacity(count, count.saturating_mul(18));
    let mut scratch = String::with_capacity(64);
    for _ in 0..count {
        scratch.clear();
        name_gen.write_file_name(&mut scratch);
        names.push(&scratch);
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_get_round_trip() {
        let mut n = Names::new();
        n.push("a");
        n.push("bc");
        n.push("caf\u{e9}");
        assert_eq!(n.len(), 3);
        assert_eq!(n.get(1), "bc");
        assert_eq!(n.get(2), "caf\u{e9}");
        assert_eq!(n.total_bytes(), 1 + 2 + 5);
        assert_eq!(n.buffer(), b"a\0bc\0caf\xc3\xa9\0");
    }

    #[test]
    fn dir_names_are_valid() {
        let mut g = NameGen::new(4);
        for _ in 0..50_000 {
            let n = g.dir_name();
            assert!(valid_name(&n), "{n:?}");
        }
    }

    #[test]
    fn dir_names_are_somewhat_shorter_than_file_names() {
        let mut g = NameGen::new(9);
        let dir_bytes: usize = (0..100_000).map(|_| g.dir_name().len()).sum();
        let mean_x100 = dir_bytes / 1_000;
        assert!((1_000..=1_800).contains(&mean_x100), "{mean_x100}");
    }
}
