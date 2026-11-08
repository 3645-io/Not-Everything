//! Type macros: `audio:`, `doc:`, `exe:`, `pic:`, `video:` and `zip:`.
//!
//! voidtools' public Searching page describes a handful of file-type
//! macros that stand for a set of extensions. We keep the same six
//! categories and fill each with our own list of common types, adding the
//! macOS formats a Mac user expects (`heic`, `pages`, `numbers`, `key`,
//! `app`, `pkg`, `dmg`, ...). The lists are ours, not copied from any help
//! text, and they may grow.
//!
//! | Macro    | What it covers                                                |
//! |----------|---------------------------------------------------------------|
//! | `audio:` | compressed and lossless audio, Apple formats (`m4a`, `caf`, `aiff`) |
//! | `doc:`   | office documents (Microsoft, OpenDocument, iWork), PDF, plain and marked-up text, e-books |
//! | `exe:`   | Windows executables and installers, scripts that launch, macOS `app` bundles and `pkg` installers |
//! | `pic:`   | raster images, HEIC/AVIF, camera raw, icons, SVG and layered editor files |
//! | `video:` | common video containers, Apple `mov`/`m4v`                     |
//! | `zip:`   | archives and compressed files, disk images (`dmg`, `iso`)      |
//!
//! Readings (our own; see the conformance suite, reading 19 and 20):
//!
//! - A bare macro (`pic:`) is exactly the filter `ext:` with the macro's
//!   list ([`Filter::Ext`]), so it matches by extension only,
//!   case-insensitively.
//! - A macro with an argument (`pic:holiday`) is that filter AND the
//!   argument as an ordinary term (wildcards and quotes behave as in any
//!   term; search modifiers in front of the macro apply to the term).
//! - Macro names are case-insensitive. A macro name inside quotes
//!   (`"pic:"`) is literal text, as is any other `name:` (`pics:`).
//! - Each list is lowercase, without leading dots and without duplicates.

use super::functions::Filter;

/// `audio:`: sound files.
const AUDIO: &[&str] = &[
    "mp3", "wav", "flac", "aac", "m4a", "ogg", "oga", "opus", "wma", "aif", "aiff", "aifc", "alac",
    "caf", "ape", "mid", "midi", "mka", "m4b", "mp2", "ra", "wv", "au",
];

/// `doc:`: documents, spreadsheets, presentations and text.
const DOC: &[&str] = &[
    "doc", "docx", "docm", "dot", "dotx", "pdf", "txt", "rtf", "rtfd", "md", "markdown", "xls",
    "xlsx", "xlsm", "xlsb", "csv", "tsv", "ppt", "pptx", "pptm", "pps", "ppsx", "pages", "numbers",
    "key", "odt", "ods", "odp", "epub", "htm", "html", "xml", "tex", "wpd", "wps",
];

/// `exe:`: programs, installers and launchable scripts.
const EXE: &[&str] = &[
    "exe", "bat", "cmd", "com", "msi", "msix", "ps1", "scr", "vbs", "app", "pkg", "mpkg", "command",
];

/// `pic:`: pictures.
const PIC: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "gif", "bmp", "tif", "tiff", "webp", "heic", "heif",
    "avif", "ico", "icns", "svg", "psd", "raw", "dng", "cr2", "cr3", "nef", "arw", "orf", "rw2",
    "raf", "tga",
];

/// `video:`: films and clips.
const VIDEO: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "wmv", "m4v", "webm", "flv", "mpg", "mpeg", "m2ts", "mts", "3gp",
    "ogv", "vob", "asf", "divx",
];

/// `zip:`: archives, compressed files and disk images.
const ZIP: &[&str] = &[
    "zip", "rar", "7z", "tar", "gz", "tgz", "bz2", "tbz2", "xz", "txz", "zst", "lz", "lzma", "cab",
    "iso", "dmg", "sit", "sitx", "xar", "cpio",
];

/// The extension list the macro `name` (case-insensitive, without the
/// `:`) expands to, or `None` if `name` is not a macro.
pub fn macro_extensions(name: &str) -> Option<&'static [&'static str]> {
    Some(match name.to_ascii_lowercase().as_str() {
        "audio" => AUDIO,
        "doc" => DOC,
        "exe" => EXE,
        "pic" => PIC,
        "video" => VIDEO,
        "zip" => ZIP,
        _ => return None,
    })
}

/// The [`Filter::Ext`] the macro `name` stands for, or `None` if `name`
/// is not a macro.
pub fn macro_filter(name: &str) -> Option<Filter> {
    macro_extensions(name).map(|exts| Filter::Ext(exts.iter().map(|e| (*e).to_owned()).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_are_disjoint() {
        let all = [AUDIO, DOC, EXE, PIC, VIDEO, ZIP];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                for ext in a.iter() {
                    assert!(!b.contains(ext), "{ext} is in two macros");
                }
            }
        }
    }

    #[test]
    fn filter_matches_list() {
        assert_eq!(
            macro_filter("Zip"),
            Some(Filter::Ext(ZIP.iter().map(|e| e.to_string()).collect()))
        );
        assert_eq!(macro_filter("ext"), None);
    }
}
