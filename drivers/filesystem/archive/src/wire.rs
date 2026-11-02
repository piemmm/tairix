//! Little-endian fields read from and written to an archive's records.

use alloc::vec::Vec;

use tairix_abi::DriverError;

/// The little-endian `u16` at `at`, or `None` past the end.
pub(crate) fn le16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}

/// The little-endian `u32` at `at`, or `None` past the end.
pub(crate) fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

/// The little-endian `u64` at `at`, or `None` past the end.
pub(crate) fn le64(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(at..at.checked_add(8)?)?.try_into().ok()?,
    ))
}

/// A record being written into room reserved for it up front, so writing a
/// field never allocates; a record that outgrows its room is refused when it
/// is finished.
pub(crate) struct Out {
    bytes: Vec<u8>,
    overflowed: bool,
}

impl Out {
    /// A record of at most `len` bytes.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfMemory`].
    pub(crate) fn with_capacity(len: usize) -> Result<Self, DriverError> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(len)
            .map_err(|_| DriverError::OutOfMemory)?;
        Ok(Self {
            bytes,
            overflowed: false,
        })
    }

    pub(crate) fn u16(&mut self, value: u16) {
        self.bytes(&value.to_le_bytes());
    }

    pub(crate) fn u32(&mut self, value: u32) {
        self.bytes(&value.to_le_bytes());
    }

    pub(crate) fn u64(&mut self, value: u64) {
        self.bytes(&value.to_le_bytes());
    }

    /// Append `bytes`, if the room reserved holds them.
    pub(crate) fn bytes(&mut self, bytes: &[u8]) {
        if self.bytes.capacity() - self.bytes.len() < bytes.len() {
            self.overflowed = true;
        } else {
            self.bytes.extend_from_slice(bytes);
        }
    }

    /// How long the record is so far.
    pub(crate) fn len(&self) -> usize {
        self.bytes.len()
    }

    /// The record.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] for one that outgrew its room: a field
    /// left out would make it say something else.
    pub(crate) fn finish(self) -> Result<Vec<u8>, DriverError> {
        if self.overflowed {
            return Err(DriverError::DeviceFault);
        }
        Ok(self.bytes)
    }
}
