//! Synthetic file trees.
//!
//! A [`Tree`] is a flat list of entries in which every entry's parent comes
//! before it. Entry 0 is the root directory. Each new entry picks a parent
//! among the directories that are still shallower than the depth limit,
//! mixing four choices so the tree is neither flat nor a single chain:
//! the most recent directory (deepening, like a scan descending), one of the
//! last few directories (locality), a directory in proportion to its child
//! count (a few very large folders, many small ones), or any directory
//! (spreading). With the default shape, mean depth is about 10.

use std::collections::HashSet;

use crate::names::{NameGen, Names, fnv1a64};
use crate::rng::Rng;

/// Name of the root entry; `Tree::path(0)` is this name, and every other
/// path is relative and starts with it.
const ROOT_NAME: &str = "root";

/// Seed offset for the tree's name stream, so names and structure draw from
/// independent streams of the same seed.
const NAME_SEED_OFFSET: u64 = 0x6e61_6d65_7374_726d;

/// Parameters of a synthetic tree. Probabilities are in parts per million.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TreeShape {
    /// Probability that a non-root entry is a directory. Default 190,000
    /// (370k folders among 1.93M entries measured in the plan).
    pub dir_per_million: u32,
    /// Maximum depth of any entry; the root has depth 0. Default 32.
    /// Must be at least 1 for trees with more than one entry.
    pub max_depth: usize,
    /// Probability that an entry goes into the most recently created
    /// eligible directory. Default 400,000.
    pub deepen_per_million: u32,
    /// Probability that an entry goes into one of the `local_window` most
    /// recent eligible directories. Default 200,000.
    pub local_per_million: u32,
    /// Probability that an entry goes into a directory chosen in proportion
    /// to its current child count (preferential attachment), which yields
    /// the heavy-tailed fan-out of real volumes. Default 400,000. The
    /// remainder picks any eligible directory uniformly.
    pub popular_per_million: u32,
    /// Number of recent directories considered by the local choice.
    /// Default 64.
    pub local_window: usize,
}

impl Default for TreeShape {
    fn default() -> Self {
        Self {
            dir_per_million: 190_000,
            max_depth: 32,
            deepen_per_million: 100_000,
            local_per_million: 200_000,
            popular_per_million: 400_000,
            local_window: 64,
        }
    }
}

/// A synthetic file tree stored as parallel columns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tree {
    /// Name of each entry. Entry 0 is the root, named `root`.
    pub names: Names,
    /// Parent index of each entry; `u32::MAX` for the root. Always
    /// `parent[i] < i` for `i > 0`, and parents are directories.
    pub parent: Vec<u32>,
    /// Whether each entry is a directory.
    pub is_dir: Vec<bool>,
}

impl Tree {
    /// Depth of entry `i`; the root has depth 0.
    pub fn depth(&self, i: usize) -> usize {
        let mut depth = 0;
        let mut cur = i;
        while self.parent[cur] != u32::MAX {
            cur = self.parent[cur] as usize;
            depth += 1;
        }
        depth
    }

    /// Relative path of entry `i`: the names from the root down to `i`,
    /// joined with `/`. The root's path is its name.
    pub fn path(&self, i: usize) -> String {
        let mut chain = vec![i];
        let mut cur = i;
        while self.parent[cur] != u32::MAX {
            cur = self.parent[cur] as usize;
            chain.push(cur);
        }
        let mut path = String::new();
        for (k, &e) in chain.iter().rev().enumerate() {
            if k > 0 {
                path.push('/');
            }
            path.push_str(self.names.get(e));
        }
        path
    }

    /// Number of directories, including the root.
    pub fn dir_count(&self) -> usize {
        self.is_dir.iter().filter(|&&d| d).count()
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.parent.len()
    }

    /// Whether the tree has no entries.
    pub fn is_empty(&self) -> bool {
        self.parent.is_empty()
    }
}

/// Generates a tree of `entries` entries from `seed`.
///
/// Output is stable per seed and shape on every platform; changing it for an
/// existing seed is a breaking change: tests pin a golden checksum, the
/// directory count and sample paths for a fixed seed. Sibling names are
/// unique ignoring ASCII case and Unicode normalization, so the tree can be
/// created on default (case-insensitive) APFS; collisions are resolved
/// deterministically by inserting ` 2`, ` 3`, ... before the extension.
///
/// # Panics
///
/// Panics if `entries` exceeds `u32::MAX`, or if `shape.max_depth` is 0
/// and `entries > 1`.
pub fn generate_tree(entries: usize, seed: u64, shape: TreeShape) -> Tree {
    assert!(
        entries < u32::MAX as usize,
        "tree entries must fit in u32 indices"
    );
    let mut tree = Tree {
        names: Names::with_capacity(entries, entries.saturating_mul(18)),
        parent: Vec::with_capacity(entries),
        is_dir: Vec::with_capacity(entries),
    };
    if entries == 0 {
        return tree;
    }
    assert!(
        shape.max_depth >= 1 || entries == 1,
        "max_depth 0 allows only the root"
    );

    let mut rng = Rng::new(seed);
    let mut name_gen = NameGen::new(seed.wrapping_add(NAME_SEED_OFFSET));
    let mut depth: Vec<u32> = Vec::with_capacity(entries);
    // Directories that may still receive children (depth < max_depth).
    let mut eligible: Vec<u32> = Vec::new();
    // Hashes of (parent, folded name) for sibling uniqueness.
    let mut taken: HashSet<u64> = HashSet::with_capacity(entries);
    let mut scratch = String::with_capacity(64);
    let mut candidate = String::with_capacity(64);
    let mut key = Vec::with_capacity(80);

    tree.names.push(ROOT_NAME);
    tree.parent.push(u32::MAX);
    tree.is_dir.push(true);
    depth.push(0);
    if shape.max_depth > 0 {
        eligible.push(0);
    }

    let window = shape.local_window.max(1);
    for i in 1..entries {
        let r = rng.below(1_000_000);
        let deepen = u64::from(shape.deepen_per_million);
        let local = deepen + u64::from(shape.local_per_million);
        let popular = local + u64::from(shape.popular_per_million);
        let parent = if r < deepen {
            eligible[eligible.len() - 1]
        } else if r < local {
            let span = window.min(eligible.len());
            eligible[eligible.len() - 1 - rng.index(span)]
        } else if r < popular && i > 1 {
            // The parent of a random earlier non-root entry: chosen in
            // proportion to its child count. Any directory that already has
            // a child is shallower than max_depth, so it is eligible.
            tree.parent[1 + rng.index(i - 1)]
        } else {
            eligible[rng.index(eligible.len())]
        };
        let is_dir = rng.chance(shape.dir_per_million);

        scratch.clear();
        if is_dir {
            name_gen.write_dir_name(&mut scratch);
        } else {
            name_gen.write_file_name(&mut scratch);
        }
        candidate.clear();
        candidate.push_str(&scratch);
        let mut n = 1u32;
        while !taken.insert(sibling_key(parent, &candidate, &mut key)) {
            n += 1;
            candidate.clear();
            with_suffix(&mut candidate, &scratch, n, !is_dir);
        }

        let d = depth[parent as usize] + 1;
        tree.names.push(&candidate);
        tree.parent.push(parent);
        tree.is_dir.push(is_dir);
        depth.push(d);
        if is_dir && (d as usize) < shape.max_depth {
            eligible.push(i as u32);
        }
    }
    tree
}

/// Writes `name` with ` n` inserted before the extension (files) or at the
/// end (directories and dotfiles without a further extension).
fn with_suffix(out: &mut String, name: &str, n: u32, split_ext: bool) {
    let dot = if split_ext {
        name.rfind('.').filter(|&d| d > 0)
    } else {
        None
    };
    match dot {
        Some(d) => {
            out.push_str(&name[..d]);
            out.push(' ');
            out.push_str(&n.to_string());
            out.push_str(&name[d..]);
        }
        None => {
            out.push_str(name);
            out.push(' ');
            out.push_str(&n.to_string());
        }
    }
}

/// Hash of a parent index and a folded name, used to keep siblings unique.
///
/// Folding is deliberately conservative: ASCII letters are lower-cased,
/// every non-ASCII character becomes one marker byte, and a combining mark
/// (U+0300..U+036F) replaces the preceding character with the marker. So
/// NFC and NFD spellings, and case variants of accented letters, fold to
/// the same key. Over-folding only causes an extra (deterministic) rename;
/// a hash collision likewise only causes a rename, never a duplicate.
fn sibling_key(parent: u32, name: &str, key: &mut Vec<u8>) -> u64 {
    const MARKER: u8 = 0xff;
    key.clear();
    key.extend_from_slice(&parent.to_le_bytes());
    for c in name.chars() {
        if c.is_ascii() {
            key.push((c as u8).to_ascii_lowercase());
        } else if ('\u{300}'..='\u{36f}').contains(&c) {
            if key.len() > 4 {
                key.pop();
            }
            key.push(MARKER);
        } else {
            key.push(MARKER);
        }
    }
    fnv1a64(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffix_goes_before_extension_for_files() {
        let mut s = String::new();
        with_suffix(&mut s, "notes.txt", 2, true);
        assert_eq!(s, "notes 2.txt");
        s.clear();
        with_suffix(&mut s, ".gitignore", 3, true);
        assert_eq!(s, ".gitignore 3");
        s.clear();
        with_suffix(&mut s, "Foo.app", 2, false);
        assert_eq!(s, "Foo.app 2");
    }

    #[test]
    fn nfc_nfd_and_case_variants_share_a_key() {
        let mut k = Vec::new();
        let a = sibling_key(7, "caf\u{e9}.txt", &mut k);
        let b = sibling_key(7, "cafe\u{301}.TXT", &mut k);
        let c = sibling_key(8, "caf\u{e9}.txt", &mut k);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn default_shape_has_moderate_depth_and_heavy_fan_out() {
        let tree = generate_tree(200_000, 1, TreeShape::default());
        let total: usize = (0..tree.len()).map(|i| tree.depth(i)).sum();
        let mean_x100 = total * 100 / tree.len();
        // Real volumes average roughly 8-12 levels.
        assert!(
            (600..=1_400).contains(&mean_x100),
            "mean depth x100 {mean_x100}"
        );
        let mut kids = vec![0usize; tree.len()];
        for &p in &tree.parent[1..] {
            kids[p as usize] += 1;
        }
        let max_kids = kids.iter().copied().max().unwrap_or(0);
        assert!(max_kids >= 100, "largest directory has {max_kids} children");
    }
}
