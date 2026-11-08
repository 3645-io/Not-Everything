//! Host-independent tests for the `getattrlistbulk` result-buffer parser and
//! the descend policy in `fx_fs_macos::attrbuf`.
//!
//! These run on every OS: the module is pure Rust with no FFI.
//!
//! Buffers are built by hand following Apple's getattrlist(2) /
//! getattrlistbulk(2) documentation and `sys/attr.h`, in little-endian byte
//! order (both macOS architectures are little-endian). The walker requests
//!
//! * common: `ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_NAME | ATTR_CMN_ERROR |
//!   ATTR_CMN_OBJTYPE | ATTR_CMN_FILEID | ATTR_CMN_FLAGS | ATTR_CMN_MODTIME`
//! * dir: `ATTR_DIR_MOUNTSTATUS`
//! * file: `ATTR_FILE_DATALENGTH`
//!
//! with `FSOPT_PACK_INVAL_ATTRS`, so every requested attribute is packed in
//! every record (invalid ones with default values) and the returned
//! `attribute_set_t` says which of them are valid. Record layout:
//!
//! | offset | field                                              |
//! |-------:|----------------------------------------------------|
//! |      0 | `u32` record length                                |
//! |      4 | returned `attribute_set_t` (5 × `u32`)             |
//! |     24 | `ATTR_CMN_ERROR` `u32` (placed right after returned) |
//! |     28 | `ATTR_CMN_NAME` `attrreference_t` (`i32` offset, `u32` length) |
//! |     36 | `ATTR_CMN_OBJTYPE` `u32` (`fsobj_type_t`)           |
//! |     40 | `ATTR_CMN_MODTIME` `timespec` (`i64` sec, `i64` nsec) |
//! |     56 | `ATTR_CMN_FLAGS` `u32`                             |
//! |     60 | `ATTR_CMN_FILEID` `u64`                            |
//! |     68 | `ATTR_DIR_MOUNTSTATUS` `u32`                       |
//! |     72 | `ATTR_FILE_DATALENGTH` `off_t`                     |
//! |     80 | name bytes incl. NUL, padded to 4 bytes            |
//!
//! The name's `attr_dataoffset` is relative to the `attrreference_t` itself
//! and `attr_length` includes the trailing NUL.
//!
//! The kernel actually returns a more compact layout: only the entry's own
//! type-specific group is packed, at offset 68 (a directory's mount status
//! plus 4 padding bytes, or a file's or symlink's data length), and the name
//! starts at 76. `Rec::compact` encodes that layout; both are covered.
//!
//! API contract pinned here: `parse_entries(&[u8], usize) ->
//! Result<Vec<RawEntry>, E: Debug>`, `descend_decision(&RawEntry, bool) ->
//! Descend` with `Descend::{Descend, Skip(SkipReason)}`, and
//! `EntryKind::{Dir, File, Symlink, Other}`.

use fx_fs_macos::attrbuf::{
    ATTR_CMN_ERROR, ATTR_CMN_FILEID, ATTR_CMN_FLAGS, ATTR_CMN_MODTIME, ATTR_CMN_NAME,
    ATTR_CMN_OBJTYPE, ATTR_CMN_RETURNED_ATTRS, ATTR_DIR_MOUNTSTATUS, ATTR_FILE_DATALENGTH,
    DIR_MNTSTATUS_MNTPOINT, DIR_MNTSTATUS_TRIGGER, Descend, EntryKind, FSOPT_PACK_INVAL_ATTRS,
    SF_DATALESS, SkipReason, descend_decision, parse_entries,
};

// Values from sys/attr.h and sys/vnode.h / sys/stat.h, kept local so the
// buffer builder does not depend on the code under test.
const CMN_RETURNED: u32 = 0x8000_0000;
const CMN_NAME: u32 = 0x0000_0001;
const CMN_ERROR: u32 = 0x2000_0000;
const CMN_OBJTYPE: u32 = 0x0000_0008;
const CMN_FILEID: u32 = 0x0200_0000;
const CMN_FLAGS: u32 = 0x0004_0000;
const CMN_MODTIME: u32 = 0x0000_0400;
const DIR_MOUNTSTATUS: u32 = 0x0000_0008;
const FILE_DATALENGTH: u32 = 0x0000_0200;
const MNTPOINT: u32 = 0x0000_0001;
const DATALESS: u32 = 0x4000_0000;

const VREG: u32 = 1;
const VDIR: u32 = 2;
const VLNK: u32 = 5;
const VFIFO: u32 = 7;

const EACCES: u32 = 13;

const FIXED_LEN: usize = 80;
/// Name offset in the compact layout the kernel returns on APFS: the
/// type-specific group (8 bytes: a file's `off_t` data length, or a
/// directory's `u32` mount status plus 4 padding bytes) sits at 68.
const COMPACT_NAME_OFF: usize = 76;
const TRIGGER: u32 = 0x0000_0002;

fn name_of<N: AsRef<[u8]>>(n: &N) -> &[u8] {
    n.as_ref()
}

#[derive(Clone)]
struct Rec {
    name: Vec<u8>,
    objtype: u32,
    file_id: u64,
    flags: u32,
    mtime: (i64, i64),
    mount_status: Option<u32>,
    size: Option<u64>,
    error: Option<u32>,
    /// Extra bytes appended after the padded name (the parser must advance by
    /// the record length, not by its own idea of the size).
    trailing_pad: usize,
    /// Encode the compact layout the kernel actually returns: only the
    /// entry's own type-specific group is packed, at offset 68, and the name
    /// starts at 76 (see [`COMPACT_NAME_OFF`]).
    compact: bool,
}

impl Rec {
    fn new(name: &str, objtype: u32) -> Self {
        Rec {
            name: name.as_bytes().to_vec(),
            objtype,
            file_id: 0,
            flags: 0,
            mtime: (0, 0),
            mount_status: None,
            size: None,
            error: None,
            trailing_pad: 0,
            compact: false,
        }
    }
    fn compact(self) -> Self {
        Rec {
            compact: true,
            ..self
        }
    }
    fn file(name: &str, size: u64) -> Self {
        Rec {
            size: Some(size),
            ..Rec::new(name, VREG)
        }
    }
    fn dir(name: &str) -> Self {
        Rec {
            mount_status: Some(0),
            ..Rec::new(name, VDIR)
        }
    }
}

fn put_u32(b: &mut [u8], off: usize, v: u32) {
    b[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn encode(r: &Rec) -> Vec<u8> {
    let name_len = r.name.len() + 1; // incl. NUL
    let padded = name_len.div_ceil(4) * 4;
    let name_off = if r.compact {
        COMPACT_NAME_OFF
    } else {
        FIXED_LEN
    };
    let total = name_off + padded + r.trailing_pad;
    let mut b = vec![0u8; total];

    let mut common = CMN_RETURNED | CMN_NAME | CMN_OBJTYPE | CMN_MODTIME | CMN_FLAGS | CMN_FILEID;
    if r.error.is_some() {
        common |= CMN_ERROR;
    }
    let dirattr = if r.mount_status.is_some() {
        DIR_MOUNTSTATUS
    } else {
        0
    };
    let fileattr = if r.size.is_some() { FILE_DATALENGTH } else { 0 };

    put_u32(&mut b, 0, total as u32);
    put_u32(&mut b, 4, common);
    put_u32(&mut b, 8, 0); // volattr
    put_u32(&mut b, 12, dirattr);
    put_u32(&mut b, 16, fileattr);
    put_u32(&mut b, 20, 0); // forkattr
    put_u32(&mut b, 24, r.error.unwrap_or(0));
    // attrreference_t: offset relative to its own start (28).
    b[28..32].copy_from_slice(&((name_off - 28) as i32).to_le_bytes());
    put_u32(&mut b, 32, name_len as u32);
    put_u32(&mut b, 36, r.objtype);
    b[40..48].copy_from_slice(&r.mtime.0.to_le_bytes());
    b[48..56].copy_from_slice(&r.mtime.1.to_le_bytes());
    put_u32(&mut b, 56, r.flags);
    b[60..68].copy_from_slice(&r.file_id.to_le_bytes());
    if r.compact {
        // Only the entry's own group, at 68: a directory's mount status
        // (plus 4 padding bytes), or a file's / symlink's data length.
        if let Some(status) = r.mount_status {
            put_u32(&mut b, 68, status);
        } else if let Some(size) = r.size {
            b[68..76].copy_from_slice(&size.to_le_bytes());
        }
    } else {
        put_u32(&mut b, 68, r.mount_status.unwrap_or(0));
        b[72..80].copy_from_slice(&r.size.unwrap_or(0).to_le_bytes());
    }
    b[name_off..name_off + r.name.len()].copy_from_slice(&r.name);
    // NUL and padding are already zero.
    b
}

fn encode_all(recs: &[Rec]) -> Vec<u8> {
    recs.iter().flat_map(encode).collect()
}

/// Parse a single record and return the decision for it.
fn decide(r: &Rec, is_root: bool) -> Descend {
    let buf = encode(r);
    let entries = parse_entries(&buf, 1).expect("valid record must parse");
    assert_eq!(entries.len(), 1);
    descend_decision(&entries[0], is_root)
}

// ---------------------------------------------------------------- constants

// The constants' integer types are left to the implementation.
#[allow(clippy::unnecessary_cast)]
#[test]
fn attribute_constants_match_sys_attr_h() {
    assert_eq!(ATTR_CMN_RETURNED_ATTRS as u64, 0x8000_0000);
    assert_eq!(ATTR_CMN_NAME as u64, 0x0000_0001);
    assert_eq!(ATTR_CMN_ERROR as u64, 0x2000_0000);
    assert_eq!(ATTR_CMN_OBJTYPE as u64, 0x0000_0008);
    assert_eq!(ATTR_CMN_FILEID as u64, 0x0200_0000);
    assert_eq!(ATTR_CMN_FLAGS as u64, 0x0004_0000);
    assert_eq!(ATTR_CMN_MODTIME as u64, 0x0000_0400);
    assert_eq!(ATTR_DIR_MOUNTSTATUS as u64, 0x0000_0008);
    assert_eq!(ATTR_FILE_DATALENGTH as u64, 0x0000_0200);
    assert_eq!(FSOPT_PACK_INVAL_ATTRS as u64, 0x0000_0008);
    assert_eq!(DIR_MNTSTATUS_MNTPOINT as u64, 0x0000_0001);
    assert_eq!(DIR_MNTSTATUS_TRIGGER as u64, 0x0000_0002);
    assert_eq!(SF_DATALESS as u64, 0x4000_0000);
}

// ------------------------------------------------------------------ parsing

// The integer type of `mtime_nsec` is left to the implementation.
#[allow(clippy::unnecessary_cast)]
#[test]
fn parses_a_regular_file_record() {
    let rec = Rec {
        file_id: 0x0123_4567_89ab_cdef,
        flags: 0x20, // UF_HIDDEN-ish, arbitrary
        mtime: (1_700_000_000, 123_456_789),
        ..Rec::file("a.txt", 4096)
    };
    let buf = encode(&rec);
    let entries = parse_entries(&buf, 1).unwrap();
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    assert_eq!(name_of(&e.name), b"a.txt", "name excludes the trailing NUL");
    assert_eq!(e.kind, EntryKind::File);
    assert_eq!(e.file_id, 0x0123_4567_89ab_cdef);
    assert_eq!(e.flags, 0x20);
    assert_eq!(e.mtime_sec, 1_700_000_000);
    assert_eq!(e.mtime_nsec as i64, 123_456_789);
    assert_eq!(e.size, Some(4096));
    assert_eq!(
        e.mount_status, None,
        "dir attribute not returned for a file"
    );
    assert_eq!(e.error, None);
}

#[test]
fn parses_a_directory_record_without_size() {
    let rec = Rec {
        file_id: 42,
        mtime: (-5, 0),
        mount_status: Some(MNTPOINT),
        ..Rec::dir("Volumes")
    };
    let buf = encode(&rec);
    let entries = parse_entries(&buf, 1).unwrap();
    let e = &entries[0];
    assert_eq!(name_of(&e.name), b"Volumes");
    assert_eq!(e.kind, EntryKind::Dir);
    assert_eq!(e.file_id, 42);
    assert_eq!(e.mtime_sec, -5, "pre-1970 mtimes keep their sign");
    assert_eq!(
        e.size, None,
        "file attribute packed (PACK_INVAL) but not returned for a dir"
    );
    assert_eq!(e.mount_status, Some(MNTPOINT));
}

#[test]
fn maps_object_types() {
    let cases = [
        (VREG, EntryKind::File),
        (VDIR, EntryKind::Dir),
        (VLNK, EntryKind::Symlink),
        (VFIFO, EntryKind::Other),
        (0, EntryKind::Other),  // VNON
        (4, EntryKind::Other),  // VCHR
        (6, EntryKind::Other),  // VSOCK
        (99, EntryKind::Other), // unknown
    ];
    for (objtype, kind) in cases {
        let buf = encode(&Rec::new("x", objtype));
        let entries = parse_entries(&buf, 1).unwrap();
        assert_eq!(entries[0].kind, kind, "objtype {objtype}");
    }
}

#[test]
fn parses_several_records_with_varying_name_padding() {
    // Name lengths (incl. NUL) of 1+1, 3+1, 4+1, 7+1, 8+1 exercise padding.
    let recs = vec![
        Rec {
            file_id: 1,
            ..Rec::file("a", 1)
        },
        Rec {
            file_id: 2,
            ..Rec::dir("abc")
        },
        Rec {
            file_id: 3,
            ..Rec::file("abcd", 3)
        },
        Rec {
            file_id: 4,
            ..Rec::new("symlink", VLNK)
        },
        Rec {
            file_id: 5,
            ..Rec::file("12345678", 0)
        },
    ];
    let buf = encode_all(&recs);
    let entries = parse_entries(&buf, recs.len()).unwrap();
    assert_eq!(entries.len(), recs.len());
    for (e, r) in entries.iter().zip(&recs) {
        assert_eq!(name_of(&e.name), r.name.as_slice());
        assert_eq!(e.file_id, r.file_id);
        assert_eq!(e.size, r.size);
        assert_eq!(e.mount_status, r.mount_status);
    }
    assert_eq!(entries[1].kind, EntryKind::Dir);
    assert_eq!(entries[3].kind, EntryKind::Symlink);
}

#[test]
fn size_and_mount_status_decode_in_both_layouts() {
    const SIZE: u64 = 0x0102_0304_0506_0708;
    // 1-character names give compact records of exactly 80 bytes, the
    // smallest record the parser accepts.
    for name in ["a", "ab", "abc", "abcd", "a longer name.txt"] {
        for compact in [false, true] {
            let label = format!("name {name:?}, compact {compact}");
            let file = Rec {
                file_id: 7,
                compact,
                ..Rec::file(name, SIZE)
            };
            let link = Rec {
                file_id: 8,
                size: Some(SIZE - 1),
                compact,
                ..Rec::new(name, VLNK)
            };
            let dir = Rec {
                file_id: 9,
                mount_status: Some(MNTPOINT | TRIGGER),
                compact,
                ..Rec::new(name, VDIR)
            };
            let recs = [file, link, dir];
            for r in &recs {
                let one = encode(r);
                let name_off = if compact { COMPACT_NAME_OFF } else { FIXED_LEN };
                assert_eq!(one.len(), name_off + (name.len() + 1).div_ceil(4) * 4);
                if compact && name.len() == 1 {
                    assert_eq!(one.len(), 80, "{label}");
                }
            }

            let buf = encode_all(&recs);
            let entries = parse_entries(&buf, recs.len()).expect(&label);
            assert_eq!(entries.len(), 3, "{label}");
            for (e, r) in entries.iter().zip(&recs) {
                assert_eq!(name_of(&e.name), name.as_bytes(), "{label}");
                assert_eq!(e.file_id, r.file_id, "{label}");
                assert_eq!(e.size, r.size, "{label}");
                assert_eq!(e.mount_status, r.mount_status, "{label}");
            }
            assert_eq!(entries[0].kind, EntryKind::File, "{label}");
            assert_eq!(entries[1].kind, EntryKind::Symlink, "{label}");
            assert_eq!(entries[2].kind, EntryKind::Dir, "{label}");
        }
    }
}

#[test]
fn compact_records_mix_with_padded_ones() {
    let recs = vec![
        Rec::file("x", 11).compact(),
        Rec::dir("y").compact(),
        Rec::file("z", 13),
        Rec {
            mount_status: Some(MNTPOINT),
            ..Rec::dir("Volumes").compact()
        },
        Rec::file("résumé.txt", 15).compact(),
    ];
    let buf = encode_all(&recs);
    let entries = parse_entries(&buf, recs.len()).unwrap();
    for (e, r) in entries.iter().zip(&recs) {
        assert_eq!(name_of(&e.name), r.name.as_slice());
        assert_eq!(e.size, r.size);
        assert_eq!(e.mount_status, r.mount_status);
    }
}

#[test]
fn compact_truncation_is_an_error() {
    let buf = encode_all(&[Rec::file("a", 10).compact(), Rec::dir("b").compact()]);
    assert_eq!(buf.len(), 160);
    assert!(parse_entries(&buf, 2).is_ok());
    for len in 0..buf.len() {
        assert!(parse_entries(&buf[..len], 2).is_err(), "truncated to {len}");
    }
}

#[test]
fn utf8_names_are_preserved() {
    let buf = encode(&Rec::file("résumé 日本.txt", 7));
    let entries = parse_entries(&buf, 1).unwrap();
    assert_eq!(name_of(&entries[0].name), "résumé 日本.txt".as_bytes());
}

#[test]
fn advances_by_the_record_length_field() {
    let recs = vec![
        Rec {
            trailing_pad: 12,
            file_id: 10,
            ..Rec::file("first", 1)
        },
        Rec {
            file_id: 11,
            ..Rec::file("second", 2)
        },
    ];
    let buf = encode_all(&recs);
    let entries = parse_entries(&buf, 2).unwrap();
    assert_eq!(name_of(&entries[1].name), b"second");
    assert_eq!(entries[1].file_id, 11);
}

#[test]
fn parses_only_the_requested_count() {
    let recs = vec![
        Rec::file("one", 1),
        Rec::file("two", 2),
        Rec::file("three", 3),
    ];
    let buf = encode_all(&recs);
    let entries = parse_entries(&buf, 2).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(name_of(&entries[1].name), b"two");

    let none = parse_entries(&buf, 0).unwrap();
    assert!(none.is_empty());
    let empty = parse_entries(&[], 0).unwrap();
    assert!(empty.is_empty());
}

#[test]
fn reports_per_entry_errors() {
    let rec = Rec {
        error: Some(EACCES),
        ..Rec::dir("private")
    };
    let buf = encode(&rec);
    let entries = parse_entries(&buf, 1).unwrap();
    assert_eq!(entries[0].error, Some(EACCES));
    assert_eq!(name_of(&entries[0].name), b"private");
}

// ---------------------------------------------------------- malformed input

#[test]
fn every_truncation_is_an_error() {
    let recs = vec![Rec::file("alpha", 10), Rec::dir("beta-dir")];
    let buf = encode_all(&recs);
    assert!(parse_entries(&buf, 2).is_ok());
    for len in 0..buf.len() {
        assert!(
            parse_entries(&buf[..len], 2).is_err(),
            "truncated to {len} of {} bytes must be an error",
            buf.len()
        );
    }
}

#[test]
fn count_larger_than_buffer_is_an_error() {
    let buf = encode_all(&[Rec::file("a", 1), Rec::file("b", 2)]);
    assert!(parse_entries(&buf, 3).is_err());
    assert!(parse_entries(&buf, usize::MAX).is_err());
}

#[test]
fn bad_record_lengths_are_errors() {
    let good = encode(&Rec::file("name", 1));

    // Zero length would loop forever if trusted.
    let mut b = good.clone();
    put_u32(&mut b, 0, 0);
    assert!(parse_entries(&b, 1).is_err(), "zero record length");

    // Shorter than the fixed part of the record.
    for short in [4u32, 23, 24, 40, FIXED_LEN as u32 - 1] {
        let mut b = good.clone();
        put_u32(&mut b, 0, short);
        assert!(parse_entries(&b, 1).is_err(), "record length {short}");
    }

    // Longer than the buffer.
    let mut b = good.clone();
    put_u32(&mut b, 0, good.len() as u32 + 4);
    assert!(parse_entries(&b, 1).is_err(), "record past end of buffer");

    let mut b = good;
    put_u32(&mut b, 0, u32::MAX);
    assert!(parse_entries(&b, 1).is_err(), "huge record length");
}

#[test]
fn bad_name_references_are_errors() {
    let good = encode(&Rec::file("name", 1));
    let len = good.len();

    // Offset pointing past the end of the buffer.
    let mut b = good.clone();
    b[28..32].copy_from_slice(&(len as i32).to_le_bytes());
    assert!(parse_entries(&b, 1).is_err(), "name offset past end");

    // Negative offset pointing before the start of the buffer.
    let mut b = good.clone();
    b[28..32].copy_from_slice(&(-1000i32).to_le_bytes());
    assert!(parse_entries(&b, 1).is_err(), "negative name offset");

    let mut b = good.clone();
    b[28..32].copy_from_slice(&i32::MIN.to_le_bytes());
    assert!(parse_entries(&b, 1).is_err(), "i32::MIN name offset");

    // Length running past the end of the buffer.
    let mut b = good.clone();
    put_u32(&mut b, 32, len as u32);
    assert!(parse_entries(&b, 1).is_err(), "name length past end");

    let mut b = good;
    put_u32(&mut b, 32, u32::MAX);
    assert!(parse_entries(&b, 1).is_err(), "huge name length");
}

#[test]
fn never_panics_on_corrupted_or_random_input() {
    // Flip every byte of a valid buffer to a few values; the parser may
    // accept or reject, but must not panic.
    let good = encode_all(&[Rec::file("alpha", 10), Rec::dir("beta-dir")]);
    for i in 0..good.len() {
        for v in [0x00u8, 0x01, 0x7f, 0x80, 0xfe, 0xff] {
            let mut b = good.clone();
            b[i] = v;
            let _ = parse_entries(&b, 2);
            let _ = parse_entries(&b, 3);
        }
    }

    // Deterministic pseudo-random buffers.
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..2000 {
        let len = (next() % 300) as usize;
        let mut b: Vec<u8> = (0..len).map(|_| next() as u8).collect();
        if len >= 4 && next() % 2 == 0 {
            // Make the record length plausible so parsing goes deeper.
            let rl = (next() % (len as u64 + 8)) as u32;
            put_u32(&mut b, 0, rl);
        }
        let count = (next() % 5) as usize;
        let _ = parse_entries(&b, count);
    }
}

// ----------------------------------------------------------- descend policy

#[test]
fn plain_directories_are_descended() {
    assert_eq!(decide(&Rec::dir("src"), false), Descend::Descend);
    assert_eq!(decide(&Rec::dir("src"), true), Descend::Descend);
    // Mount status not returned at all: still a normal directory.
    let no_status = Rec {
        mount_status: None,
        ..Rec::dir("src")
    };
    assert_eq!(decide(&no_status, false), Descend::Descend);
}

#[test]
fn non_directories_are_not_descended() {
    assert_eq!(
        decide(&Rec::file("a.txt", 1), false),
        Descend::Skip(SkipReason::NotDir)
    );
    assert_eq!(
        decide(&Rec::file("a.txt", 1), true),
        Descend::Skip(SkipReason::NotDir)
    );
    assert_eq!(
        decide(&Rec::new("fifo", VFIFO), false),
        Descend::Skip(SkipReason::NotDir)
    );
}

#[test]
fn symlinks_are_never_followed() {
    assert_eq!(
        decide(&Rec::new("link", VLNK), false),
        Descend::Skip(SkipReason::Symlink)
    );
    assert_eq!(
        decide(&Rec::new("link", VLNK), true),
        Descend::Skip(SkipReason::Symlink)
    );
}

#[test]
fn mount_points_are_skipped_except_at_the_root() {
    let mnt = Rec {
        mount_status: Some(MNTPOINT),
        ..Rec::dir("Data")
    };
    assert_eq!(decide(&mnt, false), Descend::Skip(SkipReason::MountPoint));
    assert_eq!(decide(&mnt, true), Descend::Descend);
}

#[test]
fn automount_triggers_are_skipped_except_at_the_root() {
    for compact in [false, true] {
        let trigger = Rec {
            mount_status: Some(TRIGGER),
            compact,
            ..Rec::dir("net")
        };
        assert_eq!(
            decide(&trigger, false),
            Descend::Skip(SkipReason::AutomountTrigger),
            "compact {compact}"
        );
        assert_eq!(
            decide(&trigger, true),
            Descend::Descend,
            "compact {compact}"
        );

        // A trigger that is already mounted is reported as a mount point.
        let mounted = Rec {
            mount_status: Some(TRIGGER | MNTPOINT),
            compact,
            ..Rec::dir("home")
        };
        assert_eq!(
            decide(&mounted, false),
            Descend::Skip(SkipReason::MountPoint),
            "compact {compact}"
        );
        assert_eq!(
            decide(&mounted, true),
            Descend::Descend,
            "compact {compact}"
        );
    }
}

#[test]
fn dataless_directories_are_never_descended() {
    let cloud = Rec {
        flags: DATALESS,
        ..Rec::dir("iCloud Folder")
    };
    assert_eq!(decide(&cloud, false), Descend::Skip(SkipReason::Dataless));
    assert_eq!(decide(&cloud, true), Descend::Skip(SkipReason::Dataless));

    // Other flag bits alongside SF_DATALESS do not change that.
    let cloud_hidden = Rec {
        flags: DATALESS | 0x8000,
        ..Rec::dir("hidden cloud")
    };
    assert_eq!(
        decide(&cloud_hidden, false),
        Descend::Skip(SkipReason::Dataless)
    );

    // A dataless mount point below the root is skipped either way.
    let both = Rec {
        flags: DATALESS,
        mount_status: Some(MNTPOINT),
        ..Rec::dir("both")
    };
    assert!(matches!(decide(&both, false), Descend::Skip(_)));

    // Non-dataless flags alone do not block descent.
    let hidden = Rec {
        flags: 0x8000,
        ..Rec::dir("hidden")
    };
    assert_eq!(decide(&hidden, false), Descend::Descend);
}

#[test]
fn entries_with_errors_are_not_descended() {
    let rec = Rec {
        error: Some(EACCES),
        ..Rec::dir("private")
    };
    assert_eq!(decide(&rec, false), Descend::Skip(SkipReason::EntryError));
    assert_eq!(decide(&rec, true), Descend::Skip(SkipReason::EntryError));
}
