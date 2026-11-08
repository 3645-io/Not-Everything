//! Saved order validation (the order contract in `fx_store::layout`): a
//! saved order is a permutation of the live entry IDs, so its length is the
//! live count and every element is a live ID below id_bound, with no
//! duplicates.
//!
//! - `write_checkpoint` rejects violations with
//!   `StoreError::InvalidOrder { key, problem: OrderProblem }`.
//! - `MappedIndex::open` rejects a bad order length with
//!   `LoadError::Malformed`; `verify_checksums` (and so
//!   `VerifyMode::Full`) also rejects bad values with `LoadError::Malformed`.
//!
//! Clean-room: written from docs/PLAN.md and the in-repo code only.

#![allow(clippy::needless_borrows_for_generic_args)]

use std::cell::Cell;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fx_index::{Index, IndexParts};
use fx_model::{Entry, EntryFlags, EntryKind, FileKey, ResumeCursor, VolumeId};
use fx_store::layout::{
    COLUMN_CHECKSUM_FIELD, COLUMN_LEN_FIELD, COLUMN_OFFSET_FIELD, COLUMN_RECORD_SIZE,
    FIXED_HEADER_SIZE, HEADER_CHECKSUM_OFFSET, HEADER_LEN_OFFSET, REQUIRED_COLUMNS,
    VOLUME_COUNT_OFFSET, VOLUME_RECORD_SIZE, checksum, header_checksum,
};
use fx_store::{
    LoadError, MappedIndex, Opened, OrderProblem, RebuiltIndex, SaveInput, StoreError, VerifyMode,
    open_or_rebuild, write_checkpoint,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A unique directory under the system temp dir, removed on drop.
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
            "fx-store-orders-{tag}-{}-{n}-{nanos}",
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

const VOL: VolumeId = VolumeId(5);
const KEY: u8 = 7;

fn key(n: u64) -> FileKey {
    FileKey::new(VOL, n)
}

fn entry(n: u64, parent: Option<u64>, name: &str, kind: EntryKind) -> Entry {
    Entry {
        key: key(n),
        parent: parent.map(key),
        name: name.to_string(),
        kind,
        flags: EntryFlags::empty(),
        size: n,
        modified: 1_700_000_000 + n as i64,
        created: 1_600_000_000 + n as i64,
    }
}

/// Parts with at least one removed entry, so live IDs are not `0..id_bound`.
fn parts() -> IndexParts {
    let mut index = Index::new();
    // Keep removed entries as holes instead of compacting them away.
    index.set_compact_fraction(f64::INFINITY);
    index
        .upsert(entry(1, None, "root", EntryKind::Dir))
        .expect("insert");
    for i in 0..8 {
        index
            .upsert(entry(
                10 + i,
                Some(1),
                &format!("f{i}.txt"),
                EntryKind::File,
            ))
            .expect("insert");
    }
    index.compact();
    index.remove(key(12));
    index.remove(key(15));
    let parts = index.snapshot().to_parts();
    assert!(
        (0..parts.id_bound()).any(|i| !parts.is_live(i)),
        "fixture needs a removed entry"
    );
    parts
}

fn live_ids(p: &IndexParts) -> Vec<u32> {
    (0..p.id_bound() as u32)
        .filter(|&i| p.is_live(i as usize))
        .collect()
}

fn dead_id(p: &IndexParts) -> u32 {
    (0..p.id_bound() as u32)
        .find(|&i| !p.is_live(i as usize))
        .expect("a removed entry")
}

fn vols() -> Vec<(VolumeId, ResumeCursor)> {
    vec![(VOL, ResumeCursor::None { snapshot_epoch: 1 })]
}

fn write_with(path: &Path, p: &IndexParts, order: &[u32]) -> Result<u64, StoreError> {
    let vols = vols();
    let orders = [(KEY, order)];
    write_checkpoint(
        path,
        &SaveInput {
            parts: p,
            volumes: &vols,
            orders: &orders,
            clean_shutdown: true,
        },
    )
}

fn expect_invalid(result: Result<u64, StoreError>) -> OrderProblem {
    match result {
        Err(StoreError::InvalidOrder { key, problem }) => {
            assert_eq!(key, KEY, "error names the order key");
            problem
        }
        other => panic!("expected InvalidOrder, got {other:?}"),
    }
}

/// Writes a valid checkpoint (ascending live IDs) and returns its parts.
fn save_valid(path: &Path) -> IndexParts {
    let p = parts();
    write_with(path, &p, &live_ids(&p)).expect("valid order is written");
    p
}

fn get_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

fn get_u64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}

fn put_u64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

/// Byte offset of the saved order's column record (the first order).
fn order_record(b: &[u8]) -> usize {
    let volumes = get_u32(b, VOLUME_COUNT_OFFSET) as usize;
    FIXED_HEADER_SIZE + volumes * VOLUME_RECORD_SIZE + REQUIRED_COLUMNS * COLUMN_RECORD_SIZE
}

/// Recomputes the header checksum after a hand patch.
fn reseal_header(b: &mut [u8]) {
    let header_len = get_u32(b, HEADER_LEN_OFFSET) as usize;
    let sum = header_checksum(&b[..header_len]);
    put_u64(b, HEADER_CHECKSUM_OFFSET, sum);
}

/// Sets order element `index` to `value`, then fixes the column and header
/// checksums, so only the order contract is broken.
fn patch_order_value(path: &Path, index: usize, value: u32) {
    let mut b = fs::read(path).expect("read");
    let rec = order_record(&b);
    let offset = get_u64(&b, rec + COLUMN_OFFSET_FIELD) as usize;
    let len = get_u64(&b, rec + COLUMN_LEN_FIELD) as usize;
    let at = offset + index * 4;
    b[at..at + 4].copy_from_slice(&value.to_le_bytes());
    let sum = checksum(&b[offset..offset + len]);
    put_u64(&mut b, rec + COLUMN_CHECKSUM_FIELD, sum);
    reseal_header(&mut b);
    fs::write(path, &b).expect("write");
}

fn assert_malformed(err: &LoadError) {
    assert!(matches!(err, LoadError::Malformed { .. }), "{err:?}");
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

#[test]
fn valid_orders_round_trip() {
    let dir = TempDir::new("valid");
    let path = dir.file("index.fxs");
    let p = save_valid(&path);
    let m = MappedIndex::open(&path).expect("open");
    m.verify_checksums().expect("valid order verifies");
    assert_eq!(m.order(KEY), Some(live_ids(&p).as_slice()));
    assert_eq!(m.live_count(), live_ids(&p).len());

    // Any permutation of the live IDs is fine, and so is an empty index.
    let mut rev = live_ids(&p);
    rev.reverse();
    write_with(&path, &p, &rev).expect("reversed order is written");
    let empty = Index::new().snapshot().to_parts();
    write_with(&dir.file("empty.fxs"), &empty, &[]).expect("empty order of an empty index");
    MappedIndex::open(dir.file("empty.fxs"))
        .and_then(|m| m.verify_checksums())
        .expect("empty order verifies");
}

#[test]
fn writer_rejects_wrong_length() {
    let dir = TempDir::new("len");
    let path = dir.file("index.fxs");
    let p = parts();
    let live = live_ids(&p);

    let short = &live[..live.len() - 1];
    assert_eq!(
        expect_invalid(write_with(&path, &p, short)),
        OrderProblem::Length {
            expected: live.len(),
            found: live.len() - 1
        }
    );
    // Every ID, removed ones included, is too long.
    let all: Vec<u32> = (0..p.id_bound() as u32).collect();
    assert!(matches!(
        expect_invalid(write_with(&path, &p, &all)),
        OrderProblem::Length { .. }
    ));
    assert!(!path.exists(), "nothing written on rejection");
}

#[test]
fn writer_rejects_out_of_range_id() {
    let dir = TempDir::new("range");
    let path = dir.file("index.fxs");
    let p = parts();
    let mut order = live_ids(&p);
    let last = order.len() - 1;
    order[last] = p.id_bound() as u32;
    assert!(matches!(
        expect_invalid(write_with(&path, &p, &order)),
        OrderProblem::OutOfRange { id, .. } if id as usize == p.id_bound()
    ));
    order[last] = u32::MAX;
    assert!(matches!(
        expect_invalid(write_with(&path, &p, &order)),
        OrderProblem::OutOfRange { id: u32::MAX, .. }
    ));
    assert!(!path.exists(), "nothing written on rejection");
}

#[test]
fn writer_rejects_dead_id() {
    let dir = TempDir::new("dead");
    let path = dir.file("index.fxs");
    let p = parts();
    let dead = dead_id(&p);
    let mut order = live_ids(&p);
    order[0] = dead;
    assert_eq!(
        expect_invalid(write_with(&path, &p, &order)),
        OrderProblem::DeadId { id: dead }
    );
    assert!(!path.exists(), "nothing written on rejection");
}

#[test]
fn writer_rejects_duplicate_id_and_keeps_old_file() {
    let dir = TempDir::new("dup");
    let path = dir.file("index.fxs");
    let p = save_valid(&path);
    let before = fs::read(&path).expect("read");
    let mut order = live_ids(&p);
    order[1] = order[0];
    let id = order[0];
    assert_eq!(
        expect_invalid(write_with(&path, &p, &order)),
        OrderProblem::DuplicateId { id }
    );
    let err = write_with(&path, &p, &order).expect_err("rejected");
    assert!(err.to_string().contains(&KEY.to_string()), "{err}");
    assert_eq!(fs::read(&path).expect("read"), before, "old file intact");
}

#[test]
fn open_or_rebuild_surfaces_an_invalid_rebuilt_order() {
    let dir = TempDir::new("rebuild");
    let path = dir.file("index.fxs");
    let result = open_or_rebuild(&path, VerifyMode::Full, || {
        let parts = parts();
        let all: Vec<u32> = (0..parts.id_bound() as u32).collect();
        Ok::<_, io::Error>(RebuiltIndex {
            parts,
            volumes: vols(),
            orders: vec![(KEY, all)],
            clean_shutdown: true,
        })
    });
    assert!(
        matches!(result, Err(StoreError::InvalidOrder { key: KEY, .. })),
        "{result:?}"
    );
    assert!(!path.exists());
}

// ---------------------------------------------------------------------------
// Loader
// ---------------------------------------------------------------------------

#[test]
fn bad_order_length_fails_at_open() {
    let dir = TempDir::new("openlen");
    let path = dir.file("index.fxs");
    save_valid(&path);
    let mut b = fs::read(&path).expect("read");
    let rec = order_record(&b);
    let len = get_u64(&b, rec + COLUMN_LEN_FIELD);
    put_u64(&mut b, rec + COLUMN_LEN_FIELD, len - 4);
    reseal_header(&mut b);
    fs::write(&path, &b).expect("write");

    let err = MappedIndex::open(&path).expect_err("short order rejected at open");
    assert_malformed(&err);
    assert!(err.to_string().contains("order"), "{err}");

    // The startup policy rebuilds it even without full verification.
    let calls = Cell::new(0);
    let result = open_or_rebuild(&path, VerifyMode::HeaderOnly, || {
        calls.set(calls.get() + 1);
        let parts = parts();
        let order = live_ids(&parts);
        Ok::<_, io::Error>(RebuiltIndex {
            parts,
            volumes: vols(),
            orders: vec![(KEY, order)],
            clean_shutdown: true,
        })
    });
    match result {
        Ok(Opened::Rebuilt { reason, .. }) => assert_malformed(&reason),
        other => panic!("expected a rebuild, got {other:?}"),
    }
    assert_eq!(calls.get(), 1);
}

#[test]
fn bad_order_values_fail_under_full_verify() {
    let p = parts();
    let live = live_ids(&p);
    let cases: [(&str, usize, u32); 4] = [
        ("out of range", 0, p.id_bound() as u32),
        ("way out of range", 2, u32::MAX),
        ("dead", 1, dead_id(&p)),
        ("duplicate", 1, live[0]),
    ];
    for (what, index, value) in cases {
        let dir = TempDir::new("values");
        let path = dir.file("index.fxs");
        save_valid(&path);
        patch_order_value(&path, index, value);

        // Header-only open passes: only the length is guaranteed.
        let m = MappedIndex::open(&path)
            .unwrap_or_else(|e| panic!("{what}: header-only open should pass: {e:?}"));
        let order = m.order(KEY).expect("order present");
        assert_eq!(order.len(), m.live_count(), "{what}: length guaranteed");
        assert_eq!(order[index], value, "{what}: patched value visible");

        let err = m.verify_checksums().expect_err(what);
        assert_malformed(&err);
        drop(m);

        let loaded = open_or_rebuild(&path, VerifyMode::HeaderOnly, || {
            Err::<RebuiltIndex, _>(io::Error::other("must not rebuild"))
        });
        assert!(
            matches!(loaded, Ok(Opened::Loaded(_))),
            "{what}: HeaderOnly loads"
        );
        let rebuilt = open_or_rebuild(&path, VerifyMode::Full, || {
            let parts = parts();
            let order = live_ids(&parts);
            Ok::<_, io::Error>(RebuiltIndex {
                parts,
                volumes: vols(),
                orders: vec![(KEY, order)],
                clean_shutdown: true,
            })
        });
        match rebuilt {
            Ok(Opened::Rebuilt { reason, index }) => {
                assert_malformed(&reason);
                index.verify_checksums().expect("rebuilt file verifies");
            }
            other => panic!("{what}: expected a rebuild, got {other:?}"),
        }
    }
}
