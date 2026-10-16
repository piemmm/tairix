//! Inode records: where one lives, reading it through the staging layer and
//! checking its checksum when it comes from the device, and writing it back
//! sealed.

use tairix_abi::driver::block::Block;
use tairix_abi::DriverError;

use crate::bytes::{crc32c_zeroed, le16, le32, low32, put_le16, put_le32};
use crate::Ext4;

/// `l_i_checksum_lo` and `i_checksum_hi`.
const CHECKSUM_LO: usize = 0x7C;
const CHECKSUM_HI: usize = 0x82;
/// `i_generation`.
pub(crate) const GENERATION: usize = 0x64;
/// `i_extra_isize`, just past the classic 128-byte record.
pub(crate) const EXTRA_ISIZE: usize = 128;

impl<B: Block> Ext4<B> {
    /// The checksum seed of inode `ino` at `generation`, which its extent
    /// blocks and directory blocks are sealed under too.
    pub(crate) fn inode_csum_seed(&self, ino: u32, generation: u32) -> u32 {
        let c = tairix_crc32c::update(self.layout.csum_seed, &ino.to_le_bytes());
        tairix_crc32c::update(c, &generation.to_le_bytes())
    }

    /// The table block holding inode `ino`, and the record's offset in it.
    pub(crate) fn inode_location(&mut self, ino: u32) -> Result<(u64, usize), DriverError> {
        if ino == 0 || ino > self.layout.inodes_count {
            return Err(DriverError::NotFound);
        }
        let ipg = u64::from(self.layout.inodes_per_group);
        let index = u64::from(ino - 1);
        let desc = self.group_desc(index / ipg)?;
        let byte = (index % ipg) * u64::from(self.layout.inode_size);
        let bs = u64::from(self.layout.block_size);
        let block = desc
            .inode_table
            .checked_add(byte / bs)
            .filter(|&block| block < self.layout.blocks_count)
            .ok_or(DriverError::DeviceFault)?;
        Ok((block, low32(byte % bs) as usize))
    }

    fn has_checksum_hi(raw: &[u8]) -> bool {
        raw.len() > EXTRA_ISIZE
            && usize::from(le16(raw, EXTRA_ISIZE)) >= CHECKSUM_HI + 2 - EXTRA_ISIZE
    }

    fn inode_checksum(&self, ino: u32, raw: &[u8]) -> u32 {
        let seed = self.inode_csum_seed(ino, le32(raw, GENERATION));
        if !Self::has_checksum_hi(raw) {
            return crc32c_zeroed::<2>(seed, raw, CHECKSUM_LO);
        }
        let (low, high) = raw.split_at(CHECKSUM_HI.min(raw.len()));
        crc32c_zeroed::<2>(crc32c_zeroed::<2>(seed, low, CHECKSUM_LO), high, 0)
    }

    /// Whether a record read from the device carries the checksum it should.
    fn inode_sound(&self, ino: u32, raw: &[u8]) -> bool {
        if !self.layout.metadata_csum {
            return true;
        }
        let sum = self.inode_checksum(ino, raw);
        let stored = if Self::has_checksum_hi(raw) {
            u32::from(le16(raw, CHECKSUM_LO)) | (u32::from(le16(raw, CHECKSUM_HI)) << 16)
        } else {
            u32::from(le16(raw, CHECKSUM_LO))
        };
        let mask = if Self::has_checksum_hi(raw) {
            u32::MAX
        } else {
            0xFFFF
        };
        stored == sum & mask
    }

    /// Read inode `ino`'s whole record into `raw[..inode_size]`, its checksum
    /// verified: a staged table block holds the records the transaction did
    /// not write exactly as the device did.
    pub(crate) fn read_inode_raw(&mut self, ino: u32, raw: &mut [u8]) -> Result<(), DriverError> {
        let (block, off) = self.inode_location(ino)?;
        let len = self.layout.inode_size as usize;
        let raw = raw.get_mut(..len).ok_or(DriverError::BufferTooSmall)?;
        if let Some(staged) = self.dirty.staged(block) {
            raw.copy_from_slice(&staged[off..off + len]);
        } else {
            let at = block
                .checked_mul(u64::from(self.layout.block_size))
                .and_then(|base| base.checked_add(off as u64))
                .ok_or(DriverError::DeviceFault)?;
            self.dev.read_bytes(at, raw)?;
        }
        if !self.inode_sound(ino, raw) {
            return Err(DriverError::DeviceFault);
        }
        Ok(())
    }

    /// Seal `raw[..inode_size]` with its checksum and stage it as inode
    /// `ino`'s record.
    pub(crate) fn write_inode_raw(&mut self, ino: u32, raw: &mut [u8]) -> Result<(), DriverError> {
        let (block, off) = self.inode_location(ino)?;
        let len = self.layout.inode_size as usize;
        let raw = raw.get_mut(..len).ok_or(DriverError::BufferTooSmall)?;
        if self.layout.metadata_csum {
            let sum = self.inode_checksum(ino, raw);
            put_le16(raw, CHECKSUM_LO, (sum & 0xFFFF) as u16);
            if Self::has_checksum_hi(raw) {
                put_le16(raw, CHECKSUM_HI, (sum >> 16) as u16);
            }
        }
        self.meta_mut(block)?[off..off + len].copy_from_slice(raw);
        Ok(())
    }

    /// Stamp a fresh generation into a new record, as every reuse of an
    /// inode number takes one, so a stale handle to the old file cannot
    /// reach the new one.
    pub(crate) fn new_generation(&mut self, raw: &mut [u8]) {
        self.generation = self.generation.wrapping_add(0x9E37_79B9);
        put_le32(raw, GENERATION, self.generation);
    }
}
