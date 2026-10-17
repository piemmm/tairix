//! An image served as a [`Block`] device whose 512-byte logical sector is
//! distinct from the filesystem block the image holds.

use tairix_abi::driver::block::{Block, BlockGeometry};
use tairix_abi::DriverError;

const SECTOR: usize = 512;

/// An in-memory image as a block device.
pub struct MemBlock {
    /// The image.
    pub data: Vec<u8>,
}

impl Block for MemBlock {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        Ok(BlockGeometry {
            block_size: u32::try_from(SECTOR).expect("a sector fits"),
            block_count: u64::try_from(self.data.len() / SECTOR).expect("fits"),
        })
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let start = usize::try_from(lba).expect("small") * SECTOR;
        buf.copy_from_slice(&self.data[start..start + buf.len()]);
        Ok(())
    }

    fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), DriverError> {
        let start = usize::try_from(lba).expect("small") * SECTOR;
        self.data[start..start + buf.len()].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}
