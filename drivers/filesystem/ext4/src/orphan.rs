//! The orphan list: inodes a crash may leave part-truncated or unlinked but
//! still allocated, finished at the next writable mount.
//!
//! An operation too large for one transaction — truncating or deleting a
//! big file — puts its inode on the superblock's orphan chain first, each
//! orphan's `i_dtime` naming the next. A crash between its transactions
//! leaves the inode listed, and mounting finishes the job: an orphan with no
//! links is deleted, one with links is cut back to its size. The orphan
//! file a newer kernel may also keep is drained the same way.

use tairix_abi::driver::block::Block;
use tairix_abi::DriverError;

use crate::bytes::{le16, le32, put_le32, scratch};
use crate::superblock::{
    make_dynamic, RO_COMPAT_ORPHAN_PRESENT, SB_FEATURE_RO_COMPAT, SB_LAST_ORPHAN,
};
use crate::{Ext4, INODE_DTIME, INODE_LINKS};

/// `ob_magic` closing each orphan-file block.
const ORPHAN_BLOCK_MAGIC: u32 = 0x0B10_CA04;
/// Bytes of an orphan-file block's tail: magic, then checksum.
const ORPHAN_TAIL: usize = 8;

/// The blocks an orphan file of `size` bytes in blocks of `bs` holds, or a
/// device fault for more than the volume's `inodes` could ever fill: no inode
/// takes two slots, so a longer file is damage, and walking it would only
/// stall the mount.
fn orphan_file_blocks(size: u64, bs: usize, inodes: u32) -> Result<u64, DriverError> {
    let slots = ((bs - ORPHAN_TAIL) / 4) as u64;
    let blocks = size / bs as u64;
    if blocks > u64::from(inodes).div_ceil(slots) {
        return Err(DriverError::DeviceFault);
    }
    Ok(blocks)
}

impl<B: Block> Ext4<B> {
    /// List inode `ino`, whose record is `raw`, on the orphan chain.
    pub(crate) fn orphan_add(&mut self, ino: u32, raw: &mut [u8]) -> Result<(), DriverError> {
        make_dynamic(&mut self.sb);
        let head = le32(&self.sb, SB_LAST_ORPHAN);
        put_le32(raw, INODE_DTIME, head);
        self.write_inode_raw(ino, raw)?;
        put_le32(&mut self.sb, SB_LAST_ORPHAN, ino);
        Ok(())
    }

    /// Take inode `ino`, whose record is `raw`, off the orphan chain.
    pub(crate) fn orphan_remove(&mut self, ino: u32, raw: &mut [u8]) -> Result<(), DriverError> {
        let next = le32(raw, INODE_DTIME);
        let head = le32(&self.sb, SB_LAST_ORPHAN);
        if head == ino {
            put_le32(&mut self.sb, SB_LAST_ORPHAN, next);
        } else {
            let mut at = head;
            let mut prev = self.inode_scratch()?;
            let mut found = false;
            for _ in 0..self.layout.inodes_count {
                if at == 0 {
                    break;
                }
                self.read_inode_raw(at, &mut prev)?;
                if le32(&prev, INODE_DTIME) == ino {
                    put_le32(&mut prev, INODE_DTIME, next);
                    self.write_inode_raw(at, &mut prev)?;
                    found = true;
                    break;
                }
                at = le32(&prev, INODE_DTIME);
            }
            if !found {
                return Err(DriverError::DeviceFault);
            }
        }
        put_le32(raw, INODE_DTIME, 0);
        Ok(())
    }

    /// Finish every orphan the volume lists, each in operations of its own.
    /// An orphan stays at the head of the chain until its last transaction,
    /// so a crash part-way through finishing it is finished again.
    pub(crate) fn finish_orphans(&mut self) -> Result<(), DriverError> {
        for _ in 0..self.layout.inodes_count {
            let ino = le32(&self.sb, SB_LAST_ORPHAN);
            if ino == 0 {
                break;
            }
            if ino < self.layout.first_ino || ino > self.layout.inodes_count {
                return Err(DriverError::DeviceFault);
            }
            self.operation(|fs| fs.finish_orphan(ino, true))?;
        }
        if le32(&self.sb, SB_LAST_ORPHAN) != 0 {
            // A chain longer than the volume's inodes loops.
            return Err(DriverError::DeviceFault);
        }
        self.finish_orphan_file()?;
        self.commit(true)
    }

    /// Finish one orphan: delete it when nothing links it, else cut its
    /// blocks back to its size. `listed` says it is on the chain.
    fn finish_orphan(&mut self, ino: u32, listed: bool) -> Result<(), DriverError> {
        let mut raw = self.inode_scratch()?;
        self.read_inode_raw(ino, &mut raw)?;
        if crate::kind_of(le16(&raw, 0)).is_none() {
            return Err(DriverError::DeviceFault);
        }
        if le16(&raw, INODE_LINKS) == 0 {
            return self.release_inode(ino, &mut raw, listed);
        }
        if crate::file::has_block_map(&raw, &self.layout) {
            let size = crate::file::inode_size_bytes(&raw, &self.layout);
            let keep = size.div_ceil(u64::from(self.layout.block_size));
            self.cut_blocks(ino, &mut raw, keep, listed)?;
            self.zero_tail(ino, &raw, size)?;
        }
        if listed {
            self.orphan_remove(ino, &mut raw)?;
        }
        self.write_inode_raw(ino, &mut raw)
    }

    /// Drain the orphan file a newer kernel keeps beside the chain.
    fn finish_orphan_file(&mut self) -> Result<(), DriverError> {
        let Some(file) = self.layout.orphan_file_inum else {
            return Ok(());
        };
        if le32(&self.sb, SB_FEATURE_RO_COMPAT) & RO_COMPAT_ORPHAN_PRESENT == 0 {
            return Ok(());
        }
        let bs = self.layout.block_size as usize;
        let mut raw = self.inode_scratch()?;
        self.read_inode_raw(file, &mut raw)?;
        let size = crate::file::inode_size_bytes(&raw, &self.layout);
        let blocks = orphan_file_blocks(size, bs, self.layout.inodes_count)?;
        let generation = le32(&raw, crate::inode::GENERATION);
        let seed = self.inode_csum_seed(file, generation);
        let mut block = scratch(bs)?;
        for logical in 0..blocks {
            let crate::extent::Mapping::Mapped {
                phys,
                unwritten: false,
                ..
            } = self.map_lookup(file, &raw, logical)?
            else {
                return Err(DriverError::DeviceFault);
            };
            self.read_meta(phys, &mut block)?;
            if le32(&block, bs - ORPHAN_TAIL) != ORPHAN_BLOCK_MAGIC {
                return Err(DriverError::DeviceFault);
            }
            if self.layout.metadata_csum
                && le32(&block, bs - 4)
                    != orphan_block_checksum(seed, phys, &block[..bs - ORPHAN_TAIL])
            {
                return Err(DriverError::DeviceFault);
            }
            for slot in 0..(bs - ORPHAN_TAIL) / 4 {
                let ino = le32(&block, slot * 4);
                if ino == 0 {
                    continue;
                }
                if ino < self.layout.first_ino || ino > self.layout.inodes_count {
                    return Err(DriverError::DeviceFault);
                }
                self.operation(|fs| {
                    let mut entry = scratch(bs)?;
                    fs.read_meta(phys, &mut entry)?;
                    put_le32(&mut entry, slot * 4, 0);
                    if fs.layout.metadata_csum {
                        let sum = orphan_block_checksum(seed, phys, &entry[..bs - ORPHAN_TAIL]);
                        put_le32(&mut entry, bs - 4, sum);
                    }
                    fs.put_meta(phys, &entry)?;
                    fs.finish_orphan(ino, false)
                })?;
            }
        }
        self.operation(|fs| {
            let ro = le32(&fs.sb, SB_FEATURE_RO_COMPAT) & !RO_COMPAT_ORPHAN_PRESENT;
            put_le32(&mut fs.sb, SB_FEATURE_RO_COMPAT, ro);
            Ok(())
        })
    }
}

/// An orphan-file block's checksum: the entries, under the file's inode seed
/// chained with the block's physical number.
fn orphan_block_checksum(seed: u32, phys: u64, entries: &[u8]) -> u32 {
    let c = tairix_crc32c::update(seed, &phys.to_le_bytes());
    tairix_crc32c::update(c, entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An orphan file longer than the volume's inodes could fill is damage,
    /// refused before any of it is walked.
    #[test]
    fn an_orphan_file_past_what_the_inodes_fill_is_refused() {
        // 1 KiB blocks hold 255 slots, so 4096 inodes fill 17 blocks.
        assert_eq!(orphan_file_blocks(17 * 1024, 1024, 4096), Ok(17));
        assert_eq!(
            orphan_file_blocks(18 * 1024, 1024, 4096),
            Err(DriverError::DeviceFault)
        );
        assert_eq!(
            orphan_file_blocks(u64::MAX, 1024, 4096),
            Err(DriverError::DeviceFault)
        );
    }
}
