//! Children by parent, for the index's single writer.
//!
//! The writer finds the entry a changed path names by walking names down
//! from the root, and lists what a folder holds to remove or re-scan it.
//! Until run 2.17 the engine kept its own copy of every entry for this (a
//! path tree, about 360 B per entry); the index now answers it from its own
//! columns plus two small structures:
//!
//! - [`BaseKids`]: the base IDs grouped by their base parent, built with one
//!   counting pass at the first navigation after the base is (at load and
//!   at every compaction), so a reader that only searches never builds it.
//!   A folder with more than [`SORTED_GROUP_MIN`] children has them sorted
//!   by name bytes, so a name is found by halving; smaller ones are scanned.
//!   The base never changes between compactions, so this can't go stale; a
//!   base entry the delta overrides or removed is skipped when read.
//! - [`DeltaKids`]: an ordered set of `(parent, name hash, ID)` for every
//!   live delta row, so a child is found by name in O(log n) however many
//!   entries one folder gains between compactions.
//!
//! Clean-room: written from docs/PLAN.md and this crate's own code only.

use std::collections::BTreeSet;

use crate::store::Base;

/// Folders with more children than this keep them sorted by name.
pub(crate) const SORTED_GROUP_MIN: usize = 32;

/// The base IDs grouped by base parent; see the module documentation.
#[derive(Debug, Clone, Default)]
pub(crate) struct BaseKids {
    /// Every base ID, ascending by base parent (roots, whose parent is
    /// `u32::MAX`, last); within a group larger than
    /// [`SORTED_GROUP_MIN`], ascending by name bytes.
    members: Vec<u32>,
}

impl BaseKids {
    /// Groups the IDs of `base` by parent, removed entries included (they
    /// are skipped when read).
    pub(crate) fn build(base: &Base) -> BaseKids {
        let n = base.len();
        // Bucket `n` holds the roots; parents are validated to be in range.
        let bucket = |p: u32| if (p as usize) < n { p as usize } else { n };
        let mut next = vec![0u32; n + 1];
        for &p in base.parent.iter() {
            next[bucket(p)] += 1;
        }
        let mut sum = 0u32;
        for slot in &mut next {
            let count = *slot;
            *slot = sum;
            sum += count;
        }
        let mut members = vec![0u32; n];
        for (i, &p) in base.parent.iter().enumerate() {
            let b = bucket(p);
            members[next[b] as usize] = i as u32;
            next[b] += 1;
        }
        drop(next);
        let mut start = 0;
        while start < n {
            let p = base.parent[members[start] as usize];
            let mut end = start + 1;
            while end < n && base.parent[members[end] as usize] == p {
                end += 1;
            }
            if end - start > SORTED_GROUP_MIN {
                members[start..end].sort_unstable_by(|&a, &b| {
                    base.names
                        .get(a)
                        .as_bytes()
                        .cmp(base.names.get(b).as_bytes())
                });
            }
            start = end;
        }
        BaseKids { members }
    }

    /// The base IDs whose base parent is `parent` (`u32::MAX`: the roots).
    pub(crate) fn group<'a>(&'a self, base: &Base, parent: u32) -> &'a [u32] {
        let parent_of = |m: u32| base.parent[m as usize];
        let lo = self.members.partition_point(|&m| parent_of(m) < parent);
        let len = self.members[lo..].partition_point(|&m| parent_of(m) == parent);
        &self.members[lo..lo + len]
    }

    /// The base IDs under `parent` whose base name is `name`.
    pub(crate) fn named<'a>(
        &'a self,
        base: &'a Base,
        parent: u32,
        name: &'a str,
    ) -> impl Iterator<Item = u32> + 'a {
        let group = self.group(base, parent);
        let name_of = move |m: u32| base.names.get(m);
        let (lo, sorted) = if group.len() > SORTED_GROUP_MIN {
            (
                group.partition_point(|&m| name_of(m).as_bytes() < name.as_bytes()),
                true,
            )
        } else {
            (0, false)
        };
        group[lo..]
            .iter()
            .copied()
            .take_while(move |&m| !sorted || name_of(m) == name)
            .filter(move |&m| name_of(m) == name)
    }

    /// Allocated bytes.
    pub(crate) fn heap_bytes(&self) -> usize {
        self.members.capacity() * size_of::<u32>()
    }
}

/// FNV-1a over the name's bytes: names in one folder rarely collide, and a
/// collision only costs a name comparison.
pub(crate) fn name_hash(name: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in name.as_bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Every live delta row as `(parent, name hash, ID)`; see the module
/// documentation.
#[derive(Debug, Clone, Default)]
pub(crate) struct DeltaKids {
    set: BTreeSet<(u32, u64, u32)>,
}

impl DeltaKids {
    pub(crate) fn insert(&mut self, parent: u32, hash: u64, id: u32) {
        self.set.insert((parent, hash, id));
    }

    pub(crate) fn remove(&mut self, parent: u32, hash: u64, id: u32) {
        self.set.remove(&(parent, hash, id));
    }

    pub(crate) fn clear(&mut self) {
        self.set.clear();
    }

    /// The delta rows under `parent`.
    pub(crate) fn children(&self, parent: u32) -> impl Iterator<Item = u32> + '_ {
        self.set
            .range((parent, 0, 0)..=(parent, u64::MAX, u32::MAX))
            .map(|&(_, _, id)| id)
    }

    /// The delta rows under `parent` whose name hashes to `hash`; the caller
    /// compares the names.
    pub(crate) fn hashed(&self, parent: u32, hash: u64) -> impl Iterator<Item = u32> + '_ {
        self.set
            .range((parent, hash, 0)..=(parent, hash, u32::MAX))
            .map(|&(_, _, id)| id)
    }

    /// Bytes held, estimated: the elements plus a B-tree's overhead.
    pub(crate) fn heap_bytes(&self) -> usize {
        self.set.len() * (size_of::<(u32, u64, u32)>() + 8)
    }
}
