//! The packed size/kind/flags column (docs/PLAN.md, data-layout table:
//! "Size, type, flags (packed)", 8 bytes per entry).
//!
//! Bit layout of one `u64`:
//!
//! | Bits    | Field                                                      |
//! | ------- | ---------------------------------------------------------- |
//! | 0..=55  | Size in bytes, saturating at `2^56 - 1` (64 PiB)           |
//! | 56..=59 | [`EntryFlags`] bits (`HIDDEN`, `SYSTEM`, `LOCKED`, `DATALESS`) |
//! | 60..=61 | [`EntryKind`]: 0 file, 1 dir, 2 symlink                     |
//! | 62..=63 | Reserved, always 0                                         |
//!
//! Clean-room: written from docs/PLAN.md only.

use fx_model::{EntryFlags, EntryKind};

/// Number of low bits holding the size.
pub const SIZE_BITS: u32 = 56;
/// Largest storable size; larger sizes saturate to this value.
pub const MAX_SIZE: u64 = (1 << SIZE_BITS) - 1;
const FLAGS_SHIFT: u32 = 56;
const FLAGS_MASK: u64 = 0xF;
const KIND_SHIFT: u32 = 60;
const KIND_MASK: u64 = 0x3;

/// One entry's size, kind and flags packed into a `u64`; see the
/// [module docs](self) for the bit layout.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct PackedMeta(pub u64);

impl PackedMeta {
    /// Packs a size (saturating at [`MAX_SIZE`]), kind and flags.
    pub fn new(size: u64, kind: EntryKind, flags: EntryFlags) -> Self {
        let kind_bits: u64 = match kind {
            EntryKind::File => 0,
            EntryKind::Dir => 1,
            EntryKind::Symlink => 2,
        };
        PackedMeta(
            size.min(MAX_SIZE)
                | ((u64::from(flags.bits()) & FLAGS_MASK) << FLAGS_SHIFT)
                | (kind_bits << KIND_SHIFT),
        )
    }

    /// The stored size in bytes (saturated at [`MAX_SIZE`]).
    pub fn size(self) -> u64 {
        self.0 & MAX_SIZE
    }

    /// The entry kind. The unused kind value 3 never appears in values built
    /// by [`PackedMeta::new`] and reads back as a file.
    pub fn kind(self) -> EntryKind {
        match (self.0 >> KIND_SHIFT) & KIND_MASK {
            1 => EntryKind::Dir,
            2 => EntryKind::Symlink,
            _ => EntryKind::File,
        }
    }

    /// The attribute flags.
    pub fn flags(self) -> EntryFlags {
        EntryFlags::from_bits_truncate(((self.0 >> FLAGS_SHIFT) & FLAGS_MASK) as u8)
    }
}
