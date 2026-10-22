//! Classic Mac OS (HFS / HFS+) preset conversions.
//!
//! A classic-Mac file carries a four-character type code and creator code
//! (`OSType`s), a 16-bit Finder-flags field, and an optional resource fork.
//! Type and creator are stored verbatim as their four raw bytes; the Finder
//! flags are stored big-endian (their native HFS order). The resource fork is
//! *not* an attribute value — it is a named stream addressed by
//! [`RESOURCE_FORK_KEY`] and stored through the file-data pipeline.

use crate::MetadataError;

/// Length of an `OSType` (type or creator code), in bytes.
pub const OSTYPE_LEN: usize = 4;

/// The attribute key naming the resource fork's named stream.
pub const RESOURCE_FORK_KEY: &[u8] = b"mac.resourcefork";

/// Validate a four-byte `OSType` value (`mac.type` / `mac.creator`), stored
/// verbatim.
///
/// # Errors
///
/// [`MetadataError::NotRepresentable`] if the value is not exactly four bytes.
pub fn validate_ostype(value: &[u8]) -> Result<(), MetadataError> {
    if value.len() != OSTYPE_LEN {
        return Err(MetadataError::NotRepresentable);
    }
    Ok(())
}

/// Encode the 16-bit Finder flags as their canonical big-endian
/// `mac.finderflags` value.
#[must_use]
pub fn finderflags_to_value(flags: u16) -> [u8; 2] {
    flags.to_be_bytes()
}

/// Parse a `mac.finderflags` value (two big-endian bytes) back into the 16-bit
/// flags field.
///
/// # Errors
///
/// [`MetadataError::NotRepresentable`] if the value is not exactly two bytes.
pub fn finderflags_from_value(value: &[u8]) -> Result<u16, MetadataError> {
    let bytes: [u8; 2] = value
        .try_into()
        .map_err(|_| MetadataError::NotRepresentable)?;
    Ok(u16::from_be_bytes(bytes))
}

/// The Finder information keys, in listing order.
pub const KEYS: [&[u8]; 3] = [b"mac.type", b"mac.creator", b"mac.finderflags"];

/// Longest Finder information value: an `OSType`.
pub const FINDER_VALUE_MAX: usize = OSTYPE_LEN;

/// What a classic-Mac format records of one file or folder: its type and
/// creator codes and its Finder flags, each zero when it records none. The
/// one reading every format that keeps them surfaces them through, so an
/// Apple CD and a UDF volume present a file identically.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct FinderInfo {
    /// The type code, in its natural byte order.
    pub file_type: [u8; OSTYPE_LEN],
    /// The creator code, in its natural byte order.
    pub creator: [u8; OSTYPE_LEN],
    /// The Finder flags.
    pub flags: u16,
}

impl FinderInfo {
    /// Whether the information records `key`: a code or flags not zero.
    #[must_use]
    pub fn carries(&self, key: &[u8]) -> bool {
        match key {
            b"mac.type" => self.file_type != [0; OSTYPE_LEN],
            b"mac.creator" => self.creator != [0; OSTYPE_LEN],
            b"mac.finderflags" => self.flags != 0,
            _ => false,
        }
    }

    /// The `index`-th key the information records, in [`KEYS`] order.
    #[must_use]
    pub fn key(&self, index: u64) -> Option<&'static [u8]> {
        KEYS.into_iter()
            .filter(|key| self.carries(key))
            .nth(usize::try_from(index).ok()?)
    }

    /// The canonical value of `key`, written into `out`, or `None` when the
    /// information does not record it.
    #[must_use]
    pub fn value(&self, key: &[u8], out: &mut [u8; FINDER_VALUE_MAX]) -> Option<usize> {
        if !self.carries(key) {
            return None;
        }
        let flags = finderflags_to_value(self.flags);
        let value: &[u8] = match key {
            b"mac.type" => &self.file_type,
            b"mac.creator" => &self.creator,
            _ => &flags,
        };
        out[..value.len()].copy_from_slice(value);
        Some(value.len())
    }

    /// Set `key` from its canonical `value`, answering `Ok(false)` for a key
    /// that is not Finder information.
    ///
    /// # Errors
    ///
    /// [`MetadataError::NotRepresentable`] for a value not in the key's
    /// canonical form.
    pub fn set(&mut self, key: &[u8], value: &[u8]) -> Result<bool, MetadataError> {
        let code = |value: &[u8]| -> Result<[u8; OSTYPE_LEN], MetadataError> {
            validate_ostype(value)?;
            value
                .try_into()
                .map_err(|_| MetadataError::NotRepresentable)
        };
        match key {
            b"mac.type" => self.file_type = code(value)?,
            b"mac.creator" => self.creator = code(value)?,
            b"mac.finderflags" => self.flags = finderflags_from_value(value)?,
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Clear `key`, answering whether the information recorded it.
    pub fn clear(&mut self, key: &[u8]) -> bool {
        let carried = self.carries(key);
        match key {
            b"mac.type" => self.file_type = [0; OSTYPE_LEN],
            b"mac.creator" => self.creator = [0; OSTYPE_LEN],
            b"mac.finderflags" => self.flags = 0,
            _ => {}
        }
        carried
    }
}
