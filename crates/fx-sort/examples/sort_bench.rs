//! Sort benchmark: build, emit and batched-merge timings for `fx-sort`.
//!
//! Clean-room: written from docs/PLAN.md ("Sorting", "Updates and
//! concurrency") and the public fx-sort / fx-bench APIs only.
//!
//! ```text
//! cargo run --release -p fx-sort --example sort_bench -- [ENTRIES] [--seed N] [--iters N] [--no-verify]
//! ```
//!
//! `ENTRIES` accepts plain numbers or `k`/`M` suffixes (`2M`, `500k`);
//! default 2M. The data is a synthetic tree from `fx_bench::generate_tree`
//! with a fixed seed, held in an in-memory [`SortSource`] table, so the
//! numbers are CPU-only (the file-system cache plays no part).
//!
//! Reported:
//! - wall time of `Order::build` for Name, Path, Size and Modified (each
//!   includes its own Name sort), plus `Order::build_all` for all keys;
//! - p50/p95 of `emit` for one page of 100 rows over a ~1% and a ~50%
//!   match set, ascending and descending;
//! - "Your files first": p50/p95 of the engine's per-snapshot
//!   pass, `hidden_entries` on the Name order plus `group_subtrees` on the
//!   Path order, for folders shaped like your files (the root included,
//!   every other top-level folder excluded, one folder inside an excluded
//!   one included again); then of `group_subtrees` alone with a denser
//!   synthetic hidden set (every 50th folder and 30th file, as `.git`
//!   folders and `.DS_Store` files are); then of `emit_tiered` for the
//!   first page and for the page right after the group;
//! - p50/p95 of `Order::apply` for batches of 1k and 10k random changes
//!   (creates, removes, metadata edits as `upsert`, renames and moves as
//!   `moved`, folders carrying their subtrees in the Path order), per order;
//! - p50/p95 of `Order::apply` for batches of 1k folder mtime-only upserts,
//!   per order, which must not scan or re-sort the folders' subtrees in the
//!   Path order;
//! - finally, unless `--no-verify`, a check that every merged order equals
//!   a fresh build.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use fx_bench::{
    BenchConfig, BenchReport, CacheState, MachineInfo, NameGen, Rng, Stats, TreeShape,
    generate_tree, run,
};
use fx_model::{EntryId, EntryKind};
use fx_sort::{
    Batch, Direction, MatchSet, Order, SortKey, SortSource, emit, emit_tiered, group_subtrees,
    hidden_entries,
};

const DEFAULT_ENTRIES: usize = 2_000_000;
const DEFAULT_SEED: u64 = 42;
const DEFAULT_ITERS: u32 = 20;
const PAGE: usize = 100;
/// Folders per batch in the folder mtime-only merge case.
const FOLDER_MTIME_BATCH: usize = 1_000;
const KEYS: [SortKey; 4] = [
    SortKey::Name,
    SortKey::Path,
    SortKey::Size,
    SortKey::Modified,
];
/// Base of the synthetic timestamps: 2020-09-13, seconds since the epoch.
const EPOCH_BASE: i64 = 1_600_000_000;
/// Spread of the synthetic timestamps: about five years.
const TIME_SPAN: u64 = 160_000_000;

/// An in-memory entry table. Names live in one append-only buffer, so a
/// rename appends the new name and repoints the entry's span.
struct Table {
    buf: String,
    spans: Vec<(usize, usize)>,
    parent: Vec<u32>,
    size: Vec<u64>,
    modified: Vec<i64>,
    created: Vec<i64>,
    is_dir: Vec<bool>,
    live: Vec<bool>,
}

impl SortSource for Table {
    fn id_bound(&self) -> usize {
        self.parent.len()
    }

    fn is_live(&self, id: EntryId) -> bool {
        self.live.get(id.0 as usize).copied().unwrap_or(false)
    }

    fn name(&self, id: EntryId) -> &str {
        let (start, end) = self.spans[id.0 as usize];
        &self.buf[start..end]
    }

    fn parent(&self, id: EntryId) -> EntryId {
        EntryId(self.parent[id.0 as usize])
    }

    fn size(&self, id: EntryId) -> u64 {
        self.size[id.0 as usize]
    }

    fn modified(&self, id: EntryId) -> i64 {
        self.modified[id.0 as usize]
    }

    fn created(&self, id: EntryId) -> i64 {
        self.created[id.0 as usize]
    }

    fn kind(&self, id: EntryId) -> EntryKind {
        if self.is_dir[id.0 as usize] {
            EntryKind::Dir
        } else {
            EntryKind::File
        }
    }
}

/// A random file size: log-uniform up to about 1 GiB.
fn random_size(rng: &mut Rng) -> u64 {
    let bits = rng.below(31);
    (1u64 << bits) + rng.below(1u64 << bits)
}

/// A random timestamp within the synthetic span.
fn random_time(rng: &mut Rng) -> i64 {
    EPOCH_BASE + rng.below(TIME_SPAN) as i64
}

impl Table {
    fn from_tree(entries: usize, seed: u64) -> Table {
        let tree = generate_tree(entries, seed, TreeShape::default());
        let n = tree.len();
        let mut rng = Rng::new(seed ^ 0x6d65_7461_6461_7461);
        let mut table = Table {
            buf: String::with_capacity(tree.names.total_bytes() + n),
            spans: Vec::with_capacity(n),
            parent: tree.parent,
            size: Vec::with_capacity(n),
            modified: Vec::with_capacity(n),
            created: Vec::with_capacity(n),
            is_dir: tree.is_dir,
            live: vec![true; n],
        };
        for name in tree.names.iter() {
            let start = table.buf.len();
            table.buf.push_str(name);
            table.spans.push((start, table.buf.len()));
        }
        for i in 0..n {
            let created = random_time(&mut rng);
            let modified = created + rng.below(TIME_SPAN / 4) as i64;
            let size = if table.is_dir[i] {
                0
            } else {
                random_size(&mut rng)
            };
            table.size.push(size);
            table.modified.push(modified);
            table.created.push(created);
        }
        table
    }

    /// Whether `ancestor` is `id` or on `id`'s parent chain.
    fn is_ancestor_or_self(&self, ancestor: u32, id: u32) -> bool {
        let mut cur = id;
        loop {
            if cur == ancestor {
                return true;
            }
            cur = self.parent[cur as usize];
            if cur == u32::MAX {
                return false;
            }
        }
    }
}

/// Generates random changes, applies them to the table and records them in
/// a [`Batch`].
struct Changer {
    rng: Rng,
    names: NameGen,
    /// Every directory (directories are never removed, so all stay live).
    dirs: Vec<u32>,
    /// Monotonic clock for modification times of changed entries.
    clock: i64,
}

impl Changer {
    fn new(table: &Table, seed: u64) -> Changer {
        let dirs = (0..table.is_dir.len() as u32)
            .filter(|&i| table.is_dir[i as usize])
            .collect();
        Changer {
            rng: Rng::new(seed ^ 0x6368_616e_6765_7321),
            names: NameGen::new(seed ^ 0x7265_6e61_6d65_7321),
            dirs,
            clock: EPOCH_BASE + TIME_SPAN as i64,
        }
    }

    /// A random live entry other than the root.
    fn random_live(&mut self, table: &Table) -> u32 {
        let bound = table.parent.len();
        loop {
            let i = 1 + self.rng.index(bound - 1);
            if table.live[i] {
                return i as u32;
            }
        }
    }

    fn append_name(&mut self, table: &mut Table, is_dir: bool) -> (usize, usize) {
        let start = table.buf.len();
        if is_dir {
            self.names.write_dir_name(&mut table.buf);
        } else {
            self.names.write_file_name(&mut table.buf);
        }
        (start, table.buf.len())
    }

    /// Applies `n` random changes to `table` and returns them as a batch.
    ///
    /// Mix: 15% creates (19% of them folders), 15% removes (files only, so
    /// no entry is orphaned; a picked folder gets a metadata edit instead),
    /// 40% size/date edits, 20% renames and 10% moves into a random folder
    /// (a folder never moves into its own subtree).
    fn batch(&mut self, table: &mut Table, n: usize) -> Batch {
        let mut batch = Batch::new();
        for _ in 0..n {
            self.clock += 1;
            let r = self.rng.below(100);
            if r < 15 {
                let parent = *self.rng.pick(&self.dirs);
                let is_dir = self.rng.chance(190_000);
                let span = self.append_name(table, is_dir);
                let id = table.parent.len() as u32;
                table.spans.push(span);
                table.parent.push(parent);
                table.size.push(if is_dir {
                    0
                } else {
                    random_size(&mut self.rng)
                });
                table.modified.push(self.clock);
                table.created.push(self.clock);
                table.is_dir.push(is_dir);
                table.live.push(true);
                if is_dir {
                    self.dirs.push(id);
                }
                batch.upsert(EntryId(id));
                continue;
            }
            let id = self.random_live(table);
            let i = id as usize;
            match r {
                15..30 if !table.is_dir[i] => {
                    table.live[i] = false;
                    batch.remove(EntryId(id));
                }
                70..90 => {
                    let is_dir = table.is_dir[i];
                    table.spans[i] = self.append_name(table, is_dir);
                    batch.moved(EntryId(id));
                }
                90.. => {
                    let target = *self.rng.pick(&self.dirs);
                    if !table.is_ancestor_or_self(id, target) {
                        table.parent[i] = target;
                        batch.moved(EntryId(id));
                    } else {
                        table.modified[i] = self.clock;
                        batch.upsert(EntryId(id));
                    }
                }
                _ => {
                    if !table.is_dir[i] {
                        table.size[i] = random_size(&mut self.rng);
                    }
                    table.modified[i] = self.clock;
                    batch.upsert(EntryId(id));
                }
            }
        }
        batch
    }

    /// Sets a new modification time on `n` random folders (name, parent and
    /// size unchanged) and returns them as a batch of plain upserts, the
    /// common case of files changing inside watched folders.
    fn folder_mtime_batch(&mut self, table: &mut Table, n: usize) -> Batch {
        let mut batch = Batch::new();
        for _ in 0..n {
            self.clock += 1;
            let id = *self.rng.pick(&self.dirs);
            table.modified[id as usize] = self.clock;
            batch.upsert(EntryId(id));
        }
        batch
    }
}

struct Args {
    entries: usize,
    seed: u64,
    iters: u32,
    verify: bool,
}

fn parse_count(s: &str) -> Option<usize> {
    let s = s.replace('_', "");
    let (digits, mult) = match s.chars().last()? {
        'k' | 'K' => (&s[..s.len() - 1], 1_000),
        'm' | 'M' => (&s[..s.len() - 1], 1_000_000),
        _ => (&s[..], 1),
    };
    digits.parse::<usize>().ok()?.checked_mul(mult)
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        entries: DEFAULT_ENTRIES,
        seed: DEFAULT_SEED,
        iters: DEFAULT_ITERS,
        verify: true,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--seed" => {
                let v = it.next().ok_or("--seed needs a value")?;
                args.seed = v.parse().map_err(|_| format!("bad seed {v:?}"))?;
            }
            "--iters" => {
                let v = it.next().ok_or("--iters needs a value")?;
                args.iters = v
                    .parse()
                    .map_err(|_| format!("bad iteration count {v:?}"))?;
            }
            "--no-verify" => args.verify = false,
            "-h" | "--help" => {
                return Err(
                    "usage: sort_bench [ENTRIES] [--seed N] [--iters N] [--no-verify]".into(),
                );
            }
            other => {
                args.entries =
                    parse_count(other).ok_or_else(|| format!("bad entry count {other:?}"))?;
            }
        }
    }
    if args.entries < 2 {
        return Err("need at least 2 entries".into());
    }
    if args.iters == 0 {
        return Err("--iters must be at least 1".into());
    }
    Ok(args)
}

fn ms(d: Duration) -> String {
    format!("{:.3}", d.as_secs_f64() * 1_000.0)
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

/// A random match set over the live entries, each included with
/// probability `per_million`.
fn random_matches(table: &Table, rng: &mut Rng, per_million: u32) -> MatchSet {
    let bound = table.id_bound();
    let mut set = MatchSet::with_bound(bound);
    for i in 0..bound as u32 {
        if table.live[i as usize] && rng.chance(per_million) {
            set.insert(EntryId(i));
        }
    }
    set
}

/// Times `f` over `iterations` warm runs after two warm-up runs.
fn warm_runs<T>(iterations: u32, f: impl FnMut() -> T) -> BenchReport {
    run(
        BenchConfig {
            warmup: 2,
            iterations,
            label: String::new(),
            cache: CacheState::Warm,
        },
        f,
    )
}

/// Listed folders shaped like "your files": the root included, every other
/// top-level folder excluded (as `~/Library` and the dot-folders are), and
/// the first folder inside the first excluded one included again (as
/// `~/Library/CloudStorage` is).
fn your_files_like(table: &Table, path: &Order) -> Vec<(EntryId, bool)> {
    let Some(&root) = path.ids.first() else {
        return Vec::new();
    };
    let is_dir_under =
        |id: u32, parent: u32| table.parent[id as usize] == parent && table.is_dir[id as usize];
    let mut listed = vec![(EntryId(root), true)];
    let top: Vec<u32> = path
        .ids
        .iter()
        .copied()
        .filter(|&id| is_dir_under(id, root))
        .collect();
    listed.extend(
        top.iter()
            .skip(1)
            .step_by(2)
            .map(|&id| (EntryId(id), false)),
    );
    if let Some(&(excluded, _)) = listed.get(1) {
        if let Some(&inner) = path.ids.iter().find(|&&id| is_dir_under(id, excluded.0)) {
            listed.push((EntryId(inner), true));
        }
    }
    listed
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let machine = MachineInfo::detect();
    println!("fx-sort sort_bench");
    println!(
        "setup: entries {}, seed {}, rayon threads {}, iterations {}, page {} rows",
        args.entries,
        args.seed,
        rayon::current_num_threads(),
        args.iters,
        PAGE
    );
    println!(
        "machine: {} {}, {} logical cpus, {}; data in memory (cache n/a), {} build",
        machine.os,
        machine.arch,
        machine.parallelism,
        machine.cpu_brand,
        if cfg!(debug_assertions) {
            "DEBUG"
        } else {
            "release"
        }
    );

    let t = Instant::now();
    let mut table = Table::from_tree(args.entries, args.seed);
    println!(
        "generate: {} entries ({} folders) in {} ms",
        table.id_bound(),
        table.is_dir.iter().filter(|&&d| d).count(),
        ms(t.elapsed())
    );

    // Build.
    println!();
    println!("build (wall time, one run each; every key includes its Name sort)");
    let mut orders = Vec::with_capacity(KEYS.len());
    for key in KEYS {
        let t = Instant::now();
        let order = Order::build(&table, key);
        let elapsed = t.elapsed();
        println!("  build {:<9} {:>10} ms", key_name(key), ms(elapsed));
        orders.push(order);
    }
    let t = Instant::now();
    let all = Order::build_all(&table);
    let elapsed = t.elapsed();
    println!(
        "  build_all ({} keys, one shared Name sort) {} ms",
        all.len(),
        ms(elapsed)
    );
    drop(all);

    // Emit.
    println!();
    println!(
        "emit (first page of {PAGE} rows plus total count; p50/p95 over {} runs)",
        args.iters
    );
    let mut rng = Rng::new(args.seed ^ 0x6d61_7463_6865_7321);
    let sets = [
        ("1%", random_matches(&table, &mut rng, 10_000)),
        ("50%", random_matches(&table, &mut rng, 500_000)),
    ];
    for order in &orders {
        for (label, set) in &sets {
            for (dir_label, direction) in [
                ("asc", Direction::Ascending),
                ("desc", Direction::Descending),
            ] {
                let report = run(
                    BenchConfig {
                        warmup: 2,
                        iterations: args.iters,
                        label: String::new(),
                        cache: CacheState::Warm,
                    },
                    || emit(order, set, direction, 0, PAGE),
                );
                let page = emit(order, set, direction, 0, PAGE);
                println!(
                    "  emit {:<9} {:>3} {:<4} p50 {:>8} ms  p95 {:>8} ms  (total {}, page {})",
                    key_name(order.key),
                    label,
                    dir_label,
                    ms(report.stats.p50),
                    ms(report.stats.p95),
                    page.total,
                    page.ids.len()
                );
            }
        }
    }

    // Your files first.
    println!();
    println!(
        "group (hidden_entries on the Name order, group_subtrees on the Path order; \
         p50/p95 over {} runs)",
        args.iters
    );
    let order_of = |key: SortKey| {
        orders
            .iter()
            .find(|o| o.key == key)
            .expect("the order is built")
    };
    let (path, name) = (order_of(SortKey::Path), order_of(SortKey::Name));
    let listed = your_files_like(&table, path);
    let report = warm_runs(args.iters, || {
        group_subtrees(path, &table, &listed, &hidden_entries(name, &table))
    });
    let hidden = hidden_entries(name, &table);
    let group = group_subtrees(path, &table, &listed, &hidden);
    println!(
        "  hidden_entries + group_subtrees {} folders  p50 {:>8} ms  p95 {:>8} ms  \
         (group {} of {} entries, {} hidden)",
        listed.len(),
        ms(report.stats.p50),
        ms(report.stats.p95),
        group.len(),
        path.len(),
        hidden.len()
    );
    let mut dense = MatchSet::with_bound(table.id_bound());
    let (mut dirs, mut files) = (0usize, 0usize);
    for &id in &path.ids {
        let every = if table.is_dir[id as usize] {
            dirs += 1;
            dirs % 50 == 0
        } else {
            files += 1;
            files % 30 == 0
        };
        if every {
            dense.insert(EntryId(id));
        }
    }
    let report = warm_runs(args.iters, || group_subtrees(path, &table, &listed, &dense));
    println!(
        "  group_subtrees, dense hidden set  p50 {:>8} ms  p95 {:>8} ms  \
         (group {} of {} entries, {} left out)",
        ms(report.stats.p50),
        ms(report.stats.p95),
        group_subtrees(path, &table, &listed, &dense).len(),
        path.len(),
        dense.len()
    );
    println!(
        "emit_tiered (your files first; first page of {PAGE} rows, then the page right after \
         the group; p50/p95 over {} runs)",
        args.iters
    );
    for order in &orders {
        for (label, set) in &sets {
            for (dir_label, direction) in [
                ("asc", Direction::Ascending),
                ("desc", Direction::Descending),
            ] {
                let first = emit_tiered(order, set, &group, direction, 0, PAGE);
                let in_group = emit_tiered(order, set, &group, direction, 0, usize::MAX)
                    .ids
                    .iter()
                    .take_while(|&&id| group.contains(EntryId(id)))
                    .count();
                for (page_label, offset) in [("first", 0), ("after", in_group)] {
                    let report = warm_runs(args.iters, || {
                        emit_tiered(order, set, &group, direction, offset, PAGE)
                    });
                    println!(
                        "  emit_tiered {:<9} {:>3} {:<4} {:<5} p50 {:>8} ms  p95 {:>8} ms  (total {}, in group {})",
                        key_name(order.key),
                        label,
                        dir_label,
                        page_label,
                        ms(report.stats.p50),
                        ms(report.stats.p95),
                        first.total,
                        in_group
                    );
                }
            }
        }
    }
    drop(sets);

    // Merge.
    println!();
    println!(
        "merge (Order::apply per batch of random changes; p50/p95 over {} batches)",
        args.iters
    );
    let mut changer = Changer::new(&table, args.seed);
    for batch_size in [1_000usize, 10_000] {
        let mut samples = vec![Vec::with_capacity(args.iters as usize); orders.len()];
        for _ in 0..args.iters {
            let batch = changer.batch(&mut table, batch_size);
            for (order, samples) in orders.iter_mut().zip(&mut samples) {
                let t = Instant::now();
                order.apply(&table, &batch);
                samples.push(t.elapsed());
            }
        }
        for (order, samples) in orders.iter().zip(&samples) {
            let Some(stats) = Stats::from_samples(samples) else {
                continue;
            };
            println!(
                "  apply {:<9} batch {:>6}  p50 {:>8} ms  p95 {:>8} ms  max {:>8} ms",
                key_name(order.key),
                batch_size,
                ms(stats.p50),
                ms(stats.p95),
                ms(stats.max)
            );
        }
    }

    // Folder mtime-only upserts: no subtree is scanned or re-sorted, even in
    // the Path order. Every order gets the batch (the Modified order needs
    // it to stay correct); the Path line is the one to watch.
    println!();
    println!(
        "merge (Order::apply per batch of {FOLDER_MTIME_BATCH} folder mtime-only upserts; \
         p50/p95 over {} batches)",
        args.iters
    );
    let mut samples = vec![Vec::with_capacity(args.iters as usize); orders.len()];
    for _ in 0..args.iters {
        let batch = changer.folder_mtime_batch(&mut table, FOLDER_MTIME_BATCH);
        for (order, samples) in orders.iter_mut().zip(&mut samples) {
            let t = Instant::now();
            order.apply(&table, &batch);
            samples.push(t.elapsed());
        }
    }
    for (order, samples) in orders.iter().zip(&samples) {
        let Some(stats) = Stats::from_samples(samples) else {
            continue;
        };
        println!(
            "  apply {:<9} folder-mtime {:>6}  p50 {:>8} ms  p95 {:>8} ms  max {:>8} ms",
            key_name(order.key),
            FOLDER_MTIME_BATCH,
            ms(stats.p50),
            ms(stats.p95),
            ms(stats.max)
        );
    }

    println!(
        "after merges: {} ids, {} live entries",
        table.id_bound(),
        table.live.iter().filter(|&&l| l).count()
    );

    if args.verify {
        let t = Instant::now();
        let fresh = Order::build_all(&table);
        let mut ok = true;
        for order in &orders {
            let same = fresh
                .iter()
                .find(|f| f.key == order.key)
                .is_some_and(|f| f.ids == order.ids);
            if !same {
                eprintln!(
                    "verify: merged {} order differs from a fresh build",
                    key_name(order.key)
                );
                ok = false;
            }
        }
        if !ok {
            return ExitCode::FAILURE;
        }
        println!(
            "verify: every merged order equals a fresh build ({} ms)",
            ms(t.elapsed())
        );
    }
    ExitCode::SUCCESS
}
