//! Gate: pre-sorted orders and filtered emit match a naive sort.
//!
//! Clean-room: written from docs/PLAN.md ("Sorting", "Updates and
//! concurrency", the fx-sort crate row) and the public fx-model / fx-index
//! APIs only.
//!
//! Contract pinned here:
//! - `SortSource` has `id_bound`, `is_live`, `name`, `parent` (returning
//!   `EntryId::NONE` for a root, as fx-index does), `size`, `modified`,
//!   `created` and `kind`, with the same signatures as `fx_index::Snapshot`'s
//!   accessors, and is implemented for `fx_index::Snapshot`.
//! - `Order::build(&src, key)` returns `Order { key, ids: Vec<u32> }` holding
//!   every live ID exactly once in the key's total order:
//!   - Name: case-folded name (char-wise `to_lowercase`), then raw name bytes,
//!     then EntryId.
//!   - Path: component-wise from the root, each component by the Name rule
//!     (including its EntryId), a shorter prefix first.
//!   - Size / Modified / Created: the numeric key, then the Name rule.
//! - `order.validate(&src)` accepts a correct order and rejects broken ones.
//! - `MatchSet` collects from an iterator of `EntryId` and answers `contains`.
//! - `emit(&order, &matches, Direction, offset: usize, limit: usize)` returns
//!   `Page { total: usize, ids: Vec<u32> }`: the full match count and only the
//!   requested window (no overflow for huge offsets or limits); descending is
//!   the exact reverse of ascending.
//! - `SortKey` is `Copy + PartialEq + Debug` and `Direction` is `Copy`; all
//!   items are re-exported at the crate root.
//! - `emit_tiered(&order, &matches, &first, Direction, offset, limit)`
//!   ("Your files first") lists the matches in `first` in the order and
//!   direction, then the other matches in the same order; `total` counts
//!   every match, as `emit` does.
//! - `group_subtrees(&path_order, &src, &[(EntryId, bool)], &left_out)`
//!   returns the `MatchSet` of live entries whose nearest listed entry at or
//!   above them (itself included) is listed `true`; an entry listed both
//!   ways counts as `false`, and listed IDs that are not live are ignored.
//!   An entry in `left_out`, or with one at or above it anywhere up to its
//!   root, is never in the group.
//! - `hidden_entries(&name_order, &src)` is the `MatchSet` of live entries
//!   whose name starts with `.`.

use std::collections::HashMap;

use fx_index::Index;
use fx_model::{Entry, EntryFlags, EntryId, EntryKind, FileKey, VolumeId};
use fx_sort::{
    Direction, MatchSet, Order, SortKey, SortSource, emit, emit_tiered, group_subtrees,
    hidden_entries,
};

const ALL_KEYS: [SortKey; 5] = [
    SortKey::Name,
    SortKey::Path,
    SortKey::Size,
    SortKey::Modified,
    SortKey::Created,
];

// ---------------------------------------------------------------------------
// Deterministic PRNG (splitmix64).
// ---------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next_u64() % 100 < percent
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

// ---------------------------------------------------------------------------
// In-memory mock source.
// ---------------------------------------------------------------------------

struct Mock {
    names: Vec<String>,
    parents: Vec<EntryId>,
    live: Vec<bool>,
    sizes: Vec<u64>,
    modified: Vec<i64>,
    created: Vec<i64>,
    kinds: Vec<EntryKind>,
}

impl SortSource for Mock {
    fn id_bound(&self) -> usize {
        self.names.len()
    }

    fn is_live(&self, id: EntryId) -> bool {
        self.live.get(id.index()).copied().unwrap_or(false)
    }

    fn name(&self, id: EntryId) -> &str {
        &self.names[id.index()]
    }

    fn parent(&self, id: EntryId) -> EntryId {
        self.parents[id.index()]
    }

    fn size(&self, id: EntryId) -> u64 {
        self.sizes[id.index()]
    }

    fn modified(&self, id: EntryId) -> i64 {
        self.modified[id.index()]
    }

    fn created(&self, id: EntryId) -> i64 {
        self.created[id.index()]
    }

    fn kind(&self, id: EntryId) -> EntryKind {
        self.kinds[id.index()]
    }
}

/// Names that collide in interesting ways: case variants, folds that differ
/// in raw bytes, non-ASCII, multi-char lowercase (`İ`), Kelvin sign.
const NAME_POOL: &[&str] = &[
    "a",
    "A",
    "b",
    "B",
    "readme.md",
    "README.md",
    "Readme.md",
    "é",
    "É",
    "e",
    "ß",
    "straße",
    "STRASSE",
    "Ωmega",
    "ωmega",
    "İ",
    "i",
    "I",
    "\u{212A}",
    "k",
    "K",
    "z",
    "Z",
    "x.txt",
    "日本",
    "中文",
    "a b",
    "a-b",
    "a_b",
    "10",
    "9",
    "~tmp",
    ".hidden",
];

const NAME_CHARS: &[char] = &[
    'a', 'B', 'c', 'D', 'e', 'é', 'É', 'ß', 'Ω', 'ω', 'İ', '1', '_', '.', ' ', '日', 'Z', 'k', 'K',
    '\u{212A}', '-', '~',
];

fn random_name(rng: &mut Rng) -> String {
    if rng.chance(55) {
        (*rng.pick(NAME_POOL)).to_string()
    } else {
        let len = 1 + rng.below(6);
        (0..len).map(|_| *rng.pick(NAME_CHARS)).collect()
    }
}

/// Random forest: several roots, mostly shallow folders plus some long
/// chains, many equal names, sizes and dates, and some removed subtrees.
fn random_mock(seed: u64, n: usize) -> Mock {
    let mut rng = Rng::new(seed);
    let mut m = Mock {
        names: Vec::with_capacity(n),
        parents: Vec::with_capacity(n),
        live: Vec::with_capacity(n),
        sizes: Vec::with_capacity(n),
        modified: Vec::with_capacity(n),
        created: Vec::with_capacity(n),
        kinds: Vec::with_capacity(n),
    };
    let mut dirs: Vec<u32> = Vec::new();
    let common_sizes = [0u64, 1, 100, 4096, u64::MAX >> 8];
    let common_dates = [0i64, -1, 1_600_000_000, 1_700_000_000, i64::MIN / 2];
    for i in 0..n {
        let id = i as u32;
        let parent = if dirs.is_empty() || rng.chance(2) {
            EntryId::NONE
        } else if rng.chance(25) {
            // Deep chains: hang off the most recent folder.
            EntryId(*dirs.last().unwrap())
        } else {
            EntryId(*rng.pick(&dirs))
        };
        let kind = if parent.is_none() || rng.chance(30) {
            EntryKind::Dir
        } else if rng.chance(5) {
            EntryKind::Symlink
        } else {
            EntryKind::File
        };
        if kind == EntryKind::Dir {
            dirs.push(id);
        }
        let size = if rng.chance(50) {
            *rng.pick(&common_sizes)
        } else {
            rng.next_u64() % 1_000_000
        };
        let modified = if rng.chance(50) {
            *rng.pick(&common_dates)
        } else {
            (rng.next_u64() % 2_000_000_000) as i64 - 100_000_000
        };
        let created = if rng.chance(50) {
            *rng.pick(&common_dates)
        } else {
            (rng.next_u64() % 2_000_000_000) as i64
        };
        // Removed entries take their whole subtree with them, as in fx-index.
        let parent_live = parent.is_none() || m.live[parent.index()];
        let live = parent_live && !rng.chance(4);
        m.names.push(random_name(&mut rng));
        m.parents.push(parent);
        m.live.push(live);
        m.sizes.push(size);
        m.modified.push(modified);
        m.created.push(created);
        m.kinds.push(kind);
    }
    m
}

const SEEDS: [u64; 4] = [1, 0xDEAD_BEEF, 42_424_242, 7];

// ---------------------------------------------------------------------------
// Naive reference sort, written straight from the documented rules.
// ---------------------------------------------------------------------------

type NameKey = (String, Vec<u8>, u32);

fn fold(name: &str) -> String {
    name.chars().flat_map(char::to_lowercase).collect()
}

fn name_key<S: SortSource>(src: &S, id: u32) -> NameKey {
    let name = src.name(EntryId(id));
    (fold(name), name.as_bytes().to_vec(), id)
}

fn path_key<S: SortSource>(src: &S, id: u32) -> Vec<NameKey> {
    let mut chain = Vec::new();
    let mut cur = EntryId(id);
    while !cur.is_none() {
        chain.push(name_key(src, cur.0));
        cur = src.parent(cur);
    }
    chain.reverse();
    chain
}

fn live_ids<S: SortSource>(src: &S) -> Vec<u32> {
    (0..src.id_bound() as u32)
        .filter(|&i| src.is_live(EntryId(i)))
        .collect()
}

fn naive_order<S: SortSource>(src: &S, key: SortKey) -> Vec<u32> {
    let ids = live_ids(src);
    let mut keyed: Vec<(i128, Vec<NameKey>, u32)> = ids
        .iter()
        .map(|&id| {
            let e = EntryId(id);
            let (num, names) = match key {
                SortKey::Name => (0i128, vec![name_key(src, id)]),
                SortKey::Path => (0i128, path_key(src, id)),
                SortKey::Size => (i128::from(src.size(e)), vec![name_key(src, id)]),
                SortKey::Modified => (i128::from(src.modified(e)), vec![name_key(src, id)]),
                SortKey::Created => (i128::from(src.created(e)), vec![name_key(src, id)]),
            };
            (num, names, id)
        })
        .collect();
    keyed.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.cmp(&b.1))
            .then(a.2.cmp(&b.2))
    });
    keyed.into_iter().map(|k| k.2).collect()
}

/// Naive emit: filter the naive order, reverse for descending, then page.
fn naive_page(
    sorted: &[u32],
    matches: &[bool],
    descending: bool,
    offset: usize,
    limit: usize,
) -> (usize, Vec<u32>) {
    let mut hits: Vec<u32> = sorted
        .iter()
        .copied()
        .filter(|&id| matches.get(id as usize).copied().unwrap_or(false))
        .collect();
    if descending {
        hits.reverse();
    }
    let total = hits.len();
    (total, hits.into_iter().skip(offset).take(limit).collect())
}

/// Lets the tests accept `validate` returning either `bool` or `Result`.
trait Verdict {
    fn passed(&self) -> bool;
}

impl Verdict for bool {
    fn passed(&self) -> bool {
        *self
    }
}

impl<E> Verdict for Result<(), E> {
    fn passed(&self) -> bool {
        self.is_ok()
    }
}

fn key_name(key: SortKey) -> &'static str {
    match key {
        SortKey::Name => "Name",
        SortKey::Path => "Path",
        SortKey::Size => "Size",
        SortKey::Modified => "Modified",
        SortKey::Created => "Created",
    }
}

// ---------------------------------------------------------------------------
// Order building.
// ---------------------------------------------------------------------------

#[test]
fn build_equals_naive_sort_for_every_key() {
    for seed in SEEDS {
        let src = random_mock(seed, 3000);
        for key in ALL_KEYS {
            let order = Order::build(&src, key);
            assert_eq!(order.key, key);
            let expected = naive_order(&src, key);
            assert_eq!(
                order.ids,
                expected,
                "seed {seed}, key {}: built order differs from naive sort",
                key_name(key)
            );
            assert!(
                order.validate(&src).passed(),
                "seed {seed}, key {}: validate rejected a correct order",
                key_name(key)
            );
        }
    }
}

#[test]
fn build_handles_empty_and_tiny_sources() {
    let empty = random_mock(3, 0);
    for key in ALL_KEYS {
        let order = Order::build(&empty, key);
        assert!(order.ids.is_empty());
        assert!(order.validate(&empty).passed());
    }
    let one = random_mock(3, 1);
    for key in ALL_KEYS {
        assert_eq!(Order::build(&one, key).ids, live_ids(&one));
    }
    // Everything removed: orders are empty.
    let mut dead = random_mock(5, 50);
    dead.live.iter_mut().for_each(|l| *l = false);
    for key in ALL_KEYS {
        assert!(Order::build(&dead, key).ids.is_empty());
    }
}

#[test]
fn name_rule_folds_case_then_raw_bytes_then_id() {
    // ids:        0    1    2    3    4        5
    let names = ["b", "B", "a", "A", "\u{212A}", "b"];
    let n = names.len();
    let src = Mock {
        names: names.iter().map(|s| s.to_string()).collect(),
        parents: vec![EntryId::NONE; n],
        live: vec![true; n],
        sizes: vec![7; n],
        modified: vec![0; n],
        created: vec![0; n],
        kinds: vec![EntryKind::File; n],
    };
    // "A" < "a" by raw bytes within the fold "a"; "B" < "b" < "b"(id 5);
    // the Kelvin sign folds to "k" and comes last.
    let expected = vec![3, 2, 1, 0, 5, 4];
    for key in ALL_KEYS {
        assert_eq!(Order::build(&src, key).ids, expected, "{}", key_name(key));
    }
}

#[test]
fn path_order_keeps_every_subtree_contiguous_after_its_folder() {
    for seed in SEEDS {
        let src = random_mock(seed, 2000);
        let order = Order::build(&src, SortKey::Path);
        let pos: HashMap<u32, usize> = order
            .ids
            .iter()
            .enumerate()
            .map(|(p, &id)| (id, p))
            .collect();
        // Live descendant count of every entry.
        let mut subtree = vec![0usize; src.id_bound()];
        for &id in &order.ids {
            let mut cur = src.parent(EntryId(id));
            while !cur.is_none() {
                subtree[cur.index()] += 1;
                cur = src.parent(cur);
            }
        }
        // Each descendant lies in the window of `subtree` slots right after
        // its ancestor; with exact counts, the window is the subtree.
        for (q, &id) in order.ids.iter().enumerate() {
            let mut cur = src.parent(EntryId(id));
            while !cur.is_none() {
                let p = pos[&cur.0];
                let c = subtree[cur.index()];
                assert!(
                    q > p && q <= p + c,
                    "seed {seed}: descendant {id} at {q}, ancestor {} at {p}, subtree size {c}",
                    cur.0
                );
                cur = src.parent(cur);
            }
        }
    }
}

#[test]
fn validate_rejects_broken_orders() {
    let src = random_mock(11, 500);
    for key in ALL_KEYS {
        let good = Order::build(&src, key);
        assert!(good.ids.len() > 3);

        let mut swapped = Order::build(&src, key);
        swapped.ids.swap(1, 2);
        assert!(!swapped.validate(&src).passed(), "swap: {}", key_name(key));

        let mut missing = Order::build(&src, key);
        missing.ids.pop();
        assert!(
            !missing.validate(&src).passed(),
            "missing: {}",
            key_name(key)
        );

        let mut duplicate = Order::build(&src, key);
        let first = duplicate.ids[0];
        duplicate.ids.insert(0, first);
        assert!(!duplicate.validate(&src).passed(), "dup: {}", key_name(key));

        let dead = (0..src.id_bound() as u32)
            .find(|&i| !src.is_live(EntryId(i)))
            .expect("mock has removed entries");
        let mut with_dead = Order::build(&src, key);
        with_dead.ids.push(dead);
        assert!(
            !with_dead.validate(&src).passed(),
            "dead: {}",
            key_name(key)
        );
    }
}

// ---------------------------------------------------------------------------
// Match sets and emit.
// ---------------------------------------------------------------------------

#[test]
fn match_set_contains_exactly_its_ids() {
    let ids = [0u32, 3, 63, 64, 65, 1000];
    let set: MatchSet = ids.iter().map(|&i| EntryId(i)).collect();
    for i in 0..1100u32 {
        assert_eq!(set.contains(EntryId(i)), ids.contains(&i), "id {i}");
    }
    // Past the largest member: not contained, no panic.
    assert!(!set.contains(EntryId(1_000_000)));
    let empty: MatchSet = std::iter::empty::<EntryId>().collect();
    assert!(!empty.contains(EntryId(0)));
}

fn match_vectors(src: &Mock, rng: &mut Rng) -> Vec<Vec<bool>> {
    let n = src.id_bound();
    let mut sets = vec![vec![false; n], vec![true; n]];
    for percent in [1u64, 30, 90] {
        sets.push((0..n).map(|_| rng.chance(percent)).collect());
    }
    sets
}

#[test]
fn emit_equals_naive_filter_sort_page_both_directions() {
    for seed in SEEDS {
        let src = random_mock(seed, 2500);
        let mut rng = Rng::new(seed ^ 0x5555);
        let sets = match_vectors(&src, &mut rng);
        for key in ALL_KEYS {
            let order = Order::build(&src, key);
            let sorted = naive_order(&src, key);
            for flags in &sets {
                // Includes removed IDs when they are flagged: they are not in
                // the order, so they must never be emitted or counted.
                let set: MatchSet = flags
                    .iter()
                    .enumerate()
                    .filter(|&(_, &m)| m)
                    .map(|(i, _)| EntryId(i as u32))
                    .collect();
                let hits = sorted.iter().filter(|&&id| flags[id as usize]).count();
                let mut windows = vec![
                    (0, usize::MAX),
                    (0, 0),
                    (0, 1),
                    (0, 50),
                    (7, 13),
                    (hits.saturating_sub(3), 10),
                    (hits, 5),
                    (hits + 100, 5),
                    (usize::MAX, usize::MAX),
                    (3, usize::MAX),
                ];
                for _ in 0..4 {
                    windows.push((rng.below(hits + 2), rng.below(200)));
                }
                for (dir, descending) in
                    [(Direction::Ascending, false), (Direction::Descending, true)]
                {
                    for &(offset, limit) in &windows {
                        let page = emit(&order, &set, dir, offset, limit);
                        let (total, ids) = naive_page(&sorted, flags, descending, offset, limit);
                        assert_eq!(
                            page.total,
                            total,
                            "seed {seed}, key {}, desc {descending}, window ({offset}, {limit}): total",
                            key_name(key)
                        );
                        assert_eq!(
                            page.ids,
                            ids,
                            "seed {seed}, key {}, desc {descending}, window ({offset}, {limit}): ids",
                            key_name(key)
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn descending_is_exact_reverse_of_ascending() {
    let src = random_mock(99, 1500);
    let all: MatchSet = (0..src.id_bound() as u32).map(EntryId).collect();
    for key in ALL_KEYS {
        let order = Order::build(&src, key);
        let asc = emit(&order, &all, Direction::Ascending, 0, usize::MAX);
        let desc = emit(&order, &all, Direction::Descending, 0, usize::MAX);
        assert_eq!(asc.ids, order.ids);
        let mut reversed = asc.ids.clone();
        reversed.reverse();
        assert_eq!(desc.ids, reversed);
        assert_eq!(asc.total, order.ids.len());
        assert_eq!(desc.total, order.ids.len());
    }
}

#[test]
fn emit_with_empty_match_set_or_empty_order() {
    let src = random_mock(17, 300);
    let none: MatchSet = std::iter::empty::<EntryId>().collect();
    let empty_src = random_mock(17, 0);
    let all: MatchSet = (0..300u32).map(EntryId).collect();
    for key in ALL_KEYS {
        let order = Order::build(&src, key);
        for dir in [Direction::Ascending, Direction::Descending] {
            let page = emit(&order, &none, dir, 0, 100);
            assert_eq!(page.total, 0);
            assert!(page.ids.is_empty());

            let empty_order = Order::build(&empty_src, key);
            let page = emit(&empty_order, &all, dir, 0, 100);
            assert_eq!(page.total, 0);
            assert!(page.ids.is_empty());
        }
    }
}

// ---------------------------------------------------------------------------
// Real fx-index snapshot.
// ---------------------------------------------------------------------------

fn fk(n: u64) -> FileKey {
    FileKey::new(VolumeId(1), n)
}

fn entry(rng: &mut Rng, key: u64, parent: Option<u64>, kind: EntryKind) -> Entry {
    Entry {
        key: fk(key),
        parent: parent.map(fk),
        name: random_name(rng),
        kind,
        flags: EntryFlags::default(),
        size: if rng.chance(50) {
            4096
        } else {
            rng.next_u64() % 10_000
        },
        modified: if rng.chance(50) {
            1_700_000_000
        } else {
            (rng.next_u64() % 1_000_000) as i64
        },
        created: if rng.chance(50) {
            0
        } else {
            (rng.next_u64() % 1_000_000) as i64
        },
    }
}

#[test]
fn builds_orders_from_a_real_index_snapshot() {
    let mut rng = Rng::new(0xF00D);
    let mut index = Index::new();
    let mut dirs: Vec<u64> = Vec::new();
    let mut files: Vec<u64> = Vec::new();
    for key in 1..=1500u64 {
        let parent = if dirs.is_empty() || rng.chance(1) {
            None
        } else {
            Some(*rng.pick(&dirs))
        };
        let kind = if parent.is_none() || rng.chance(25) {
            EntryKind::Dir
        } else {
            EntryKind::File
        };
        index
            .upsert(entry(&mut rng, key, parent, kind))
            .expect("upsert");
        if kind == EntryKind::Dir {
            dirs.push(key);
        } else {
            files.push(key);
        }
    }

    let check = |index: &Index, label: &str| {
        let snap = index.snapshot();
        let snap: &fx_index::Snapshot = &snap;
        assert_eq!(SortSource::id_bound(snap), snap.id_bound());
        // The trait walks parents the same way the index builds paths.
        for id in snap.live_ids() {
            let joined: Vec<String> = path_key(snap, id.0)
                .into_iter()
                .map(|(_, raw, _)| String::from_utf8(raw).unwrap())
                .collect();
            assert_eq!(joined.join("/"), snap.path(id));
        }
        for key in ALL_KEYS {
            let order = Order::build(snap, key);
            assert_eq!(order.ids.len(), snap.live_count());
            assert_eq!(
                order.ids,
                naive_order(snap, key),
                "{label}, key {}",
                key_name(key)
            );
            assert!(order.validate(snap).passed());
        }
    };
    check(&index, "fresh");

    // Renames, moves (into later folders too) and subtree removals.
    for _ in 0..200 {
        let key = *rng.pick(&files);
        if index.lookup(fk(key)).is_none() {
            continue;
        }
        let parent = *rng.pick(&dirs);
        if index.lookup(fk(parent)).is_none() {
            continue;
        }
        let _ = index.upsert(entry(&mut rng, key, Some(parent), EntryKind::File));
    }
    // Folder renames and moves carry their subtrees; cycles are refused by
    // the index and simply skipped here.
    for _ in 0..60 {
        let key = *rng.pick(&dirs);
        let parent = *rng.pick(&dirs);
        if key == parent || index.lookup(fk(key)).is_none() || index.lookup(fk(parent)).is_none() {
            continue;
        }
        let _ = index.upsert(entry(&mut rng, key, Some(parent), EntryKind::Dir));
    }
    for _ in 0..30 {
        let key = *rng.pick(&dirs);
        index.remove(fk(key));
    }
    let snap_live = index.live_count();
    assert!(snap_live > 0);
    check(&index, "after changes");

    // A snapshot keeps its order valid after the index moves on.
    let snap = index.snapshot();
    let before: Vec<Order> = ALL_KEYS.iter().map(|&k| Order::build(&*snap, k)).collect();
    for key in 2000..2100u64 {
        let _ = index.upsert(entry(&mut rng, key, Some(dirs[0]), EntryKind::File));
    }
    for (order, &key) in before.iter().zip(ALL_KEYS.iter()) {
        assert_eq!(order.ids, naive_order(&*snap, key));
        assert!(order.validate(&*snap).passed());
    }
}

// ---------------------------------------------------------------------------
// Tiered emit and subtree groups ("Your files first").
// ---------------------------------------------------------------------------

fn set_of(flags: &[bool]) -> MatchSet {
    flags
        .iter()
        .enumerate()
        .filter(|&(_, &m)| m)
        .map(|(i, _)| EntryId(i as u32))
        .collect()
}

/// Naive tiered emit: filter the naive order, reverse for descending, put
/// the matches in `first` before the others (each part keeping its order),
/// then page.
fn naive_tiered(
    sorted: &[u32],
    matches: &[bool],
    first: &[bool],
    descending: bool,
) -> (usize, Vec<u32>) {
    let (total, hits) = naive_page(sorted, matches, descending, 0, usize::MAX);
    let (mut front, back): (Vec<u32>, Vec<u32>) =
        hits.into_iter().partition(|&id| first[id as usize]);
    front.extend(back);
    (total, front)
}

#[test]
fn emit_tiered_equals_naive_grouped_filter_sort_page() {
    for seed in [3u64, 0xC0FF_EE00] {
        let src = random_mock(seed, 240);
        let n = src.id_bound();
        let mut rng = Rng::new(seed ^ 0x7777);
        let match_sets = match_vectors(&src, &mut rng);
        let mut first_sets = vec![vec![false; n], vec![true; n]];
        for percent in [20u64, 60] {
            first_sets.push((0..n).map(|_| rng.chance(percent)).collect());
        }
        for key in ALL_KEYS {
            let order = Order::build(&src, key);
            let sorted = naive_order(&src, key);
            for flags in &match_sets {
                let set = set_of(flags);
                for first_flags in &first_sets {
                    let first = set_of(first_flags);
                    for (dir, descending) in
                        [(Direction::Ascending, false), (Direction::Descending, true)]
                    {
                        let (total, all) = naive_tiered(&sorted, flags, first_flags, descending);
                        let mut windows: Vec<(usize, usize)> = (0..=total + 2)
                            .flat_map(|offset| [(offset, 1), (offset, 7), (offset, usize::MAX)])
                            .collect();
                        windows.extend([(0, 0), (usize::MAX, usize::MAX), (usize::MAX, 1)]);
                        for (offset, limit) in windows {
                            let page = emit_tiered(&order, &set, &first, dir, offset, limit);
                            let want: Vec<u32> =
                                all.iter().copied().skip(offset).take(limit).collect();
                            assert_eq!(
                                page.total,
                                total,
                                "seed {seed}, key {}, desc {descending}, window ({offset}, {limit}): total",
                                key_name(key)
                            );
                            assert_eq!(
                                page.ids,
                                want,
                                "seed {seed}, key {}, desc {descending}, window ({offset}, {limit}): ids",
                                key_name(key)
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn emit_tiered_without_a_first_set_is_emit() {
    let src = random_mock(23, 800);
    let mut rng = Rng::new(23);
    let none = MatchSet::new();
    for key in ALL_KEYS {
        let order = Order::build(&src, key);
        for flags in match_vectors(&src, &mut rng) {
            let set = set_of(&flags);
            for dir in [Direction::Ascending, Direction::Descending] {
                for (offset, limit) in [(0, 100), (50, 25), (0, usize::MAX)] {
                    assert_eq!(
                        emit_tiered(&order, &set, &none, dir, offset, limit),
                        emit(&order, &set, dir, offset, limit),
                        "key {}",
                        key_name(key)
                    );
                }
            }
        }
    }
}

/// Whether `id` is in the group: nothing at or above it is left out, and
/// the nearest listed entry at or above it decides, `false` winning when one
/// entry is listed both ways.
fn naive_in_group(src: &Mock, listed: &[(EntryId, bool)], left_out: &[bool], id: u32) -> bool {
    let mut cur = EntryId(id);
    while !cur.is_none() {
        if left_out[cur.index()] {
            return false;
        }
        cur = src.parent(cur);
    }
    let mut cur = EntryId(id);
    while !cur.is_none() {
        let flags: Vec<bool> = listed
            .iter()
            .filter(|(e, _)| *e == cur && src.is_live(*e))
            .map(|&(_, f)| f)
            .collect();
        if !flags.is_empty() {
            return flags.iter().all(|&f| f);
        }
        cur = src.parent(cur);
    }
    false
}

#[test]
fn group_subtrees_takes_the_nearest_listed_folder() {
    for seed in SEEDS {
        let src = random_mock(seed, 2000);
        let path = Order::build(&src, SortKey::Path);
        let mut rng = Rng::new(seed ^ 0x9999);
        let ids: Vec<u32> = (0..src.id_bound() as u32).collect();
        let dirs: Vec<u32> = ids
            .iter()
            .copied()
            .filter(|&i| src.kinds[i as usize] == EntryKind::Dir)
            .collect();
        let roots: Vec<u32> = ids
            .iter()
            .copied()
            .filter(|&i| src.parents[i as usize].is_none() && src.is_live(EntryId(i)))
            .collect();
        let name = Order::build(&src, SortKey::Name);
        let hidden = hidden_entries(&name, &src);
        let hidden_flags: Vec<bool> = ids.iter().map(|&i| hidden.contains(EntryId(i))).collect();
        for round in 0..40 {
            let mut listed: Vec<(EntryId, bool)> = (0..1 + rng.below(6))
                .map(|_| {
                    // Mostly folders, sometimes files or removed entries.
                    let id = if rng.chance(80) {
                        *rng.pick(&dirs)
                    } else {
                        *rng.pick(&ids)
                    };
                    (EntryId(id), rng.chance(50))
                })
                .collect();
            if round % 5 == 0 {
                let (id, flag) = listed[0];
                listed.push((id, !flag));
            }
            if round % 2 == 0 {
                // A whole tree first, with folders listed inside it.
                listed.insert(0, (EntryId(*rng.pick(&roots)), true));
            }
            // Nothing, the hidden entries, or random entries left out.
            let left_out_flags: Vec<bool> = match round % 4 {
                0 => vec![false; ids.len()],
                1 | 2 => hidden_flags.clone(),
                _ => ids.iter().map(|_| rng.chance(3)).collect(),
            };
            let left_out = set_of(&left_out_flags);
            let group = group_subtrees(&path, &src, &listed, &left_out);
            let mut count = 0;
            for &id in &ids {
                let want =
                    src.is_live(EntryId(id)) && naive_in_group(&src, &listed, &left_out_flags, id);
                count += usize::from(want);
                assert_eq!(
                    group.contains(EntryId(id)),
                    want,
                    "seed {seed}, round {round}, id {id}, listed {listed:?}"
                );
            }
            assert_eq!(group.len(), count, "seed {seed}, round {round}");
        }
        assert!(group_subtrees(&path, &src, &[], &MatchSet::new()).is_empty());
    }
}

#[test]
fn hidden_entries_are_the_live_names_starting_with_a_dot() {
    for seed in SEEDS {
        let mut src = random_mock(seed, 3000);
        // Names around the dot in the Name order, and the root-like "/".
        for (i, name) in [".", "..", ".a", "-x", "/", "", ".DS_Store", "\u{00B7}x"]
            .iter()
            .enumerate()
        {
            src.names[i * 7] = name.to_string();
        }
        let name = Order::build(&src, SortKey::Name);
        let hidden = hidden_entries(&name, &src);
        let mut count = 0;
        for id in 0..src.id_bound() as u32 {
            let want = src.is_live(EntryId(id)) && src.names[id as usize].starts_with('.');
            count += usize::from(want);
            assert_eq!(
                hidden.contains(EntryId(id)),
                want,
                "seed {seed}, id {id} {:?}",
                src.names[id as usize]
            );
        }
        assert!(count > 0, "seed {seed}: the mock has hidden names");
        assert_eq!(hidden.len(), count);
    }
    let empty = random_mock(1, 0);
    assert!(hidden_entries(&Order::build(&empty, SortKey::Name), &empty).is_empty());
}
