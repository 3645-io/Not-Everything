//! Size values for `size:`: numbers with units, and the named constants.
//!
//! Readings, in our own words after voidtools' public Search Functions and
//! Searching pages:
//!
//! - Units are `b`, `kb`, `mb`, `gb` and `tb`, case-insensitive, in steps of
//!   1024. A bare number is bytes. A number may have decimals
//!   (`1.5mb` is 1 572 864 bytes); fractions of a byte are dropped.
//! - A written size stands for a bucket at the precision it was written
//!   with: one step of its unit, or of its last decimal digit. `1kb` is
//!   1024..=2047, `1mb` is 1 MiB up to 2 MiB - 1, `1.5mb` is 1.5 MiB up to
//!   1.6 MiB - 1, and `1500` (bytes) is just 1500. [`SizeBucket`] holds both
//!   ends; [`super::functions`] turns a comparison against a bucket into a
//!   byte test.
//! - Constants (see [`SizeConstant`]), each band including its upper
//!   bound: `empty` is 0, `tiny` 0 to 10 KB, `small` above 10 KB up to
//!   100 KB, `medium` above 100 KB up to 1 MB, `large` above 1 MB up to
//!   16 MB, `huge` above 16 MB up to 128 MB, `gigantic` above 128 MB.
//!   `unknown` (a size that is not known) is handled by
//!   [`super::functions::SizeTest::Unknown`].

/// Bytes in a kilobyte.
pub const KB: u64 = 1024;
/// Bytes in a megabyte.
pub const MB: u64 = 1024 * KB;
/// Bytes in a gigabyte.
pub const GB: u64 = 1024 * MB;
/// Bytes in a terabyte.
pub const TB: u64 = 1024 * GB;

/// Most decimal digits a size may have; more are rejected rather than
/// risking overflow in the arithmetic.
const MAX_DECIMALS: u32 = 18;

/// A written size and the range of byte counts it stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SizeBucket {
    /// The exact value written, in bytes: the bucket's first byte count.
    pub start: u64,
    /// The bucket's last byte count (equal to `start` for plain bytes).
    pub end: u64,
}

/// Why a size could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SizeError {
    /// Not a number with an optional known unit.
    Invalid,
    /// The value does not fit in 64 bits.
    TooLarge,
}

/// Parses a size such as `1500`, `1kb`, `1.5MB` or `2 tb` (no inner
/// spaces in practice, since the lexer splits on them).
pub fn parse_size(text: &str) -> Result<SizeBucket, SizeError> {
    let digits_end = text
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(digits_end);
    let unit = match unit.to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "kb" => KB,
        "mb" => MB,
        "gb" => GB,
        "tb" => TB,
        _ => return Err(SizeError::Invalid),
    };
    let (int_part, frac_part) = match number.split_once('.') {
        Some((i, f)) => (i, f),
        None => (number, ""),
    };
    let well_formed = !int_part.is_empty()
        && !frac_part.contains('.')
        && !(number.contains('.') && frac_part.is_empty());
    if !well_formed {
        return Err(SizeError::Invalid);
    }
    let decimals = u32::try_from(frac_part.len()).map_err(|_| SizeError::TooLarge)?;
    if decimals > MAX_DECIMALS {
        return Err(SizeError::TooLarge);
    }
    let scale = 10u128.pow(decimals);
    let int: u128 = int_part.parse().map_err(|_| SizeError::TooLarge)?;
    let frac: u128 = if frac_part.is_empty() {
        0
    } else {
        frac_part.parse().map_err(|_| SizeError::Invalid)?
    };
    let unit = u128::from(unit);
    let scaled = int
        .checked_mul(scale)
        .and_then(|v| v.checked_add(frac))
        .and_then(|v| v.checked_mul(unit))
        .ok_or(SizeError::TooLarge)?;
    let start = scaled / scale;
    let width = (unit / scale).max(1);
    let start = u64::try_from(start).map_err(|_| SizeError::TooLarge)?;
    let end = u128::from(start) + width - 1;
    let end = u64::try_from(end).unwrap_or(u64::MAX);
    Ok(SizeBucket { start, end })
}

/// A named size band.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SizeConstant {
    /// 0 bytes.
    Empty,
    /// 0 to 10 KB.
    Tiny,
    /// Above 10 KB, up to 100 KB.
    Small,
    /// Above 100 KB, up to 1 MB.
    Medium,
    /// Above 1 MB, up to 16 MB.
    Large,
    /// Above 16 MB, up to 128 MB.
    Huge,
    /// Above 128 MB.
    Gigantic,
}

impl SizeConstant {
    /// The constant for a word, case-insensitively.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name.to_ascii_lowercase().as_str() {
            "empty" => SizeConstant::Empty,
            "tiny" => SizeConstant::Tiny,
            "small" => SizeConstant::Small,
            "medium" => SizeConstant::Medium,
            "large" => SizeConstant::Large,
            "huge" => SizeConstant::Huge,
            "gigantic" => SizeConstant::Gigantic,
            _ => return None,
        })
    }

    /// The band as an inclusive byte range.
    pub fn bounds(self) -> (u64, u64) {
        match self {
            SizeConstant::Empty => (0, 0),
            SizeConstant::Tiny => (0, 10 * KB),
            SizeConstant::Small => (10 * KB + 1, 100 * KB),
            SizeConstant::Medium => (100 * KB + 1, MB),
            SizeConstant::Large => (MB + 1, 16 * MB),
            SizeConstant::Huge => (16 * MB + 1, 128 * MB),
            SizeConstant::Gigantic => (128 * MB + 1, u64::MAX),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(start: u64, end: u64) -> Result<SizeBucket, SizeError> {
        Ok(SizeBucket { start, end })
    }

    #[test]
    fn buckets() {
        assert_eq!(parse_size("1500"), b(1500, 1500));
        assert_eq!(parse_size("1kb"), b(KB, 2 * KB - 1));
        assert_eq!(parse_size("1KB"), b(KB, 2 * KB - 1));
        assert_eq!(parse_size("7B"), b(7, 7));
        assert_eq!(parse_size("1.5kb"), b(1536, 1536 + 102 - 1));
        assert_eq!(parse_size("0.5kb"), b(512, 512 + 101));
        assert_eq!(parse_size("1tb"), b(TB, 2 * TB - 1));
    }

    #[test]
    fn errors() {
        for bad in ["", "abc", "1xb", "1.", ".5", "1..2", "1e5", "-1", "kb"] {
            assert_eq!(parse_size(bad), Err(SizeError::Invalid), "{bad:?}");
        }
        assert_eq!(
            parse_size("99999999999999999999tb"),
            Err(SizeError::TooLarge)
        );
        assert_eq!(parse_size("16777216tb"), Err(SizeError::TooLarge));
    }
}
