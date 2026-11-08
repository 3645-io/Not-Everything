//! The extension table: interns file extensions to 16-bit IDs.
//!
//! Each entry stores a 2-byte [`ExtId`] instead of its extension text, which
//! makes an `ext:` filter an integer compare (docs/PLAN.md, data-layout
//! table: "Extension ID").
//!
//! Rules:
//! - The extension is the part of the name after its last `'.'`.
//! - A name with no dot, a dot-file such as `.bashrc` (the only dot is the
//!   first byte) and a name ending in a dot have no extension:
//!   [`ExtId::NONE`].
//! - Extensions are ASCII-lowercased before interning, so `JPG` and `jpg`
//!   share an ID. Non-ASCII letters are left as they are.
//!
//! # Overflow
//!
//! IDs `1..=65_534` name real extensions. Once they are all used, every
//! further new extension maps to the reserved [`ExtId::OVERFLOW`] and is not
//! stored. An `OVERFLOW` ID carries no information about the extension, so
//! callers filtering by extension must treat it as "compare by name": take
//! the extension from the file name itself (see [`extension_of`]) and compare
//! it case-insensitively. IDs issued before overflow never change.
//!
//! Clean-room: written from docs/PLAN.md only.

use std::borrow::Cow;
use std::collections::HashMap;

use crate::names::nul_separated_layout_matches;
use crate::parts::PartsError;

/// An interned file extension, 2 bytes per entry.
///
/// `ExtId(0)` ([`ExtId::NONE`]) means "no extension" and
/// [`ExtId::OVERFLOW`] means "the table was full; compare by name".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ExtId(pub u16);

impl ExtId {
    /// The name has no extension.
    pub const NONE: ExtId = ExtId(0);

    /// The extension table was full when this extension was first seen.
    /// Callers must fall back to comparing the extension taken from the name.
    pub const OVERFLOW: ExtId = ExtId(u16::MAX);

    /// Whether this ID names a stored extension (neither `NONE` nor
    /// `OVERFLOW`).
    pub fn is_real(self) -> bool {
        self != Self::NONE && self != Self::OVERFLOW
    }
}

/// Returns the extension of a file name as written (not case-folded): the
/// part after the last `'.'`, or `None` for names without a dot, dot-files
/// like `.bashrc`, and names ending in a dot.
pub fn extension_of(name: &str) -> Option<&str> {
    let dot = name.rfind('.')?;
    if dot == 0 || dot + 1 == name.len() {
        return None;
    }
    Some(&name[dot + 1..])
}

/// ASCII-lowercases `ext`, borrowing when it is already lowercase.
fn fold(ext: &str) -> Cow<'_, str> {
    if ext.bytes().any(|b| b.is_ascii_uppercase()) {
        Cow::Owned(ext.to_ascii_lowercase())
    } else {
        Cow::Borrowed(ext)
    }
}

/// Interns file extensions to [`ExtId`]s. The extension is the part of the
/// name after its last `'.'`, ASCII-lowercased ([`extension_of`]); once IDs
/// `1..=65_534` are used up, every further new extension maps to
/// [`ExtId::OVERFLOW`], whose entries are compared by name.
#[derive(Debug, Clone, Default)]
pub struct ExtensionTable {
    /// Lowercased extension text; `exts[i]` has ID `i + 1`.
    exts: Vec<Box<str>>,
    /// Lowercased extension text to its ID.
    ids: HashMap<Box<str>, ExtId>,
}

impl ExtensionTable {
    /// Largest number of real extensions the table can hold.
    pub const CAPACITY: usize = (u16::MAX - 1) as usize;

    /// Creates an empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the ID of `name`'s extension, interning it if it is new.
    ///
    /// `name` is a whole file name, not a bare extension. Returns
    /// [`ExtId::NONE`] if the name has no extension and [`ExtId::OVERFLOW`]
    /// if the extension is new and the table is full.
    pub fn intern(&mut self, name: &str) -> ExtId {
        let Some(ext) = extension_of(name) else {
            return ExtId::NONE;
        };
        let key = fold(ext);
        if let Some(&id) = self.ids.get(key.as_ref()) {
            return id;
        }
        if self.exts.len() >= Self::CAPACITY {
            return ExtId::OVERFLOW;
        }
        // CAPACITY < u16::MAX, so the new ID fits and is never OVERFLOW.
        let id = ExtId((self.exts.len() + 1) as u16);
        let key: Box<str> = key.into_owned().into_boxed_str();
        self.exts.push(key.clone());
        self.ids.insert(key, id);
        id
    }

    /// Returns the ID of a bare extension (without the dot), matched
    /// ASCII-case-insensitively. Never interns. Returns `None` for unknown
    /// extensions, including ones that overflowed.
    pub fn lookup(&self, ext: &str) -> Option<ExtId> {
        self.ids.get(fold(ext).as_ref()).copied()
    }

    /// Returns the lowercased extension text for a real ID, or `None` for
    /// [`ExtId::NONE`], [`ExtId::OVERFLOW`] and unknown IDs.
    pub fn as_str(&self, id: ExtId) -> Option<&str> {
        if !id.is_real() {
            return None;
        }
        self.exts.get(usize::from(id.0) - 1).map(|s| &**s)
    }

    /// Number of distinct real extensions stored (not counting `NONE`).
    pub fn len(&self) -> usize {
        self.exts.len()
    }

    /// Whether no extension has been interned yet.
    pub fn is_empty(&self) -> bool {
        self.exts.is_empty()
    }

    /// Whether the table is full, so new extensions map to
    /// [`ExtId::OVERFLOW`].
    pub fn is_full(&self) -> bool {
        self.exts.len() >= Self::CAPACITY
    }

    /// The table's raw layout: the lowercased extensions in ID order joined
    /// by single NUL bytes (no trailing NUL), and the start offset of each
    /// one. Extension `i` in the offsets has ID `i + 1`. This is the same
    /// layout as a [`NameBuffer`](crate::NameBuffer).
    pub fn to_raw_parts(&self) -> (Vec<u8>, Vec<u32>) {
        let total: usize = self.exts.iter().map(|e| e.len() + 1).sum();
        let mut bytes = Vec::with_capacity(total.saturating_sub(1));
        let mut offsets = Vec::with_capacity(self.exts.len());
        for (i, ext) in self.exts.iter().enumerate() {
            if i > 0 {
                bytes.push(0);
            }
            // At most CAPACITY extensions of bounded length; a table that
            // large in bytes cannot be built through `intern` in practice.
            offsets.push(bytes.len() as u32);
            bytes.extend_from_slice(ext.as_bytes());
        }
        (bytes, offsets)
    }

    /// Rebuilds a table from [`to_raw_parts`](ExtensionTable::to_raw_parts)
    /// output, with the same IDs.
    ///
    /// Validates instead of trusting the input: at most
    /// [`CAPACITY`](ExtensionTable::CAPACITY) extensions, offsets matching
    /// the NUL-separated bytes, valid UTF-8, and every extension non-empty,
    /// already lowercased, free of `'.'` and unique. Never panics.
    pub fn from_raw_parts(bytes: Vec<u8>, offsets: Vec<u32>) -> Result<Self, PartsError> {
        let bad = |reason| Err(PartsError::ExtensionTable { reason });
        if offsets.len() > Self::CAPACITY {
            return bad("more extensions than the table can hold");
        }
        if u32::try_from(bytes.len()).is_err() {
            return bad("extension bytes exceed the u32 offset space");
        }
        if !nul_separated_layout_matches(&bytes, &offsets) {
            return bad("extension offsets do not match the NUL-separated bytes");
        }
        let Ok(text) = std::str::from_utf8(&bytes) else {
            return bad("extension bytes are not valid UTF-8");
        };
        let mut table = ExtensionTable {
            exts: Vec::with_capacity(offsets.len()),
            ids: HashMap::with_capacity(offsets.len()),
        };
        if offsets.is_empty() {
            return Ok(table);
        }
        for (i, ext) in text.split('\0').enumerate() {
            if ext.contains('.') {
                return bad("an extension contains a dot");
            }
            if matches!(fold(ext), Cow::Owned(_)) {
                return bad("an extension is not lowercased");
            }
            let key: Box<str> = ext.into();
            // i < CAPACITY < u16::MAX, so the ID fits and is never OVERFLOW.
            let id = ExtId((i + 1) as u16);
            if table.ids.insert(key.clone(), id).is_some() {
                return bad("an extension appears twice");
            }
            table.exts.push(key);
        }
        Ok(table)
    }
}
