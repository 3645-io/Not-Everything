//! Load-to-first-result correctness for the index file (see
//! `tests/format.rs` for the assumed API).
//!
//! About 200k synthetic entries from `fx-bench` are put in an
//! `fx_index::Index` (entry `i` of the tree gets ID `i`), saved with
//! `fx_store::write_checkpoint` and loaded back with `MappedIndex::open`. An
//! `fx_query` substring search over the mapped name column must return
//! exactly the IDs of a naive search over the in-memory tree, and the first
//! result must be the smallest matching ID. This is the path the
//! `load_bench` example times; no timing is asserted here.
//!
//! `fx_query::substring::NameColumn` wants a NUL after the last name, while
//! `IndexParts::name_bytes` has none, so the tests append one to a copy of
//! the mapped bytes when the mapped buffer does not already end with NUL.
//!
//! The last tests check that the benchmark example and the crate README
//! exist and carry the gate line and the run instructions.

#![allow(clippy::needless_range_loop)]

use std::borrow::Cow;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fx_bench::{Tree, TreeShape, generate_tree};
use fx_index::Index;
use fx_model::{Entry, EntryFlags, EntryKind, FileKey, ResumeCursor, VolumeId};
use fx_query::substring::{CancelToken, CaseMode, NameColumn, SubstringQuery, search};
use fx_store::{MappedIndex, SaveInput, write_checkpoint};

const ENTRIES: usize = 200_000;
const SEED: u64 = 0x05_10ad;

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "fx-store-load-first-{tag}-{}-{n}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Unwraps a result without requiring `E: Debug`.
fn must<T, E>(r: Result<T, E>, what: &str) -> T {
    match r {
        Ok(v) => v,
        Err(_) => panic!("{what}: unexpected error"),
    }
}

/// A generated tree saved to an index file in its own temp dir.
struct Fixture {
    _dir: TempDir,
    path: PathBuf,
    tree: Tree,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let dir = TempDir::new(tag);
        let path = dir.0.join("index.fxs");
        let tree = generate_tree(ENTRIES, SEED, TreeShape::default());
        assert_eq!(tree.len(), ENTRIES);

        let vol = VolumeId(1);
        let key = |i: usize| FileKey::new(vol, i as u64 + 1);
        let mut index = Index::new();
        for i in 0..tree.len() {
            let parent = tree.parent[i];
            let is_dir = tree.is_dir[i];
            must(
                index.upsert(Entry {
                    key: key(i),
                    parent: (parent != u32::MAX).then(|| key(parent as usize)),
                    name: tree.names.get(i).to_string(),
                    kind: if is_dir {
                        EntryKind::Dir
                    } else {
                        EntryKind::File
                    },
                    flags: EntryFlags::empty(),
                    size: if is_dir {
                        0
                    } else {
                        (i as u64 * 2_654_435_761) % (1 << 32)
                    },
                    modified: 1_600_000_000 + (i as i64 % 100_000_000),
                    created: 1_600_000_000,
                }),
                "upsert",
            );
        }
        let parts = index.snapshot().to_parts();
        let vols = [(
            vol,
            ResumeCursor::FsEvents {
                event_id: 1,
                db_uuid: [7; 16],
            },
        )];
        let input = SaveInput {
            parts: &parts,
            volumes: &vols,
            orders: &[],
            clean_shutdown: true,
        };
        must(write_checkpoint(&path, &input), "write_checkpoint");
        Fixture {
            _dir: dir,
            path,
            tree,
        }
    }

    fn open(&self) -> MappedIndex {
        must(MappedIndex::open(&self.path), "MappedIndex::open")
    }
}

/// The mapped name bytes, NUL-terminated for `NameColumn`.
fn terminated(m: &MappedIndex) -> Cow<'_, [u8]> {
    let bytes = m.name_bytes();
    if bytes.last() == Some(&0) {
        Cow::Borrowed(bytes)
    } else {
        let mut v = bytes.to_vec();
        v.push(0);
        Cow::Owned(v)
    }
}

fn run(buf: &[u8], offsets: &[u32], needle: &str, case: CaseMode) -> Vec<u32> {
    let col = must(NameColumn::new(buf, offsets), "NameColumn::new");
    let q = SubstringQuery::new(needle, case);
    must(search(col, &q, &CancelToken::never()), "search")
}

fn naive(tree: &Tree, needle: &str, case: CaseMode) -> Vec<u32> {
    let fold = |s: &str| match case {
        CaseMode::Sensitive => s.to_owned(),
        CaseMode::AsciiInsensitive => s.to_ascii_lowercase(),
    };
    let needle = fold(needle);
    tree.names
        .iter()
        .enumerate()
        .filter(|(_, name)| fold(name).contains(needle.as_str()))
        .map(|(i, _)| i as u32)
        .collect()
}

#[test]
fn mapped_names_equal_the_tree_with_ids_preserved() {
    let fx = Fixture::new("columns");
    let m = fx.open();
    assert_eq!(m.id_bound(), ENTRIES);
    assert_eq!(m.live_count(), ENTRIES);
    let buf = terminated(&m);
    let col = must(NameColumn::new(&buf, m.name_offsets()), "NameColumn::new");
    assert_eq!(col.len(), ENTRIES);
    for i in [0, 1, ENTRIES / 2, ENTRIES - 1] {
        assert_eq!(
            col.name_bytes(i),
            fx.tree.names.get(i).as_bytes(),
            "name {i}"
        );
    }
}

#[test]
fn mapped_search_matches_naive_search() {
    let fx = Fixture::new("search");
    let m = fx.open();
    let buf = terminated(&m);
    for needle in ["conf", "e", "a", ".txt", "zqxjvk_no_such_name_qzxj"] {
        let got = run(&buf, m.name_offsets(), needle, CaseMode::Sensitive);
        assert_eq!(
            got,
            naive(&fx.tree, needle, CaseMode::Sensitive),
            "{needle:?}"
        );
    }
}

#[test]
fn first_result_is_smallest_matching_id() {
    let fx = Fixture::new("first");
    let m = fx.open();
    let buf = terminated(&m);
    for needle in ["conf", "e", ".txt", "a"] {
        let ids = run(&buf, m.name_offsets(), needle, CaseMode::Sensitive);
        assert!(ids.windows(2).all(|w| w[0] < w[1]), "{needle:?}: ascending");
        let smallest = fx
            .tree
            .names
            .iter()
            .position(|name| name.contains(needle))
            .map(|i| i as u32);
        assert_eq!(ids.first().copied(), smallest, "{needle:?}: first result");
    }
    assert!(
        !run(&buf, m.name_offsets(), "conf", CaseMode::Sensitive).is_empty(),
        "default benchmark query \"conf\" has no hit in {ENTRIES} entries"
    );
}

#[test]
fn reopening_gives_same_results_and_valid_checksums() {
    let fx = Fixture::new("reopen");
    let a = fx.open();
    let first = run(
        &terminated(&a),
        a.name_offsets(),
        "conf",
        CaseMode::Sensitive,
    );
    drop(a);
    let b = fx.open();
    assert_eq!(
        run(
            &terminated(&b),
            b.name_offsets(),
            "conf",
            CaseMode::Sensitive
        ),
        first
    );
    must(b.verify_checksums(), "verify_checksums on a fresh file");
    assert!(b.clean_shutdown());
}

fn crate_file(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => panic!("cannot read {}: {e}", path.display()),
    }
}

#[test]
fn load_bench_example_reports_the_gate() {
    let src = crate_file("examples/load_bench.rs");
    assert!(
        src.contains("GATE load-to-first-result p95 <= 50 ms"),
        "load_bench must print the gate line"
    );
    for arg in [
        "--count", "--seed", "--iters", "--query", "--dir", "--cache", "--keep",
    ] {
        assert!(src.contains(arg), "load_bench must accept {arg}");
    }
    for api in ["MappedIndex::open", "write_checkpoint", "NameColumn::new"] {
        assert!(src.contains(api), "load_bench must use {api}");
    }
}

#[test]
fn readme_documents_format_and_benchmark() {
    let readme = crate_file("README.md");
    assert!(readme.contains("cargo run --release -p fx-store --example load_bench"));
    assert!(
        readme.contains("sudo purge"),
        "README must explain cold-cache runs need `sudo purge` or a reboot"
    );
    assert!(
        readme.contains("checkpoint"),
        "README must describe the checkpoint format"
    );
}

#[test]
fn crate_doc_mentions_checkpoint() {
    assert!(crate_file("src/lib.rs").contains("checkpoint"));
}
