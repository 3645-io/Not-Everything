//! [`MacScanner`]: the production macOS scanner as an
//! [`fx_fs::VolumeEnumerator`] (macOS only).
//!
//! It runs [`crate::scan::scan`] (exclusions, mount, firmlink and cloud
//! rules included) and converts every [`ScannedEntry`] into an
//! [`fx_model::Entry`]:
//!
//! * `key` is `(VolumeId(st_dev), file ID)`, where `st_dev` is the device of
//!   the directory the entry was listed in; `parent` is the key of the
//!   directory that listed it. Entries directly in the root get the root's
//!   own key as parent; the root itself is not emitted. See
//!   [Keys of firmlinked directories](#keys-of-firmlinked-directories) for
//!   the one case where this key differs from `lstat`.
//! * `name` is the entry's name (lossily converted if it is not UTF-8),
//!   `size` the data fork length for files (0 for directories), `modified`
//!   the modification time and `created` the creation (birth) time, both in
//!   whole seconds.
//! * `kind`: files, directories and symlinks map one to one; other vnode
//!   types (FIFOs, sockets, devices) are reported as
//!   [`EntryKind::File`], since `fx-model` has no
//!   kind for them.
//! * `flags`: `HIDDEN` (name starts with `.` or `UF_HIDDEN`), `SYSTEM`
//!   (`SF_RESTRICTED` or under `/System`), `DATALESS` (`SF_DATALESS`,
//!   cloud-only) and `LOCKED` (`UF_IMMUTABLE` or `SF_IMMUTABLE`).
//!
//! Entries reach the sink in parent-before-child order: a depth-first walk
//! over the collected parent links. Below a traverse-only ancestor of an
//! include-only root, the first emitted level has a parent that is never
//! emitted (the top of the emitted tree).
//!
//! # Keys of firmlinked directories
//!
//! The keying rule is: an entry's key is `(st_dev of the directory being
//! listed, file ID from getattrlistbulk)`. The device comes from `fstat`
//! on the open directory, not from the entry itself.
//!
//! For almost every entry this equals `(st_dev, st_ino)` from `lstat`. The
//! exception is a firmlinked directory seen from `/`, such as `/Users` or
//! `/Applications`: it is listed in `/`, so its key pairs the system
//! volume's device with the Data volume's inode for that directory, while
//! `lstat("/Users")` reports the Data volume's device. So the key differs
//! from what `lstat` reports. Its children are listed inside the
//! firmlinked directory, whose `fstat` device is the Data volume's, so
//! their own keys do match `lstat`; their `parent` is the firmlinked
//! directory's key as emitted (system device, Data inode), which keeps the
//! emitted tree self-consistent.
//!
//! Note that the reconcile and FSEvents run (1.8) must either key entries
//! by this same rule or remap these keys; looking a firmlinked directory up
//! by its `lstat` identity will not find the scanned entry.
//!
//! The scanner is read-only.

use std::path::Path;

use fx_fs::VolumeEnumerator;
use fx_model::{Entry, EntryFlags, EntryKind, FileKey, VolumeId};

use crate::attrbuf;
use crate::scan::{ScanOptions, ScannedEntry, scan};

/// `UF_IMMUTABLE` (`sys/stat.h`).
const UF_IMMUTABLE: u32 = 0x0000_0002;
/// `SF_IMMUTABLE` (`sys/stat.h`).
const SF_IMMUTABLE: u32 = 0x0002_0000;

/// Marker for "no parent index".
const NONE: u32 = u32::MAX;

/// The production macOS scanner. See the module docs.
///
/// Keys are `(st_dev of the directory being listed, file ID from
/// getattrlistbulk)`. For firmlinked directories seen from `/` (such as
/// `/Users` and `/Applications`) that pairs the system volume's device with
/// a Data-volume inode, so the key differs from what `lstat` reports; see
/// [Keys of firmlinked directories](crate::enumerator#keys-of-firmlinked-directories).
/// The reconcile and FSEvents run (1.8) must use the same rule or remap
/// these keys.
#[derive(Debug, Clone, Default)]
pub struct MacScanner {
    opts: ScanOptions,
}

impl MacScanner {
    /// A scanner using `opts`. `collect` is forced on when enumerating.
    #[must_use]
    pub fn new(opts: ScanOptions) -> MacScanner {
        MacScanner { opts }
    }

    /// The options this scanner was built with.
    #[must_use]
    pub fn options(&self) -> &ScanOptions {
        &self.opts
    }
}

/// Converts one scanned entry; `parent` is its parent's key.
fn to_entry(e: &ScannedEntry, parent: FileKey) -> Entry {
    let kind = match e.kind {
        attrbuf::EntryKind::Dir => EntryKind::Dir,
        attrbuf::EntryKind::Symlink => EntryKind::Symlink,
        attrbuf::EntryKind::File | attrbuf::EntryKind::Other => EntryKind::File,
    };
    let mut flags = EntryFlags::empty();
    flags.set(EntryFlags::HIDDEN, e.hidden);
    flags.set(EntryFlags::SYSTEM, e.system);
    flags.set(EntryFlags::DATALESS, crate::cloud::is_dataless(e.flags));
    flags.set(
        EntryFlags::LOCKED,
        e.flags & (UF_IMMUTABLE | SF_IMMUTABLE) != 0,
    );
    Entry {
        key: FileKey::new(VolumeId(e.dev), e.file_id),
        parent: Some(parent),
        name: e.name.to_string_lossy().into_owned(),
        kind,
        flags,
        size: if kind == EntryKind::Dir { 0 } else { e.size },
        modified: e.mtime_sec,
        created: e.crtime_sec,
    }
}

/// Calls `visit` with every index of `entries` in parent-before-child
/// order (depth first), using the `parent` links.
fn parent_first(entries: &[ScannedEntry], mut visit: impl FnMut(usize)) {
    let n = entries.len();
    let mut first_child = vec![NONE; n];
    let mut next_sibling = vec![NONE; n];
    let mut stack = Vec::new();
    for (i, e) in entries.iter().enumerate().rev() {
        if e.parent == NONE || e.parent as usize >= n {
            stack.push(i as u32);
        } else {
            let p = e.parent as usize;
            next_sibling[i] = first_child[p];
            first_child[p] = i as u32;
        }
    }
    stack.reverse();
    while let Some(i) = stack.pop() {
        let i = i as usize;
        visit(i);
        let mut c = first_child[i];
        while c != NONE {
            stack.push(c);
            c = next_sibling[c as usize];
        }
    }
}

impl VolumeEnumerator for MacScanner {
    fn enumerate(&self, root: &Path, sink: &mut dyn FnMut(Entry)) -> fx_fs::Result<()> {
        let mut opts = self.opts.clone();
        opts.collect = true;
        let result = scan(root, &opts)?;
        let entries = result.entries.unwrap_or_default();
        parent_first(&entries, |i| {
            let e = &entries[i];
            let (dev, id) = e.parent_key;
            sink(to_entry(e, FileKey::new(VolumeId(dev), id)));
        });
        Ok(())
    }
}
