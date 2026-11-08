//! The priority group ("Your files first"): the entries of a
//! snapshot that lie in the configured priority folders and are not hidden.
//!
//! The writer rebuilds the group whenever it publishes a snapshot with a
//! changed index: it resolves the listed folders through its path tree,
//! reads the hidden entries (names starting with `.`) off the merged Name
//! order with [`fx_sort::hidden_entries`], then reads the listed subtrees
//! off the merged Path order with [`fx_sort::group_subtrees`], skipping the
//! hidden entries' subtrees (a few binary searches plus one bit per entry
//! of the group or hidden entry). Folders are resolved again every time, so
//! a listed folder created later, or a cloud drive added inside one, joins
//! at once.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fx_model::EntryId;
use fx_sort::{MatchSet, Order, SortKey, group_subtrees, hidden_entries};

use crate::tree::Tree;

/// The configured priority folders, resolved against the engine's root.
#[derive(Debug, Clone, Default)]
pub(crate) struct PriorityFolders {
    root: PathBuf,
    /// Each listed folder, `true` when it starts the group.
    folders: Vec<(PathBuf, bool)>,
}

impl PriorityFolders {
    /// The folders that start (`include`) and end (`exclude`) the group of
    /// an engine watching `root`.
    pub(crate) fn new(root: &Path, include: &[PathBuf], exclude: &[PathBuf]) -> PriorityFolders {
        let folders = include
            .iter()
            .map(|p| (p.clone(), true))
            .chain(exclude.iter().map(|p| (p.clone(), false)))
            .collect();
        PriorityFolders {
            root: root.to_path_buf(),
            folders,
        }
    }

    /// The group of `snap` (whose live entries `tree` mirrors), read from
    /// the Path order among `orders`. `None` without priority folders.
    pub(crate) fn group(
        &self,
        tree: &Tree,
        snap: &fx_index::Snapshot,
        orders: &[Order],
    ) -> Option<Arc<MatchSet>> {
        if self.folders.is_empty() {
            return None;
        }
        let order = |key: SortKey| orders.iter().find(|o| o.key == key);
        let (path, name) = (order(SortKey::Path)?, order(SortKey::Name)?);
        let listed = self.resolve(tree, snap);
        // Hidden entries and everything inside hidden folders are never
        // first (the owner's rule).
        let hidden = hidden_entries(name, snap);
        Some(Arc::new(group_subtrees(path, snap, &listed, &hidden)))
    }

    /// The listed folders that exist in `snap`, as entry IDs with their
    /// flag. Folders at or above the root decide for the root entry: the
    /// deepest one wins, and an exclusion wins a tie.
    fn resolve(&self, tree: &Tree, snap: &fx_index::Snapshot) -> Vec<(EntryId, bool)> {
        let id_of = |path: &Path| {
            tree.resolve(&self.root, path)
                .and_then(|key| snap.lookup(key))
        };
        let mut listed = Vec::with_capacity(self.folders.len());
        let mut above_root: Option<(usize, bool)> = None;
        for (path, include) in &self.folders {
            if self.root.starts_with(path) {
                let depth = path.components().count();
                above_root = Some(match above_root {
                    Some((d, flag)) if d > depth => (d, flag),
                    Some((d, flag)) if d == depth => (d, flag && *include),
                    _ => (depth, *include),
                });
            } else if let Some(id) = id_of(path) {
                listed.push((id, *include));
            }
        }
        if let Some((_, flag)) = above_root {
            if let Some(id) = id_of(&self.root) {
                listed.push((id, flag));
            }
        }
        listed
    }
}
