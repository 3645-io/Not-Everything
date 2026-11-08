//! The writer's navigation mirror of the index.
//!
//! [`fx_index::Index`] answers lookups by key but has no child lists and no
//! row reads outside a snapshot (and taking a snapshot per change would make
//! the next write copy the delta). The writer therefore keeps every live
//! entry plus a child-by-name map here, in step with the index, to resolve
//! absolute paths, compare entries for `reconcile_path` and list subtrees
//! for `diff_subtree` and removals.

use std::collections::HashMap;
use std::path::{Component, Path};

use fx_model::{Entry, FileKey};

/// Live entries by key plus children by parent and name.
#[derive(Debug, Default)]
pub(crate) struct Tree {
    entries: HashMap<FileKey, Entry>,
    /// Children of each folder by name. A name maps to several keys only
    /// while a replaced entry is still waiting for its removal; the most
    /// recently inserted key is last.
    kids: HashMap<FileKey, HashMap<String, Vec<FileKey>>>,
    /// The entry with no parent, where path resolution starts.
    root: Option<FileKey>,
}

/// Whether `e` is a root: no parent, or itself as parent.
fn is_root(e: &Entry) -> bool {
    e.parent.is_none_or(|p| p == e.key)
}

impl Tree {
    /// Mirrors every live entry of an index snapshot.
    pub(crate) fn from_snapshot(snap: &fx_index::Snapshot) -> Tree {
        let mut tree = Tree::default();
        for id in snap.live_ids() {
            let p = snap.parent(id);
            let parent = if p.is_none() { None } else { Some(snap.key(p)) };
            tree.insert(Entry {
                key: snap.key(id),
                parent,
                name: snap.name(id).to_string(),
                kind: snap.kind(id),
                flags: snap.flags(id),
                size: snap.size(id),
                modified: snap.modified(id),
                created: snap.created(id),
            });
        }
        tree
    }

    /// Number of live entries.
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// The live entry with this key.
    pub(crate) fn get(&self, key: FileKey) -> Option<&Entry> {
        self.entries.get(&key)
    }

    /// Every live entry by key.
    pub(crate) fn entries(&self) -> &HashMap<FileKey, Entry> {
        &self.entries
    }

    /// The key of the entry at absolute `path`, walking child names down
    /// from the root entry after stripping `root_path`.
    pub(crate) fn resolve(&self, root_path: &Path, path: &Path) -> Option<FileKey> {
        let rel = path.strip_prefix(root_path).ok()?;
        let mut cur = self.root?;
        for comp in rel.components() {
            match comp {
                Component::Normal(name) => {
                    cur = *self.kids.get(&cur)?.get(name.to_str()?)?.last()?;
                }
                Component::CurDir => {}
                _ => return None,
            }
        }
        Some(cur)
    }

    /// `key` and every entry below it, parent before child. Empty if `key`
    /// is not live.
    pub(crate) fn subtree(&self, key: FileKey) -> Vec<FileKey> {
        if !self.entries.contains_key(&key) {
            return Vec::new();
        }
        let mut out = vec![key];
        let mut i = 0;
        while i < out.len() {
            if let Some(kids) = self.kids.get(&out[i]) {
                out.extend(kids.values().flatten().copied());
            }
            i += 1;
        }
        out
    }

    /// Inserts or replaces an entry, moving it in the child map.
    pub(crate) fn insert(&mut self, entry: Entry) {
        if let Some(old) = self.entries.get(&entry.key).cloned() {
            self.detach(&old);
        }
        if is_root(&entry) {
            self.root = Some(entry.key);
        } else if let Some(p) = entry.parent {
            self.kids
                .entry(p)
                .or_default()
                .entry(entry.name.clone())
                .or_default()
                .push(entry.key);
        }
        self.entries.insert(entry.key, entry);
    }

    /// Removes `key` and everything below it; returns the removed keys,
    /// parent before child.
    pub(crate) fn remove_subtree(&mut self, key: FileKey) -> Vec<FileKey> {
        let doomed = self.subtree(key);
        if let Some(old) = self.entries.get(&key).cloned() {
            self.detach(&old);
        }
        for k in &doomed {
            self.entries.remove(k);
            self.kids.remove(k);
        }
        doomed
    }

    /// Takes `old` out of its parent's child map (or clears the root).
    fn detach(&mut self, old: &Entry) {
        if is_root(old) {
            if self.root == Some(old.key) {
                self.root = None;
            }
            return;
        }
        let Some(p) = old.parent else { return };
        let Some(by_name) = self.kids.get_mut(&p) else {
            return;
        };
        if let Some(keys) = by_name.get_mut(&old.name) {
            keys.retain(|k| *k != old.key);
            if keys.is_empty() {
                by_name.remove(&old.name);
            }
        }
        if by_name.is_empty() {
            self.kids.remove(&p);
        }
    }
}
