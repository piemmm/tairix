//! What both engines' chains share: the memory a chain is carved from and the
//! cyclic layout of its blocks.
//!
//! Either engine walks 32-byte control blocks it fetches from memory, so a
//! cyclic transfer is a chain whose last block names the first, with the
//! interrupt enabled on the last block of every period. The engines differ
//! only in how a block is encoded.

use tairix_abi::driver::dma::{DmaHost, DmaReach, DmaSlab};
use tairix_abi::{DriverError, PAGE_SIZE};

use crate::request::Shape;

/// Bytes, and alignment, of one control block.
pub const BLOCK_LEN: u32 = 32;
/// [`BLOCK_LEN`] as a length.
pub const BLOCK_BYTES: usize = BLOCK_LEN as usize;

/// The most blocks one chain holds: a page of them, the smallest carve.
pub const MAX_BLOCKS: usize = PAGE_SIZE / BLOCK_BYTES;

/// Memory the controller reaches, from which each channel's chain is carved.
pub trait BlockStore {
    /// One carve.
    type Table: BlockTable;

    /// Carve `bytes` of zeroed memory the controller reaches.
    ///
    /// # Errors
    ///
    /// The carve could not be made.
    fn carve(&self, bytes: usize) -> Result<Self::Table, DriverError>;
}

/// A carve holding one chain.
pub trait BlockTable {
    /// Bus address of its first byte.
    fn bus_address(&self) -> u64;

    /// Store one control block at byte `offset`.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfRange`] for a block past the carve.
    fn store(&mut self, offset: usize, block: &[u32; 8]) -> Result<(), DriverError>;

    /// Never return the carve to its store: a channel may still fetch it, so
    /// it waits for the quarantine to prove the controller quiet.
    fn withhold(&mut self);
}

impl BlockStore for &dyn DmaHost {
    type Table = DmaSlab;

    fn carve(&self, bytes: usize) -> Result<DmaSlab, DriverError> {
        self.alloc_dma_zeroed(bytes)
    }
}

impl BlockTable for DmaSlab {
    fn bus_address(&self) -> u64 {
        self.device_addr()
    }

    fn store(&mut self, offset: usize, block: &[u32; 8]) -> Result<(), DriverError> {
        let end = offset
            .checked_add(BLOCK_BYTES)
            .ok_or(DriverError::OutOfRange)?;
        let bytes = self
            .as_bytes_mut()
            .get_mut(offset..end)
            .ok_or(DriverError::OutOfRange)?;
        for (bytes, word) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(block) {
            *bytes = word.to_le_bytes();
        }
        self.sync_range(offset, BLOCK_BYTES);
        Ok(())
    }

    fn withhold(&mut self) {
        DmaSlab::withhold(self);
    }
}

/// `address`, with `span` bytes from it still within `reach`.
///
/// # Errors
///
/// [`DriverError::OutOfRange`] past the reach.
pub fn within(address: u64, span: u64, reach: DmaReach) -> Result<u64, DriverError> {
    match address.checked_add(span) {
        Some(end) if reach.end().is_none_or(|limit| end <= limit) => Ok(address),
        _ => Err(DriverError::OutOfRange),
    }
}

/// One block of a cyclic chain, before an engine encodes it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Segment {
    /// Bytes into the buffer the block starts at.
    pub offset: u32,
    /// Bytes the block moves.
    pub len: u32,
    /// Whether the block ends a period, and so raises the interrupt.
    pub ends_period: bool,
    /// Byte offset of the block within its table.
    pub slot: usize,
    /// Byte offset of the block that follows it; the last names the first.
    pub next: u32,
}

/// The blocks of a chain shaped `shape` over periods of `period_bytes`, each
/// at most `max_block` bytes, in the order the channel walks them.
pub fn segments(shape: &Shape, period_bytes: u32, max_block: u32) -> impl Iterator<Item = Segment> {
    let per_period = shape.per_period;
    // `shape` holds at most a page of blocks, so the count fits.
    let blocks = u32::try_from(shape.blocks).unwrap_or(0);
    let periods = blocks.checked_div(per_period).unwrap_or(0);
    (0..periods).flat_map(move |period| {
        (0..per_period).map(move |piece| {
            let done = piece * max_block;
            let index = period * per_period + piece;
            Segment {
                offset: period * period_bytes + done,
                len: (period_bytes - done).min(max_block),
                ends_period: piece + 1 == per_period,
                slot: index as usize * BLOCK_BYTES,
                next: (index + 1) % blocks * BLOCK_LEN,
            }
        })
    })
}
