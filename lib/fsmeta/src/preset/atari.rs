//! Atari GEMDOS / TOS preset conversions.
//!
//! Atari TOS uses a FAT-derived on-disk format. A file carries an 8-bit
//! GEMDOS attribute byte and a FAT-style packed date/time with two-second
//! resolution and a 1980 epoch. The registry keeps the Atari attributes
//! distinct from a generic FAT mapping so intent is not lost, and converts the
//! packed date/time to and from a [`Time64`] through [`DosTime`], refusing
//! what it cannot hold exactly.

use tairix_abi::time::{DosTime, Time64};

use crate::MetadataError;

/// The GEMDOS attribute bits, which are FAT's.
pub use tairix_fat::dir::attr::{
    ARCHIVE as ATTR_ARCHIVE, DIRECTORY as ATTR_DIRECTORY, HIDDEN as ATTR_HIDDEN,
    READ_ONLY as ATTR_READ_ONLY, SYSTEM as ATTR_SYSTEM, VOLUME_ID as ATTR_VOLUME_LABEL,
};

/// Every defined GEMDOS attribute bit.
const ATTR_KNOWN_MASK: u8 =
    ATTR_READ_ONLY | ATTR_HIDDEN | ATTR_SYSTEM | ATTR_VOLUME_LABEL | ATTR_DIRECTORY | ATTR_ARCHIVE;

/// Validate a raw GEMDOS attribute byte, rejecting unknown bits. The byte is
/// stored verbatim as the `atari.attributes` value.
///
/// # Errors
///
/// [`MetadataError::NotRepresentable`] if any bit outside the known mask is
/// set.
pub fn validate_attributes(bits: u8) -> Result<(), MetadataError> {
    if bits & !ATTR_KNOWN_MASK != 0 {
        return Err(MetadataError::NotRepresentable);
    }
    Ok(())
}

/// Convert a FAT-style packed `(date, time)` pair to a [`Time64`].
///
/// # Errors
///
/// [`MetadataError::NotRepresentable`] if the fields name no real date and
/// time: a month, day, hour, minute or second out of range, or a day its
/// month does not have.
pub fn datetime_to_time64(date: u16, time: u16) -> Result<Time64, MetadataError> {
    DosTime { date, time }
        .to_time64()
        .ok_or(MetadataError::NotRepresentable)
}

/// Convert a [`Time64`] to a FAT-style packed `(date, time)` pair, checked.
///
/// # Errors
///
/// [`MetadataError::TimestampOutOfRange`] if the instant falls outside the
/// 1980..=2107 range the FAT date field can encode, or carries sub-second
/// (nanosecond) or odd-second precision the two-second FAT time field cannot
/// represent (never silently dropped).
pub fn time64_to_datetime(time: Time64) -> Result<(u16, u16), MetadataError> {
    if time.subsec_nanos() != 0 || time.secs().rem_euclid(2) != 0 {
        return Err(MetadataError::TimestampOutOfRange);
    }
    let DosTime { date, time } =
        DosTime::from_time64(time).ok_or(MetadataError::TimestampOutOfRange)?;
    Ok((date, time))
}
