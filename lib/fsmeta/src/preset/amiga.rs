//! `AmigaDOS` / FFS preset conversions.
//!
//! An Amiga file carries an 8-bit protection mask (conventionally written
//! `hsparwed`) and an optional file comment. This module represents the
//! protection mask as its canonical eight-character string — each position
//! shows its letter when the bit is set and `-` when clear — which round-trips
//! the raw byte losslessly, and bounds the comment to the `AmigaDOS` limit.

use tairix_abi::time::Time64;

use crate::MetadataError;

/// Longest `AmigaDOS` file comment, in bytes.
pub const MAX_COMMENT_LEN: usize = 79;

/// Seconds from the Unix epoch to `AmigaDOS`'s, 1978-01-01.
const AMIGA_EPOCH_SECS: i64 = 2922 * 86_400;

/// Date-stamp ticks a second.
pub const TICKS_PER_SECOND: u32 = 50;

const NANOS_PER_TICK: u32 = 1_000_000_000 / TICKS_PER_SECOND;

/// An `AmigaDOS` date stamp as an instant: `days` since 1978-01-01, `mins`
/// past midnight and `ticks` (fiftieths of a second) past the minute. The
/// format records no zone; the fields are read as UTC civil time.
///
/// # Errors
///
/// [`MetadataError::NotRepresentable`] for minutes past a day's or ticks past
/// a minute's.
pub fn datestamp_to_time64(days: u32, mins: u32, ticks: u32) -> Result<Time64, MetadataError> {
    if mins >= 24 * 60 || ticks >= 60 * TICKS_PER_SECOND {
        return Err(MetadataError::NotRepresentable);
    }
    let secs = AMIGA_EPOCH_SECS
        + i64::from(days) * 86_400
        + i64::from(mins) * 60
        + i64::from(ticks / TICKS_PER_SECOND);
    Time64::new(secs, (ticks % TICKS_PER_SECOND) * NANOS_PER_TICK)
        .map_err(|_| MetadataError::NotRepresentable)
}

/// The date stamp `(days, mins, ticks)` of `time`, checked.
///
/// # Errors
///
/// [`MetadataError::TimestampOutOfRange`] for an instant before 1978, past
/// the last day a stamp can count, or carrying precision finer than a tick
/// (never silently dropped).
pub fn time64_to_datestamp(time: Time64) -> Result<(u32, u32, u32), MetadataError> {
    if !time.subsec_nanos().is_multiple_of(NANOS_PER_TICK) {
        return Err(MetadataError::TimestampOutOfRange);
    }
    let since = time
        .secs()
        .checked_sub(AMIGA_EPOCH_SECS)
        .filter(|&since| since >= 0)
        .ok_or(MetadataError::TimestampOutOfRange)?;
    let days = u32::try_from(since / 86_400).map_err(|_| MetadataError::TimestampOutOfRange)?;
    let rest = u32::try_from(since % 86_400).map_err(|_| MetadataError::TimestampOutOfRange)?;
    Ok((
        days,
        rest / 60,
        (rest % 60) * TICKS_PER_SECOND + time.subsec_nanos() / NANOS_PER_TICK,
    ))
}

/// `time` with the precision finer than a tick taken off, as a date stamp
/// records the moment an object changes.
#[must_use]
pub fn to_tick(time: Time64) -> Time64 {
    let nanos = time.subsec_nanos();
    Time64::new(time.secs(), nanos - nanos % NANOS_PER_TICK).unwrap_or(time)
}

/// The protection letters, most-significant bit (`h`, bit 7) first.
const LETTERS: [u8; 8] = *b"hsparwed";

/// Render an 8-bit protection mask as its canonical `hsparwed` string, with
/// `-` for each clear bit.
#[must_use]
pub fn protection_to_value(bits: u8) -> [u8; 8] {
    let mut out = [b'-'; 8];
    for (i, slot) in out.iter_mut().enumerate() {
        let bit = 7 - u32::try_from(i).unwrap_or(0);
        if bits & (1 << bit) != 0 {
            *slot = LETTERS[i];
        }
    }
    out
}

/// Parse a canonical `hsparwed` protection string back into its 8-bit mask.
///
/// Each of the eight positions must be either its expected letter or `-`.
///
/// # Errors
///
/// [`MetadataError::NotRepresentable`] if the value is not exactly eight
/// characters or a position is neither its letter nor `-`.
pub fn protection_from_value(value: &[u8]) -> Result<u8, MetadataError> {
    if value.len() != 8 {
        return Err(MetadataError::NotRepresentable);
    }
    let mut bits = 0u8;
    for (i, &byte) in value.iter().enumerate() {
        let bit = 7 - u32::try_from(i).unwrap_or(0);
        if byte == LETTERS[i] {
            bits |= 1 << bit;
        } else if byte != b'-' {
            return Err(MetadataError::NotRepresentable);
        }
    }
    Ok(bits)
}

/// Validate a file comment against the `AmigaDOS` length limit. The comment is
/// stored verbatim as the `amiga.comment` value.
///
/// # Errors
///
/// [`MetadataError::ValueTooLong`] if `comment` exceeds [`MAX_COMMENT_LEN`].
pub fn validate_comment(comment: &[u8]) -> Result<(), MetadataError> {
    if comment.len() > MAX_COMMENT_LEN {
        return Err(MetadataError::ValueTooLong);
    }
    Ok(())
}

/// The protection key.
pub const PROTECTION_KEY: &[u8] = b"amiga.protection";
/// The comment key.
pub const COMMENT_KEY: &[u8] = b"amiga.comment";

/// The `amiga` keys an object's fields are surfaced as, in listing order.
pub const KEYS: [&[u8]; 2] = [PROTECTION_KEY, COMMENT_KEY];

/// What `AmigaDOS` records of an object beyond its name and dates: the low
/// byte of its protection bits and its comment. The one reading every format
/// that keeps them surfaces them through, so an Amiga volume and a disc's Amiga
/// records present an object identically.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Object<'a> {
    /// The protection bits' low byte, raw, or `None` where none are recorded.
    pub protection: Option<u8>,
    /// The comment, empty for none.
    pub comment: &'a [u8],
}

/// The canonical value of one `amiga` key.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Value<'a> {
    /// The protection mask's `hsparwed` string.
    Protection([u8; 8]),
    /// The comment's bytes.
    Comment(&'a [u8]),
}

impl Value<'_> {
    /// The value's bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Protection(text) => text,
            Self::Comment(comment) => comment,
        }
    }
}

impl<'a> Object<'a> {
    /// Whether the object carries `key`: its protection where it is recorded,
    /// its comment where it has one.
    #[must_use]
    pub fn carries(&self, key: &[u8]) -> bool {
        match key {
            PROTECTION_KEY => self.protection.is_some(),
            COMMENT_KEY => !self.comment.is_empty(),
            _ => false,
        }
    }

    /// The `index`-th key the object carries, in [`KEYS`] order.
    #[must_use]
    pub fn key(&self, index: u64) -> Option<&'static [u8]> {
        KEYS.into_iter()
            .filter(|key| self.carries(key))
            .nth(usize::try_from(index).ok()?)
    }

    /// The canonical value of `key`, or `None` when the object does not carry
    /// it.
    #[must_use]
    pub fn value(&self, key: &[u8]) -> Option<Value<'a>> {
        match key {
            PROTECTION_KEY => self
                .protection
                .map(|bits| Value::Protection(protection_to_value(bits))),
            COMMENT_KEY if !self.comment.is_empty() => Some(Value::Comment(self.comment)),
            _ => None,
        }
    }
}
