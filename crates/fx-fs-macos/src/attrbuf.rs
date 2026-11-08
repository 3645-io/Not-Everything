//! Pure parser for `getattrlistbulk(2)` result buffers, plus the descend
//! policy the walker applies to each entry.
//!
//! This module has no FFI and no `unsafe`, so it compiles and is tested on
//! every host. The constants and the packing rules come from Apple's public
//! getattrlist(2) / getattrlistbulk(2) manual pages and `sys/attr.h`.
//!
//! The walker requests
//!
//! * common: [`REQUESTED_COMMON_ATTRS`] (`RETURNED_ATTRS | NAME | ERROR |
//!   OBJTYPE | FILEID | FLAGS | MODTIME`),
//! * dir: [`REQUESTED_DIR_ATTRS`] (`MOUNTSTATUS`),
//! * file: [`REQUESTED_FILE_ATTRS`] (`DATALENGTH`),
//!
//! with [`FSOPT_PACK_INVAL_ATTRS`]. With that option every requested
//! attribute is packed in every record (invalid ones with default values)
//! and the returned `attribute_set_t` tells which values are valid, so each
//! record has the same fixed layout followed by the variable-length name:
//!
//! | offset | field                                                  |
//! |-------:|--------------------------------------------------------|
//! |      0 | `u32` record length                                    |
//! |      4 | returned `attribute_set_t` (5 × `u32`)                 |
//! |     24 | `ATTR_CMN_ERROR` `u32` (placed right after returned)   |
//! |     28 | `ATTR_CMN_NAME` `attrreference_t` (`i32` offset, `u32` length) |
//! |     36 | `ATTR_CMN_OBJTYPE` `u32` (`fsobj_type_t`)               |
//! |     40 | `ATTR_CMN_MODTIME` `timespec` (`i64` sec, `i64` nsec)   |
//! |     56 | `ATTR_CMN_FLAGS` `u32`                                 |
//! |     60 | `ATTR_CMN_FILEID` `u64`                                |
//! |     68 | `ATTR_DIR_MOUNTSTATUS` `u32`                           |
//! |     72 | `ATTR_FILE_DATALENGTH` `off_t`                         |
//! |     80 | name bytes incl. NUL, padded to 4 bytes                |
//!
//! The name's `attr_dataoffset` is relative to the `attrreference_t` itself
//! and `attr_length` includes the trailing NUL. The parser always advances by
//! the record length field.
//!
//! What the kernel actually returns (checked with the walker on macOS, APFS)
//! is more compact than the table above: the type-specific groups are packed
//! only for their own object type, both at offset 68. A directory record
//! has `ATTR_DIR_MOUNTSTATUS` at 68 (plus 4 padding bytes), a file, symlink
//! or device record has `ATTR_FILE_DATALENGTH` at 68, and the name starts at
//! 76. The parser therefore reads the type-specific attributes as in the
//! table when the name starts at or after offset 80 (room for both groups),
//! and from offset 68 otherwise. The kernel also returns `ATTR_CMN_ERROR`
//! for every entry with the value 0 for "no error", so
//! [`RawEntry::error`] is `Some` only for a nonzero errno. Both macOS
//! architectures are little-endian, so the buffer is decoded as
//! little-endian on every host.
//!
//! # Creation time
//!
//! The production walker additionally requests `ATTR_CMN_CRTIME`
//! ([`WALKER_COMMON_ATTRS`]) and decodes with [`Entries::with_crtime`].
//! Attributes are packed in bit order, so the creation `timespec` sits
//! between `ATTR_CMN_OBJTYPE` and `ATTR_CMN_MODTIME` at offset 40 and every
//! later field moves 16 bytes further: mod time at 56, flags at 72, file ID
//! at 76, the type-specific group at 84 (data length at 88 when both groups
//! are packed) and the name at 92 (compact) or 96. [`parse_entries`] and
//! [`Entries::new`] keep decoding the layout without it.

use std::fmt;

// ------------------------------------------------------------ sys/attr.h

/// `ATTR_BIT_MAP_COUNT`: number of `attrgroup_t` fields in `attrlist`.
pub const ATTR_BIT_MAP_COUNT: u16 = 5;

/// `ATTR_CMN_NAME`: `attrreference_t` to the NUL-terminated UTF-8 name.
pub const ATTR_CMN_NAME: u32 = 0x0000_0001;
/// `ATTR_CMN_OBJTYPE`: `fsobj_type_t` (vnode type).
pub const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
/// `ATTR_CMN_CRTIME`: `struct timespec` creation time.
pub const ATTR_CMN_CRTIME: u32 = 0x0000_0200;
/// `ATTR_CMN_MODTIME`: `struct timespec` modification time.
pub const ATTR_CMN_MODTIME: u32 = 0x0000_0400;
/// `ATTR_CMN_FLAGS`: `u32` BSD flags (`st_flags`).
pub const ATTR_CMN_FLAGS: u32 = 0x0004_0000;
/// `ATTR_CMN_FILEID`: `u64` file ID (inode number).
pub const ATTR_CMN_FILEID: u32 = 0x0200_0000;
/// `ATTR_CMN_ERROR`: `u32` per-entry error (errno), packed right after the
/// returned attribute set.
pub const ATTR_CMN_ERROR: u32 = 0x2000_0000;
/// `ATTR_CMN_RETURNED_ATTRS`: `attribute_set_t` of the attributes returned.
pub const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;

/// `ATTR_DIR_MOUNTSTATUS`: `u32` mount status of a directory.
pub const ATTR_DIR_MOUNTSTATUS: u32 = 0x0000_0008;
/// `DIR_MNTSTATUS_MNTPOINT`: the directory is a mount point.
pub const DIR_MNTSTATUS_MNTPOINT: u32 = 0x0000_0001;
/// `DIR_MNTSTATUS_TRIGGER`: the directory is an automount trigger.
pub const DIR_MNTSTATUS_TRIGGER: u32 = 0x0000_0002;

/// `ATTR_FILE_DATALENGTH`: `off_t` logical length of the data fork.
pub const ATTR_FILE_DATALENGTH: u32 = 0x0000_0200;

/// `FSOPT_NOFOLLOW`: do not follow a symlink at the end of the path.
pub const FSOPT_NOFOLLOW: u64 = 0x0000_0001;
/// `FSOPT_PACK_INVAL_ATTRS`: pack every requested attribute, valid or not.
pub const FSOPT_PACK_INVAL_ATTRS: u64 = 0x0000_0008;

/// `SF_DATALESS` (`sys/stat.h`): the file or directory is dataless
/// (cloud-only); reading it would trigger materialization.
pub const SF_DATALESS: u32 = 0x4000_0000;

/// Common attributes requested by the walker.
pub const REQUESTED_COMMON_ATTRS: u32 = ATTR_CMN_RETURNED_ATTRS
    | ATTR_CMN_NAME
    | ATTR_CMN_ERROR
    | ATTR_CMN_OBJTYPE
    | ATTR_CMN_FILEID
    | ATTR_CMN_FLAGS
    | ATTR_CMN_MODTIME;
/// Common attributes requested by the production walker:
/// [`REQUESTED_COMMON_ATTRS`] plus `ATTR_CMN_CRTIME`. Decode with
/// [`Entries::with_crtime`].
pub const WALKER_COMMON_ATTRS: u32 = REQUESTED_COMMON_ATTRS | ATTR_CMN_CRTIME;
/// Directory attributes requested by the walker.
pub const REQUESTED_DIR_ATTRS: u32 = ATTR_DIR_MOUNTSTATUS;
/// File attributes requested by the walker.
pub const REQUESTED_FILE_ATTRS: u32 = ATTR_FILE_DATALENGTH;
/// Options passed to `getattrlistbulk` by the walker.
pub const REQUESTED_OPTIONS: u64 = FSOPT_PACK_INVAL_ATTRS;

// vnode types (`enum vtype` in sys/vnode.h).
const VREG: u32 = 1;
const VDIR: u32 = 2;
const VLNK: u32 = 5;

// Fixed offsets within one record (see the module docs).
const OFF_RETURNED_COMMON: usize = 4;
const OFF_RETURNED_DIR: usize = 12;
const OFF_RETURNED_FILE: usize = 16;
const OFF_ERROR: usize = 24;
const OFF_NAME_REF: usize = 28;
const OFF_OBJTYPE: usize = 36;
const OFF_MTIME_SEC: usize = 40;
const OFF_MTIME_NSEC: usize = 48;
const OFF_FLAGS: usize = 56;
const OFF_FILEID: usize = 60;
const OFF_MOUNTSTATUS: usize = 68;
const OFF_DATALENGTH: usize = 72;

/// Size of the fixed part of a record, before the name bytes.
pub const RECORD_FIXED_LEN: usize = 80;

/// Offset of `ATTR_CMN_CRTIME` when requested; it takes 16 bytes and moves
/// every later field by [`CRTIME_LEN`].
const OFF_CRTIME: usize = 40;
/// Size of the `ATTR_CMN_CRTIME` `timespec`.
const CRTIME_LEN: usize = 16;

// ---------------------------------------------------------------- types

/// Object type of an entry, from `ATTR_CMN_OBJTYPE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryKind {
    /// `VDIR`.
    Dir,
    /// `VREG`.
    File,
    /// `VLNK`.
    Symlink,
    /// Any other vnode type (FIFO, socket, device, unknown).
    Other,
}

impl EntryKind {
    fn from_objtype(objtype: u32) -> Self {
        match objtype {
            VREG => EntryKind::File,
            VDIR => EntryKind::Dir,
            VLNK => EntryKind::Symlink,
            _ => EntryKind::Other,
        }
    }
}

/// One decoded `getattrlistbulk` record. The name borrows from the buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawEntry<'a> {
    /// Name bytes without the trailing NUL (UTF-8 on APFS/HFS+).
    pub name: &'a [u8],
    /// Object type.
    pub kind: EntryKind,
    /// File ID (inode number); 0 if not returned.
    pub file_id: u64,
    /// BSD flags (`st_flags`); 0 if not returned.
    pub flags: u32,
    /// Modification time, seconds since the epoch.
    pub mtime_sec: i64,
    /// Modification time, nanoseconds part.
    pub mtime_nsec: i64,
    /// Creation time, seconds since the epoch; 0 unless decoded with
    /// [`Entries::with_crtime`] and returned by the file system.
    pub crtime_sec: i64,
    /// Creation time, nanoseconds part (0 when not available).
    pub crtime_nsec: i64,
    /// Data fork length; `None` unless `ATTR_FILE_DATALENGTH` was returned.
    pub size: Option<u64>,
    /// Mount status; `None` unless `ATTR_DIR_MOUNTSTATUS` was returned.
    pub mount_status: Option<u32>,
    /// Per-entry errno; `None` unless `ATTR_CMN_ERROR` was returned with a
    /// nonzero value.
    pub error: Option<u32>,
}

/// Why a buffer could not be parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// Fewer bytes left than a record header needs.
    Truncated {
        /// Index of the record being parsed.
        index: usize,
    },
    /// The record length field is smaller than the fixed part or runs past
    /// the end of the buffer.
    BadRecordLength {
        /// Index of the record being parsed.
        index: usize,
        /// The record length field.
        len: u32,
    },
    /// The name reference points outside the record or has zero length.
    BadNameRef {
        /// Index of the record being parsed.
        index: usize,
    },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Truncated { index } => {
                write!(f, "attribute buffer truncated at record {index}")
            }
            ParseError::BadRecordLength { index, len } => {
                write!(f, "record {index} has invalid length {len}")
            }
            ParseError::BadNameRef { index } => {
                write!(f, "record {index} has an invalid name reference")
            }
        }
    }
}

impl std::error::Error for ParseError {}

// --------------------------------------------------------------- parsing

fn read_u32(b: &[u8], off: usize) -> u32 {
    let mut a = [0u8; 4];
    a.copy_from_slice(&b[off..off + 4]);
    u32::from_le_bytes(a)
}

fn read_i32(b: &[u8], off: usize) -> i32 {
    let mut a = [0u8; 4];
    a.copy_from_slice(&b[off..off + 4]);
    i32::from_le_bytes(a)
}

fn read_u64(b: &[u8], off: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[off..off + 8]);
    u64::from_le_bytes(a)
}

fn read_i64(b: &[u8], off: usize) -> i64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[off..off + 8]);
    i64::from_le_bytes(a)
}

/// Decodes one record. `rec` is exactly the record's bytes and has already
/// been checked to be at least `RECORD_FIXED_LEN + shift` long. `shift` is
/// [`CRTIME_LEN`] when `ATTR_CMN_CRTIME` was requested, else 0.
fn parse_record(rec: &[u8], index: usize, shift: usize) -> Result<RawEntry<'_>, ParseError> {
    debug_assert!(rec.len() >= RECORD_FIXED_LEN + shift);
    let common = read_u32(rec, OFF_RETURNED_COMMON);
    let dirattr = read_u32(rec, OFF_RETURNED_DIR);
    let fileattr = read_u32(rec, OFF_RETURNED_FILE);

    // Name: attrreference_t, offset relative to the reference itself, length
    // including the trailing NUL. It must lie entirely within the record.
    let bad_name = ParseError::BadNameRef { index };
    let name_off = i64::from(read_i32(rec, OFF_NAME_REF));
    let name_len = u64::from(read_u32(rec, OFF_NAME_REF + 4));
    let start = (OFF_NAME_REF as i64)
        .checked_add(name_off)
        .filter(|&s| s >= 0)
        .ok_or(bad_name)?;
    let end = start.checked_add(name_len as i64).ok_or(bad_name)?;
    if name_len == 0 || end > rec.len() as i64 {
        return Err(bad_name);
    }
    let raw_name = &rec[start as usize..end as usize];
    // Cut at the first NUL (normally the last byte).
    let name = match raw_name.iter().position(|&c| c == 0) {
        Some(nul) => &raw_name[..nul],
        None => raw_name,
    };

    // Type-specific attributes start right after the common ones. If the
    // fixed area before the name has room for both groups, both are packed
    // (dir first). Otherwise only the entry's own group is there, at the
    // same offset: that is what the kernel actually returns
    // (see the module docs).
    let both_groups = start as usize >= RECORD_FIXED_LEN + shift;
    let off_datalength = shift
        + if both_groups {
            OFF_DATALENGTH
        } else {
            OFF_MOUNTSTATUS
        };
    let (crtime_sec, crtime_nsec) = if shift != 0 && common & ATTR_CMN_CRTIME != 0 {
        (read_i64(rec, OFF_CRTIME), read_i64(rec, OFF_CRTIME + 8))
    } else {
        (0, 0)
    };

    // The kernel returns ATTR_CMN_ERROR for every entry, with 0 meaning no
    // error, so only a nonzero value is reported.
    let error = (common & ATTR_CMN_ERROR != 0)
        .then(|| read_u32(rec, OFF_ERROR))
        .filter(|&e| e != 0);

    Ok(RawEntry {
        name,
        kind: EntryKind::from_objtype(read_u32(rec, OFF_OBJTYPE)),
        file_id: read_u64(rec, OFF_FILEID + shift),
        flags: read_u32(rec, OFF_FLAGS + shift),
        mtime_sec: read_i64(rec, OFF_MTIME_SEC + shift),
        mtime_nsec: read_i64(rec, OFF_MTIME_NSEC + shift),
        crtime_sec,
        crtime_nsec,
        size: (fileattr & ATTR_FILE_DATALENGTH != 0).then(|| read_u64(rec, off_datalength)),
        mount_status: (dirattr & ATTR_DIR_MOUNTSTATUS != 0)
            .then(|| read_u32(rec, OFF_MOUNTSTATUS + shift)),
        error,
    })
}

/// Iterator over the records of one `getattrlistbulk` result buffer.
///
/// Yields `count` results; after the first error it yields nothing more.
#[derive(Debug, Clone)]
pub struct Entries<'a> {
    buf: &'a [u8],
    pos: usize,
    index: usize,
    remaining: usize,
    /// [`CRTIME_LEN`] when the buffer was requested with `ATTR_CMN_CRTIME`.
    shift: usize,
}

impl<'a> Entries<'a> {
    /// Iterates over the first `count` records of `buf`, requested with
    /// [`REQUESTED_COMMON_ATTRS`].
    pub fn new(buf: &'a [u8], count: usize) -> Self {
        Entries {
            buf,
            pos: 0,
            index: 0,
            remaining: count,
            shift: 0,
        }
    }

    /// Iterates over the first `count` records of `buf`, requested with
    /// [`WALKER_COMMON_ATTRS`] (creation time included; see the module
    /// docs for the layout).
    pub fn with_crtime(buf: &'a [u8], count: usize) -> Self {
        Entries {
            shift: CRTIME_LEN,
            ..Entries::new(buf, count)
        }
    }

    fn next_record(&mut self) -> Result<RawEntry<'a>, ParseError> {
        let index = self.index;
        let rest = &self.buf[self.pos..];
        if rest.len() < 4 {
            return Err(ParseError::Truncated { index });
        }
        let len = read_u32(rest, 0);
        let len_usize = len as usize;
        if len_usize < RECORD_FIXED_LEN + self.shift || len_usize > rest.len() {
            return Err(ParseError::BadRecordLength { index, len });
        }
        let entry = parse_record(&rest[..len_usize], index, self.shift)?;
        self.pos += len_usize;
        self.index += 1;
        Ok(entry)
    }
}

impl<'a> Iterator for Entries<'a> {
    type Item = Result<RawEntry<'a>, ParseError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let result = self.next_record();
        self.remaining = if result.is_ok() {
            self.remaining - 1
        } else {
            0
        };
        Some(result)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(self.remaining))
    }
}

/// Parses the first `count` records of a `getattrlistbulk` result buffer
/// (`count` is the call's return value).
///
/// Truncated or malformed buffers return an error; the parser never panics
/// and never reads outside `buf`.
pub fn parse_entries(buf: &[u8], count: usize) -> Result<Vec<RawEntry<'_>>, ParseError> {
    let mut out = Vec::with_capacity(count.min(buf.len() / RECORD_FIXED_LEN));
    for entry in Entries::new(buf, count) {
        out.push(entry?);
    }
    Ok(out)
}

// -------------------------------------------------------- descend policy

/// Why the walker does not descend into an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkipReason {
    /// Not a directory.
    NotDir,
    /// A mount point below the scan root (another volume).
    MountPoint,
    /// An automount trigger below the scan root; entering it would mount
    /// another (possibly network) file system.
    AutomountTrigger,
    /// A dataless (cloud-only) directory; descending would materialize it.
    Dataless,
    /// A symbolic link; links are never followed.
    Symlink,
    /// The entry carries a per-entry error.
    EntryError,
}

/// Whether the walker descends into an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Descend {
    /// Scan the directory's children.
    Descend,
    /// Do not scan it, for the given reason.
    Skip(SkipReason),
}

/// Decides whether to descend into `entry`.
///
/// Entries with errors, symlinks and non-directories are never descended.
/// Dataless (`SF_DATALESS`) directories are never descended. Mount points
/// (`DIR_MNTSTATUS_MNTPOINT`) and automount triggers
/// (`DIR_MNTSTATUS_TRIGGER`, entering one would mount a possibly remote
/// file system) are descended only when `is_root` is true, so a scan rooted
/// at a volume's mount point still covers that volume.
pub fn descend_decision(entry: &RawEntry<'_>, is_root: bool) -> Descend {
    if entry.error.is_some() {
        return Descend::Skip(SkipReason::EntryError);
    }
    match entry.kind {
        EntryKind::Dir => {}
        EntryKind::Symlink => return Descend::Skip(SkipReason::Symlink),
        EntryKind::File | EntryKind::Other => return Descend::Skip(SkipReason::NotDir),
    }
    if entry.flags & SF_DATALESS != 0 {
        return Descend::Skip(SkipReason::Dataless);
    }
    if is_root {
        return Descend::Descend;
    }
    let status = entry.mount_status.unwrap_or(0);
    if status & DIR_MNTSTATUS_MNTPOINT != 0 {
        return Descend::Skip(SkipReason::MountPoint);
    }
    if status & DIR_MNTSTATUS_TRIGGER != 0 {
        return Descend::Skip(SkipReason::AutomountTrigger);
    }
    Descend::Descend
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requested_masks_include_every_attribute() {
        assert_eq!(REQUESTED_COMMON_ATTRS, 0xA204_0409);
        assert_eq!(REQUESTED_DIR_ATTRS, 0x8);
        assert_eq!(REQUESTED_FILE_ATTRS, 0x200);
        assert_eq!(WALKER_COMMON_ATTRS, 0xA204_0609);
    }

    /// A compact directory record in the crtime layout: name at 92.
    #[test]
    fn crtime_layout_shifts_later_fields() {
        let name = b"dir\0";
        let total = 92 + 4;
        let mut b = vec![0u8; total];
        b[0..4].copy_from_slice(&(total as u32).to_le_bytes());
        b[4..8].copy_from_slice(&(WALKER_COMMON_ATTRS).to_le_bytes());
        b[12..16].copy_from_slice(&ATTR_DIR_MOUNTSTATUS.to_le_bytes());
        b[28..32].copy_from_slice(&(92i32 - 28).to_le_bytes());
        b[32..36].copy_from_slice(&4u32.to_le_bytes());
        b[36..40].copy_from_slice(&VDIR.to_le_bytes());
        b[40..48].copy_from_slice(&111i64.to_le_bytes());
        b[48..56].copy_from_slice(&5i64.to_le_bytes());
        b[56..64].copy_from_slice(&222i64.to_le_bytes());
        b[64..72].copy_from_slice(&6i64.to_le_bytes());
        b[72..76].copy_from_slice(&0x8000u32.to_le_bytes());
        b[76..84].copy_from_slice(&42u64.to_le_bytes());
        b[84..88].copy_from_slice(&DIR_MNTSTATUS_MNTPOINT.to_le_bytes());
        b[92..96].copy_from_slice(name);
        let e = Entries::with_crtime(&b, 1)
            .next()
            .expect("one record")
            .expect("valid record");
        assert_eq!(e.name, b"dir");
        assert_eq!(e.kind, EntryKind::Dir);
        assert_eq!((e.crtime_sec, e.crtime_nsec), (111, 5));
        assert_eq!((e.mtime_sec, e.mtime_nsec), (222, 6));
        assert_eq!(e.flags, 0x8000);
        assert_eq!(e.file_id, 42);
        assert_eq!(e.mount_status, Some(DIR_MNTSTATUS_MNTPOINT));
        assert_eq!(e.size, None);
    }
}
