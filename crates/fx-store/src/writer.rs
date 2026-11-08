//! The atomic checkpoint writer. The byte layout is in [`crate::layout`].

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fx_index::IndexParts;
use fx_model::{ResumeCursor, VolumeId};

use crate::error::{Column, StoreError};
use crate::layout::{
    COLUMN_ALIGN, COLUMN_CHECKSUM_FIELD, COLUMN_COUNT_OFFSET, COLUMN_ELEM_FIELD, COLUMN_KEY_FIELD,
    COLUMN_KIND_FIELD, COLUMN_LEN_FIELD, COLUMN_OFFSET_FIELD, COLUMN_RECORD_SIZE, CREATED_OFFSET,
    CURSOR_FSEVENTS, CURSOR_NONE, CURSOR_USN, ENDIAN_MARKER, ENDIAN_OFFSET, FILE_LEN_OFFSET,
    FIXED_HEADER_SIZE, FLAG_CLEAN_SHUTDOWN, FLAGS_OFFSET, FORMAT_VERSION, HEADER_CHECKSUM_OFFSET,
    HEADER_LEN_OFFSET, ID_BOUND_OFFSET, LIVE_COUNT_OFFSET, MAGIC, MAGIC_OFFSET, MAX_HEADER_LEN,
    REQUIRED_COLUMNS, VERSION_OFFSET, VOLUME_A_FIELD, VOLUME_B_FIELD, VOLUME_COUNT_OFFSET,
    VOLUME_ID_FIELD, VOLUME_RECORD_SIZE, VOLUME_TAG_FIELD, align_up, check_order, checksum,
    header_checksum,
};

/// Everything one checkpoint saves.
#[derive(Clone, Copy, Debug)]
pub struct SaveInput<'a> {
    /// The index columns, from `fx_index::Snapshot::to_parts`. Entry IDs
    /// are the positions in the per-entry columns, so they survive a reload.
    pub parts: &'a IndexParts,
    /// Per-volume resume cursors, stored in this order. Volume IDs must be
    /// unique.
    pub volumes: &'a [(VolumeId, ResumeCursor)],
    /// Saved sort orders: a `u8` key code (mapped to a sort key by the
    /// engine) and a `u32` permutation of the live entry IDs: exactly
    /// `live_count` elements, each a live ID below `id_bound`, no
    /// duplicates (see the order contract in [`crate::layout`]). Key codes
    /// must be unique; stored in this order.
    pub orders: &'a [(u8, &'a [u32])],
    /// Whether this checkpoint is written by a clean shutdown.
    pub clean_shutdown: bool,
}

pub(crate) fn put_u16(buf: &mut [u8], at: usize, v: u16) {
    buf[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

pub(crate) fn put_u32(buf: &mut [u8], at: usize, v: u32) {
    buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

pub(crate) fn put_u64(buf: &mut [u8], at: usize, v: u64) {
    buf[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

/// Number of live entries in `parts` (IDs below `id_bound` only).
fn count_live(parts: &IndexParts) -> usize {
    (0..parts.id_bound()).filter(|&i| parts.is_live(i)).count()
}

/// Checks the column lengths, the entry count, the table uniqueness and
/// that every saved order is a permutation of the live IDs.
fn validate(input: &SaveInput<'_>, live_count: usize) -> Result<(), StoreError> {
    let p = input.parts;
    let n = p.id_bound();
    if n > (u32::MAX - 1) as usize {
        return Err(StoreError::TooManyEntries { count: n });
    }
    for (column, found) in [
        (Column::NameOffsets, p.name_offsets.len()),
        (Column::Volumes, p.volumes.len()),
        (Column::FileIds, p.file_ids.len()),
        (Column::Meta, p.meta.len()),
        (Column::Modified, p.modified.len()),
        (Column::Created, p.created.len()),
        (Column::ExtIds, p.ext_ids.len()),
    ] {
        if found != n {
            return Err(StoreError::ColumnLength {
                column,
                expected: n,
                found,
            });
        }
    }
    if p.live.len() < n.div_ceil(64) {
        return Err(StoreError::ColumnLength {
            column: Column::Live,
            expected: n.div_ceil(64),
            found: p.live.len(),
        });
    }
    let mut keys = HashSet::new();
    for &(key, _) in input.orders {
        if !keys.insert(key) {
            return Err(StoreError::DuplicateOrder { key });
        }
    }
    for &(key, order) in input.orders {
        check_order(order, n, live_count, |i| p.is_live(i))
            .map_err(|problem| StoreError::InvalidOrder { key, problem })?;
    }
    let mut vols = HashSet::new();
    for &(vol, _) in input.volumes {
        if !vols.insert(vol.0) {
            return Err(StoreError::DuplicateVolume { volume: vol.0 });
        }
    }
    Ok(())
}

/// Every column in table order with its raw little-endian bytes.
fn columns<'a>(input: &SaveInput<'a>) -> Vec<(Column, &'a [u8])> {
    let p = input.parts;
    let mut v: Vec<(Column, &'a [u8])> = vec![
        (Column::NameBytes, &p.name_bytes),
        (Column::NameOffsets, bytemuck::cast_slice(&p.name_offsets)),
        (Column::Parents, bytemuck::cast_slice(&p.parents)),
        (Column::Volumes, bytemuck::cast_slice(&p.volumes)),
        (Column::FileIds, bytemuck::cast_slice(&p.file_ids)),
        (Column::Meta, bytemuck::cast_slice(&p.meta)),
        (Column::Modified, bytemuck::cast_slice(&p.modified)),
        (Column::Created, bytemuck::cast_slice(&p.created)),
        (Column::ExtIds, bytemuck::cast_slice(&p.ext_ids)),
        (Column::ExtBytes, &p.ext_bytes),
        (Column::ExtOffsets, bytemuck::cast_slice(&p.ext_offsets)),
        (Column::Live, bytemuck::cast_slice(&p.live)),
    ];
    for &(key, order) in input.orders {
        v.push((Column::Order(key), bytemuck::cast_slice(order)));
    }
    v
}

/// Encodes one volume record.
fn put_volume(rec: &mut [u8], volume: VolumeId, cursor: &ResumeCursor) {
    put_u32(rec, VOLUME_ID_FIELD, volume.0);
    let (tag, a) = match *cursor {
        ResumeCursor::FsEvents { event_id, db_uuid } => {
            rec[VOLUME_B_FIELD..VOLUME_B_FIELD + 16].copy_from_slice(&db_uuid);
            (CURSOR_FSEVENTS, event_id)
        }
        ResumeCursor::Usn {
            journal_id,
            next_usn,
        } => {
            rec[VOLUME_B_FIELD..VOLUME_B_FIELD + 8].copy_from_slice(&next_usn.to_le_bytes());
            (CURSOR_USN, journal_id)
        }
        ResumeCursor::None { snapshot_epoch } => (CURSOR_NONE, snapshot_epoch),
    };
    put_u32(rec, VOLUME_TAG_FIELD, tag);
    put_u64(rec, VOLUME_A_FIELD, a);
}

/// Builds the header (fixed part and both tables) and each column's file
/// offset; also returns the total file length.
fn build_header(
    input: &SaveInput<'_>,
    cols: &[(Column, &[u8])],
    live: usize,
) -> (Vec<u8>, Vec<u64>, u64) {
    let vtable = FIXED_HEADER_SIZE;
    let ctable = vtable + input.volumes.len() * VOLUME_RECORD_SIZE;
    let header_len = ctable + cols.len() * COLUMN_RECORD_SIZE;
    let mut h = vec![0u8; header_len];

    h[MAGIC_OFFSET..MAGIC_OFFSET + 8].copy_from_slice(&MAGIC);
    put_u32(&mut h, VERSION_OFFSET, FORMAT_VERSION);
    put_u32(&mut h, ENDIAN_OFFSET, ENDIAN_MARKER);
    put_u32(&mut h, HEADER_LEN_OFFSET, header_len as u32);
    let flags = if input.clean_shutdown {
        FLAG_CLEAN_SHUTDOWN
    } else {
        0
    };
    put_u32(&mut h, FLAGS_OFFSET, flags);
    let n = input.parts.id_bound();
    put_u64(&mut h, ID_BOUND_OFFSET, n as u64);
    put_u64(&mut h, LIVE_COUNT_OFFSET, live as u64);
    let created = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0);
    put_u64(&mut h, CREATED_OFFSET, created as u64);
    put_u32(&mut h, VOLUME_COUNT_OFFSET, input.volumes.len() as u32);
    put_u32(&mut h, COLUMN_COUNT_OFFSET, cols.len() as u32);

    for (i, (vol, cursor)) in input.volumes.iter().enumerate() {
        let at = vtable + i * VOLUME_RECORD_SIZE;
        put_volume(&mut h[at..at + VOLUME_RECORD_SIZE], *vol, cursor);
    }

    let mut offsets = Vec::with_capacity(cols.len());
    let mut cur = align_up(header_len as u64);
    for (i, &(column, bytes)) in cols.iter().enumerate() {
        let rec = ctable + i * COLUMN_RECORD_SIZE;
        put_u16(&mut h, rec + COLUMN_KIND_FIELD, column.kind());
        h[rec + COLUMN_KEY_FIELD] = column.key_code();
        h[rec + COLUMN_ELEM_FIELD] = column.element_size() as u8;
        put_u64(&mut h, rec + COLUMN_OFFSET_FIELD, cur);
        put_u64(&mut h, rec + COLUMN_LEN_FIELD, bytes.len() as u64);
        put_u64(&mut h, rec + COLUMN_CHECKSUM_FIELD, checksum(bytes));
        offsets.push(cur);
        let mut end = cur + bytes.len() as u64;
        if column == Column::NameBytes {
            // Always leave a zero pad byte after the names, so readers can
            // use the mapped buffer as NUL-terminated.
            end += 1;
        }
        cur = align_up(end);
    }
    put_u64(&mut h, FILE_LEN_OFFSET, cur);
    let sum = header_checksum(&h);
    put_u64(&mut h, HEADER_CHECKSUM_OFFSET, sum);
    (h, offsets, cur)
}

/// The prefix of every temp file name [`write_checkpoint`] uses for
/// `target`: `.<file name>.tmp.`, followed by the process ID and a counter.
/// A file with this prefix in the target's directory is a leftover of an
/// interrupted checkpoint when no checkpoint is running. `None` when the
/// target has no file name.
pub fn temp_name_prefix(target: &Path) -> Option<OsString> {
    let name = target.file_name()?;
    let mut tmp = OsString::from(".");
    tmp.push(name);
    tmp.push(".tmp.");
    Some(tmp)
}

fn temp_path(target: &Path) -> io::Result<PathBuf> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut name = temp_name_prefix(target).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "index path has no file name")
    })?;
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    name.push(format!("{}.{n}", std::process::id()));
    Ok(target.with_file_name(name))
}

fn write_body(
    file: &File,
    header: &[u8],
    cols: &[(Column, &[u8])],
    offsets: &[u64],
    file_len: u64,
) -> io::Result<()> {
    const ZEROS: [u8; COLUMN_ALIGN] = [0; COLUMN_ALIGN];
    let mut w = BufWriter::with_capacity(1 << 20, file);
    w.write_all(header)?;
    let mut pos = header.len() as u64;
    for (&(_, bytes), &offset) in cols.iter().zip(offsets) {
        // Padding up to an aligned offset is at most 64 bytes.
        w.write_all(&ZEROS[..(offset - pos) as usize])?;
        w.write_all(bytes)?;
        pos = offset + bytes.len() as u64;
    }
    w.write_all(&ZEROS[..(file_len - pos) as usize])?;
    w.flush()?;
    drop(w);
    // On macOS, std's sync_all issues fcntl(F_FULLFSYNC), so the data
    // reaches stable storage, not just the drive's cache.
    file.sync_all()
}

/// Makes the rename itself durable by syncing the containing directory.
#[cfg(unix)]
fn sync_dir(target: &Path) -> io::Result<()> {
    let dir = match target.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
fn sync_dir(_target: &Path) -> io::Result<()> {
    Ok(())
}

/// Writes a checkpoint atomically and returns its size in bytes.
///
/// The input is validated first (column lengths, entry count, unique
/// volume IDs and order keys, and every saved order a permutation of the
/// live IDs, else [`StoreError::InvalidOrder`]). The file is then written to a uniquely named
/// hidden temp file in the target's directory (see [`temp_name_prefix`]),
/// flushed with `File::sync_all` (`F_FULLFSYNC` on macOS), renamed over
/// `path`, and the directory is synced. A reader therefore sees either the
/// old file or the new one, never a torn file, and an existing mapping of
/// the old file stays valid.
///
/// On Unix the new file has mode 0600 from the moment the temp file is
/// created, so the target ends up 0600 whatever mode an earlier file at
/// `path` had. The directory must exist; [`create_private_dir`] makes a
/// missing one. The directory's mode is never changed.
///
/// On error the temp file is removed and the old target is untouched. The
/// one exception is [`StoreError::DirSync`]: the new file is already in
/// place, but its rename may not survive a power loss.
pub fn write_checkpoint(path: impl AsRef<Path>, input: &SaveInput<'_>) -> Result<u64, StoreError> {
    if cfg!(target_endian = "big") {
        return Err(StoreError::BigEndianHost);
    }
    let path = path.as_ref();
    let live = count_live(input.parts);
    validate(input, live)?;
    let header_len = FIXED_HEADER_SIZE
        + input.volumes.len() * VOLUME_RECORD_SIZE
        + (REQUIRED_COLUMNS + input.orders.len()) * COLUMN_RECORD_SIZE;
    if header_len > MAX_HEADER_LEN {
        return Err(StoreError::HeaderTooLarge);
    }
    let cols = columns(input);
    let (header, offsets, file_len) = build_header(input, &cols, live);

    let tmp = temp_path(path)?;
    let file = create_temp(&tmp)?;
    let result = write_body(&file, &header, &cols, &offsets, file_len);
    drop(file);
    if let Err(e) = result.and_then(|()| fs::rename(&tmp, path)) {
        let _ = fs::remove_file(&tmp);
        return Err(StoreError::Io(e));
    }
    sync_dir(path).map_err(StoreError::DirSync)?;
    Ok(file_len)
}

/// The mode of the store's files (Unix): readable and writable by the user
/// only. A checkpoint holds the names of every indexed file, including
/// names in folders other programs may not list.
pub const PRIVATE_FILE_MODE: u32 = 0o600;

/// The mode of the folders [`create_private_dir`] makes and
/// [`make_dir_private`] sets (Unix): the user may read, write and enter
/// them; nobody else may.
pub const PRIVATE_DIR_MODE: u32 = 0o700;

/// Creates the temp file `tmp` for a new checkpoint, private from the
/// start: mode 0600 when it is created, so no other user can open it while
/// it is written, and set to exactly 0600 whatever the umask. The rename
/// carries the mode over to the target, so an older, wider target is
/// replaced by a private file.
fn create_temp(tmp: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        options.mode(PRIVATE_FILE_MODE);
        let file = options.open(tmp)?;
        if let Err(e) = file.set_permissions(fs::Permissions::from_mode(PRIVATE_FILE_MODE)) {
            drop(file);
            let _ = fs::remove_file(tmp);
            return Err(e);
        }
        Ok(file)
    }
    #[cfg(not(unix))]
    {
        options.open(tmp)
    }
}

/// Creates `dir` and any missing parents, each new folder with mode 0700
/// ([`PRIVATE_DIR_MODE`]). A folder that already exists, `dir` included,
/// keeps its mode: the store can be pointed at any folder (a benchmark's
/// `--store-dir ~`, a shared `/tmp`), and a folder it did not make is not
/// its to change. Elsewhere than Unix it only creates the folders.
///
/// [`make_dir_private`] also tightens an existing folder, for a caller
/// that owns it.
///
/// # Errors
///
/// Creating a folder failed, for example because a file is in the way.
pub fn create_private_dir(dir: impl AsRef<Path>) -> io::Result<()> {
    let dir = dir.as_ref();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(PRIVATE_DIR_MODE)
            .create(dir)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(dir)
    }
}

/// Makes `dir` a private folder: creates it as [`create_private_dir`]
/// does, and sets it to exactly 0700 ([`PRIVATE_DIR_MODE`]) if it already
/// existed with another mode, so a folder an older version left readable
/// by other users is tightened. Its files and its parents keep their
/// modes. Elsewhere than Unix it only creates the folder.
///
/// Call it only on a folder of the caller's own, such as the app's data
/// folder: it changes the folder's mode. The store never calls it.
///
/// # Errors
///
/// Creating the folder, reading its mode or changing it.
pub fn make_dir_private(dir: impl AsRef<Path>) -> io::Result<()> {
    let dir = dir.as_ref();
    create_private_dir(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(dir)?.permissions().mode() & 0o777 != PRIVATE_DIR_MODE {
            fs::set_permissions(dir, fs::Permissions::from_mode(PRIVATE_DIR_MODE))?;
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn temp_files_are_private_before_the_rename() {
        let dir = std::env::temp_dir().join(format!(
            "fx-store-temp-mode-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).expect("mkdir");
        let tmp = temp_path(&dir.join("index.fxs")).expect("temp path");
        let file = create_temp(&tmp).expect("create temp");
        let mode = file.metadata().expect("metadata").permissions().mode() & 0o777;
        drop(file);
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(mode, 0o600);
    }
}
