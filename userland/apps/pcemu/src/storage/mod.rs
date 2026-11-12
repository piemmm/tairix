//! Disks: the block devices behind a machine's drives, floppy media as their
//! tracks and sector IDs, and the controllers that read them.

pub mod fdc;
pub mod floppy;
pub mod hdc;
pub mod imd;
pub mod sparse;

use alloc::vec::Vec;

/// Bytes in a fixed disk's sector, and in every raw floppy's.
pub const SECTOR: usize = 512;

/// Why a disk access failed: what the drive would report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaError {
    /// The sector is past the end of the medium.
    OutOfRange,
    /// The medium is write-protected.
    WriteProtected,
    /// The medium could not be read or written: a bad sector to the guest.
    Failed,
}

/// A medium addressed by 512-byte sector: a fixed disk, or a raw floppy
/// image. The machine worker's implementation fetches from the front-end;
/// a write returns only once it is durable, so a failed write is the
/// guest's disk error.
pub trait BlockDevice {
    /// Sectors the medium holds.
    fn sectors(&self) -> u64;

    /// Whether writes are refused.
    fn read_only(&self) -> bool;

    /// Reads sector `sector`.
    fn read(&mut self, sector: u64, out: &mut [u8; SECTOR]) -> Result<(), MediaError>;

    /// Writes sector `sector`.
    fn write(&mut self, sector: u64, data: &[u8; SECTOR]) -> Result<(), MediaError>;
}

/// A medium held in memory.
#[derive(Clone, Debug)]
pub struct MemoryDisk {
    bytes: Vec<u8>,
    read_only: bool,
}

impl MemoryDisk {
    /// A medium of `bytes`, which must be whole sectors.
    #[must_use]
    pub fn new(bytes: Vec<u8>, read_only: bool) -> Option<Self> {
        bytes
            .len()
            .is_multiple_of(SECTOR)
            .then_some(Self { bytes, read_only })
    }

    /// The medium's bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The medium's bytes, given up.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    fn range(&self, sector: u64) -> Result<core::ops::Range<usize>, MediaError> {
        let start = usize::try_from(sector)
            .ok()
            .and_then(|s| s.checked_mul(SECTOR))
            .ok_or(MediaError::OutOfRange)?;
        let end = start
            .checked_add(SECTOR)
            .filter(|&end| end <= self.bytes.len())
            .ok_or(MediaError::OutOfRange)?;
        Ok(start..end)
    }
}

impl BlockDevice for MemoryDisk {
    fn sectors(&self) -> u64 {
        (self.bytes.len() / SECTOR) as u64
    }

    fn read_only(&self) -> bool {
        self.read_only
    }

    fn read(&mut self, sector: u64, out: &mut [u8; SECTOR]) -> Result<(), MediaError> {
        let range = self.range(sector)?;
        out.copy_from_slice(&self.bytes[range]);
        Ok(())
    }

    fn write(&mut self, sector: u64, data: &[u8; SECTOR]) -> Result<(), MediaError> {
        if self.read_only {
            return Err(MediaError::WriteProtected);
        }
        let range = self.range(sector)?;
        self.bytes[range].copy_from_slice(data);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn a_memory_disk_reads_writes_and_refuses_out_of_range() {
        let mut disk = MemoryDisk::new(vec![0; SECTOR * 4], false).unwrap();
        disk.write(3, &[0x5A; SECTOR]).unwrap();
        let mut out = [0; SECTOR];
        disk.read(3, &mut out).unwrap();
        assert_eq!(out, [0x5A; SECTOR]);
        assert_eq!(disk.read(4, &mut out), Err(MediaError::OutOfRange));
        assert_eq!(disk.read(u64::MAX, &mut out), Err(MediaError::OutOfRange));
        let mut locked = MemoryDisk::new(vec![0; SECTOR], true).unwrap();
        assert_eq!(
            locked.write(0, &[1; SECTOR]),
            Err(MediaError::WriteProtected)
        );
        assert!(MemoryDisk::new(vec![0; 100], false).is_none());
    }
}
