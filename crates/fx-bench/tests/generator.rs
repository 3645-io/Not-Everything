//! Gate tests for the deterministic generator: RNG, names and tree.
//!
//! API assumed by these tests (written before the implementation):
//! - `fx_bench::Rng::new(seed: u64)`, `next_u64()`, `below(n)`, `chance(p_per_million)`.
//!   The stream must be exactly xoshiro256** whose four state words are the
//!   first four SplitMix64 outputs for `seed` (checked against a reference
//!   copy of both algorithms below).
//! - `fx_bench::generate_names(count, seed) -> Names` with `len()`,
//!   `get(i) -> &str`, `iter()`, `total_bytes()` (sum of the names' byte
//!   lengths, separators excluded), `fnv1a64()` (FNV-1a 64 over `buffer()`),
//!   `buffer() -> &[u8]` (the NUL-separated name bytes) and
//!   `offsets() -> &[u32]` (start offset of each name in `buffer()`).
//! - `fx_bench::generate_tree(entries, seed, TreeShape) -> Tree` with public
//!   fields `names`, `parent`, `is_dir`, methods `depth(i)`, `path(i)`,
//!   `dir_count()`, and `TreeShape: Default` with a `max_depth` field.

use std::collections::HashSet;

use fx_bench::{Names, Rng, Tree, TreeShape, generate_names, generate_tree};

// ---------------------------------------------------------------------------
// Reference RNG, written from the published SplitMix64 / xoshiro256**
// descriptions. The crate's `Rng` must produce the same stream bit for bit.
// ---------------------------------------------------------------------------

struct RefSplitMix64(u64);

impl RefSplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

struct RefXoshiro256StarStar([u64; 4]);

impl RefXoshiro256StarStar {
    fn new(seed: u64) -> Self {
        let mut sm = RefSplitMix64(seed);
        Self([sm.next(), sm.next(), sm.next(), sm.next()])
    }

    fn next(&mut self) -> u64 {
        let s = &mut self.0;
        let result = s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = s[1] << 17;
        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];
        s[2] ^= t;
        s[3] = s[3].rotate_left(45);
        result
    }
}

fn ref_fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

// ---------------------------------------------------------------------------
// RNG
// ---------------------------------------------------------------------------

#[test]
fn rng_matches_reference_xoshiro256starstar_seeded_by_splitmix64() {
    for seed in [0u64, 1, 42, 0xdead_beef, u64::MAX] {
        let mut rng = Rng::new(seed);
        let mut reference = RefXoshiro256StarStar::new(seed);
        for i in 0..10_000 {
            assert_eq!(
                rng.next_u64(),
                reference.next(),
                "stream diverges from reference at step {i} for seed {seed}"
            );
        }
    }
}

#[test]
fn rng_same_seed_same_stream_different_seed_differs() {
    let a: Vec<u64> = {
        let mut r = Rng::new(7);
        (0..64).map(|_| r.next_u64()).collect()
    };
    let b: Vec<u64> = {
        let mut r = Rng::new(7);
        (0..64).map(|_| r.next_u64()).collect()
    };
    let c: Vec<u64> = {
        let mut r = Rng::new(8);
        (0..64).map(|_| r.next_u64()).collect()
    };
    assert_eq!(a, b);
    assert_ne!(a, c);
}

#[test]
fn rng_below_stays_in_range_and_is_roughly_uniform() {
    let mut rng = Rng::new(3);
    for _ in 0..1_000 {
        assert_eq!(rng.below(1), 0);
    }
    // Non-power-of-two bound to exercise the unbiased path.
    let n = 7u64;
    let draws = 700_000;
    let mut counts = [0u32; 7];
    for _ in 0..draws {
        let v = rng.below(n);
        assert!(v < n, "below({n}) returned {v}");
        counts[v as usize] += 1;
    }
    for (k, &c) in counts.iter().enumerate() {
        // Expected 100_000 each; allow a generous ±2%.
        assert!(
            (98_000..=102_000).contains(&c),
            "bucket {k} has {c} draws, expected about 100000"
        );
    }
    // Large bounds must also stay in range.
    for _ in 0..10_000 {
        let big = (1u64 << 63) + 12_345;
        assert!(rng.below(big) < big);
        assert!(rng.below(u64::MAX) < u64::MAX);
    }
}

#[test]
fn rng_below_is_deterministic_per_seed() {
    let mut a = Rng::new(99);
    let mut b = Rng::new(99);
    for n in 1..5_000u64 {
        assert_eq!(a.below(n), b.below(n));
    }
}

#[test]
fn rng_chance_extremes_and_rate() {
    let mut rng = Rng::new(11);
    for _ in 0..10_000 {
        assert!(!rng.chance(0));
        assert!(rng.chance(1_000_000));
    }
    let hits = (0..1_000_000).filter(|_| rng.chance(250_000)).count();
    assert!(
        (245_000..=255_000).contains(&hits),
        "chance(25%) hit {hits} of 1000000"
    );
}

// ---------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------

/// Checks the structural invariants of a `Names` value and returns the
/// number of non-ASCII names.
fn check_names_invariants(names: &Names, count: usize) -> usize {
    assert_eq!(names.len(), count);
    assert_eq!(names.offsets().len(), count);
    let buf = names.buffer();
    let offs = names.offsets();
    let mut total = 0usize;
    let mut non_ascii = 0usize;
    let mut iter = names.iter();
    for i in 0..count {
        let name = names.get(i);
        assert_eq!(iter.next(), Some(name), "iter() and get({i}) disagree");
        assert!(!name.is_empty(), "name {i} is empty");
        assert!(name != "." && name != "..", "name {i} is {name:?}");
        assert!(!name.contains('/'), "name {i} contains '/': {name:?}");
        assert!(!name.contains('\0'), "name {i} contains NUL: {name:?}");
        if !name.is_ascii() {
            non_ascii += 1;
        }
        // The name sits in the buffer at its offset, followed by a NUL
        // separator (or the end of the buffer for the last name).
        let start = offs[i] as usize;
        let end = start + name.len();
        assert_eq!(
            &buf[start..end],
            name.as_bytes(),
            "name {i} not at its offset"
        );
        if i + 1 < count {
            assert_eq!(buf[end], 0, "name {i} is not NUL-terminated");
            assert_eq!(
                offs[i + 1] as usize,
                end + 1,
                "offsets not contiguous at {i}"
            );
        } else {
            assert!(end == buf.len() || (buf[end] == 0 && end + 1 == buf.len()));
        }
        total += name.len();
    }
    assert_eq!(iter.next(), None, "iter() yields more than len() names");
    assert_eq!(names.total_bytes(), total);
    assert_eq!(
        names.fnv1a64(),
        ref_fnv1a64(buf),
        "fnv1a64() is not FNV-1a 64 over buffer()"
    );
    non_ascii
}

/// Gate: 5M names generated reproducibly, calibrated to the plan.
#[test]
fn five_million_names_are_reproducible_and_calibrated() {
    const COUNT: usize = 5_000_000;
    let a = generate_names(COUNT, 1);
    let b = generate_names(COUNT, 1);
    assert!(
        a.buffer() == b.buffer(),
        "same seed produced different buffers"
    );
    assert!(
        a.offsets() == b.offsets(),
        "same seed produced different offsets"
    );
    assert_eq!(a.fnv1a64(), b.fnv1a64());
    drop(b);

    let non_ascii = check_names_invariants(&a, COUNT);

    let mean_x100 = a.total_bytes() * 100 / COUNT;
    assert!(
        (1_500..=2_000).contains(&mean_x100),
        "mean name length {}.{:02} B outside 15-20 B",
        mean_x100 / 100,
        mean_x100 % 100
    );

    // About 1 in 80,000 is non-ASCII: ~62 expected in 5M.
    assert!(
        (10..=300).contains(&non_ascii),
        "{non_ascii} non-ASCII names in 5M, expected about 62"
    );
}

#[test]
fn different_seed_gives_different_names() {
    let a = generate_names(10_000, 1);
    let b = generate_names(10_000, 2);
    assert_ne!(a.buffer(), b.buffer());
    assert_ne!(a.fnv1a64(), b.fnv1a64());
}

#[test]
fn small_name_counts_work() {
    for count in [0usize, 1, 2, 17] {
        let n = generate_names(count, 5);
        check_names_invariants(&n, count);
    }
}

#[test]
fn names_have_a_realistic_mix() {
    let names = generate_names(200_000, 3);
    let mut with_ext = 0usize;
    let mut without_dot = 0usize;
    let mut digits = 0usize;
    let mut upper = 0usize;
    let mut dash = 0usize;
    let mut underscore = 0usize;
    let mut space = 0usize;
    let mut exts: HashSet<&str> = HashSet::new();
    for name in names.iter() {
        match name.rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => {
                with_ext += 1;
                exts.insert(ext);
            }
            _ => {}
        }
        if !name.contains('.') {
            without_dot += 1;
        }
        if name.bytes().any(|c| c.is_ascii_digit()) {
            digits += 1;
        }
        if name.bytes().any(|c| c.is_ascii_uppercase()) {
            upper += 1;
        }
        dash += usize::from(name.contains('-'));
        underscore += usize::from(name.contains('_'));
        space += usize::from(name.contains(' '));
    }
    let n = names.len();
    assert!(
        with_ext * 100 >= n * 40,
        "only {with_ext}/{n} names have an extension"
    );
    assert!(
        without_dot * 100 >= n * 3,
        "only {without_dot}/{n} names are extensionless"
    );
    assert!(
        digits * 100 >= n * 5,
        "only {digits}/{n} names contain digits"
    );
    assert!(
        upper * 100 >= n * 5,
        "only {upper}/{n} names contain uppercase (CamelCase)"
    );
    assert!(dash * 100 >= n, "only {dash}/{n} names contain '-'");
    assert!(
        underscore * 100 >= n,
        "only {underscore}/{n} names contain '_'"
    );
    assert!(space * 100 >= n, "only {space}/{n} names contain a space");
    for ext in [
        "txt", "rs", "js", "json", "png", "jpg", "h", "c", "swift", "plist", "md", "dylib",
    ] {
        assert!(exts.contains(ext), "extension .{ext} never generated");
    }
    // Names are not all distinct copies of a handful of templates.
    let distinct: HashSet<&str> = names.iter().collect();
    assert!(
        distinct.len() * 2 >= n,
        "only {} distinct names of {n}",
        distinct.len()
    );
}

/// Golden checksum for `generate_names(GOLDEN_COUNT, GOLDEN_SEED)`.
///
/// Together with `rng_matches_reference_xoshiro256starstar_seeded_by_splitmix64`
/// this pins the generator's output across platforms and later edits.
/// Changing this value is a breaking change to seed stability: output for
/// an existing seed must never change.
const GOLDEN_COUNT: usize = 1_000;
const GOLDEN_SEED: u64 = 20_240_601;
const GOLDEN_NAMES_FNV1A64: u64 = 0xc3a8_0cee_929b_3be3;

#[test]
fn golden_names_checksum() {
    let names = generate_names(GOLDEN_COUNT, GOLDEN_SEED);
    check_names_invariants(&names, GOLDEN_COUNT);
    let actual = names.fnv1a64();
    // Same within a process, independent of other generator calls.
    let _ = generate_names(10, 0);
    assert_eq!(generate_names(GOLDEN_COUNT, GOLDEN_SEED).fnv1a64(), actual);
    assert_eq!(
        actual, GOLDEN_NAMES_FNV1A64,
        "generate_names({GOLDEN_COUNT}, {GOLDEN_SEED}) drifted: got {actual:#018x}, \
         pinned {GOLDEN_NAMES_FNV1A64:#018x}. Changing output for an existing seed is a \
         breaking change."
    );
}

/// Golden values for `generate_tree(GOLDEN_COUNT, GOLDEN_SEED, TreeShape::default())`.
///
/// The checksum is FNV-1a 64 over `names.buffer()`, then the little-endian
/// bytes of every `parent` value, then one byte per `is_dir` flag (the same
/// value `fx-bench gen --tree` prints). Changing any of these constants is a
/// breaking change to seed stability: output for an existing seed and shape
/// must never change.
const GOLDEN_TREE_FNV1A64: u64 = 0xf3a1_273b_dff6_b0e4;
const GOLDEN_TREE_DIR_COUNT: usize = 221;
/// `(index, Tree::path(index))` for a few fixed entries, including the last.
const GOLDEN_TREE_PATHS: &[(usize, &str)] = &[
    (0, "root"),
    (1, "root/20050602_050055.rs"),
    (
        500,
        "root/ModelImport/Schema Parser/2018-08-09 media/chunk-4799.h",
    ),
    (
        GOLDEN_COUNT - 1,
        "root/tmp559/.npm/DraftTableAudioFrame/ObjectConfigNetLight",
    ),
];

fn tree_fnv1a64(tree: &Tree) -> u64 {
    let mut bytes = tree.names.buffer().to_vec();
    for &p in &tree.parent {
        bytes.extend_from_slice(&p.to_le_bytes());
    }
    bytes.extend(tree.is_dir.iter().map(|&d| u8::from(d)));
    ref_fnv1a64(&bytes)
}

#[test]
fn golden_tree_checksum() {
    let tree = generate_tree(GOLDEN_COUNT, GOLDEN_SEED, TreeShape::default());
    check_tree_invariants(&tree, GOLDEN_COUNT, TreeShape::default().max_depth);
    let actual = tree_fnv1a64(&tree);
    assert_eq!(
        actual, GOLDEN_TREE_FNV1A64,
        "generate_tree({GOLDEN_COUNT}, {GOLDEN_SEED}, default) drifted: got {actual:#018x}, \
         pinned {GOLDEN_TREE_FNV1A64:#018x}. Changing output for an existing seed is a \
         breaking change."
    );
    assert_eq!(tree.dir_count(), GOLDEN_TREE_DIR_COUNT);
    for &(i, expected) in GOLDEN_TREE_PATHS {
        assert_eq!(tree.path(i), expected, "path({i}) drifted");
    }
}

// ---------------------------------------------------------------------------
// Tree
// ---------------------------------------------------------------------------

fn check_tree_invariants(tree: &Tree, entries: usize, max_depth: usize) {
    assert_eq!(tree.names.len(), entries);
    assert_eq!(tree.parent.len(), entries);
    assert_eq!(tree.is_dir.len(), entries);
    if entries == 0 {
        return;
    }
    assert_eq!(tree.parent[0], u32::MAX, "root's parent must be u32::MAX");
    assert!(tree.is_dir[0], "root must be a directory");
    assert_eq!(tree.depth(0), 0);
    assert_eq!(tree.dir_count(), tree.is_dir.iter().filter(|&&d| d).count());

    let mut siblings: HashSet<(u32, String)> = HashSet::with_capacity(entries);
    for i in 1..entries {
        let p = tree.parent[i];
        assert!(
            (p as usize) < i,
            "parent[{i}] = {p} does not precede the child"
        );
        assert!(
            tree.is_dir[p as usize],
            "parent {p} of entry {i} is not a directory"
        );
        assert_eq!(
            tree.depth(i),
            tree.depth(p as usize) + 1,
            "depth({i}) wrong"
        );
        assert!(
            tree.depth(i) <= max_depth,
            "depth({i}) = {} > {max_depth}",
            tree.depth(i)
        );
        // Unique within a directory, even on a case-insensitive file system
        // (default APFS), so the tree can be materialized on disk.
        let key = tree.names.get(i).to_ascii_lowercase();
        assert!(
            siblings.insert((p, key)),
            "duplicate sibling name {:?} under parent {p}",
            tree.names.get(i)
        );
    }
}

fn check_paths(tree: &Tree) {
    for i in 1..tree.names.len() {
        let p = tree.parent[i] as usize;
        let parent_path = tree.path(p);
        let path = tree.path(i);
        let name = tree.names.get(i);
        let expected = if parent_path.ends_with('/') {
            format!("{parent_path}{name}")
        } else {
            format!("{parent_path}/{name}")
        };
        assert_eq!(path, expected, "path({i}) is not path(parent) + '/' + name");
    }
}

#[test]
fn tree_structure_invariants_hold() {
    let entries = 200_000;
    let shape = TreeShape::default();
    assert_eq!(shape.max_depth, 32, "default max depth should be 32");
    let tree = generate_tree(entries, 1, shape);
    check_tree_invariants(&tree, entries, 32);

    // Directory share about 19% (370k folders of 1.93M entries).
    let dirs = tree.dir_count();
    assert!(
        dirs * 100 >= entries * 16 && dirs * 100 <= entries * 22,
        "{dirs} directories of {entries}, expected about 19%"
    );

    // Not a flat tree.
    let deepest = (0..entries).map(|i| tree.depth(i)).max().unwrap_or(0);
    assert!(
        deepest >= 5,
        "deepest entry at depth {deepest}, tree too flat"
    );
}

#[test]
fn tree_paths_join_parent_path_and_name() {
    let tree = generate_tree(20_000, 4, TreeShape::default());
    check_paths(&tree);
}

#[test]
fn tree_respects_custom_max_depth() {
    let shape = TreeShape {
        max_depth: 3,
        ..TreeShape::default()
    };
    let tree = generate_tree(50_000, 6, shape);
    check_tree_invariants(&tree, 50_000, 3);
}

#[test]
fn tree_small_sizes_work() {
    for entries in [1usize, 2, 10] {
        let tree = generate_tree(entries, 9, TreeShape::default());
        check_tree_invariants(&tree, entries, 32);
        check_paths(&tree);
    }
}

#[test]
fn tree_is_reproducible_per_seed() {
    let a = generate_tree(100_000, 12, TreeShape::default());
    let b = generate_tree(100_000, 12, TreeShape::default());
    assert!(a.names.buffer() == b.names.buffer());
    assert!(a.names.offsets() == b.names.offsets());
    assert_eq!(a.parent, b.parent);
    assert_eq!(a.is_dir, b.is_dir);

    let c = generate_tree(100_000, 13, TreeShape::default());
    assert!(a.parent != c.parent || a.names.buffer() != c.names.buffer());
}
