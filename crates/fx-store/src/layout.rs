//! Byte layout of the version 2 index checkpoint file, and its checksum.
//!
//! Everything is little-endian. A file is a header (a fixed part, then the
//! volume table, then the column table), followed by the columns. Every
//! column starts on a [`COLUMN_ALIGN`]-byte boundary; the gaps are zero.
//!
//! # Fixed header ([`FIXED_HEADER_SIZE`] bytes)
//!
//! | Offset | Size | Field |
//! | --- | --- | --- |
//! | 0 | 8 | [`MAGIC`] `FXSTORE1` ([`MAGIC_OFFSET`]) |
//! | 8 | 4 | `u32` format version, [`FORMAT_VERSION`] ([`VERSION_OFFSET`]) |
//! | 12 | 4 | `u32` endianness marker [`ENDIAN_MARKER`] ([`ENDIAN_OFFSET`]) |
//! | 16 | 4 | `u32` header length: fixed part + volume table + column table ([`HEADER_LEN_OFFSET`]) |
//! | 20 | 4 | `u32` flags: bit 0 = clean shutdown ([`FLAG_CLEAN_SHUTDOWN`]); other bits written as zero, ignored on load ([`FLAGS_OFFSET`]) |
//! | 24 | 8 | `u64` id_bound: entry IDs `0..id_bound`, live or removed ([`ID_BOUND_OFFSET`]) |
//! | 32 | 8 | `u64` live entry count ([`LIVE_COUNT_OFFSET`]) |
//! | 40 | 8 | `i64` creation time, microseconds since the Unix epoch ([`CREATED_OFFSET`]) |
//! | 48 | 4 | `u32` volume record count ([`VOLUME_COUNT_OFFSET`]) |
//! | 52 | 4 | `u32` column record count ([`COLUMN_COUNT_OFFSET`]) |
//! | 56 | 8 | `u64` total file length in bytes ([`FILE_LEN_OFFSET`]) |
//! | 64 | 8 | `u64` header checksum ([`HEADER_CHECKSUM_OFFSET`]) |
//!
//! The header checksum is [`checksum`] of bytes `0..header_len` with the
//! checksum field itself read as zero, so it covers the fixed part, the
//! volume table and the column table.
//!
//! Files of the early prototype format carry version 1 at the same offset,
//! so they are reported as an unsupported version.
//!
//! # Volume table ([`VOLUME_RECORD_SIZE`] bytes per volume)
//!
//! Starts at [`FIXED_HEADER_SIZE`]; one record per volume, in save order,
//! volume IDs unique.
//!
//! | Offset in record | Size | Field |
//! | --- | --- | --- |
//! | 0 | 4 | `u32` volume ID (`fx_model::VolumeId`) |
//! | 4 | 4 | `u32` cursor tag: [`CURSOR_FSEVENTS`], [`CURSOR_USN`] or [`CURSOR_NONE`] |
//! | 8 | 8 | `u64` FSEvents `event_id`, USN `journal_id`, or None `snapshot_epoch` |
//! | 16 | 16 | FSEvents `db_uuid`; USN: `i64` `next_usn` then 8 zero bytes; None: zero |
//!
//! Unused bytes must be zero.
//!
//! # Column table ([`COLUMN_RECORD_SIZE`] bytes per column)
//!
//! Follows the volume table. The 12 `IndexParts` columns come first in
//! kind order, then zero or more saved orders in save order.
//!
//! | Offset in record | Size | Field |
//! | --- | --- | --- |
//! | 0 | 2 | `u16` column kind ([`KIND_NAME_BYTES`] ..= [`KIND_ORDER`]) |
//! | 2 | 1 | `u8` sort key code for [`KIND_ORDER`] records, zero otherwise |
//! | 3 | 1 | `u8` element size in bytes (must match the kind) |
//! | 4 | 4 | reserved, zero |
//! | 8 | 8 | `u64` byte offset of the column in the file (multiple of 64) |
//! | 16 | 8 | `u64` byte length (multiple of the element size) |
//! | 24 | 8 | `u64` [`checksum`] of the column bytes |
//!
//! Columns (all raw little-endian arrays, as in `fx_index::IndexParts`):
//!
//! | Kind | Column | Element | Length |
//! | --- | --- | --- | --- |
//! | 1 | name_bytes: names joined by single NULs, no trailing NUL | `u8` | any; the byte right after it is always a zero pad byte |
//! | 2 | name_offsets | `u32` | id_bound |
//! | 3 | parents (`u32::MAX` for a root) | `u32` | id_bound |
//! | 4 | volumes | `u32` | id_bound |
//! | 5 | file_ids | `u64` | id_bound |
//! | 6 | meta (packed size, kind, flags) | `u64` | id_bound |
//! | 7 | modified | `i64` | id_bound |
//! | 8 | created | `i64` | id_bound |
//! | 9 | ext_ids | `u16` | id_bound |
//! | 10 | ext_bytes | `u8` | any |
//! | 11 | ext_offsets | `u32` | any |
//! | 12 | live bitmap | `u64` | at least `id_bound.div_ceil(64)` |
//! | 13 | saved order: a `u32` permutation, tagged with its key code | `u32` | live count |
//!
//! Kinds 1 to 12 appear exactly once; order key codes are unique. Columns
//! must lie after the header, inside the file and must not overlap. The
//! file ends at the last column rounded up to 64 bytes.
//!
//! # Saved order contract
//!
//! A saved order (kind 13) is a permutation of the live entry IDs, as
//! `fx_sort::Order` holds:
//!
//! - its length is the header's live count;
//! - every element is below id_bound and is a live entry (its bit is set in
//!   the live bitmap);
//! - no element appears twice.
//!
//! The writer rejects any violation with
//! `StoreError::InvalidOrder`. On load, `MappedIndex::open` checks only the
//! length (O(1)); `MappedIndex::verify_checksums` also checks the range,
//! liveness and uniqueness of every element. Both report
//! `LoadError::Malformed`.

/// File magic, the first 8 bytes of every file (unchanged since the
/// version 1 prototype).
pub const MAGIC: [u8; 8] = *b"FXSTORE1";
/// The format version this build writes and reads. Version 1 was an early
/// prototype's.
pub const FORMAT_VERSION: u32 = 2;
/// Written little-endian at [`ENDIAN_OFFSET`]; reads back byte-swapped on a
/// file produced by a big-endian writer.
pub const ENDIAN_MARKER: u32 = 0x0A0B_0C0D;
/// Alignment of every column, in bytes.
pub const COLUMN_ALIGN: usize = 64;

/// Offset of [`MAGIC`].
pub const MAGIC_OFFSET: usize = 0;
/// Offset of the `u32` format version.
pub const VERSION_OFFSET: usize = 8;
/// Offset of the `u32` endianness marker.
pub const ENDIAN_OFFSET: usize = 12;
/// Offset of the `u32` header length.
pub const HEADER_LEN_OFFSET: usize = 16;
/// Offset of the `u32` flags.
pub const FLAGS_OFFSET: usize = 20;
/// Offset of the `u64` id_bound.
pub const ID_BOUND_OFFSET: usize = 24;
/// Offset of the `u64` live count.
pub const LIVE_COUNT_OFFSET: usize = 32;
/// Offset of the `i64` creation time in microseconds.
pub const CREATED_OFFSET: usize = 40;
/// Offset of the `u32` volume record count.
pub const VOLUME_COUNT_OFFSET: usize = 48;
/// Offset of the `u32` column record count.
pub const COLUMN_COUNT_OFFSET: usize = 52;
/// Offset of the `u64` total file length.
pub const FILE_LEN_OFFSET: usize = 56;
/// Offset of the `u64` header checksum.
pub const HEADER_CHECKSUM_OFFSET: usize = 64;
/// Size of the fixed part of the header; the volume table starts here.
pub const FIXED_HEADER_SIZE: usize = 72;

/// Flags bit 0: the file was written by a clean shutdown.
pub const FLAG_CLEAN_SHUTDOWN: u32 = 1;

/// Size of one volume record.
pub const VOLUME_RECORD_SIZE: usize = 32;
/// Volume record: offset of the `u32` volume ID.
pub const VOLUME_ID_FIELD: usize = 0;
/// Volume record: offset of the `u32` cursor tag.
pub const VOLUME_TAG_FIELD: usize = 4;
/// Volume record: offset of the first `u64` cursor value.
pub const VOLUME_A_FIELD: usize = 8;
/// Volume record: offset of the 16-byte second cursor value.
pub const VOLUME_B_FIELD: usize = 16;
/// Cursor tag of `ResumeCursor::FsEvents`.
pub const CURSOR_FSEVENTS: u32 = 1;
/// Cursor tag of `ResumeCursor::Usn`.
pub const CURSOR_USN: u32 = 2;
/// Cursor tag of `ResumeCursor::None`.
pub const CURSOR_NONE: u32 = 3;

/// Size of one column record.
pub const COLUMN_RECORD_SIZE: usize = 32;
/// Column record: offset of the `u16` kind.
pub const COLUMN_KIND_FIELD: usize = 0;
/// Column record: offset of the `u8` order key code.
pub const COLUMN_KEY_FIELD: usize = 2;
/// Column record: offset of the `u8` element size.
pub const COLUMN_ELEM_FIELD: usize = 3;
/// Column record: offset of the reserved `u32`.
pub const COLUMN_RESERVED_FIELD: usize = 4;
/// Column record: offset of the `u64` byte offset.
pub const COLUMN_OFFSET_FIELD: usize = 8;
/// Column record: offset of the `u64` byte length.
pub const COLUMN_LEN_FIELD: usize = 16;
/// Column record: offset of the `u64` checksum.
pub const COLUMN_CHECKSUM_FIELD: usize = 24;

/// Column kind: name bytes.
pub const KIND_NAME_BYTES: u16 = 1;
/// Column kind: name offsets.
pub const KIND_NAME_OFFSETS: u16 = 2;
/// Column kind: parent IDs.
pub const KIND_PARENTS: u16 = 3;
/// Column kind: volume IDs.
pub const KIND_VOLUMES: u16 = 4;
/// Column kind: file IDs.
pub const KIND_FILE_IDS: u16 = 5;
/// Column kind: packed metadata.
pub const KIND_META: u16 = 6;
/// Column kind: modification times.
pub const KIND_MODIFIED: u16 = 7;
/// Column kind: creation times.
pub const KIND_CREATED: u16 = 8;
/// Column kind: extension IDs.
pub const KIND_EXT_IDS: u16 = 9;
/// Column kind: extension table bytes.
pub const KIND_EXT_BYTES: u16 = 10;
/// Column kind: extension table offsets.
pub const KIND_EXT_OFFSETS: u16 = 11;
/// Column kind: live bitmap.
pub const KIND_LIVE: u16 = 12;
/// Column kind: a saved order.
pub const KIND_ORDER: u16 = 13;
/// Number of required (non-order) columns.
pub const REQUIRED_COLUMNS: usize = 12;

/// Largest header length accepted on load (about 32k volumes and columns);
/// anything larger is malformed.
pub const MAX_HEADER_LEN: usize = 1 << 20;

/// FNV-1a 64-bit offset basis (public domain).
const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
/// FNV-1a 64-bit prime (public domain).
const FNV_PRIME: u64 = 0x100000001b3;
/// Number of independent lanes.
const LANES: usize = 4;
/// Bytes consumed per round: one 8-byte word per lane.
const STRIDE: usize = LANES * 8;

/// Rotation applied after each multiply. A multiply only carries bit
/// changes upward, so without it a change in bit 63 never reaches a lower
/// bit and two bit-63 flips in the same lane would cancel.
const STEP_ROTATE: u32 = 31;

/// One FNV-1a-style step on a whole 64-bit word, followed by a rotation:
/// `rotl((h ^ word) * prime, 31)`. For a fixed state this is a bijection of
/// `word`, and for a fixed `word` a bijection of the state (the prime is
/// odd and rotation is a permutation), so a change to any single word is
/// never lost. The rotation feeds high bits back into the low bits, so the
/// next multiply spreads them again.
#[inline(always)]
fn step(h: u64, word: u64) -> u64 {
    (h ^ word).wrapping_mul(FNV_PRIME).rotate_left(STEP_ROTATE)
}

/// The SplitMix64 finaliser (Steele, Lea, Flood, "Fast Splittable
/// Pseudorandom Number Generators", 2014; public domain). A bijection with
/// full avalanche, so every input bit affects every output bit.
#[inline(always)]
fn splitmix64_finalize(mut z: u64) -> u64 {
    z ^= z >> 30;
    z = z.wrapping_mul(0xbf58476d1ce4e5b9);
    z ^= z >> 27;
    z = z.wrapping_mul(0x94d049bb133111eb);
    z ^= z >> 31;
    z
}

/// Reads the little-endian `u64` at the start of `bytes` (at least 8 long).
#[inline(always)]
fn le_word(bytes: &[u8]) -> u64 {
    let mut w = [0u8; 8];
    w.copy_from_slice(&bytes[..8]);
    u64::from_le_bytes(w)
}

/// The file checksum: a std-only, word-at-a-time 64-bit hash derived from
/// the public-domain FNV-1a algorithm (Fowler/Noll/Vo,
/// <http://www.isthe.com/chongo/tech/comp/fnv/>), using its 64-bit offset
/// basis `0xcbf29ce484222325` and prime `0x100000001b3`.
///
/// The input is read as little-endian `u64` words spread round-robin over
/// four independent lanes, so the multiplies of neighbouring words overlap.
/// Lane `i` starts from the offset basis XORed with `i`, and each word is
/// mixed in with the step `h = rotl((h ^ word) * prime, 31)`: the FNV-1a
/// step plus a rotation, so changes in high bits are fed back down and two
/// bit-63 flips cannot cancel. The tail bytes (zero-padded to a word) and
/// then the total length are folded into lane 0, the four lane values are
/// combined with the same step starting from the offset basis, and the
/// result goes through the SplitMix64 finaliser (Steele, Lea, Flood,
/// "Fast Splittable Pseudorandom Number Generators", 2014; public domain).
///
/// It is **not** compatible with FNV-1a over bytes: it works on whole
/// words, uses lanes, rotates and finalises, so it gives different values.
///
/// It is for corruption detection only (torn writes, bit rot,
/// truncation). It is **not** cryptographic and **not** tamper-proof:
/// anyone can forge data with a matching value.
pub fn checksum(data: &[u8]) -> u64 {
    let mut lanes = [0u64; LANES];
    for (i, lane) in lanes.iter_mut().enumerate() {
        *lane = FNV_OFFSET_BASIS ^ i as u64;
    }

    let mut blocks = data.chunks_exact(STRIDE);
    for block in &mut blocks {
        lanes[0] = step(lanes[0], le_word(&block[0..]));
        lanes[1] = step(lanes[1], le_word(&block[8..]));
        lanes[2] = step(lanes[2], le_word(&block[16..]));
        lanes[3] = step(lanes[3], le_word(&block[24..]));
    }

    // Up to three whole words left: they continue the round-robin.
    let mut words = blocks.remainder().chunks_exact(8);
    for (i, w) in (&mut words).enumerate() {
        lanes[i] = step(lanes[i], le_word(w));
    }

    let tail = words.remainder();
    if !tail.is_empty() {
        let mut w = [0u8; 8];
        w[..tail.len()].copy_from_slice(tail);
        lanes[0] = step(lanes[0], u64::from_le_bytes(w));
    }
    // The length tells zero-padding apart from real zero bytes.
    lanes[0] = step(lanes[0], data.len() as u64);

    let combined = lanes
        .iter()
        .fold(FNV_OFFSET_BASIS, |h, &lane| step(h, lane));
    splitmix64_finalize(combined)
}

/// The header checksum: [`checksum`] of `header` (bytes `0..header_len`)
/// with the 8 bytes at [`HEADER_CHECKSUM_OFFSET`] read as zero.
///
/// `header` must be at least [`FIXED_HEADER_SIZE`] bytes long.
pub fn header_checksum(header: &[u8]) -> u64 {
    let mut fixed = [0u8; FIXED_HEADER_SIZE];
    fixed.copy_from_slice(&header[..FIXED_HEADER_SIZE]);
    fixed[HEADER_CHECKSUM_OFFSET..HEADER_CHECKSUM_OFFSET + 8].fill(0);
    // Hash the fixed part and the tables separately, then fold both, so
    // the header never needs to be copied whole.
    let a = checksum(&fixed);
    let b = checksum(&header[FIXED_HEADER_SIZE..]);
    splitmix64_finalize(step(step(FNV_OFFSET_BASIS, a), b))
}

/// Checks a saved order against the order contract (see the module docs):
/// `live_count` elements, each below `id_bound`, live per `is_live`, and
/// no duplicates. O(n) time, `id_bound / 8` bytes of scratch.
pub(crate) fn check_order(
    order: &[u32],
    id_bound: usize,
    live_count: usize,
    is_live: impl Fn(usize) -> bool,
) -> Result<(), crate::error::OrderProblem> {
    use crate::error::OrderProblem;
    if order.len() != live_count {
        return Err(OrderProblem::Length {
            expected: live_count,
            found: order.len(),
        });
    }
    let mut seen = vec![0u64; id_bound.div_ceil(64)];
    for &id in order {
        let i = id as usize;
        if i >= id_bound {
            return Err(OrderProblem::OutOfRange { id, id_bound });
        }
        if !is_live(i) {
            return Err(OrderProblem::DeadId { id });
        }
        let (word, bit) = (i / 64, 1u64 << (i % 64));
        if seen[word] & bit != 0 {
            return Err(OrderProblem::DuplicateId { id });
        }
        seen[word] |= bit;
    }
    Ok(())
}

/// Rounds `n` up to a multiple of [`COLUMN_ALIGN`].
#[inline]
pub fn align_up(n: u64) -> u64 {
    n.div_ceil(COLUMN_ALIGN as u64) * COLUMN_ALIGN as u64
}

// Layout sanity, checked at compile time.
const _: () = assert!(HEADER_CHECKSUM_OFFSET + 8 == FIXED_HEADER_SIZE);
const _: () = assert!(FIXED_HEADER_SIZE % 8 == 0);
const _: () = assert!(VOLUME_B_FIELD + 16 == VOLUME_RECORD_SIZE);
const _: () = assert!(COLUMN_CHECKSUM_FIELD + 8 == COLUMN_RECORD_SIZE);
