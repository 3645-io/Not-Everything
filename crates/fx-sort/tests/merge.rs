//! Gate: batched merges keep every order equal to a fresh build.
//!
//! Clean-room: written from docs/PLAN.md ("Sorting", "Updates and
//! concurrency", the fx-sort crate row) and the public fx-model / fx-sort
//! APIs only.
//!
//! Contract pinned here:
//! - `Batch::new()` starts an empty batch; `batch.remove(EntryId)` records a
//!   removed entry, `batch.upsert(EntryId)` a new entry or an attribute-only
//!   change (size, dates, kind), and `batch.moved(EntryId)` an entry whose
//!   name or parent changed (a rename or a move). A moved ID also counts as
//!   upserted for every order. The changes are already applied to the source
//!   the batch is merged against.
//! - Only moved IDs expand to their subtree, and only for `SortKey::Path`.
//!   A plain upsert re-sorts just the entry itself in every order, so a
//!   folder whose size or dates changed does not re-sort its subtree.
//!   Callers that rename or reparent a folder must use `moved()`.
//! - `order.apply(&src, &batch)` updates the order in place so that it equals
//!   `Order::build(&src, order.key)` for the new state in `src`:
//!   - duplicate IDs in either list are harmless;
//!   - removing an ID that is not in the order is a no-op, upserting an
//!     unchanged entry is harmless, and upserting an ID that is not live is
//!     ignored;
//!   - an ID both removed and upserted ends up present exactly when it is
//!     live in `src`;
//!   - for `SortKey::Path`, listing only a moved or renamed folder through
//!     `moved()` moves its whole subtree (descendants are not listed in the
//!     batch).
//!
//! The random model keeps the tree consistent the way the index does: a
//! deleted folder takes its whole subtree with it, so every live entry's
//! ancestors are live, and moves never create cycles.

use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

use fx_model::{EntryId, EntryKind};
use fx_sort::{Batch, Order, SortKey, SortSource};

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

#[derive(Default)]
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

impl Mock {
    /// Appends a live entry and returns its ID.
    fn push(&mut self, name: &str, parent: EntryId, kind: EntryKind) -> u32 {
        let id = self.names.len() as u32;
        self.names.push(name.to_string());
        self.parents.push(parent);
        self.live.push(true);
        self.sizes.push(0);
        self.modified.push(0);
        self.created.push(0);
        self.kinds.push(kind);
        id
    }

    /// Overwrites slot `id` with a fresh live entry (ID reuse).
    fn reuse(&mut self, id: u32, name: String, parent: EntryId, kind: EntryKind) {
        let i = id as usize;
        self.names[i] = name;
        self.parents[i] = parent;
        self.live[i] = true;
        self.kinds[i] = kind;
    }

    fn live_ids(&self) -> Vec<u32> {
        (0..self.names.len() as u32)
            .filter(|&i| self.live[i as usize])
            .collect()
    }

    fn live_dirs(&self) -> Vec<u32> {
        (0..self.names.len() as u32)
            .filter(|&i| self.live[i as usize] && self.kinds[i as usize] == EntryKind::Dir)
            .collect()
    }

    /// Whether `anc` is `id` or one of its ancestors.
    fn is_ancestor_or_self(&self, anc: u32, id: u32) -> bool {
        let mut cur = EntryId(id);
        let mut steps = 0;
        while !cur.is_none() && steps <= self.names.len() {
            if cur.0 == anc {
                return true;
            }
            cur = self.parents[cur.index()];
            steps += 1;
        }
        false
    }

    /// `id` and every live descendant.
    fn subtree(&self, id: u32) -> Vec<u32> {
        self.live_ids()
            .into_iter()
            .filter(|&i| self.is_ancestor_or_self(id, i))
            .collect()
    }
}

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
    "a b",
    "a-b",
    "10",
    "9",
    ".hidden",
];

const NAME_CHARS: &[char] = &[
    'a', 'B', 'c', 'D', 'e', 'é', 'É', 'ß', 'Ω', 'ω', 'İ', '1', '_', '.', ' ', '日', 'Z', 'k', 'K',
    '\u{212A}', '-',
];

const COMMON_SIZES: [u64; 5] = [0, 1, 100, 4096, u64::MAX >> 8];
const COMMON_DATES: [i64; 5] = [0, -1, 1_600_000_000, 1_700_000_000, i64::MIN / 2];

fn random_name(rng: &mut Rng) -> String {
    if rng.chance(55) {
        (*rng.pick(NAME_POOL)).to_string()
    } else {
        let len = 1 + rng.below(5);
        (0..len).map(|_| *rng.pick(NAME_CHARS)).collect()
    }
}

fn random_size(rng: &mut Rng) -> u64 {
    if rng.chance(60) {
        *rng.pick(&COMMON_SIZES)
    } else {
        rng.next_u64() >> rng.below(64)
    }
}

fn random_date(rng: &mut Rng) -> i64 {
    if rng.chance(60) {
        *rng.pick(&COMMON_DATES)
    } else {
        (rng.next_u64() % 4_000_000_000) as i64 - 1_000_000_000
    }
}

/// Swaps the case of every character that has a simple case partner.
fn swap_case(name: &str) -> String {
    name.chars()
        .flat_map(|c| {
            if c.is_lowercase() {
                c.to_uppercase().collect::<Vec<_>>()
            } else {
                c.to_lowercase().collect::<Vec<_>>()
            }
        })
        .collect()
}

/// A random parent for a new or moved entry: a live folder, sometimes a root.
fn random_parent(rng: &mut Rng, m: &Mock) -> EntryId {
    let dirs = m.live_dirs();
    if dirs.is_empty() || rng.chance(5) {
        EntryId::NONE
    } else {
        EntryId(*rng.pick(&dirs))
    }
}

fn random_entry(rng: &mut Rng, m: &mut Mock, parent: EntryId) -> u32 {
    let kind = if rng.chance(35) {
        EntryKind::Dir
    } else if rng.chance(5) {
        EntryKind::Symlink
    } else {
        EntryKind::File
    };
    let name = random_name(rng);
    let id = m.push(&name, parent, kind);
    let i = id as usize;
    m.sizes[i] = random_size(rng);
    m.modified[i] = random_date(rng);
    m.created[i] = random_date(rng);
    id
}

/// A random forest of `n` live entries: several roots, deep chains, many
/// equal names, sizes and dates.
fn random_mock(rng: &mut Rng, n: usize) -> Mock {
    let mut m = Mock::default();
    let mut last_dir: Option<u32> = None;
    for _ in 0..n {
        let parent = match last_dir {
            Some(d) if rng.chance(25) => EntryId(d),
            _ => random_parent(rng, &m),
        };
        let id = random_entry(rng, &mut m, parent);
        if m.kinds[id as usize] == EntryKind::Dir {
            last_dir = Some(id);
        }
    }
    m
}

/// Applies one random change to `m` and records it in `batch`.
fn random_change(rng: &mut Rng, m: &mut Mock, batch: &mut Batch) {
    let live = m.live_ids();
    let op = if live.is_empty() { 0 } else { rng.below(10) };
    match op {
        // Insert: a fresh ID, or reuse a dead slot.
        0 | 1 => {
            let dead: Vec<u32> = (0..m.names.len() as u32)
                .filter(|&i| !m.live[i as usize])
                .collect();
            let parent = random_parent(rng, m);
            if !dead.is_empty() && rng.chance(30) {
                let id = *rng.pick(&dead);
                let kind = if rng.chance(35) {
                    EntryKind::Dir
                } else {
                    EntryKind::File
                };
                let name = random_name(rng);
                m.reuse(id, name, parent, kind);
                let i = id as usize;
                m.sizes[i] = random_size(rng);
                m.modified[i] = random_date(rng);
                m.created[i] = random_date(rng);
                batch.upsert(EntryId(id));
            } else {
                let id = random_entry(rng, m, parent);
                batch.upsert(EntryId(id));
            }
        }
        // Delete an entry with its whole subtree.
        2 => {
            let id = *rng.pick(&live);
            for d in m.subtree(id) {
                m.live[d as usize] = false;
                batch.remove(EntryId(d));
            }
        }
        // Rename, sometimes changing only the case.
        3 => {
            let id = *rng.pick(&live);
            let i = id as usize;
            m.names[i] = if rng.chance(40) {
                swap_case(&m.names[i])
            } else {
                random_name(rng)
            };
            batch.moved(EntryId(id));
        }
        // Size change, mostly on folders so their subtrees are exercised.
        4 => {
            let id = pick_mostly_dir(rng, m, &live);
            m.sizes[id as usize] = random_size(rng);
            batch.upsert(EntryId(id));
        }
        // Date changes, mostly on folders.
        5 => {
            let id = pick_mostly_dir(rng, m, &live);
            let i = id as usize;
            m.modified[i] = random_date(rng);
            if rng.chance(40) {
                m.created[i] = random_date(rng);
            }
            batch.upsert(EntryId(id));
        }
        // Move (mostly folders, so subtrees travel), sometimes also renamed.
        6..=8 => {
            let dirs = m.live_dirs();
            let id = if !dirs.is_empty() && rng.chance(75) {
                *rng.pick(&dirs)
            } else {
                *rng.pick(&live)
            };
            let targets: Vec<u32> = dirs
                .into_iter()
                .filter(|&d| !m.is_ancestor_or_self(id, d))
                .collect();
            let i = id as usize;
            m.parents[i] = if targets.is_empty() || rng.chance(10) {
                EntryId::NONE
            } else {
                EntryId(*rng.pick(&targets))
            };
            if rng.chance(30) {
                m.names[i] = random_name(rng);
            }
            // Attribute changes may ride along with a move.
            if rng.chance(20) {
                m.sizes[i] = random_size(rng);
            }
            batch.moved(EntryId(id));
        }
        // Touch: list an entry that did not change, as an upsert or (rarely)
        // as a move; both are harmless.
        _ => {
            let id = *rng.pick(&live);
            if rng.chance(20) {
                batch.moved(EntryId(id));
            } else {
                batch.upsert(EntryId(id));
            }
        }
    }
}

/// A live entry, a folder three times out of four when one exists.
fn pick_mostly_dir(rng: &mut Rng, m: &Mock, live: &[u32]) -> u32 {
    let dirs = m.live_dirs();
    if !dirs.is_empty() && rng.chance(75) {
        *rng.pick(&dirs)
    } else {
        *rng.pick(live)
    }
}

fn build_all(m: &Mock) -> Vec<Order> {
    ALL_KEYS.iter().map(|&k| Order::build(m, k)).collect()
}

fn apply_and_check(orders: &mut [Order], m: &Mock, batch: &Batch, context: &str) {
    for order in orders.iter_mut() {
        let key = order.key;
        order.apply(m, batch);
        assert_eq!(order.key, key, "{context}: apply changed the key");
        let fresh = Order::build(m, key);
        assert_eq!(
            order.ids, fresh.ids,
            "{context}: merged {key:?} order differs from a fresh build"
        );
        if let Err(e) = order.validate(m) {
            panic!("{context}: merged {key:?} order is invalid: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Random rounds.
// ---------------------------------------------------------------------------

fn random_rounds(seed: u64, n: usize, rounds: usize, max_changes: usize) {
    let mut rng = Rng::new(seed);
    let mut m = random_mock(&mut rng, n);
    let mut orders = build_all(&m);
    for round in 0..rounds {
        let mut batch = Batch::new();
        let changes = 1 + rng.below(max_changes);
        for _ in 0..changes {
            random_change(&mut rng, &mut m, &mut batch);
        }
        // Occasionally repeat an ID in the batch.
        if rng.chance(15) {
            let live = m.live_ids();
            if !live.is_empty() {
                let id = *rng.pick(&live);
                batch.upsert(EntryId(id));
                batch.upsert(EntryId(id));
            }
        }
        apply_and_check(
            &mut orders,
            &m,
            &batch,
            &format!("seed {seed} round {round}"),
        );
    }
}

#[test]
fn random_small_batches_match_fresh_build() {
    for seed in 0..6u64 {
        random_rounds(0x5EED_0000 + seed, 150, 120, 4);
    }
}

#[test]
fn random_large_batches_match_fresh_build() {
    for seed in 0..4u64 {
        random_rounds(0xBA7C_0000 + seed, 400, 40, 40);
    }
}

#[test]
fn random_rounds_from_an_empty_source() {
    for seed in 0..4u64 {
        random_rounds(0xE417_0000 + seed, 0, 80, 6);
    }
}

// ---------------------------------------------------------------------------
// Targeted cases.
// ---------------------------------------------------------------------------

/// root/
///   docs/        (dir)  1
///     a.txt             2
///     B.txt             3
///     sub/       (dir)  4
///       z               5
///   dOcs                6
///   other/     (dir)    7
///     x                 8
///   readme              9
fn small_tree() -> Mock {
    let mut m = Mock::default();
    let root = m.push("root", EntryId::NONE, EntryKind::Dir);
    let docs = m.push("docs", EntryId(root), EntryKind::Dir);
    m.push("a.txt", EntryId(docs), EntryKind::File);
    m.push("B.txt", EntryId(docs), EntryKind::File);
    let sub = m.push("sub", EntryId(docs), EntryKind::Dir);
    m.push("z", EntryId(sub), EntryKind::File);
    m.push("dOcs", EntryId(root), EntryKind::File);
    let other = m.push("other", EntryId(root), EntryKind::Dir);
    m.push("x", EntryId(other), EntryKind::File);
    m.push("readme", EntryId(root), EntryKind::File);
    for i in 0..m.names.len() {
        m.sizes[i] = (i as u64 * 7) % 4;
        m.modified[i] = (i as i64 * 5) % 3;
        m.created[i] = (i as i64 * 3) % 4;
    }
    m
}

#[test]
fn empty_batch_changes_nothing() {
    let mut rng = Rng::new(0xE3E3);
    let m = random_mock(&mut rng, 300);
    let mut orders = build_all(&m);
    let before = orders.clone();
    let batch = Batch::new();
    apply_and_check(&mut orders, &m, &batch, "empty batch");
    assert_eq!(orders, before);

    let empty = Mock::default();
    let mut orders = build_all(&empty);
    apply_and_check(
        &mut orders,
        &empty,
        &Batch::new(),
        "empty batch, empty source",
    );
    assert!(orders.iter().all(|o| o.ids.is_empty()));
}

#[test]
fn batch_that_deletes_everything_empties_every_order() {
    let mut rng = Rng::new(0xDE1E7E);
    let mut m = random_mock(&mut rng, 300);
    let mut orders = build_all(&m);
    let mut batch = Batch::new();
    for id in m.live_ids() {
        m.live[id as usize] = false;
        batch.remove(EntryId(id));
    }
    apply_and_check(&mut orders, &m, &batch, "delete everything");
    assert!(orders.iter().all(|o| o.ids.is_empty()));
}

#[test]
fn duplicate_ids_in_a_batch_are_harmless() {
    let mut m = small_tree();
    let mut orders = build_all(&m);
    let mut batch = Batch::new();
    // The same rename listed three times, twice as a move and once as an
    // upsert.
    m.names[9] = "Aaa".to_string();
    batch.moved(EntryId(9));
    batch.upsert(EntryId(9));
    batch.moved(EntryId(9));
    // The same delete listed twice.
    m.live[8] = false;
    batch.remove(EntryId(8));
    batch.remove(EntryId(8));
    // Removed then re-inserted under the same ID: live, so present.
    m.names[3] = "c.txt".to_string();
    m.sizes[3] = 99;
    batch.remove(EntryId(3));
    batch.moved(EntryId(3));
    batch.remove(EntryId(3));
    // Inserted then removed in the same batch: not live, so absent.
    let gone = m.push("gone", EntryId(1), EntryKind::File);
    m.live[gone as usize] = false;
    batch.upsert(EntryId(gone));
    batch.remove(EntryId(gone));
    batch.upsert(EntryId(gone));
    apply_and_check(&mut orders, &m, &batch, "duplicates");
    for order in &orders {
        assert!(
            order.ids.contains(&3),
            "{:?} lost re-inserted id 3",
            order.key
        );
        assert!(!order.ids.contains(&8), "{:?} kept removed id 8", order.key);
        assert!(!order.ids.contains(&gone), "{:?} kept dead id", order.key);
    }

    // Applying the same batch again is a no-op: removed IDs are already
    // gone and upserted ones are already in place.
    let before = orders.clone();
    apply_and_check(&mut orders, &m, &batch, "same batch again");
    assert_eq!(orders, before);
}

#[test]
fn upserting_a_dead_id_is_ignored() {
    let mut m = small_tree();
    let mut orders = build_all(&m);
    let mut batch = Batch::new();
    m.live[2] = false;
    batch.remove(EntryId(2));
    apply_and_check(&mut orders, &m, &batch, "remove 2");
    // A later batch upserts the dead ID without reviving it.
    let mut batch = Batch::new();
    batch.upsert(EntryId(2));
    apply_and_check(&mut orders, &m, &batch, "upsert dead 2");
    for order in &orders {
        assert!(!order.ids.contains(&2), "{:?} revived dead id 2", order.key);
    }
}

#[test]
fn case_only_rename_reorders_by_raw_bytes() {
    // "dOcs" (6) < "docs" (1) by raw bytes, so the folder comes after the file.
    let mut m = small_tree();
    let mut orders = build_all(&m);
    let path = orders.iter().find(|o| o.key == SortKey::Path).unwrap();
    assert_eq!(path.ids, vec![0, 6, 1, 2, 3, 4, 5, 7, 8, 9]);

    // "DOCS" < "dOcs": the folder and its whole subtree move before the file.
    let mut batch = Batch::new();
    m.names[1] = "DOCS".to_string();
    batch.moved(EntryId(1));
    apply_and_check(&mut orders, &m, &batch, "case-only folder rename");
    let path = orders.iter().find(|o| o.key == SortKey::Path).unwrap();
    assert_eq!(path.ids, vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);

    // Case-only file renames: "B.txt" -> "b.txt" keeps its place after
    // "a.txt"; "dOcs" -> "DoCS" still folds equal to "DOCS" and stays after
    // it by raw bytes.
    let mut batch = Batch::new();
    m.names[3] = "b.txt".to_string();
    m.names[6] = "DoCS".to_string();
    batch.moved(EntryId(3));
    batch.moved(EntryId(6));
    apply_and_check(&mut orders, &m, &batch, "case-only file renames");
    let path = orders.iter().find(|o| o.key == SortKey::Path).unwrap();
    // "DOCS" < "DoCS" by raw bytes ('O' < 'o').
    assert_eq!(path.ids, vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
    let name = orders.iter().find(|o| o.key == SortKey::Name).unwrap();
    let pos = |id: u32| name.ids.iter().position(|&x| x == id).unwrap();
    assert!(pos(1) < pos(6), "DOCS should precede DoCS");
}

#[test]
fn folder_moved_under_a_sibling_carries_its_subtree() {
    let mut m = small_tree();
    let mut orders = build_all(&m);
    // Move docs/ (1, with 2..=5) under other/ (7); only the folder is listed.
    let mut batch = Batch::new();
    m.parents[1] = EntryId(7);
    batch.moved(EntryId(1));
    apply_and_check(&mut orders, &m, &batch, "move docs under other");
    let path = orders.iter().find(|o| o.key == SortKey::Path).unwrap();
    // root, dOcs, other, docs, a.txt, B.txt, sub, z, x, readme
    assert_eq!(path.ids, vec![0, 6, 7, 1, 2, 3, 4, 5, 8, 9]);

    // Move docs/ back to the root and rename it in the same change; its
    // subtree follows it to the end of the root folder.
    let mut batch = Batch::new();
    m.parents[1] = EntryId(0);
    m.names[1] = "zz".to_string();
    batch.moved(EntryId(1));
    apply_and_check(&mut orders, &m, &batch, "move docs back and rename");
    let path = orders.iter().find(|o| o.key == SortKey::Path).unwrap();
    // root, dOcs, other, x, readme, zz, a.txt, B.txt, sub, z
    assert_eq!(path.ids, vec![0, 6, 7, 8, 9, 1, 2, 3, 4, 5]);
}

#[test]
fn nested_folder_moves_with_changes_inside_the_subtree() {
    let mut m = small_tree();
    let mut orders = build_all(&m);
    let mut batch = Batch::new();
    // Move docs/ under other/, move sub/ out of docs/ to the root folder,
    // rename a.txt, delete B.txt, and add a new file under docs/.
    m.parents[1] = EntryId(7);
    batch.moved(EntryId(1));
    m.parents[4] = EntryId(0);
    batch.moved(EntryId(4));
    m.names[2] = "zeta".to_string();
    batch.moved(EntryId(2));
    m.live[3] = false;
    batch.remove(EntryId(3));
    let new = m.push("new", EntryId(1), EntryKind::File);
    batch.upsert(EntryId(new));
    // A second root appears holding a moved file.
    let root2 = m.push("Root2", EntryId::NONE, EntryKind::Dir);
    batch.upsert(EntryId(root2));
    m.parents[8] = EntryId(root2);
    batch.moved(EntryId(8));
    apply_and_check(&mut orders, &m, &batch, "nested moves");
}

// ---------------------------------------------------------------------------
// Structural (moved) versus attribute (upsert) changes.
// ---------------------------------------------------------------------------

/// Wraps a [`Mock`] and counts the per-entry reads `apply` makes (name,
/// parent, size, dates, kind). `is_live` and `id_bound` are not counted.
struct Counting<'a> {
    inner: &'a Mock,
    reads: AtomicUsize,
}

impl<'a> Counting<'a> {
    fn new(inner: &'a Mock) -> Self {
        Counting {
            inner,
            reads: AtomicUsize::new(0),
        }
    }

    fn hit(&self) {
        self.reads.fetch_add(1, AtomicOrdering::Relaxed);
    }

    fn take(&self) -> usize {
        self.reads.swap(0, AtomicOrdering::Relaxed)
    }
}

impl SortSource for Counting<'_> {
    fn id_bound(&self) -> usize {
        self.inner.id_bound()
    }

    fn is_live(&self, id: EntryId) -> bool {
        self.inner.is_live(id)
    }

    fn name(&self, id: EntryId) -> &str {
        self.hit();
        self.inner.name(id)
    }

    fn parent(&self, id: EntryId) -> EntryId {
        self.hit();
        self.inner.parent(id)
    }

    fn size(&self, id: EntryId) -> u64 {
        self.hit();
        self.inner.size(id)
    }

    fn modified(&self, id: EntryId) -> i64 {
        self.hit();
        self.inner.modified(id)
    }

    fn created(&self, id: EntryId) -> i64 {
        self.hit();
        self.inner.created(id)
    }

    fn kind(&self, id: EntryId) -> EntryKind {
        self.hit();
        self.inner.kind(id)
    }
}

/// Children of the big folder.
const BIG_CHILDREN: usize = 20_000;

/// Upper bound on per-entry reads for merging a one-entry batch that must
/// not touch the big folder's subtree. A correct merge needs a few hundred
/// (a galloping search with short parent chains); scanning or re-sorting
/// the subtree needs well over `BIG_CHILDREN`.
const SMALL_MERGE_READS: usize = 5_000;

/// root/ (0)
///   aaa        (file) 1
///   big/       (dir)  2   with BIG_CHILDREN files and a nested dir/file
///   mid        (file) 3
///   zzz/       (dir)  4
///     inner    (file) 5
///   big/...    children 6..
fn big_folder_tree() -> Mock {
    let mut m = Mock::default();
    let root = m.push("root", EntryId::NONE, EntryKind::Dir);
    m.push("aaa", EntryId(root), EntryKind::File);
    let big = m.push("big", EntryId(root), EntryKind::Dir);
    m.push("mid", EntryId(root), EntryKind::File);
    let zzz = m.push("zzz", EntryId(root), EntryKind::Dir);
    m.push("inner", EntryId(zzz), EntryKind::File);
    let mut rng = Rng::new(0xB16_F01D);
    for i in 0..BIG_CHILDREN {
        let name = format!("f{:05}", (i * 7919) % BIG_CHILDREN);
        let id = m.push(&name, EntryId(big), EntryKind::File);
        let k = id as usize;
        m.sizes[k] = random_size(&mut rng);
        m.modified[k] = random_date(&mut rng);
        m.created[k] = random_date(&mut rng);
    }
    let nested = m.push("nested", EntryId(big), EntryKind::Dir);
    m.push("deep", EntryId(nested), EntryKind::File);
    for i in 0..6 {
        m.sizes[i] = 10 + i as u64;
        m.modified[i] = 100 + i as i64;
        m.created[i] = 200 + i as i64;
    }
    m
}

fn assert_matches_fresh(order: &Order, m: &Mock, context: &str) {
    let fresh = Order::build(m, order.key);
    assert_eq!(
        order.ids, fresh.ids,
        "{context}: merged {:?} order differs from a fresh build",
        order.key
    );
    if let Err(e) = order.validate(m) {
        panic!("{context}: merged {:?} order is invalid: {e}", order.key);
    }
}

#[test]
fn moved_ids_make_a_batch_non_empty() {
    let mut batch = Batch::new();
    assert!(batch.is_empty());
    batch.moved(EntryId(1));
    assert!(
        !batch.is_empty(),
        "a batch with only a moved ID is not empty"
    );
    batch.clear();
    assert!(batch.is_empty(), "clear forgets moved IDs");
}

#[test]
fn folder_attribute_upsert_keeps_path_order_without_resorting_subtree() {
    let mut m = big_folder_tree();
    let mut orders = build_all(&m);

    // The big folder's mtime, ctime and size change; its name and parent do
    // not, so a plain upsert is the right call.
    m.modified[2] = 1_999_999_999;
    m.created[2] = -5;
    m.sizes[2] = u64::MAX >> 4;
    let mut batch = Batch::new();
    batch.upsert(EntryId(2));

    let counting = Counting::new(&m);
    for order in orders.iter_mut() {
        counting.take();
        order.apply(&counting, &batch);
        let reads = counting.take();
        assert_matches_fresh(order, &m, "folder attribute upsert");
        assert!(
            reads < SMALL_MERGE_READS,
            "{:?}: upserting one folder made {reads} entry reads; the subtree of \
             {BIG_CHILDREN} children must not be scanned or re-sorted",
            order.key
        );
    }

    // The Path order keeps the subtree right after the folder.
    let path = orders.iter().find(|o| o.key == SortKey::Path).unwrap();
    let pos = path.ids.iter().position(|&x| x == 2).unwrap();
    assert_eq!(&path.ids[..2], &[0, 1]);
    let after = pos + 1 + BIG_CHILDREN + 2;
    assert_eq!(path.ids[after], 3, "mid follows big's subtree");
}

#[test]
fn file_attribute_upserts_inside_a_big_folder_are_cheap() {
    let mut m = big_folder_tree();
    let mut orders = build_all(&m);
    // A child of big/ and the nested folder change size only.
    let child = 6 + BIG_CHILDREN as u32 / 2;
    let nested = 6 + BIG_CHILDREN as u32;
    m.sizes[child as usize] = 12_345;
    m.modified[nested as usize] = 42;
    let mut batch = Batch::new();
    batch.upsert(EntryId(child));
    batch.upsert(EntryId(nested));

    let counting = Counting::new(&m);
    for order in orders.iter_mut() {
        counting.take();
        order.apply(&counting, &batch);
        let reads = counting.take();
        assert_matches_fresh(order, &m, "child attribute upserts");
        assert!(
            reads < SMALL_MERGE_READS,
            "{:?}: two attribute upserts made {reads} entry reads",
            order.key
        );
    }
}

#[test]
fn renamed_big_folder_moves_its_whole_subtree() {
    let mut m = big_folder_tree();
    let mut orders = build_all(&m);

    // Rename big/ -> "zzzz": it and its whole subtree move after zzz/.
    m.names[2] = "zzzz".to_string();
    let mut batch = Batch::new();
    batch.moved(EntryId(2));
    apply_and_check(&mut orders, &m, &batch, "rename big folder");
    let path = orders.iter().find(|o| o.key == SortKey::Path).unwrap();
    assert_eq!(
        &path.ids[..5],
        &[0, 1, 3, 4, 5],
        "siblings now precede big/"
    );
    assert_eq!(path.ids[5], 2, "the renamed folder follows zzz/inner");
    let tail: Vec<u32> = path.ids[6..].to_vec();
    assert_eq!(tail.len(), BIG_CHILDREN + 2, "the whole subtree follows it");
    assert!(
        tail.iter().all(|&id| id >= 6),
        "only big/'s descendants follow it"
    );

    // Move it under zzz/ and change its size in the same moved() entry.
    m.parents[2] = EntryId(4);
    m.sizes[2] = 7;
    let mut batch = Batch::new();
    batch.moved(EntryId(2));
    apply_and_check(&mut orders, &m, &batch, "move big folder under zzz");
    let path = orders.iter().find(|o| o.key == SortKey::Path).unwrap();
    // root, aaa, mid, zzz, inner, zzzz, ...subtree
    assert_eq!(&path.ids[..6], &[0, 1, 3, 4, 5, 2]);
}

#[test]
fn renaming_a_big_folder_is_cheap_in_name_order() {
    let mut m = big_folder_tree();
    let mut name = Order::build(&m, SortKey::Name);
    m.names[2] = "a-big".to_string();
    let mut batch = Batch::new();
    batch.moved(EntryId(2));
    let counting = Counting::new(&m);
    name.apply(&counting, &batch);
    let reads = counting.take();
    assert_matches_fresh(&name, &m, "rename big folder, name order");
    assert!(
        reads < SMALL_MERGE_READS,
        "Name order: renaming one folder made {reads} entry reads; only the \
         entry itself should be re-sorted"
    );
}

#[test]
fn moved_entries_count_as_upserted_for_attribute_orders() {
    let mut m = small_tree();
    let mut orders = build_all(&m);
    // docs/ (1) is renamed and its size and dates change; it is listed only
    // through moved(), which must also re-sort it in Size/Modified/Created.
    m.names[1] = "Docs2".to_string();
    m.sizes[1] = 1_000;
    m.modified[1] = 77;
    m.created[1] = -77;
    // A file is moved and resized the same way.
    m.parents[9] = EntryId(4);
    m.sizes[9] = 2_000;
    let mut batch = Batch::new();
    batch.moved(EntryId(1));
    batch.moved(EntryId(9));
    apply_and_check(&mut orders, &m, &batch, "moved with attribute changes");
}

// ---------------------------------------------------------------------------
// Merging from an order shared with snapshots (run 2.14).
// ---------------------------------------------------------------------------

#[test]
fn applied_leaves_its_order_alone_and_equals_apply() {
    for seed in 0..6u64 {
        let mut rng = Rng::new(0xA991_0000 + seed);
        let mut m = random_mock(&mut rng, 300);
        let mut orders = build_all(&m);
        for round in 0..60 {
            let mut batch = Batch::new();
            for _ in 0..1 + rng.below(12) {
                random_change(&mut rng, &mut m, &mut batch);
            }
            for order in orders.iter_mut() {
                let before = order.clone();
                let next = order.applied(&m, &batch);
                assert_eq!(
                    *order, before,
                    "seed {seed} round {round}: applied changed the order it read"
                );
                let mut expected = order.clone();
                expected.apply(&m, &batch);
                assert_eq!(
                    next, expected,
                    "seed {seed} round {round}: applied differs from apply"
                );
                *order = next;
            }
        }
    }
}

#[test]
fn applied_with_an_empty_batch_is_a_copy() {
    let m = small_tree();
    for order in build_all(&m) {
        assert_eq!(order.applied(&m, &Batch::new()), order);
    }
}
