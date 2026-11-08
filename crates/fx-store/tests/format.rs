//! Tests for the v2 index file format: a whole `fx_index` snapshot,
//! per-volume resume cursors, the clean-shutdown flag and saved orders,
//! written atomically and loaded zero-copy through mmap.
//!
//! API assumed by these tests (written before the implementation):
//!
//! - `fx_store::SaveInput<'a>` with public fields `parts: &'a IndexParts`,
//!   `volumes: &'a [(VolumeId, ResumeCursor)]`, `orders: &'a [(u8, &'a [u32])]`
//!   and `clean_shutdown: bool`.
//! - `fx_store::write_checkpoint(&Path, &SaveInput) -> Result<_, StoreError>`
//!   (`StoreError: Debug`): temp file in the same directory, sync, rename,
//!   directory fsync. On error the temp file is removed and the old file is
//!   left untouched.
//! - `fx_store::MappedIndex::open(&Path) -> Result<MappedIndex, LoadError>`
//!   validates only the header and tables. Accessors: `id_bound()`,
//!   `live_count()` (`usize`); `name_bytes()`, `name_offsets()`, `parents()`,
//!   `volumes()`, `file_ids()`, `meta()`, `modified()`, `created()`,
//!   `ext_ids()`, `ext_bytes()`, `ext_offsets()`, `live()` returning slices
//!   equal to the `IndexParts` fields of the same name; `cursors()` returning
//!   the volume table as a `Vec<(VolumeId, ResumeCursor)>` (or anything that
//!   derefs to such a slice) in save order; `clean_shutdown() -> bool`;
//!   `orders() -> Vec<(u8, &[u32])>` in save order; `order(u8) ->
//!   Option<&[u32]>`; `verify_checksums() -> Result<(), LoadError>`;
//!   `to_parts() -> IndexParts`; `to_index() -> Result<Index, E: Debug>`.
//! - Introspection for corruption tests: `header_len() -> usize`, the number
//!   of bytes at the start of the file covered by the header checksum
//!   (header, volume table and column table), and `column_ranges() ->
//!   Vec<(Column, Range<usize>)>`, the file byte range of every stored column
//!   (saved orders included). `fx_store::Column: Debug + Copy`.
//! - `fx_store::LoadError: Debug + Display + std::error::Error` with variants
//!   `NotFound`, `BadMagic`, `UnsupportedVersion`, `WrongEndianness`,
//!   `Truncated`, `HeaderChecksum`, `ColumnChecksum` (its Debug output
//!   includes the Debug output of the column), `Malformed` and `Io`. Variants
//!   are matched with `{ .. }`, so their payloads are free.
//! - `fx_store::layout::{MAGIC, FORMAT_VERSION, VERSION_OFFSET,
//!   ENDIAN_OFFSET, ENDIAN_MARKER}`: the magic stays `*b"FXSTORE1"` at
//!   offset 0 and the little-endian `u32` version stays at offset 8 (as in
//!   the version 1 prototype), so a v1 file is reported as
//!   `UnsupportedVersion`.
//!   `FORMAT_VERSION >= 2`. `ENDIAN_MARKER: u32` is stored little-endian at
//!   `ENDIAN_OFFSET`. Open checks magic, then version, then endianness,
//!   before the header length and checksum.
//!
//! Clean-room: written from docs/PLAN.md and the in-repo code only.

#![allow(clippy::needless_borrows_for_generic_args)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fx_index::{Index, IndexParts};
use fx_model::{Entry, EntryFlags, EntryKind, FileKey, ResumeCursor, VolumeId};
use fx_store::layout::{ENDIAN_MARKER, ENDIAN_OFFSET, FORMAT_VERSION, MAGIC, VERSION_OFFSET};
use fx_store::{LoadError, MappedIndex, SaveInput, write_checkpoint};

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
            "fx-store-format-{tag}-{}-{n}-{nanos}",
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
        }
        let _ = fs::remove_dir_all(&self.0);
    }
}

const VOL_A: VolumeId = VolumeId(1);
const VOL_B: VolumeId = VolumeId(7);

fn ka(n: u64) -> FileKey {
    FileKey::new(VOL_A, n)
}

fn kb(n: u64) -> FileKey {
    FileKey::new(VOL_B, n)
}

fn mk(key: FileKey, parent: Option<FileKey>, name: &str, kind: EntryKind) -> Entry {
    let n = key.file_id;
    let mut flags = EntryFlags::empty();
    if n % 5 == 0 {
        flags.insert(EntryFlags::HIDDEN);
    }
    if n % 7 == 0 {
        flags.insert(EntryFlags::DATALESS);
    }
    Entry {
        key,
        parent,
        name: name.to_string(),
        kind,
        flags,
        size: if kind == EntryKind::File {
            n * 1013 + 1
        } else {
            0
        },
        modified: 1_700_000_000 + n as i64,
        created: -(n as i64) * 31,
    }
}

fn dir(key: FileKey, parent: Option<FileKey>, name: &str) -> Entry {
    mk(key, parent, name, EntryKind::Dir)
}

fn file(key: FileKey, parent: FileKey, name: &str) -> Entry {
    mk(key, Some(parent), name, EntryKind::File)
}

/// Two volumes, a compacted base, a delta, renames and moves, removals in
/// base and delta, and a key removed and inserted again.
fn rich_index() -> Index {
    let mut index = Index::new();
    index.set_compact_fraction(f64::INFINITY);
    let base = [
        dir(ka(1), None, "vol"),
        dir(ka(2), Some(ka(1)), "docs"),
        file(ka(3), ka(2), "a.txt"),
        file(ka(4), ka(2), "B.TXT"),
        dir(ka(5), Some(ka(2)), "sub"),
        file(ka(6), ka(5), "c.rs"),
        file(ka(7), ka(5), "d.tar.gz"),
        dir(ka(8), Some(ka(1)), "pics"),
        file(ka(9), ka(8), "p.jpg"),
        file(ka(10), ka(1), "Makefile"),
        file(ka(12), ka(8), "résumé.pdf"),
        dir(kb(1), None, "other"),
        file(kb(2), kb(1), "x.txt"),
        dir(kb(3), Some(kb(1)), "deep"),
        file(kb(4), kb(3), "y.md"),
    ];
    for e in base {
        index.upsert(e).expect("insert base entry");
    }
    for i in 0..300u64 {
        index
            .upsert(file(ka(100 + i), ka(2), &format!("bulk{i}.log")))
            .expect("insert bulk entry");
    }
    index.compact();

    // Delta.
    index
        .upsert(dir(ka(20), Some(ka(1)), "new dir"))
        .expect("insert");
    index
        .upsert(file(ka(21), ka(20), "fresh.swift"))
        .expect("insert");
    index
        .upsert(file(ka(22), ka(20), "gone.tmp"))
        .expect("insert");
    index.upsert(file(kb(5), kb(3), "z.json")).expect("insert");
    // Renames and moves.
    index
        .upsert(file(ka(3), ka(2), "a-renamed.txt"))
        .expect("rename");
    index
        .upsert(file(ka(9), ka(20), "p-moved.jpg"))
        .expect("move");
    // Removals: base leaf, delta leaf, base subtree, bulk entries.
    assert_eq!(index.remove(ka(4)), 1);
    assert_eq!(index.remove(ka(22)), 1);
    assert_eq!(index.remove(ka(5)), 3);
    for i in (0..300u64).step_by(3) {
        assert_eq!(index.remove(ka(100 + i)), 1);
    }
    // Removed then inserted again under a new ID.
    assert_eq!(index.remove(kb(2)), 1);
    index
        .upsert(file(kb(2), kb(1), "x-again.txt"))
        .expect("reinsert");
    assert!(index.deleted_len() > 0, "fixture has deletions");
    assert!(index.delta_len() > 0, "fixture has a delta");
    index
}

fn cursors() -> Vec<(VolumeId, ResumeCursor)> {
    vec![
        (
            VOL_A,
            ResumeCursor::FsEvents {
                event_id: 0x0123_4567_89ab_cdef,
                db_uuid: [
                    0xde, 0xad, 0xbe, 0xef, 0, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
                    0x99, 0xaa, 0xbb,
                ],
            },
        ),
        (
            VOL_B,
            ResumeCursor::Usn {
                journal_id: u64::MAX - 3,
                next_usn: -42,
            },
        ),
        (VolumeId(9), ResumeCursor::None { snapshot_epoch: 77 }),
        (
            VolumeId(u32::MAX - 1),
            ResumeCursor::FsEvents {
                event_id: 0,
                db_uuid: [0xff; 16],
            },
        ),
    ]
}

/// Two permutations of the live IDs: ascending and reversed.
fn orders_for(parts: &IndexParts) -> Vec<(u8, Vec<u32>)> {
    let id: Vec<u32> = (0..parts.id_bound() as u32)
        .filter(|&i| parts.is_live(i as usize))
        .collect();
    let rev: Vec<u32> = id.iter().rev().copied().collect();
    vec![(1, id), (200, rev)]
}

/// Saves a rich index to `path` and returns what was saved.
fn save_rich(path: &Path, clean: bool) -> (Index, IndexParts, Vec<(u8, Vec<u32>)>) {
    let index = rich_index();
    let parts = index.snapshot().to_parts();
    let orders = orders_for(&parts);
    let order_refs: Vec<(u8, &[u32])> = orders.iter().map(|(k, o)| (*k, o.as_slice())).collect();
    let vols = cursors();
    let input = SaveInput {
        parts: &parts,
        volumes: &vols,
        orders: &order_refs,
        clean_shutdown: clean,
    };
    write_checkpoint(path, &input).expect("write checkpoint");
    (index, parts, orders)
}

fn open_ok(path: &Path) -> MappedIndex {
    match MappedIndex::open(path) {
        Ok(m) => m,
        Err(e) => panic!("open {} failed: {e} ({e:?})", path.display()),
    }
}

/// Opens and verifies; returns the first error.
fn open_and_verify(path: &Path) -> Result<(), LoadError> {
    let m = MappedIndex::open(path)?;
    m.verify_checksums()
}

fn assert_parts_eq(m: &MappedIndex, p: &IndexParts) {
    assert_eq!(m.id_bound(), p.id_bound(), "id_bound");
    assert_eq!(m.name_bytes(), &p.name_bytes[..], "name_bytes");
    assert_eq!(m.name_offsets(), &p.name_offsets[..], "name_offsets");
    assert_eq!(m.parents(), &p.parents[..], "parents");
    assert_eq!(m.volumes(), &p.volumes[..], "volumes");
    assert_eq!(m.file_ids(), &p.file_ids[..], "file_ids");
    assert_eq!(m.meta(), &p.meta[..], "meta");
    assert_eq!(m.modified(), &p.modified[..], "modified");
    assert_eq!(m.created(), &p.created[..], "created");
    assert_eq!(m.ext_ids(), &p.ext_ids[..], "ext_ids");
    assert_eq!(m.ext_bytes(), &p.ext_bytes[..], "ext_bytes");
    assert_eq!(m.ext_offsets(), &p.ext_offsets[..], "ext_offsets");
    let words = p.id_bound().div_ceil(64);
    assert!(m.live().len() >= words, "live bitmap covers id_bound");
    for i in 0..p.id_bound() {
        let bit = (m.live()[i / 64] >> (i % 64)) & 1 == 1;
        assert_eq!(bit, p.is_live(i), "live bit {i}");
    }
}

fn header_error(e: &LoadError) -> bool {
    matches!(
        e,
        LoadError::BadMagic { .. }
            | LoadError::UnsupportedVersion { .. }
            | LoadError::WrongEndianness { .. }
            | LoadError::Truncated { .. }
            | LoadError::HeaderChecksum { .. }
            | LoadError::Malformed { .. }
    )
}

// ---------------------------------------------------------------------------
// Round trips
// ---------------------------------------------------------------------------

#[test]
fn full_snapshot_round_trips_with_ids_cursors_flag_and_orders() {
    let dir = TempDir::new("roundtrip");
    let path = dir.file("index.fxs");
    let (index, parts, orders) = save_rich(&path, true);

    let m = open_ok(&path);
    m.verify_checksums().expect("checksums of a good file");
    assert_parts_eq(&m, &parts);
    assert_eq!(m.live_count(), index.live_count(), "live_count");
    assert!(m.clean_shutdown(), "clean flag");
    let got: Vec<(VolumeId, ResumeCursor)> = m.cursors().to_vec();
    assert_eq!(got, cursors(), "volume cursors");
    let got_orders: Vec<(u8, Vec<u32>)> =
        m.orders().iter().map(|(k, o)| (*k, o.to_vec())).collect();
    assert_eq!(got_orders, orders, "saved orders");
    assert_eq!(m.order(200), Some(&orders[1].1[..]));
    assert_eq!(m.order(3), None);

    assert_eq!(m.to_parts(), parts, "to_parts");

    // The restored index has the same IDs, lookups, names and live set.
    let restored = m.to_index().expect("to_index");
    let (a, b) = (index.snapshot(), restored.snapshot());
    assert_eq!(b.id_bound(), a.id_bound());
    assert_eq!(b.live_count(), a.live_count());
    assert_eq!(
        b.live_ids().collect::<Vec<_>>(),
        a.live_ids().collect::<Vec<_>>()
    );
    for id in a.live_ids() {
        assert_eq!(b.name(id), a.name(id), "name {id:?}");
        assert_eq!(b.path(id), a.path(id), "path {id:?}");
        assert_eq!(b.key(id), a.key(id), "key {id:?}");
        assert_eq!(b.size(id), a.size(id));
        assert_eq!(b.kind(id), a.kind(id));
        assert_eq!(b.flags(id), a.flags(id));
        assert_eq!(b.modified(id), a.modified(id));
        assert_eq!(b.created(id), a.created(id));
        assert_eq!(b.ext(id), a.ext(id));
        assert_eq!(b.lookup(a.key(id)), Some(id));
    }
    for key in [
        ka(3),
        ka(9),
        ka(21),
        kb(2),
        kb(5),
        ka(4),
        ka(6),
        ka(22),
        ka(100),
    ] {
        assert_eq!(restored.lookup(key), index.lookup(key), "lookup {key:?}");
    }
    assert_eq!(restored.lookup(ka(4)), None);
    assert_eq!(
        b.to_parts(),
        parts,
        "restored snapshot exports the same parts"
    );
}

#[test]
fn unclean_flag_empty_tables_and_empty_index_round_trip() {
    let dir = TempDir::new("empty");
    let path = dir.file("index.fxs");
    let parts = Index::new().snapshot().to_parts();
    let input = SaveInput {
        parts: &parts,
        volumes: &[],
        orders: &[],
        clean_shutdown: false,
    };
    write_checkpoint(&path, &input).expect("write empty checkpoint");
    let m = open_ok(&path);
    m.verify_checksums().expect("checksums");
    assert!(!m.clean_shutdown());
    assert_eq!(m.id_bound(), 0);
    assert_eq!(m.live_count(), 0);
    assert!(m.cursors().is_empty());
    assert!(m.orders().is_empty());
    assert_eq!(m.to_parts(), parts);
    assert_eq!(m.to_index().expect("to_index").live_count(), 0);
}

#[test]
fn overwrite_replaces_the_file_and_leaves_no_temp_files() {
    let dir = TempDir::new("overwrite");
    let path = dir.file("index.fxs");
    save_rich(&path, false);
    assert!(!open_ok(&path).clean_shutdown());
    save_rich(&path, true);
    assert!(open_ok(&path).clean_shutdown());
    assert_eq!(dir.entries(), vec!["index.fxs".to_string()]);
}

#[test]
fn columns_are_aligned_and_inside_the_file() {
    let dir = TempDir::new("layout");
    let path = dir.file("index.fxs");
    save_rich(&path, true);
    let len = fs::metadata(&path).expect("metadata").len() as usize;
    let m = open_ok(&path);
    let header = m.header_len();
    assert!(header > VERSION_OFFSET + 4 && header <= len);
    let mut ranges: Vec<_> = m.column_ranges().into_iter().map(|(_, r)| r).collect();
    // 12 IndexParts columns plus two orders.
    assert!(
        ranges.len() >= 14,
        "every column is listed: {}",
        ranges.len()
    );
    ranges.sort_by_key(|r| r.start);
    let mut prev_end = header;
    for r in &ranges {
        assert_eq!(r.start % 64, 0, "column at {} is 64-byte aligned", r.start);
        assert!(
            r.start >= prev_end,
            "columns do not overlap the header or each other"
        );
        assert!(r.end <= len, "column inside the file");
        prev_end = r.end;
    }
    let bytes = fs::read(&path).expect("read");
    assert_eq!(&bytes[..8], &MAGIC);
    assert_eq!(&MAGIC, b"FXSTORE1");
    const { assert!(FORMAT_VERSION >= 2) };
    let v = u32::from_le_bytes(
        bytes[VERSION_OFFSET..VERSION_OFFSET + 4]
            .try_into()
            .unwrap(),
    );
    assert_eq!(v, FORMAT_VERSION);
}

// ---------------------------------------------------------------------------
// Rejections
// ---------------------------------------------------------------------------

#[test]
fn missing_file_is_not_found() {
    let dir = TempDir::new("missing");
    let err = MappedIndex::open(&dir.file("nope.fxs")).expect_err("must fail");
    assert!(matches!(err, LoadError::NotFound { .. }), "{err:?}");
}

#[test]
fn prototype_v1_header_is_unsupported_version() {
    let dir = TempDir::new("v1");
    let path = dir.file("index.fxs");
    // A version 1 prototype file: magic, version 1, then zeros.
    let mut bytes = vec![0u8; 4096];
    bytes[..8].copy_from_slice(b"FXSTORE1");
    bytes[8..12].copy_from_slice(&1u32.to_le_bytes());
    fs::write(&path, &bytes).expect("write");
    let err = MappedIndex::open(&path).expect_err("v1 must fail");
    assert!(
        matches!(err, LoadError::UnsupportedVersion { .. }),
        "{err:?}"
    );

    // A v2 file relabelled as v1 or as a future version.
    let good = dir.file("good.fxs");
    save_rich(&good, true);
    let orig = fs::read(&good).expect("read");
    for version in [1u32, 0, FORMAT_VERSION + 1, u32::MAX] {
        let mut b = orig.clone();
        b[VERSION_OFFSET..VERSION_OFFSET + 4].copy_from_slice(&version.to_le_bytes());
        fs::write(&path, &b).expect("write");
        let err = MappedIndex::open(&path).expect_err("must fail");
        assert!(
            matches!(err, LoadError::UnsupportedVersion { .. }),
            "v{version}: {err:?}"
        );
    }
}

#[test]
fn bad_magic_and_wrong_endianness_are_rejected() {
    let dir = TempDir::new("magic");
    let good = dir.file("good.fxs");
    save_rich(&good, true);
    let orig = fs::read(&good).expect("read");
    let path = dir.file("bad.fxs");

    for at in 0..8 {
        let mut b = orig.clone();
        b[at] ^= 0x20;
        fs::write(&path, &b).expect("write");
        let err = MappedIndex::open(&path).expect_err("must fail");
        assert!(
            matches!(err, LoadError::BadMagic { .. }),
            "byte {at}: {err:?}"
        );
    }
    let mut b = orig.clone();
    b[..8].copy_from_slice(b"NOTANIDX");
    fs::write(&path, &b).expect("write");
    let err = MappedIndex::open(&path).expect_err("must fail");
    assert!(matches!(err, LoadError::BadMagic { .. }), "{err:?}");

    let mut b = orig.clone();
    b[ENDIAN_OFFSET..ENDIAN_OFFSET + 4].copy_from_slice(&ENDIAN_MARKER.swap_bytes().to_le_bytes());
    fs::write(&path, &b).expect("write");
    let err = MappedIndex::open(&path).expect_err("must fail");
    assert!(matches!(err, LoadError::WrongEndianness { .. }), "{err:?}");
}

#[test]
fn truncation_at_any_length_is_rejected() {
    let dir = TempDir::new("trunc");
    let good = dir.file("good.fxs");
    save_rich(&good, true);
    let orig = fs::read(&good).expect("read");
    let m = open_ok(&good);
    let data_end = m
        .column_ranges()
        .iter()
        .map(|(_, r)| r.end)
        .max()
        .unwrap_or(0);
    let header = m.header_len();
    drop(m);
    assert!(data_end > header);

    let path = dir.file("cut.fxs");
    let mut lengths: Vec<usize> = (0..header.min(80)).collect();
    lengths.extend([
        header - 1,
        header,
        header + 1,
        data_end / 2,
        data_end - 64,
        data_end - 1,
    ]);
    lengths.extend((header..data_end).step_by(97));
    for len in lengths {
        fs::write(&path, &orig[..len]).expect("write");
        let err = MappedIndex::open(&path).err();
        let err = err.unwrap_or_else(|| panic!("length {len} must fail"));
        assert!(
            matches!(err, LoadError::Truncated { .. }),
            "length {len}: {err:?}"
        );
    }
}

#[test]
fn flipped_byte_in_the_header_or_tables_is_rejected() {
    let dir = TempDir::new("hdrflip");
    let good = dir.file("good.fxs");
    save_rich(&good, true);
    let orig = fs::read(&good).expect("read");
    let header = open_ok(&good).header_len();
    let path = dir.file("bad.fxs");

    for at in 0..header {
        let mut b = orig.clone();
        b[at] ^= 0x01;
        fs::write(&path, &b).expect("write");
        let err = MappedIndex::open(&path)
            .err()
            .unwrap_or_else(|| panic!("flip at header byte {at} must fail open"));
        assert!(header_error(&err), "byte {at}: {err:?}");
        if (VERSION_OFFSET + 4..header).contains(&at)
            && !(ENDIAN_OFFSET..ENDIAN_OFFSET + 4).contains(&at)
        {
            assert!(
                matches!(
                    err,
                    LoadError::HeaderChecksum { .. }
                        | LoadError::Truncated { .. }
                        | LoadError::Malformed { .. }
                ),
                "byte {at}: {err:?}"
            );
        }
    }
}

#[test]
fn flipped_byte_in_each_column_is_a_column_checksum_error() {
    let dir = TempDir::new("colflip");
    let good = dir.file("good.fxs");
    save_rich(&good, true);
    let orig = fs::read(&good).expect("read");
    let columns = open_ok(&good).column_ranges();
    let path = dir.file("bad.fxs");

    let mut checked = 0;
    for (column, range) in columns {
        if range.is_empty() {
            continue;
        }
        let picks = [range.start, range.start + range.len() / 2, range.end - 1];
        for at in picks {
            let mut b = orig.clone();
            b[at] ^= 0x80;
            fs::write(&path, &b).expect("write");
            let err = open_and_verify(&path)
                .err()
                .unwrap_or_else(|| panic!("flip in {column:?} at {at} must fail"));
            assert!(
                matches!(err, LoadError::ColumnChecksum { .. }),
                "{column:?}: {err:?}"
            );
            assert!(
                format!("{err:?}").contains(&format!("{column:?}")),
                "error {err:?} names column {column:?}"
            );
        }
        checked += 1;
    }
    assert!(
        checked >= 14,
        "every non-empty column was flipped: {checked}"
    );
}

// ---------------------------------------------------------------------------
// Failed writes
// ---------------------------------------------------------------------------

#[test]
fn failed_rename_removes_the_temp_file_and_keeps_the_target() {
    let dir = TempDir::new("failrename");
    // The target is a non-empty directory, so the final rename fails.
    let target = dir.file("index.fxs");
    fs::create_dir(&target).expect("mkdir");
    fs::write(target.join("keep"), b"old").expect("write");
    let before = dir.entries();

    let parts = rich_index().snapshot().to_parts();
    let input = SaveInput {
        parts: &parts,
        volumes: &[],
        orders: &[],
        clean_shutdown: true,
    };
    assert!(
        write_checkpoint(&target, &input).is_err(),
        "write must fail"
    );
    assert_eq!(dir.entries(), before, "no temp file left behind");
    assert_eq!(fs::read(target.join("keep")).expect("read"), b"old");
}

#[cfg(unix)]
#[test]
fn failed_write_in_read_only_dir_keeps_the_old_file() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("readonly");
    let path = dir.file("index.fxs");
    save_rich(&path, false);
    let old = fs::read(&path).expect("read");
    fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o555)).expect("chmod");

    let parts = Index::new().snapshot().to_parts();
    let input = SaveInput {
        parts: &parts,
        volumes: &[],
        orders: &[],
        clean_shutdown: true,
    };
    let result = write_checkpoint(&path, &input);
    fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o755)).expect("chmod back");
    if result.is_ok() {
        // Running as root: permissions are not enforced, nothing to check.
        return;
    }
    assert_eq!(fs::read(&path).expect("read"), old, "old file untouched");
    assert_eq!(dir.entries(), vec!["index.fxs".to_string()], "no temp file");
    let m = open_ok(&path);
    m.verify_checksums().expect("old file still valid");
    assert!(!m.clean_shutdown());
}
