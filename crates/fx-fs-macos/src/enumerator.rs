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
//! Entries reach the sink in parent-before-child order, as the walk lists
//! them ([`crate::scan::scan_streaming`]): the walk runs on its own threads
//! and sends each directory's entries through a channel bounded by the heap
//! they hold ([`crate::budget`], [`ScanOptions::stream_budget`]), and the
//! sink is called on the calling thread. Nothing collects every entry
//! first (run 2.17: that copy took about 330 MB for a whole disk), and a
//! sink slower than the walk, such as the writer rescanning a big folder,
//! holds the walk to the budget. Below a traverse-only ancestor of an
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
//! A mount point is keyed the other way round (run 2.4): its listing
//! reports the folder the volume is mounted on, so it is keyed by the
//! mounted volume's root, `lstat`'s own `(st_dev, st_ino)`, and the
//! volume's entries hang off that key. Two volumes' roots often share an
//! inode number (2 on APFS and HFS+), never a device, so mount points never
//! share a key. `MacPathStat` keys them the same way.
//!
//! The scanner is read-only.

use std::path::Path;

use fx_fs::VolumeEnumerator;
use fx_model::{Entry, EntryFlags, EntryKind, FileKey, VolumeId};

use crate::attrbuf;
use crate::budget;
use crate::scan::{ScanOptions, ScannedEntry, scan_streaming};

/// `UF_IMMUTABLE` (`sys/stat.h`).
const UF_IMMUTABLE: u32 = 0x0000_0002;
/// `SF_IMMUTABLE` (`sys/stat.h`).
const SF_IMMUTABLE: u32 = 0x0002_0000;

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

impl VolumeEnumerator for MacScanner {
    fn enumerate(&self, root: &Path, sink: &mut dyn FnMut(Entry)) -> fx_fs::Result<()> {
        let opts = self.opts.clone();
        let (tx, rx) = budget::channel::<Vec<ScannedEntry>>(opts.stream_budget);
        std::thread::scope(|s| {
            // The walk owns the sender: when it ends, the channel closes and
            // the loop below finishes. A sink that panics drops the
            // receiver, which releases a walk waiting for room.
            let walk = s.spawn(move || scan_streaming(root, &opts, &tx));
            for batch in rx {
                for e in &batch {
                    let (dev, id) = e.parent_key;
                    sink(to_entry(e, FileKey::new(VolumeId(dev), id)));
                }
            }
            match walk.join() {
                Ok(result) => result.map(|_| ()),
                Err(panic) => std::panic::resume_unwind(panic),
            }
        })
    }
}
