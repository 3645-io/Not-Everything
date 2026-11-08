//! The zero-copy mmap loader. The byte layout is in [`crate::layout`].

use std::collections::HashSet;
use std::fs::File;
use std::io;
use std::ops::Range;
use std::path::Path;

use fx_index::{Index, IndexError, IndexParts};
use fx_model::{ResumeCursor, VolumeId};
use memmap2::Mmap;

use crate::error::{Column, LoadError};
use crate::layout::{
    COLUMN_ALIGN, COLUMN_CHECKSUM_FIELD, COLUMN_COUNT_OFFSET, COLUMN_ELEM_FIELD, COLUMN_KEY_FIELD,
    COLUMN_KIND_FIELD, COLUMN_LEN_FIELD, COLUMN_OFFSET_FIELD, COLUMN_RECORD_SIZE,
    COLUMN_RESERVED_FIELD, CREATED_OFFSET, CURSOR_FSEVENTS, CURSOR_NONE, CURSOR_USN, ENDIAN_MARKER,
    ENDIAN_OFFSET, FILE_LEN_OFFSET, FIXED_HEADER_SIZE, FLAG_CLEAN_SHUTDOWN, FLAGS_OFFSET,
    FORMAT_VERSION, HEADER_CHECKSUM_OFFSET, HEADER_LEN_OFFSET, ID_BOUND_OFFSET, LIVE_COUNT_OFFSET,
    MAGIC, MAGIC_OFFSET, MAX_HEADER_LEN, REQUIRED_COLUMNS, VERSION_OFFSET, VOLUME_A_FIELD,
    VOLUME_B_FIELD, VOLUME_COUNT_OFFSET, VOLUME_ID_FIELD, VOLUME_RECORD_SIZE, VOLUME_TAG_FIELD,
    check_order, checksum, header_checksum,
};

fn get_u16(buf: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([buf[at], buf[at + 1]])
}

fn get_u32(buf: &[u8], at: usize) -> u32 {
    let mut b = [0u8; 4];
    b.copy_from_slice(&buf[at..at + 4]);
    u32::from_le_bytes(b)
}

fn get_u64(buf: &[u8], at: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&buf[at..at + 8]);
    u64::from_le_bytes(b)
}

fn truncated(len: usize, needed: usize) -> LoadError {
    LoadError::Truncated {
        len: len as u64,
        needed: needed as u64,
    }
}

/// One validated column inside the mapping.
#[derive(Clone, Debug)]
struct Stored {
    column: Column,
    range: Range<usize>,
    checksum: u64,
}

/// A read-only, zero-copy view of a checkpoint file.
///
/// [`open`](Self::open) validates only the header and the tables, so it
/// costs O(1) in the data size; pages are read lazily on first access.
/// Column contents are not checked: offsets and parent IDs come straight
/// from the file, so consumers must bounds-check them, or call
/// [`verify_checksums`](Self::verify_checksums) first. [`to_index`](Self::to_index)
/// validates everything.
#[derive(Debug)]
pub struct MappedIndex {
    map: Mmap,
    header_len: usize,
    id_bound: usize,
    live_count: usize,
    clean_shutdown: bool,
    created_micros: i64,
    cursors: Vec<(VolumeId, ResumeCursor)>,
    /// Indices into `columns` of the required columns, in [`Column::REQUIRED`] order.
    required: [usize; REQUIRED_COLUMNS],
    /// Every column in table order (required ones first, then orders).
    columns: Vec<Stored>,
}

impl MappedIndex {
    /// Maps and validates the file at `path`.
    ///
    /// Checks magic, version, endianness, header length, header checksum,
    /// then the fixed fields, the volume table and every column record
    /// (known kind, element size, alignment, bounds, no overlap, required
    /// columns present once, lengths consistent with `id_bound`). Column
    /// checksums are not verified here; see
    /// [`verify_checksums`](Self::verify_checksums).
    pub fn open(path: impl AsRef<Path>) -> Result<MappedIndex, LoadError> {
        let path = path.as_ref();
        let file = File::open(path).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => LoadError::NotFound {
                path: path.to_path_buf(),
            },
            _ => LoadError::Io { source: e },
        })?;
        let len = file.metadata()?.len();
        if len < (VERSION_OFFSET + 4) as u64 {
            // Also avoids mapping an empty file, which mmap rejects.
            let mut prefix = vec![0u8; len as usize];
            io::Read::read_exact(&mut &file, &mut prefix)?;
            let n = prefix.len().min(8);
            if !MAGIC.starts_with(&prefix[..n]) {
                let mut found = [0u8; 8];
                found[..n].copy_from_slice(&prefix[..n]);
                return Err(LoadError::BadMagic { found });
            }
            return Err(truncated(len as usize, FIXED_HEADER_SIZE));
        }
        // SAFETY: the mapping is read-only and private to this process. Our
        // writer never modifies a file in place: it writes a new file and
        // renames it over the old path, so the inode behind this mapping is
        // never changed by fx-store. Another process truncating or writing
        // the file could still change the bytes under us; that is outside
        // the supported use, and every access is bounds-checked against the
        // mapping length taken here.
        let map = unsafe { Mmap::map(&file)? };
        Self::from_map(map)
    }

    fn from_map(map: Mmap) -> Result<MappedIndex, LoadError> {
        let len = map.len();
        let b = &map[..];
        let mut magic = [0u8; 8];
        magic.copy_from_slice(&b[MAGIC_OFFSET..MAGIC_OFFSET + 8]);
        if magic != MAGIC {
            return Err(LoadError::BadMagic { found: magic });
        }
        let version = get_u32(b, VERSION_OFFSET);
        if version != FORMAT_VERSION {
            return Err(LoadError::UnsupportedVersion { found: version });
        }
        if len < ENDIAN_OFFSET + 4 {
            return Err(truncated(len, FIXED_HEADER_SIZE));
        }
        let marker = get_u32(b, ENDIAN_OFFSET);
        if cfg!(target_endian = "big") || marker == ENDIAN_MARKER.swap_bytes() {
            return Err(LoadError::WrongEndianness { found: marker });
        }
        if marker != ENDIAN_MARKER {
            return Err(LoadError::malformed(format!(
                "endianness marker {marker:#010x} is not {ENDIAN_MARKER:#010x}"
            )));
        }
        if len < FIXED_HEADER_SIZE {
            return Err(truncated(len, FIXED_HEADER_SIZE));
        }
        let header_len = get_u32(b, HEADER_LEN_OFFSET) as usize;
        if !(FIXED_HEADER_SIZE..=MAX_HEADER_LEN).contains(&header_len) {
            return Err(LoadError::malformed(format!(
                "header length {header_len} out of range"
            )));
        }
        if len < header_len {
            return Err(truncated(len, header_len));
        }
        let h = &b[..header_len];
        let stored = get_u64(h, HEADER_CHECKSUM_OFFSET);
        let computed = header_checksum(h);
        if stored != computed {
            return Err(LoadError::HeaderChecksum { stored, computed });
        }

        // The header is intact from here on: remaining failures are
        // inconsistencies of a writer, or a file cut short.
        let volume_count = get_u32(h, VOLUME_COUNT_OFFSET) as usize;
        let column_count = get_u32(h, COLUMN_COUNT_OFFSET) as usize;
        let vtable = FIXED_HEADER_SIZE;
        let ctable = vtable + volume_count * VOLUME_RECORD_SIZE;
        if ctable + column_count * COLUMN_RECORD_SIZE != header_len {
            return Err(LoadError::malformed(
                "header length disagrees with the table sizes",
            ));
        }
        let file_len = get_u64(h, FILE_LEN_OFFSET);
        if (len as u64) < file_len {
            return Err(LoadError::Truncated {
                len: len as u64,
                needed: file_len,
            });
        }
        if len as u64 > file_len {
            return Err(LoadError::malformed(format!(
                "file is {len} bytes, header says {file_len}"
            )));
        }
        let id_bound = get_u64(h, ID_BOUND_OFFSET);
        if id_bound > u64::from(u32::MAX - 1) {
            return Err(LoadError::malformed(format!(
                "id_bound {id_bound} exceeds the u32 entry ID range"
            )));
        }
        let id_bound = id_bound as usize;
        let live_count = get_u64(h, LIVE_COUNT_OFFSET);
        if live_count > id_bound as u64 {
            return Err(LoadError::malformed("live count exceeds id_bound"));
        }
        let live_count = live_count as usize;

        let cursors = decode_volumes(&h[vtable..ctable])?;
        let (columns, required) =
            decode_columns(&map, &h[ctable..], header_len, id_bound, live_count)?;

        Ok(MappedIndex {
            header_len,
            id_bound,
            live_count,
            clean_shutdown: get_u32(h, FLAGS_OFFSET) & FLAG_CLEAN_SHUTDOWN != 0,
            created_micros: get_u64(h, CREATED_OFFSET) as i64,
            cursors,
            required,
            columns,
            map,
        })
    }

    fn bytes_at(&self, index: usize) -> &[u8] {
        &self.map[self.columns[index].range.clone()]
    }

    fn bytes(&self, column: Column) -> &[u8] {
        // `required_index` is `Some` for every column passed here.
        let slot = column.required_index().unwrap_or(0);
        self.bytes_at(self.required[slot])
    }

    /// Number of entry IDs (live or removed): the per-entry column length.
    pub fn id_bound(&self) -> usize {
        self.id_bound
    }

    /// Number of live entries, from the header.
    pub fn live_count(&self) -> usize {
        self.live_count
    }

    /// Whether the file was written by a clean shutdown.
    pub fn clean_shutdown(&self) -> bool {
        self.clean_shutdown
    }

    /// When the file was written, in microseconds since the Unix epoch.
    pub fn created_micros(&self) -> i64 {
        self.created_micros
    }

    /// Size of the mapped file in bytes.
    pub fn file_len(&self) -> u64 {
        self.map.len() as u64
    }

    /// Bytes covered by the header checksum: the fixed header, the volume
    /// table and the column table.
    pub fn header_len(&self) -> usize {
        self.header_len
    }

    /// The volume table: each volume's resume cursor, in save order.
    pub fn cursors(&self) -> &[(VolumeId, ResumeCursor)] {
        &self.cursors
    }

    /// The resume cursor saved for `volume`, if any.
    pub fn cursor(&self, volume: VolumeId) -> Option<ResumeCursor> {
        self.cursors
            .iter()
            .find(|(v, _)| *v == volume)
            .map(|&(_, c)| c)
    }

    /// The saved orders with their key codes, in save order. The same
    /// guarantees as [`order`](Self::order) apply.
    pub fn orders(&self) -> Vec<(u8, &[u32])> {
        (REQUIRED_COLUMNS..self.columns.len())
            .map(|i| {
                (
                    self.columns[i].column.key_code(),
                    bytemuck::cast_slice(self.bytes_at(i)),
                )
            })
            .collect()
    }

    /// The saved order with key code `key`.
    ///
    /// [`open`](Self::open) (`VerifyMode::HeaderOnly`) guarantees only the
    /// length: exactly [`live_count`](Self::live_count) elements. The values
    /// themselves are not read, so they may be out of range, removed or
    /// repeated in a corrupt file; bounds-check them before indexing. After
    /// [`verify_checksums`](Self::verify_checksums) (`VerifyMode::Full`)
    /// succeeds, the order is a permutation of the live entry IDs.
    pub fn order(&self, key: u8) -> Option<&[u32]> {
        (REQUIRED_COLUMNS..self.columns.len())
            .find(|&i| self.columns[i].column == Column::Order(key))
            .map(|i| bytemuck::cast_slice(self.bytes_at(i)))
    }

    /// Every stored column with its byte range in the file, in table order.
    pub fn column_ranges(&self) -> Vec<(Column, Range<usize>)> {
        self.columns
            .iter()
            .map(|s| (s.column, s.range.clone()))
            .collect()
    }

    /// Names joined by single NULs, no trailing NUL. UTF-8 is not checked.
    pub fn name_bytes(&self) -> &[u8] {
        self.bytes(Column::NameBytes)
    }

    /// [`name_bytes`](Self::name_bytes) plus the zero pad byte that always
    /// follows it in the file, so every name, the last included, is
    /// NUL-terminated (what `fx_query::substring::NameColumn` expects).
    pub fn name_bytes_nul(&self) -> &[u8] {
        let r = &self.columns[self.required[0]].range;
        &self.map[r.start..r.end + 1]
    }

    /// Start of each entry's name in [`name_bytes`](Self::name_bytes).
    pub fn name_offsets(&self) -> &[u32] {
        bytemuck::cast_slice(self.bytes(Column::NameOffsets))
    }

    /// Parent entry ID per entry, `u32::MAX` for a root.
    pub fn parents(&self) -> &[u32] {
        bytemuck::cast_slice(self.bytes(Column::Parents))
    }

    /// Volume ID per entry.
    pub fn volumes(&self) -> &[u32] {
        bytemuck::cast_slice(self.bytes(Column::Volumes))
    }

    /// File ID per entry.
    pub fn file_ids(&self) -> &[u64] {
        bytemuck::cast_slice(self.bytes(Column::FileIds))
    }

    /// Packed size, kind and flags per entry.
    pub fn meta(&self) -> &[u64] {
        bytemuck::cast_slice(self.bytes(Column::Meta))
    }

    /// Modification time per entry, seconds since the Unix epoch.
    pub fn modified(&self) -> &[i64] {
        bytemuck::cast_slice(self.bytes(Column::Modified))
    }

    /// Creation time per entry, seconds since the Unix epoch.
    pub fn created(&self) -> &[i64] {
        bytemuck::cast_slice(self.bytes(Column::Created))
    }

    /// Extension ID per entry.
    pub fn ext_ids(&self) -> &[u16] {
        bytemuck::cast_slice(self.bytes(Column::ExtIds))
    }

    /// The extension table's bytes.
    pub fn ext_bytes(&self) -> &[u8] {
        self.bytes(Column::ExtBytes)
    }

    /// The extension table's offsets.
    pub fn ext_offsets(&self) -> &[u32] {
        bytemuck::cast_slice(self.bytes(Column::ExtOffsets))
    }

    /// The live bitmap: bit `i % 64` of word `i / 64` is set when entry `i`
    /// is live.
    pub fn live(&self) -> &[u64] {
        bytemuck::cast_slice(self.bytes(Column::Live))
    }

    /// Checks every column against its checksum (reads the whole file),
    /// then that the live bitmap agrees with the header's live count, then
    /// that every saved order is a permutation of the live IDs (each
    /// element below `id_bound`, live, and unique; see the order contract in
    /// [`crate::layout`]). Order violations are [`LoadError::Malformed`].
    pub fn verify_checksums(&self) -> Result<(), LoadError> {
        for (i, s) in self.columns.iter().enumerate() {
            if checksum(self.bytes_at(i)) != s.checksum {
                return Err(LoadError::ColumnChecksum { column: s.column });
            }
        }
        let n = self.id_bound;
        let words = &self.live()[..n.div_ceil(64)];
        let live: usize = words
            .iter()
            .enumerate()
            .map(|(w, &bits)| {
                // Bits at or past id_bound do not count.
                let valid = (n - w * 64).min(64);
                let mask = if valid == 64 {
                    u64::MAX
                } else {
                    (1u64 << valid) - 1
                };
                (bits & mask).count_ones() as usize
            })
            .sum();
        if live != self.live_count {
            return Err(LoadError::malformed(format!(
                "live bitmap has {live} entries, header says {}",
                self.live_count
            )));
        }
        let bitmap = self.live();
        for (key, order) in self.orders() {
            check_order(order, n, self.live_count, |i| {
                (bitmap[i / 64] >> (i % 64)) & 1 == 1
            })
            .map_err(|problem| {
                LoadError::malformed(format!("column {} {problem}", Column::Order(key)))
            })?;
        }
        Ok(())
    }

    /// Copies every column into owned [`IndexParts`].
    pub fn to_parts(&self) -> IndexParts {
        IndexParts {
            name_bytes: self.name_bytes().to_vec(),
            name_offsets: self.name_offsets().to_vec(),
            parents: self.parents().to_vec(),
            volumes: self.volumes().to_vec(),
            file_ids: self.file_ids().to_vec(),
            meta: self.meta().to_vec(),
            modified: self.modified().to_vec(),
            created: self.created().to_vec(),
            ext_ids: self.ext_ids().to_vec(),
            ext_bytes: self.ext_bytes().to_vec(),
            ext_offsets: self.ext_offsets().to_vec(),
            live: self.live().to_vec(),
        }
    }

    /// Rebuilds an index with the same entry IDs through
    /// `Index::from_parts`, which validates every column.
    pub fn to_index(&self) -> Result<Index, IndexError> {
        Index::from_parts(self.to_parts())
    }
}

/// Decodes the volume table.
fn decode_volumes(table: &[u8]) -> Result<Vec<(VolumeId, ResumeCursor)>, LoadError> {
    let mut out = Vec::with_capacity(table.len() / VOLUME_RECORD_SIZE);
    let mut seen = HashSet::new();
    for rec in table.chunks_exact(VOLUME_RECORD_SIZE) {
        let volume = get_u32(rec, VOLUME_ID_FIELD);
        if !seen.insert(volume) {
            return Err(LoadError::malformed(format!(
                "volume {volume} appears twice"
            )));
        }
        let a = get_u64(rec, VOLUME_A_FIELD);
        let mut uuid = [0u8; 16];
        uuid.copy_from_slice(&rec[VOLUME_B_FIELD..VOLUME_B_FIELD + 16]);
        let (cursor, used) = match get_u32(rec, VOLUME_TAG_FIELD) {
            CURSOR_FSEVENTS => (
                ResumeCursor::FsEvents {
                    event_id: a,
                    db_uuid: uuid,
                },
                16,
            ),
            CURSOR_USN => (
                ResumeCursor::Usn {
                    journal_id: a,
                    next_usn: get_u64(rec, VOLUME_B_FIELD) as i64,
                },
                8,
            ),
            CURSOR_NONE => (ResumeCursor::None { snapshot_epoch: a }, 0),
            tag => {
                return Err(LoadError::malformed(format!(
                    "volume {volume} has unknown cursor tag {tag}"
                )));
            }
        };
        if uuid[used..].iter().any(|&x| x != 0) {
            return Err(LoadError::malformed(format!(
                "volume {volume} record has non-zero padding"
            )));
        }
        out.push((VolumeId(volume), cursor));
    }
    Ok(out)
}

/// Decodes and checks the column table against the mapping.
fn decode_columns(
    map: &[u8],
    table: &[u8],
    header_len: usize,
    id_bound: usize,
    live_count: usize,
) -> Result<(Vec<Stored>, [usize; REQUIRED_COLUMNS]), LoadError> {
    let file_len = map.len() as u64;
    let mut columns = Vec::with_capacity(table.len() / COLUMN_RECORD_SIZE);
    let mut required = [usize::MAX; REQUIRED_COLUMNS];
    let mut keys = HashSet::new();
    for rec in table.chunks_exact(COLUMN_RECORD_SIZE) {
        let kind = get_u16(rec, COLUMN_KIND_FIELD);
        let key = rec[COLUMN_KEY_FIELD];
        let column = Column::from_kind(kind, key)
            .ok_or_else(|| LoadError::malformed(format!("unknown column kind {kind}")))?;
        let i = columns.len();
        match column.required_index() {
            Some(slot) if required[slot] != usize::MAX => {
                return Err(LoadError::malformed(format!(
                    "column {column} appears twice"
                )));
            }
            Some(slot) => required[slot] = i,
            None if !keys.insert(key) => {
                return Err(LoadError::malformed(format!(
                    "column {column} appears twice"
                )));
            }
            None => {}
        }
        if column.required_index().is_some() && key != 0 {
            return Err(LoadError::malformed(format!(
                "column {column} has a key code"
            )));
        }
        if get_u32(rec, COLUMN_RESERVED_FIELD) != 0 {
            return Err(LoadError::malformed(format!(
                "column {column} has non-zero reserved bits"
            )));
        }
        let elem = rec[COLUMN_ELEM_FIELD] as usize;
        if elem != column.element_size() {
            return Err(LoadError::malformed(format!(
                "column {column} has element size {elem}, expected {}",
                column.element_size()
            )));
        }
        let offset = get_u64(rec, COLUMN_OFFSET_FIELD);
        let byte_len = get_u64(rec, COLUMN_LEN_FIELD);
        if offset % COLUMN_ALIGN as u64 != 0 {
            return Err(LoadError::malformed(format!(
                "column {column} at offset {offset} is not {COLUMN_ALIGN}-byte aligned"
            )));
        }
        if byte_len % elem as u64 != 0 {
            return Err(LoadError::malformed(format!(
                "column {column} length {byte_len} is not a multiple of {elem}"
            )));
        }
        // The names are always followed by one zero pad byte.
        let pad = u64::from(column == Column::NameBytes);
        let end = offset
            .checked_add(byte_len)
            .and_then(|e| e.checked_add(pad))
            .ok_or_else(|| LoadError::malformed(format!("column {column} overflows")))?;
        if offset < header_len as u64 {
            return Err(LoadError::malformed(format!(
                "column {column} overlaps the header"
            )));
        }
        if end > file_len {
            return Err(LoadError::Truncated {
                len: file_len,
                needed: end,
            });
        }
        let range = offset as usize..(offset + byte_len) as usize;
        if pad == 1 && map[range.end] != 0 {
            return Err(LoadError::malformed("name bytes are not followed by NUL"));
        }
        let elements = byte_len as usize / elem;
        let length_ok = match column {
            Column::NameOffsets
            | Column::Parents
            | Column::Volumes
            | Column::FileIds
            | Column::Meta
            | Column::Modified
            | Column::Created
            | Column::ExtIds => elements == id_bound,
            Column::Live => elements >= id_bound.div_ceil(64),
            // Values are checked by `verify_checksums`; the length is O(1).
            Column::Order(_) => elements == live_count,
            Column::NameBytes | Column::ExtBytes | Column::ExtOffsets => true,
        };
        if !length_ok {
            return Err(LoadError::malformed(match column {
                Column::Order(_) => {
                    format!("column {column} has {elements} elements for {live_count} live entries")
                }
                _ => format!("column {column} has {elements} elements for {id_bound} entries"),
            }));
        }
        // The mapping is page-aligned and the offset 64-byte aligned, so
        // this cannot fail; checking keeps the accessors panic-free.
        let bytes = &map[range.clone()];
        let cast_ok = match elem {
            2 => bytemuck::try_cast_slice::<u8, u16>(bytes).is_ok(),
            4 => bytemuck::try_cast_slice::<u8, u32>(bytes).is_ok(),
            8 => bytemuck::try_cast_slice::<u8, u64>(bytes).is_ok(),
            _ => true,
        };
        if !cast_ok {
            return Err(LoadError::malformed(format!(
                "column {column} is misaligned"
            )));
        }
        columns.push(Stored {
            column,
            range,
            checksum: get_u64(rec, COLUMN_CHECKSUM_FIELD),
        });
    }
    if let Some(slot) = required.iter().position(|&i| i == usize::MAX) {
        return Err(LoadError::malformed(format!(
            "required column {} is missing",
            Column::REQUIRED[slot]
        )));
    }
    if required.iter().enumerate().any(|(slot, &i)| slot != i) {
        return Err(LoadError::malformed(
            "required columns are not first and in kind order",
        ));
    }
    let mut spans: Vec<(u64, u64)> = columns
        .iter()
        .map(|s| {
            let pad = u64::from(s.column == Column::NameBytes);
            (s.range.start as u64, s.range.end as u64 + pad)
        })
        .collect();
    spans.sort_unstable();
    if spans.windows(2).any(|w| w[1].0 < w[0].1) {
        return Err(LoadError::malformed("columns overlap"));
    }
    Ok((columns, required))
}
