//! Absolute paths to entries, through the index's children.
//!
//! Until run 2.17 the writer kept a path tree, a second copy of every entry
//! (about 360 B per entry, 1 GB for a whole disk), to answer this. The index
//! now finds a child by name itself ([`fx_index::Index::child`]).

use std::path::{Component, Path};

use fx_index::Index;
use fx_model::{EntryId, FileKey};

/// The live entry at absolute `path`, walking names down from the root
/// entry after stripping `root`. Names match by exact bytes. When two live
/// entries share the last name (a replaced entry still waiting for its
/// removal), the one keyed `prefer` wins, else the most recently added.
pub(crate) fn resolve(
    index: &Index,
    root: &Path,
    path: &Path,
    prefer: Option<FileKey>,
) -> Option<EntryId> {
    let rel = path.strip_prefix(root).ok()?;
    let mut cur = *index.roots().first()?;
    let mut names = rel.components().peekable();
    while let Some(comp) = names.next() {
        match comp {
            Component::Normal(name) => {
                let name = name.to_str()?;
                let last = names.peek().is_none();
                cur = match prefer {
                    Some(key) if last => {
                        let named = index.children_named(cur, name);
                        named
                            .iter()
                            .copied()
                            .find(|&c| index.key(c) == key)
                            .or_else(|| named.first().copied())?
                    }
                    _ => index.child(cur, name)?,
                };
            }
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(cur)
}
