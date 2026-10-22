//! Byte-addressed I/O over a block device.
//!
//! A filesystem addresses its structures in bytes, or in a block size of its
//! own, while the device beneath it moves whole logical blocks of whatever
//! size it reports. [`ByteDevice`] joins the two: the whole device blocks of a
//! range move straight between the caller's buffer and the device in
//! coalesced runs of at most [`RUN_BYTES`], and only the range's partial first
//! and last blocks are staged — read, and for a write merged and written back.
//!
//! Every range is checked against the device's capacity before any transfer,
//! so a corrupt on-disk pointer is refused with
//! [`DriverError::LengthOutOfRange`] rather than reaching the device.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

use alloc::vec::Vec;

use tairix_abi::driver::block::{Block, MAX_BLOCK_SIZE, MIN_BLOCK_SIZE};
use tairix_abi::DriverError;
use tairix_writeback::RUN_BYTES;

#[cfg(test)]
mod tests;

/// A block device addressed in bytes.
pub struct ByteDevice<B: Block> {
    block: B,
    block_size: u32,
    blocks: u64,
    capacity: u64,
    /// One device block, staging a range's partial ends: on the heap, off the
    /// small kernel stack a driver may run on.
    partial: Vec<u8>,
}

impl<B: Block> ByteDevice<B> {
    /// Wrap `block`, validating its geometry.
    ///
    /// # Errors
    ///
    /// * [`DriverError::Unsupported`] if the block size is not a power of two
    ///   in [`MIN_BLOCK_SIZE`]`..=`[`MAX_BLOCK_SIZE`], or the capacity in
    ///   bytes overflows `u64`.
    /// * [`DriverError::OutOfMemory`] if the staging block cannot be held.
    /// * Whatever the device's geometry query reports.
    pub fn new(block: B) -> Result<Self, DriverError> {
        let geometry = block.geometry()?;
        let block_size = geometry.block_size;
        if !block_size.is_power_of_two() || !(MIN_BLOCK_SIZE..=MAX_BLOCK_SIZE).contains(&block_size)
        {
            return Err(DriverError::Unsupported);
        }
        let capacity = geometry
            .block_count
            .checked_mul(u64::from(block_size))
            .ok_or(DriverError::Unsupported)?;
        let partial = zeroed(block_size as usize)?;
        Ok(Self {
            block,
            block_size,
            blocks: geometry.block_count,
            capacity,
            partial,
        })
    }

    /// The device's logical block size, in bytes.
    #[must_use]
    pub fn block_size(&self) -> u32 {
        self.block_size
    }

    /// The device's logical block count.
    #[must_use]
    pub fn block_count(&self) -> u64 {
        self.blocks
    }

    /// The device's capacity, in bytes.
    #[must_use]
    pub fn capacity(&self) -> u64 {
        self.capacity
    }

    /// The wrapped device.
    #[must_use]
    pub fn device(&self) -> &B {
        &self.block
    }

    /// The wrapped device, mutably.
    pub fn device_mut(&mut self) -> &mut B {
        &mut self.block
    }

    /// The wrapped device, handed back.
    pub fn into_inner(self) -> B {
        self.block
    }

    /// Fill `buf` from byte `offset`.
    ///
    /// # Errors
    ///
    /// * [`DriverError::LengthOutOfRange`] if the range leaves the device.
    /// * Whatever the device reports for a transfer.
    pub fn read(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        self.check(offset, buf.len() as u64)?;
        let bs = self.block_size as usize;
        let run = run_bytes(bs);
        let mut done = 0usize;
        while done < buf.len() {
            let at = offset + done as u64;
            let (lba, within) = self.locate(at);
            let left = buf.len() - done;
            if within == 0 && left >= bs {
                let whole = (left / bs * bs).min(run);
                self.block.read_blocks(lba, &mut buf[done..done + whole])?;
                done += whole;
            } else {
                let take = (bs - within).min(left);
                self.block.read_blocks(lba, &mut self.partial)?;
                buf[done..done + take].copy_from_slice(&self.partial[within..within + take]);
                done += take;
            }
        }
        Ok(())
    }

    /// Write `data` at byte `offset`, reading back the bytes of a partially
    /// covered device block so they survive.
    ///
    /// # Errors
    ///
    /// * [`DriverError::LengthOutOfRange`] if the range leaves the device.
    /// * Whatever the device reports for a transfer.
    pub fn write(&mut self, offset: u64, data: &[u8]) -> Result<(), DriverError> {
        self.check(offset, data.len() as u64)?;
        let bs = self.block_size as usize;
        let run = run_bytes(bs);
        let mut done = 0usize;
        while done < data.len() {
            let at = offset + done as u64;
            let (lba, within) = self.locate(at);
            let left = data.len() - done;
            if within == 0 && left >= bs {
                let whole = (left / bs * bs).min(run);
                self.block.write_blocks(lba, &data[done..done + whole])?;
                done += whole;
            } else {
                let take = (bs - within).min(left);
                self.block.read_blocks(lba, &mut self.partial)?;
                self.partial[within..within + take].copy_from_slice(&data[done..done + take]);
                self.block.write_blocks(lba, &self.partial)?;
                done += take;
            }
        }
        Ok(())
    }

    /// Zero `len` bytes from byte `offset`.
    ///
    /// The whole blocks go out in coalesced runs from one zeroed buffer; a
    /// machine too short of memory to hold one run zeroes a block at a time
    /// instead, which is slower but never fails the write.
    ///
    /// # Errors
    ///
    /// * [`DriverError::LengthOutOfRange`] if the range leaves the device.
    /// * Whatever the device reports for a transfer.
    pub fn zero(&mut self, offset: u64, len: u64) -> Result<(), DriverError> {
        self.check(offset, len)?;
        let bs = u64::from(self.block_size);
        let end = offset + len;
        let mut at = offset;
        let head_end = end.min(offset.next_multiple_of(bs));
        if head_end > at {
            self.zero_partial(at, head_end)?;
            at = head_end;
        }
        let whole_end = end - end % bs;
        if whole_end > at {
            let span = whole_end - at;
            let run = u64::try_from(run_bytes(self.block_size as usize)).unwrap_or(bs);
            let zeros =
                zeroed(usize::try_from(span.min(run)).unwrap_or(0)).unwrap_or_else(|_| Vec::new());
            while at < whole_end {
                let (lba, _) = self.locate(at);
                if zeros.is_empty() {
                    self.partial.fill(0);
                    self.block.write_blocks(lba, &self.partial)?;
                    at += bs;
                } else {
                    let take = usize::try_from((whole_end - at).min(zeros.len() as u64))
                        .unwrap_or(zeros.len());
                    self.block.write_blocks(lba, &zeros[..take])?;
                    at += take as u64;
                }
            }
        }
        if end > at {
            self.zero_partial(at, end)?;
        }
        Ok(())
    }

    /// Make every write the device accepted durable.
    ///
    /// # Errors
    ///
    /// Whatever the device's flush reports.
    pub fn flush(&mut self) -> Result<(), DriverError> {
        self.block.flush()
    }

    /// Zero `[from, to)`, which lies inside one device block.
    fn zero_partial(&mut self, from: u64, to: u64) -> Result<(), DriverError> {
        let (lba, within) = self.locate(from);
        let take = usize::try_from(to - from).map_err(|_| DriverError::LengthOutOfRange)?;
        self.block.read_blocks(lba, &mut self.partial)?;
        self.partial[within..within + take].fill(0);
        self.block.write_blocks(lba, &self.partial)
    }

    /// The device block holding byte `at`, and the byte's place in it.
    fn locate(&self, at: u64) -> (u64, usize) {
        let bs = u64::from(self.block_size);
        // The remainder is below the block size, which is a `u32`.
        (at / bs, usize::try_from(at % bs).unwrap_or(0))
    }

    fn check(&self, offset: u64, len: u64) -> Result<(), DriverError> {
        match offset.checked_add(len) {
            Some(end) if end <= self.capacity => Ok(()),
            _ => Err(DriverError::LengthOutOfRange),
        }
    }
}

/// The bytes of one coalesced run, whole blocks of `block_size`.
fn run_bytes(block_size: usize) -> usize {
    RUN_BYTES - RUN_BYTES % block_size
}

/// `len` zero bytes, reserved fallibly.
fn zeroed(len: usize) -> Result<Vec<u8>, DriverError> {
    tairix_util::fallible::filled(len, 0).ok_or(DriverError::OutOfMemory)
}
