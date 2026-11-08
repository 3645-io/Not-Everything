//! Tests for the open-or-rebuild startup policy and stale temp
//! cleanup in `fx_store`'s `checkpoint` module.
//!
//! API assumed by these tests (written before the implementation), all
//! re-exported at the crate root:
//!
//! - `fx_store::VerifyMode::{HeaderOnly, Full}`. `Full` also runs
//!   `MappedIndex::verify_checksums`.
//! - `fx_store::RebuiltIndex`, the owned result of a rebuild, with public
//!   fields `parts: IndexParts`, `volumes: Vec<(VolumeId, ResumeCursor)>`,
//!   `orders: Vec<(u8, Vec<u32>)>` and `clean_shutdown: bool` (the flag
//!   written into the new checkpoint).
//! - `fx_store::open_or_rebuild(path, verify, rebuild) -> Result<Opened,
//!   StoreError>` where `rebuild: impl FnOnce() -> Result<RebuiltIndex, E>`.
//!   These tests use `E = std::io::Error`. A rebuild error is returned as an
//!   `Err` whose `Debug` or `Display` output contains the rebuild error's
//!   message; I/O errors other than "not found" are returned without calling
//!   `rebuild`.
//! - `fx_store::Opened::{Loaded(MappedIndex), Rebuilt { index: MappedIndex,
//!   reason: RejectReason }}` with methods `index() -> &MappedIndex` and
//!   `clean_shutdown() -> bool`. `Opened: Debug`.
//! - `fx_store::RejectReason: Debug` with (at least) the variants
//!   `NotFound`, `UnsupportedVersion`, `ColumnChecksum`, matched with
//!   `{ .. }`, so payloads are free (a type alias for `LoadError` fits too).
//! - `fx_store::cleanup_stale_temps(target) -> Result<_, E: Debug>` removes
//!   leftover temp files in the target's directory whose names are
//!   `temp_name_prefix(target)` followed by `<pid>.<counter>` (the writer's
//!   pattern) and that are older than `fx_store::STALE_TEMP_MIN_AGE`, and
//!   nothing else.
//!
//! Clean-room: written from docs/PLAN.md and the in-repo code only.

#![allow(clippy::needless_borrows_for_generic_args)]

use std::cell::Cell;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fx_index::{Index, IndexParts};
use fx_model::{Entry, EntryFlags, EntryKind, FileKey, ResumeCursor, VolumeId};
use fx_store::layout::VERSION_OFFSET;
use fx_store::{
    MappedIndex, Opened, RebuiltIndex, RejectReason, SaveInput, VerifyMode, cleanup_stale_temps,
    open_or_rebuild, temp_name_prefix, write_checkpoint,
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
            "fx-store-checkpoint-{tag}-{}-{n}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        TempDir(dir)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn entries(&self) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(&self.0)
            .expect("read temp dir")
            .map(|e| {
                e.expect("dir entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        v.sort();
        v
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const VOL: VolumeId = VolumeId(3);

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
        size: if kind == EntryKind::File { n * 10 } else { 0 },
        modified: 1_700_000_000 + n as i64,
        created: 1_600_000_000 + n as i64,
    }
}

/// A small index whose shape depends on `variant`, so two different
/// checkpoints are distinguishable.
fn small_index(variant: u64) -> Index {
    let mut index = Index::new();
    index
        .upsert(entry(1, None, "root", EntryKind::Dir))
        .expect("insert");
    index
        .upsert(entry(2, Some(1), "docs", EntryKind::Dir))
        .expect("insert");
    for i in 0..(5 + variant) {
        index
            .upsert(entry(
                10 + i,
                Some(2),
                &format!("file{variant}-{i}.txt"),
                EntryKind::File,
            ))
            .expect("insert");
    }
    index.compact();
    // A delta and a removal, so the parts are not trivially dense.
    index
        .upsert(entry(100, Some(1), "fresh.rs", EntryKind::File))
        .expect("insert");
    index.remove(key(10));
    index
}

fn small_parts(variant: u64) -> IndexParts {
    small_index(variant).snapshot().to_parts()
}

fn cursors(variant: u64) -> Vec<(VolumeId, ResumeCursor)> {
    vec![
        (
            VOL,
            ResumeCursor::FsEvents {
                event_id: 1000 + variant,
                db_uuid: [variant as u8; 16],
            },
        ),
        (
            VolumeId(8),
            ResumeCursor::None {
                snapshot_epoch: 5 + variant,
            },
        ),
    ]
}

/// The live IDs in ascending order: a valid saved order.
fn live_ids(parts: &IndexParts) -> Vec<u32> {
    (0..parts.id_bound() as u32)
        .filter(|&i| parts.is_live(i as usize))
        .collect()
}

fn rebuilt(variant: u64, clean: bool) -> RebuiltIndex {
    let parts = small_parts(variant);
    let order = live_ids(&parts);
    RebuiltIndex {
        parts,
        volumes: cursors(variant),
        orders: vec![(4, order)],
        clean_shutdown: clean,
    }
}

/// Writes a valid checkpoint of `small_index(variant)` with `write_checkpoint`.
fn save(path: &Path, variant: u64, clean: bool) -> IndexParts {
    let parts = small_parts(variant);
    let order = live_ids(&parts);
    let orders: Vec<(u8, &[u32])> = vec![(4, order.as_slice())];
    let vols = cursors(variant);
    let input = SaveInput {
        parts: &parts,
        volumes: &vols,
        orders: &orders,
        clean_shutdown: clean,
    };
    write_checkpoint(path, &input).expect("write checkpoint");
    parts
}

fn assert_holds(m: &MappedIndex, variant: u64, clean: bool) {
    assert_eq!(
        m.to_parts(),
        small_parts(variant),
        "parts of variant {variant}"
    );
    assert_eq!(m.cursors().to_vec(), cursors(variant), "cursors");
    assert_eq!(m.clean_shutdown(), clean, "clean flag");
    let expected = live_ids(&small_parts(variant));
    assert_eq!(m.order(4), Some(expected.as_slice()), "saved order");
    m.verify_checksums().expect("rebuilt file verifies");
    m.to_index().expect("rebuilt file converts to an index");
}

/// The file byte range of a non-empty data column (not a saved order).
fn some_column_byte(path: &Path) -> usize {
    let m = MappedIndex::open(path).expect("open");
    let (_, range) = m
        .column_ranges()
        .into_iter()
        .find(|(c, r)| !r.is_empty() && *c == fx_store::Column::Parents)
        .expect("a non-empty parents column");
    range.start + range.len() / 2
}

/// Flips one byte inside a column, leaving the header intact.
fn corrupt_column(path: &Path) {
    let at = some_column_byte(path);
    let mut b = fs::read(path).expect("read");
    b[at] ^= 0x40;
    fs::write(path, &b).expect("write");
}

fn prototype_v1_bytes() -> Vec<u8> {
    let mut bytes = vec![0u8; 4096];
    bytes[..8].copy_from_slice(b"FXSTORE1");
    bytes[VERSION_OFFSET..VERSION_OFFSET + 4].copy_from_slice(&1u32.to_le_bytes());
    bytes
}

/// Runs `open_or_rebuild` with a counting rebuild closure.
fn run(
    path: &Path,
    verify: VerifyMode,
    variant: u64,
    clean: bool,
) -> (Result<Opened, fx_store::StoreError>, usize) {
    let calls = Cell::new(0usize);
    let result = open_or_rebuild(path, verify, || {
        calls.set(calls.get() + 1);
        Ok::<_, io::Error>(rebuilt(variant, clean))
    });
    (result, calls.get())
}

fn expect_rebuilt(result: Result<Opened, fx_store::StoreError>) -> (MappedIndex, RejectReason) {
    match result {
        Ok(Opened::Rebuilt { index, reason }) => (index, reason),
        Ok(other) => panic!("expected a rebuild, got {other:?}"),
        Err(e) => panic!("open_or_rebuild failed: {e:?}"),
    }
}

fn expect_loaded(result: Result<Opened, fx_store::StoreError>) -> MappedIndex {
    match result {
        Ok(Opened::Loaded(index)) => index,
        Ok(other) => panic!("expected a load, got {other:?}"),
        Err(e) => panic!("open_or_rebuild failed: {e:?}"),
    }
}

fn stale_name(target: &Path, pid: u32, n: u64) -> String {
    let mut name: OsString = temp_name_prefix(target).expect("temp prefix");
    name.push(format!("{pid}.{n}"));
    name.to_string_lossy().into_owned()
}

/// Writes `bytes` to `path` and sets its modification time an hour back,
/// past `fx_store::STALE_TEMP_MIN_AGE`, like a leftover of a crashed run.
fn write_old(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).expect("write stale");
    let past = SystemTime::now() - fx_store::STALE_TEMP_MIN_AGE - Duration::from_secs(3600);
    fs::File::options()
        .write(true)
        .open(path)
        .and_then(|f| f.set_modified(past))
        .expect("backdate stale temp");
}

/// A process ID that is not this process.
fn other_pid() -> u32 {
    let me = std::process::id();
    if me == 999_983 { 999_979 } else { 999_983 }
}

// ---------------------------------------------------------------------------
// Loading a good file
// ---------------------------------------------------------------------------

#[test]
fn valid_file_is_loaded_without_calling_rebuild() {
    let dir = TempDir::new("valid");
    let path = dir.file("index.fxs");
    save(&path, 1, true);
    let before = fs::read(&path).expect("read");

    for verify in [VerifyMode::HeaderOnly, VerifyMode::Full] {
        let (result, calls) = run(&path, verify, 9, false);
        let m = expect_loaded(result);
        assert_eq!(calls, 0, "rebuild must not run for a valid file");
        assert_holds(&m, 1, true);
    }
    assert_eq!(fs::read(&path).expect("read"), before, "file untouched");
}

#[test]
fn clean_and_dirty_flags_are_reported_on_load() {
    let dir = TempDir::new("flags");
    for clean in [true, false] {
        let path = dir.file(&format!("index-{clean}.fxs"));
        save(&path, 2, clean);
        for verify in [VerifyMode::HeaderOnly, VerifyMode::Full] {
            let (result, calls) = run(&path, verify, 9, !clean);
            let opened = result.expect("open_or_rebuild");
            assert_eq!(calls, 0, "a dirty file still loads, no rebuild");
            assert!(matches!(opened, Opened::Loaded(_)), "{opened:?}");
            assert_eq!(opened.clean_shutdown(), clean, "Opened reports the flag");
            assert_eq!(opened.index().clean_shutdown(), clean);
        }
    }
}

// ---------------------------------------------------------------------------
// Rebuilding
// ---------------------------------------------------------------------------

#[test]
fn missing_file_is_rebuilt_with_reason_not_found() {
    let dir = TempDir::new("missing");
    let path = dir.file("index.fxs");
    let (result, calls) = run(&path, VerifyMode::Full, 3, false);
    let (index, reason) = expect_rebuilt(result);
    assert_eq!(calls, 1, "rebuild runs exactly once");
    assert!(
        matches!(reason, RejectReason::NotFound { .. }),
        "{reason:?}"
    );
    assert_holds(&index, 3, false);

    // The rebuilt result is on disk and reopens on its own.
    let reopened = MappedIndex::open(&path).expect("rebuilt file is on disk");
    assert_holds(&reopened, 3, false);
    assert_eq!(dir.entries(), vec!["index.fxs".to_string()], "no temp left");

    // A second start loads what the rebuild wrote.
    let (result, calls) = run(&path, VerifyMode::Full, 4, true);
    let m = expect_loaded(result);
    assert_eq!(calls, 0);
    assert_holds(&m, 3, false);
}

#[test]
fn rebuilt_opened_reports_the_rebuilt_clean_flag() {
    let dir = TempDir::new("rebuiltflag");
    for clean in [true, false] {
        let path = dir.file(&format!("index-{clean}.fxs"));
        let (result, calls) = run(&path, VerifyMode::HeaderOnly, 5, clean);
        let opened = result.expect("open_or_rebuild");
        assert_eq!(calls, 1);
        assert!(matches!(opened, Opened::Rebuilt { .. }), "{opened:?}");
        assert_eq!(opened.clean_shutdown(), clean);
        assert_eq!(opened.index().clean_shutdown(), clean);
        assert_holds(opened.index(), 5, clean);
    }
}

#[test]
fn corrupt_column_is_rebuilt_with_full_verify() {
    let dir = TempDir::new("corruptfull");
    let path = dir.file("index.fxs");
    save(&path, 1, true);
    corrupt_column(&path);

    let (result, calls) = run(&path, VerifyMode::Full, 6, false);
    let (index, reason) = expect_rebuilt(result);
    assert_eq!(calls, 1);
    assert!(
        matches!(reason, RejectReason::ColumnChecksum { .. }),
        "{reason:?}"
    );
    assert_holds(&index, 6, false);
    let reopened = MappedIndex::open(&path).expect("open rewritten file");
    assert_holds(&reopened, 6, false);
}

#[test]
fn corrupt_column_is_loaded_with_header_only_verify() {
    let dir = TempDir::new("corrupthdr");
    let path = dir.file("index.fxs");
    save(&path, 1, false);
    corrupt_column(&path);
    let before = fs::read(&path).expect("read");

    let (result, calls) = run(&path, VerifyMode::HeaderOnly, 6, true);
    let m = expect_loaded(result);
    assert_eq!(calls, 0, "header-only verify does not read columns");
    assert!(!m.clean_shutdown());
    assert_eq!(fs::read(&path).expect("read"), before, "file not rewritten");
}

#[test]
fn prototype_v1_file_is_rebuilt_as_unsupported_version() {
    let dir = TempDir::new("v1");
    let path = dir.file("index.fxs");
    fs::write(&path, prototype_v1_bytes()).expect("write v1");

    for verify in [VerifyMode::HeaderOnly, VerifyMode::Full] {
        fs::write(&path, prototype_v1_bytes()).expect("write v1");
        let (result, calls) = run(&path, verify, 7, true);
        let (index, reason) = expect_rebuilt(result);
        assert_eq!(calls, 1);
        assert!(
            matches!(reason, RejectReason::UnsupportedVersion { .. }),
            "{reason:?}"
        );
        assert_holds(&index, 7, true);
        let reopened = MappedIndex::open(&path).expect("v1 replaced by a current file");
        assert_holds(&reopened, 7, true);
    }
}

#[test]
fn other_corrupt_files_are_rebuilt() {
    let dir = TempDir::new("othercorrupt");
    let path = dir.file("index.fxs");
    save(&path, 1, true);
    let good = fs::read(&path).expect("read");

    let mut magic = good.clone();
    magic[0] ^= 0xff;
    // A byte inside the fixed header, covered by the header checksum.
    let mut header = good.clone();
    header[VERSION_OFFSET + 40] ^= 0x01;
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("truncated", good[..good.len() / 2].to_vec()),
        ("bad magic", magic),
        ("header flip", header),
        ("garbage", vec![0x5a; 8192]),
    ];

    for (what, bytes) in cases {
        fs::write(&path, &bytes).expect("write corrupt");
        let (result, calls) = run(&path, VerifyMode::Full, 8, false);
        let (index, reason) = match result {
            Ok(Opened::Rebuilt { index, reason }) => (index, reason),
            other => panic!("{what}: expected a rebuild, got {other:?}"),
        };
        assert_eq!(calls, 1, "{what}");
        assert!(
            !matches!(reason, RejectReason::NotFound { .. }),
            "{what}: {reason:?}"
        );
        assert_holds(&index, 8, false);
        let reopened = MappedIndex::open(&path).expect("rewritten");
        assert_holds(&reopened, 8, false);
    }
}

#[test]
fn rebuild_error_is_propagated_and_keeps_the_old_file() {
    let dir = TempDir::new("rebuilderr");
    let path = dir.file("index.fxs");
    save(&path, 1, true);
    corrupt_column(&path);
    let before = fs::read(&path).expect("read");

    let calls = Cell::new(0usize);
    let result = open_or_rebuild(path.as_path(), VerifyMode::Full, || {
        calls.set(calls.get() + 1);
        Err::<RebuiltIndex, _>(io::Error::other("scan exploded xyzzy"))
    });
    assert_eq!(calls.get(), 1);
    let err = match result {
        Err(e) => e,
        Ok(o) => panic!("a failed rebuild must be an error, got {o:?}"),
    };
    let text = format!("{err} / {err:?}");
    assert!(text.contains("xyzzy"), "rebuild error is carried: {text}");
    assert_eq!(fs::read(&path).expect("read"), before, "old file intact");
    assert_eq!(dir.entries(), vec!["index.fxs".to_string()], "no temp left");

    // Missing file plus failed rebuild: still an error, nothing created.
    let missing = dir.file("missing.fxs");
    let result = open_or_rebuild(missing.as_path(), VerifyMode::HeaderOnly, || {
        Err::<RebuiltIndex, _>(io::Error::other("no scan"))
    });
    assert!(result.is_err());
    assert!(
        !missing.exists(),
        "no checkpoint written on rebuild failure"
    );
}

#[test]
fn io_errors_other_than_not_found_are_returned_without_rebuild() {
    let dir = TempDir::new("ioerr");
    // The "directory" of the index path is a regular file, so opening the
    // index fails with an I/O error that is not "not found".
    let blocker = dir.file("not-a-dir");
    fs::write(&blocker, b"plain file").expect("write");
    let path = blocker.join("index.fxs");

    let (result, calls) = run(&path, VerifyMode::Full, 1, true);
    assert!(result.is_err(), "expected an error, got {result:?}");
    assert_eq!(calls, 0, "I/O errors are not silently rebuilt");
    assert_eq!(fs::read(&blocker).expect("read"), b"plain file");
}

// ---------------------------------------------------------------------------
// Stale temp files
// ---------------------------------------------------------------------------

#[test]
fn cleanup_removes_stale_temps_and_keeps_unrelated_files() {
    let dir = TempDir::new("cleanup");
    let path = dir.file("index.fxs");
    save(&path, 1, true);
    let pid = other_pid();
    let stale = [stale_name(&path, pid, 0), stale_name(&path, pid, 17)];
    for name in &stale {
        write_old(&dir.file(name), b"half-written checkpoint");
    }
    let unrelated = ["notes.txt", "index.fxs.bak", ".hidden", "tmp.log"];
    for name in unrelated {
        fs::write(dir.file(name), b"keep me").expect("write unrelated");
    }
    fs::create_dir(dir.file("subdir")).expect("mkdir");

    cleanup_stale_temps(&path).expect("cleanup");

    let mut expected: Vec<String> = unrelated.iter().map(|s| s.to_string()).collect();
    expected.push("index.fxs".to_string());
    expected.push("subdir".to_string());
    expected.sort();
    assert_eq!(dir.entries(), expected);
    for name in unrelated {
        assert_eq!(fs::read(dir.file(name)).expect("read"), b"keep me");
    }
    MappedIndex::open(&path).expect("checkpoint itself kept");

    // Running it again on a clean directory is fine.
    cleanup_stale_temps(&path).expect("second cleanup");
    assert_eq!(dir.entries(), expected);
}

#[test]
fn open_or_rebuild_cleans_up_stale_temps() {
    let dir = TempDir::new("openclean");
    let path = dir.file("index.fxs");
    save(&path, 2, true);
    let pid = other_pid();
    let stale = stale_name(&path, pid, 3);
    write_old(&dir.file(&stale), b"partial");
    fs::write(dir.file("notes.txt"), b"keep me").expect("write unrelated");

    let (result, calls) = run(&path, VerifyMode::HeaderOnly, 9, false);
    let m = expect_loaded(result);
    assert_eq!(calls, 0);
    assert_holds(&m, 2, true);
    assert_eq!(
        dir.entries(),
        vec!["index.fxs".to_string(), "notes.txt".to_string()],
        "stale temp removed, others kept"
    );

    // Also on the rebuild path.
    let missing = dir.file("other.fxs");
    let stale = stale_name(&missing, pid, 4);
    write_old(&dir.file(&stale), b"partial");
    let (result, calls) = run(&missing, VerifyMode::Full, 9, false);
    expect_rebuilt(result);
    assert_eq!(calls, 1);
    assert_eq!(
        dir.entries(),
        vec![
            "index.fxs".to_string(),
            "notes.txt".to_string(),
            "other.fxs".to_string()
        ],
    );
}
