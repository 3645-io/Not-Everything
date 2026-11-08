//! Round-trip and O(1)-open tests for the index file (see
//! `tests/format.rs` for the API these tests assume).
//!
//! - A generated tree saved through `write_checkpoint` reopens with every
//!   column equal and the names decodable straight from the mapping.
//! - `MappedIndex::open` validates only the header and tables: a corrupt
//!   data byte is not noticed by `open`, only by `verify_checksums`.
//! - `fx_store::layout::checksum(&[u8]) -> u64` is the hand-rolled, std-only
//!   column checksum; it is checked by properties only, never a pinned value.
//!
//! Clean-room: written from docs/PLAN.md and the in-repo code only.

#![allow(clippy::needless_range_loop)]

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fx_bench::{Tree, TreeShape, generate_tree};
use fx_index::{Index, IndexParts};
use fx_model::{Entry, EntryFlags, EntryKind, FileKey, ResumeCursor, VolumeId};
use fx_store::layout::checksum;
use fx_store::{LoadError, MappedIndex, SaveInput, write_checkpoint};

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
            "fx-store-image-{tag}-{}-{n}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        TempDir(dir)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// An index holding a generated tree; entry `i` of the tree gets ID `i`.
fn tree_index(tree: &Tree) -> Index {
    let vol = VolumeId(3);
    let key = |i: usize| FileKey::new(vol, i as u64 + 1);
    let mut index = Index::new();
    for i in 0..tree.len() {
        let parent = tree.parent[i];
        let is_dir = tree.is_dir[i];
        index
            .upsert(Entry {
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
                    (i as u64 * 2_654_435_761) % (1 << 40)
                },
                modified: 1_600_000_000 + i as i64 * 7
                    - if i % 97 == 0 { 3_000_000_000 } else { 0 },
                created: 1_500_000_000 + i as i64,
            })
            .expect("insert tree entry");
    }
    index
}

fn save(path: &std::path::Path, parts: &IndexParts) {
    let vols = [(VolumeId(3), ResumeCursor::None { snapshot_epoch: 5 })];
    let input = SaveInput {
        parts,
        volumes: &vols,
        orders: &[],
        clean_shutdown: true,
    };
    write_checkpoint(path, &input).expect("write checkpoint");
}

#[test]
fn generated_tree_round_trips_and_names_decode_from_the_mapping() {
    let dir = TempDir::new("tree");
    let path = dir.file("index.fxs");
    let tree = generate_tree(20_000, 42, TreeShape::default());
    let parts = tree_index(&tree).snapshot().to_parts();
    save(&path, &parts);

    let m = MappedIndex::open(&path).expect("open");
    m.verify_checksums().expect("checksums");
    assert_eq!(m.id_bound(), tree.len());
    assert_eq!(m.live_count(), tree.len());
    assert_eq!(m.to_parts(), parts);
    let index = m.to_index().expect("to_index");
    let snap = index.snapshot();
    for i in [0usize, 1, 2, 999, 19_999] {
        let id = fx_model::EntryId(i as u32);
        assert_eq!(snap.name(id), tree.names.get(i), "name {i}");
        let start = m.name_offsets()[i] as usize;
        let bytes = &m.name_bytes()[start..start + tree.names.get(i).len()];
        assert_eq!(bytes, tree.names.get(i).as_bytes(), "mapped name {i}");
    }
}

#[test]
fn open_does_not_read_column_data_but_verify_does() {
    let dir = TempDir::new("o1");
    let path = dir.file("index.fxs");
    let tree = generate_tree(50_000, 7, TreeShape::default());
    save(&path, &tree_index(&tree).snapshot().to_parts());

    let ranges = MappedIndex::open(&path).expect("open").column_ranges();
    let (column, range) = ranges
        .iter()
        .max_by_key(|(_, r)| r.len())
        .cloned()
        .expect("columns");
    let mut bytes = fs::read(&path).expect("read");
    bytes[range.start + range.len() / 2] ^= 0x04;
    fs::write(&path, bytes).expect("write");

    let m = MappedIndex::open(&path).expect("open is O(1) and skips column data");
    let err = m.verify_checksums().expect_err("verify must fail");
    assert!(
        matches!(err, LoadError::ColumnChecksum { .. }),
        "{column:?}: {err:?}"
    );
}

#[test]
fn checksum_detects_single_bit_flips_at_any_length() {
    for len in [1usize, 7, 8, 31, 32, 33, 63, 64, 65, 100, 4_099] {
        let data: Vec<u8> = (0..len as u32).map(|i| (i * 17 % 253) as u8).collect();
        let base = checksum(&data);
        assert_eq!(base, checksum(&data), "len {len}: deterministic");
        for at in (0..len).step_by(len.div_ceil(40).max(1)).chain([len - 1]) {
            for bit in [0x01u8, 0x80] {
                let mut d = data.clone();
                d[at] ^= bit;
                assert_ne!(checksum(&d), base, "len {len}: flip {bit:#x} at {at}");
            }
        }
        if len > 1 {
            assert_ne!(checksum(&data[..len - 1]), base, "len {len}: truncation");
        }
    }
    let _ = checksum(&[]);
}
