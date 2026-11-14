//! How `day_replay_bench` compares the engine's index with a fresh scan of
//! the root (macOS only). `tests/macos_fresh_scan_compare.rs` checks it on
//! a temporary folder.
//!
//! Entries are compared by file key: the folder they are in (its key),
//! their name, their kind and, for a file, its size. A file with several
//! hard links is one entry in the index, at the first link its walk listed
//! (fx-engine's writer and fx-index's builder), while the scanner lists it
//! once per link, and its walk is parallel, so which link comes first
//! changes from one walk to the next. So the fresh scan keeps every link of
//! such a file ([`Fresh::other_links`]), and an index entry at any of them
//! is equal. On a Mac with build folders, hundreds of thousands of files
//! are such links: Cargo links its object files from `incremental/` into
//! `deps/`.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use fx_engine::EngineSnapshot;
use fx_fs::VolumeEnumerator;
use fx_fs_macos::enumerator::MacScanner;
use fx_fs_macos::scan::ScanOptions;
use fx_model::{Entry, EntryKind, FileKey};

/// One entry as the comparison sees it: its place, kind and, for a file,
/// its size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub parent: FileKey,
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
}

/// Every entry below the root, by key. The root itself, which has no
/// parent, is left out: the scanner doesn't list the folder it scans.
pub type Listing = HashMap<FileKey, Item>;

/// A fresh scan of the root.
#[derive(Default)]
pub struct Fresh {
    /// Every entry, at the first link the walk listed.
    pub items: Listing,
    /// For a file with several hard links, the place (folder and name) of
    /// every other link the walk listed.
    pub other_links: HashMap<FileKey, Vec<(FileKey, String)>>,
}

impl Fresh {
    /// Adds one entry the walk listed.
    pub fn add(&mut self, e: Entry) {
        let Some((key, item)) = item(e) else {
            return;
        };
        match self.items.get(&key) {
            Some(_) => self
                .other_links
                .entry(key)
                .or_default()
                .push((item.parent, item.name)),
            None => {
                self.items.insert(key, item);
            }
        }
    }

    /// Whether the index's `indexed` entry for `key` equals the scan's:
    /// the same kind and size, at one of the file's links.
    pub fn matches(&self, key: FileKey, indexed: &Item) -> bool {
        self.items.get(&key).is_some_and(|fresh| {
            indexed.kind == fresh.kind && indexed.size == fresh.size && self.at_a_link(key, indexed)
        })
    }

    /// Whether the index's `indexed` entry for `key` is a file at one of
    /// its links whose size alone differs from the scan's.
    pub fn only_size_differs(&self, key: FileKey, indexed: &Item) -> bool {
        self.items.get(&key).is_some_and(|fresh| {
            indexed.kind == EntryKind::File
                && fresh.kind == EntryKind::File
                && indexed.size != fresh.size
                && self.at_a_link(key, indexed)
        })
    }

    /// Whether `indexed` is at the place (folder and name) of one of the
    /// links the walk listed for `key`.
    fn at_a_link(&self, key: FileKey, indexed: &Item) -> bool {
        let at = |parent: &FileKey, name: &str| *parent == indexed.parent && name == indexed.name;
        self.items
            .get(&key)
            .is_some_and(|fresh| at(&fresh.parent, &fresh.name))
            || self
                .other_links
                .get(&key)
                .is_some_and(|links| links.iter().any(|(parent, name)| at(parent, name)))
    }
}

/// Whether a folder above `key` in `listing`, below `root`, refuses to be
/// listed (`EPERM` or `EACCES`). macOS keeps some folders from being
/// listed, even by a process with Full Disk Access (data stores under
/// `~/Library`), and some system folders allow passing through but not
/// listing (`drwxr-x--x`), while a path inside them can still be read with
/// `lstat`. The scanner skips such a folder and everything below it, but
/// when FSEvents reports a change inside it, the writer reads the path and
/// indexes the entry: the index then holds entries a fresh scan can't see.
/// That is not something the replay lost.
pub fn in_unlistable_folder(listing: &Listing, root: &Path, key: FileKey) -> bool {
    if !listing.contains_key(&key) {
        return false;
    }
    let path = path_of(listing, root, key);
    path.ancestors()
        .skip(1)
        .take_while(|folder| folder.starts_with(root) && *folder != root)
        .any(|folder| {
            matches!(std::fs::read_dir(folder),
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied)
        })
}

/// The differences that vanish when compared by path: entries at the same
/// path, with the same kind and size, on both sides, but under another file
/// key (one of `changed` whose path is the same on both sides, or one of
/// `missing` matched by one of `extra`). The scanner and `lstat` can key a
/// folder differently: a firmlink's System-volume entry against the Data
/// volume folder it leads to.
pub fn same_path_other_key(
    indexed: &Listing,
    fresh: &Fresh,
    root: &Path,
    missing: &[FileKey],
    extra: &[FileKey],
    changed: &[FileKey],
) -> std::collections::HashSet<FileKey> {
    let shape = |listing: &Listing, key: FileKey| {
        let item = &listing[&key];
        (path_of(listing, root, key), item.kind, item.size)
    };
    let mut out = std::collections::HashSet::new();
    for &key in changed {
        if shape(&fresh.items, key) == shape(indexed, key) {
            out.insert(key);
        }
    }
    let by_shape: HashMap<_, FileKey> = missing
        .iter()
        .map(|&key| (shape(&fresh.items, key), key))
        .collect();
    for &key in extra {
        if let Some(&other) = by_shape.get(&shape(indexed, key)) {
            out.insert(key);
            out.insert(other);
        }
    }
    out
}

/// Whether the entry at `key`'s path in `listing` was modified at or after
/// `since`. A file written while the comparison runs, such as a log kept
/// open and appended to, can have another size on the disk than the index
/// has, until FSEvents reports the write.
pub fn written_since(listing: &Listing, root: &Path, key: FileKey, since: SystemTime) -> bool {
    listing.contains_key(&key)
        && std::fs::symlink_metadata(path_of(listing, root, key))
            .and_then(|m| m.modified())
            .is_ok_and(|t| t >= since)
}

fn item(e: Entry) -> Option<(FileKey, Item)> {
    let size = if e.kind == EntryKind::File { e.size } else { 0 };
    Some((
        e.key,
        Item {
            parent: e.parent?,
            name: e.name,
            kind: e.kind,
            size,
        },
    ))
}

/// The index of `snap`, as the comparison sees it.
pub fn engine_listing(snap: &EngineSnapshot) -> Listing {
    let idx = snap.index();
    idx.live_ids()
        .filter_map(|id| {
            let p = idx.parent(id);
            let parent = (!p.is_none() && p != id).then(|| idx.key(p));
            item(Entry {
                key: idx.key(id),
                parent,
                name: idx.name(id).to_string(),
                kind: idx.kind(id),
                flags: idx.flags(id),
                size: idx.size(id),
                modified: idx.modified(id),
                created: idx.created(id),
            })
        })
        .collect()
}

/// A fresh scan of `root` with the scanner and options the app's engine
/// uses.
pub fn scan(root: &Path) -> io::Result<Fresh> {
    let mut fresh = Fresh::default();
    MacScanner::new(ScanOptions::default())
        .enumerate(root, &mut |e: Entry| fresh.add(e))
        .map_err(|e| io::Error::other(format!("fresh scan: {e}")))?;
    Ok(fresh)
}

/// Where the index and a fresh scan differ, by key.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Differences {
    /// In the scan, not in the index.
    pub missing: Vec<FileKey>,
    /// In the index, not in the scan.
    pub extra: Vec<FileKey>,
    /// In both, at another place or with another kind or size.
    pub changed: Vec<FileKey>,
    /// Of those, the ones whose kind or size differs.
    pub changed_kind_or_size: usize,
}

impl Differences {
    pub fn count(&self) -> usize {
        self.missing.len() + self.extra.len() + self.changed.len()
    }
}

/// Compares the index with the scan.
pub fn compare(indexed: &Listing, fresh: &Fresh) -> Differences {
    let mut d = Differences::default();
    for (key, item) in &fresh.items {
        match indexed.get(key) {
            None => d.missing.push(*key),
            Some(i) if !fresh.matches(*key, i) => {
                if i.kind != item.kind || i.size != item.size {
                    d.changed_kind_or_size += 1;
                }
                d.changed.push(*key);
            }
            Some(_) => {}
        }
    }
    d.extra = indexed
        .keys()
        .filter(|k| !fresh.items.contains_key(k))
        .copied()
        .collect();
    d
}

/// The path of `key` in `listing`, below `root`.
pub fn path_of(listing: &Listing, root: &Path, key: FileKey) -> PathBuf {
    let mut names = Vec::new();
    let mut at = listing.get(&key);
    while let Some(item) = at {
        names.push(item.name.as_str());
        if names.len() > 4096 {
            break;
        }
        at = listing.get(&item.parent);
    }
    let mut path = root.to_path_buf();
    path.extend(names.iter().rev());
    path
}
