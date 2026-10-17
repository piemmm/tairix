//! Device access in filesystem blocks, and the staging layer metadata is
//! read and written through.
//!
//! Every metadata change lands in the running transaction's dirty set and
//! reaches the device only when the transaction commits; a read prefers the
//! staged copy, so the transaction always sees its own changes. File data is
//! written in place at once — ordered mode — and the commit's first barrier
//! makes it durable before any metadata naming it.

use alloc::vec::Vec;

use tairix_abi::blkio::BlkCommandCost;
use tairix_abi::driver::block::{Block, BlockClass};
use tairix_abi::DriverError;
use tairix_writeback::{WritePhase, RUN_BYTES};

use crate::bytes::{low32, scratch};
use crate::{Ext4, MAX_BLOCK_SIZE};

/// A block device addressed in filesystem blocks.
pub(crate) struct Device<B: Block> {
    block: B,
    /// Device logical-block size and count.
    sector: u32,
    sectors: u64,
    /// Filesystem block size, once the superblock names it; 1024 until then,
    /// which addresses the superblock itself.
    fs_block: u32,
    cost: BlkCommandCost,
    /// One sector, for the read-modify-write of a span that does not cover
    /// whole sectors: kept off the small kernel stack the driver nests on.
    partial: Vec<u8>,
}

impl<B: Block> Device<B> {
    /// Wrap `block`, validating its geometry.
    pub(crate) fn new(block: B) -> Result<Self, DriverError> {
        let geometry = block.geometry()?;
        let sector = geometry.block_size;
        if sector == 0 || sector > MAX_BLOCK_SIZE || !sector.is_power_of_two() {
            return Err(DriverError::DeviceFault);
        }
        let cost = block.command_cost();
        Ok(Self {
            block,
            sector,
            sectors: geometry.block_count,
            fs_block: 1024,
            cost,
            partial: scratch(sector as usize)?,
        })
    }

    /// Address the device in blocks of `fs_block` bytes from now on.
    pub(crate) fn set_fs_block(&mut self, fs_block: u32) {
        self.fs_block = fs_block;
    }

    /// Bytes the device holds.
    pub(crate) fn bytes(&self) -> Option<u64> {
        u64::from(self.sector).checked_mul(self.sectors)
    }

    /// What one command to the device costs.
    pub(crate) fn command_cost(&self) -> BlkCommandCost {
        self.cost
    }

    /// The device, handed back.
    pub(crate) fn into_inner(self) -> B {
        self.block
    }

    fn fs_offset(&self, block: u64) -> Result<u64, DriverError> {
        block
            .checked_mul(u64::from(self.fs_block))
            .ok_or(DriverError::DeviceFault)
    }

    /// Whether whole filesystem blocks are whole device blocks, so a run is
    /// one request with no read-modify-write.
    fn aligned(&self) -> bool {
        self.fs_block >= self.sector
    }

    fn check_span(&self, offset: u64, len: usize) -> Result<(), DriverError> {
        let end = offset
            .checked_add(len as u64)
            .ok_or(DriverError::DeviceFault)?;
        if end > self.bytes().ok_or(DriverError::DeviceFault)? {
            return Err(DriverError::DeviceFault);
        }
        Ok(())
    }

    /// Read the filesystem blocks from `block` that fill `buf`, in requests
    /// of at most one coalesced transfer.
    pub(crate) fn read_blocks(&mut self, block: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let offset = self.fs_offset(block)?;
        if !self.aligned() {
            return self.read_bytes(offset, buf);
        }
        self.check_span(offset, buf.len())?;
        let lba = offset / u64::from(self.sector);
        let per = RUN_BYTES - RUN_BYTES % self.sector as usize;
        for (i, chunk) in buf.chunks_mut(per).enumerate() {
            let at = lba + (i * per / self.sector as usize) as u64;
            self.block.read_blocks(at, chunk)?;
        }
        Ok(())
    }

    /// Write the filesystem blocks from `block` that `buf` holds.
    pub(crate) fn write_blocks(&mut self, block: u64, buf: &[u8]) -> Result<(), DriverError> {
        let offset = self.fs_offset(block)?;
        if !self.aligned() {
            return self.write_bytes(offset, buf);
        }
        self.check_span(offset, buf.len())?;
        let lba = offset / u64::from(self.sector);
        let per = RUN_BYTES - RUN_BYTES % self.sector as usize;
        for (i, chunk) in buf.chunks(per).enumerate() {
            let at = lba + (i * per / self.sector as usize) as u64;
            self.block.write_blocks(at, chunk)?;
        }
        Ok(())
    }

    /// Read `buf.len()` bytes from device byte `offset`, wherever they fall.
    pub(crate) fn read_bytes(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        self.check_span(offset, buf.len())?;
        let sector = u64::from(self.sector);
        let ss = self.sector as usize;
        let mut done = 0usize;
        while done < buf.len() {
            let cursor = offset + done as u64;
            let within = low32(cursor % sector) as usize;
            let take = (ss - within).min(buf.len() - done);
            let lba = cursor / sector;
            if within == 0 && buf.len() - done >= ss {
                // Whole sectors read straight into the caller's buffer.
                let whole = (buf.len() - done) / ss * ss;
                let whole = whole.min(RUN_BYTES - RUN_BYTES % ss);
                self.block.read_blocks(lba, &mut buf[done..done + whole])?;
                done += whole;
                continue;
            }
            self.block.read_blocks(lba, &mut self.partial)?;
            buf[done..done + take].copy_from_slice(&self.partial[within..within + take]);
            done += take;
        }
        Ok(())
    }

    /// Write `buf` at device byte `offset`, reading back the partial sectors
    /// it covers.
    pub(crate) fn write_bytes(&mut self, offset: u64, buf: &[u8]) -> Result<(), DriverError> {
        self.check_span(offset, buf.len())?;
        let sector = u64::from(self.sector);
        let ss = self.sector as usize;
        let mut done = 0usize;
        while done < buf.len() {
            let cursor = offset + done as u64;
            let within = low32(cursor % sector) as usize;
            let take = (ss - within).min(buf.len() - done);
            let lba = cursor / sector;
            if within == 0 && buf.len() - done >= ss {
                let whole = (buf.len() - done) / ss * ss;
                let whole = whole.min(RUN_BYTES - RUN_BYTES % ss);
                self.block.write_blocks(lba, &buf[done..done + whole])?;
                done += whole;
                continue;
            }
            self.block.read_blocks(lba, &mut self.partial)?;
            self.partial[within..within + take].copy_from_slice(&buf[done..done + take]);
            self.block.write_blocks(lba, &self.partial)?;
            done += take;
        }
        Ok(())
    }

    /// Make every write the device accepted durable.
    pub(crate) fn flush(&mut self) -> Result<(), DriverError> {
        self.block.flush()
    }
}

impl<B: Block> Ext4<B> {
    /// Refuse a metadata block number the volume does not have.
    fn check_meta(&self, block: u64) -> Result<(), DriverError> {
        if block == 0 || block >= self.layout.blocks_count {
            return Err(DriverError::DeviceFault);
        }
        Ok(())
    }

    /// Read metadata block `block` into `buf`, the running transaction's
    /// copy if it holds one. Answers whether the bytes came from the device,
    /// so a caller verifies a checksum only on what it did not write itself.
    pub(crate) fn read_meta(&mut self, block: u64, buf: &mut [u8]) -> Result<bool, DriverError> {
        self.check_meta(block)?;
        let bs = self.layout.block_size as usize;
        let buf = buf.get_mut(..bs).ok_or(DriverError::BufferTooSmall)?;
        if let Some(staged) = self.dirty.staged(block) {
            buf.copy_from_slice(staged);
            return Ok(false);
        }
        self.dev.read_blocks(block, buf)?;
        Ok(true)
    }

    /// Stage `buf` as metadata block `block`'s new contents.
    pub(crate) fn put_meta(&mut self, block: u64, buf: &[u8]) -> Result<(), DriverError> {
        self.check_meta(block)?;
        self.ensure_writable()?;
        let bs = self.layout.block_size as usize;
        self.dirty.stage(
            WritePhase::BeforeBarrier,
            block,
            buf.get(..bs).ok_or(DriverError::BufferTooSmall)?,
        )
    }

    /// Metadata block `block`, staged and writable in place; loaded from the
    /// device the first time the transaction touches it.
    pub(crate) fn meta_mut(&mut self, block: u64) -> Result<&mut [u8], DriverError> {
        self.check_meta(block)?;
        self.ensure_writable()?;
        let dev = &mut self.dev;
        self.dirty
            .stage_with(WritePhase::BeforeBarrier, block, |buf| {
                dev.read_blocks(block, buf)
            })
    }

    /// Drop the staged copies of `len` blocks from `start`, which nothing
    /// will reference once the transaction commits.
    pub(crate) fn forget_meta(&mut self, start: u64, len: u64) {
        self.dirty.discard_run(start, len);
    }

    /// Read `buf.len()` bytes of file data from device byte `offset`.
    pub(crate) fn read_data_bytes(
        &mut self,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<(), DriverError> {
        self.dev.read_bytes(offset, buf)
    }

    /// Write file data `bytes` in place from byte `within` of block `phys`,
    /// ordered ahead of the commit that names them: refused unless every
    /// block the write reaches lies inside the volume.
    pub(crate) fn write_data(
        &mut self,
        phys: u64,
        within: u64,
        bytes: &[u8],
    ) -> Result<(), DriverError> {
        self.ensure_writable()?;
        let bs = u64::from(self.layout.block_size);
        let end = within
            .checked_add(bytes.len() as u64)
            .map(|span| span.div_ceil(bs))
            .and_then(|blocks| phys.checked_add(blocks));
        if phys == 0 || end.is_none_or(|end| end > self.layout.blocks_count) {
            return Err(DriverError::DeviceFault);
        }
        self.data_written = true;
        if within == 0 && (bytes.len() as u64).is_multiple_of(bs) {
            self.dev.write_blocks(phys, bytes)
        } else {
            self.dev.write_bytes(phys * bs + within, bytes)
        }
    }
}

#[cfg(test)]
#[path = "io_tests.rs"]
mod tests;
