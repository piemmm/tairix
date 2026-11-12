//! A machine's disks as the front-end serves them: an image read and written
//! where its sectors lie, a run of them at a time or one.

use alloc::vec::Vec;

use crate::storage::sparse::SparseDisk;
use crate::storage::{BlockDevice, MediaError, SECTOR};

/// Bytes in a sector, as an image's offsets count them.
const SECTOR_BYTES: u64 = SECTOR as u64;

/// An image the front-end holds: bytes read and written at offsets.
pub trait Medium {
    /// Read up to `out.len()` bytes from `offset`: how many there were, fewer
    /// past the image's end.
    ///
    /// # Errors
    ///
    /// [`MediaError::Failed`] for an image that could not be read.
    fn read_at(&self, offset: u64, out: &mut [u8]) -> Result<usize, MediaError>;

    /// Write `data` at `offset`.
    ///
    /// # Errors
    ///
    /// [`MediaError::Failed`] for an image that could not be written.
    fn write_at(&self, offset: u64, data: &[u8]) -> Result<(), MediaError>;

    /// Make the image `len` bytes long.
    ///
    /// # Errors
    ///
    /// [`MediaError::Failed`] for an image that could not be resized.
    fn set_len(&self, len: u64) -> Result<(), MediaError>;
}

/// `count` sectors of `medium` from `first` into `out`, what lies past its
/// end read as zeros.
///
/// # Errors
///
/// [`MediaError::OutOfRange`] for a run whose offset overflows, and the
/// medium's own refusal.
pub fn fetch(
    medium: &impl Medium,
    first: u64,
    count: u16,
    out: &mut Vec<u8>,
) -> Result<(), MediaError> {
    let len = usize::from(count) * SECTOR;
    let offset = first
        .checked_mul(SECTOR_BYTES)
        .ok_or(MediaError::OutOfRange)?;
    out.clear();
    out.try_reserve_exact(len).map_err(|_| MediaError::Failed)?;
    out.resize(len, 0);
    let mut done = 0;
    while done < len {
        let read = medium.read_at(offset + done as u64, &mut out[done..])?;
        if read == 0 {
            break;
        }
        done += read;
    }
    Ok(())
}

/// Sector `sector` of `medium` written with `data`.
///
/// # Errors
///
/// [`MediaError::OutOfRange`] for a sector whose offset overflows, and the
/// medium's own refusal.
pub fn store(medium: &impl Medium, sector: u64, data: &[u8; SECTOR]) -> Result<(), MediaError> {
    let offset = sector
        .checked_mul(SECTOR_BYTES)
        .ok_or(MediaError::OutOfRange)?;
    medium.write_at(offset, data)
}

/// `disk` laid into `medium` in place of whatever it held: emptied, the
/// sectors that hold anything written, then its whole length, so an image that
/// keeps holes costs only what the disk holds.
///
/// # Errors
///
/// The medium's refusal of a write or of the length.
pub fn lay(medium: &impl Medium, disk: &SparseDisk) -> Result<(), MediaError> {
    medium.set_len(0)?;
    for (sector, data) in disk.sectors_held() {
        store(medium, sector, data)?;
    }
    let len = disk
        .sectors()
        .checked_mul(SECTOR_BYTES)
        .ok_or(MediaError::OutOfRange)?;
    medium.set_len(len)
}

#[cfg(test)]
#[path = "disk_tests.rs"]
mod tests;
