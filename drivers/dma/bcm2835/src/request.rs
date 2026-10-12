//! The request-cell binding both engines serve: a request line's one
//! specifier cell names its request line and how the transfer is to run.
//!
//! The downstream binding packs the legacy engine's transfer-information and
//! status flags into the cell, and the DMA4 engine reads the same cell, so the
//! cell is decoded once and each engine encodes it into its own registers.

use tairix_abi::driver::dmaengine::{CyclicParams, DmaDirection, DMA_CYCLIC_MIN_PERIODS};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::DriverError;

use crate::chain::MAX_BLOCKS;

/// The request line that paces the transfer.
const DREQ: u32 = 0x1F;
/// AXI priority, panic priority, wait-for-outstanding-writes and no debug
/// pause, each at the position both engines' status registers hold it.
pub const CS_FLAGS: u32 = 0x30FF_0000;
const WIDE_SOURCE: u32 = 1 << 24;
const WIDE_DEST: u32 = 1 << 25;
const NO_WAIT_RESP: u32 = 1 << 27;
const BURST: u32 = 1 << 30;
/// Every bit the binding defines; any other refuses the line.
const DEFINED: u32 = DREQ | CS_FLAGS | WIDE_SOURCE | WIDE_DEST | NO_WAIT_RESP | BURST;

/// The burst length the binding's burst bit stands for.
pub const BURST_LENGTH: u32 = 3;

const NARROW_ACCESS: u32 = 4;
const WIDE_ACCESS: u32 = 16;

/// How a request line asks to be served: its one specifier cell, validated.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Serving(u32);

impl Serving {
    /// The serving `line` asks for.
    ///
    /// # Errors
    ///
    /// [`DriverError::Unsupported`] for a cell the binding does not define,
    /// or one naming no request line: an unpaced request would run the chain
    /// flat out forever.
    pub fn decode(line: &LinkRequest) -> Result<Self, DriverError> {
        let &[cell] = line.selector() else {
            return Err(DriverError::Unsupported);
        };
        if cell & !DEFINED != 0 || cell & DREQ == 0 {
            return Err(DriverError::Unsupported);
        }
        Ok(Self(cell))
    }

    const fn has(self, flag: u32) -> bool {
        self.0 & flag != 0
    }

    /// The request line.
    #[must_use]
    pub const fn dreq(self) -> u32 {
        self.0 & DREQ
    }

    /// Whether every write waits for its response.
    #[must_use]
    pub const fn waits_for_writes(self) -> bool {
        !self.has(NO_WAIT_RESP)
    }

    /// Whether the transfer runs in bursts of [`BURST_LENGTH`].
    #[must_use]
    pub const fn bursts(self) -> bool {
        self.has(BURST)
    }

    /// Whether the source side is read 128 bits at a time.
    #[must_use]
    pub const fn wide_source(self) -> bool {
        self.has(WIDE_SOURCE)
    }

    /// Whether the destination side is written 128 bits at a time.
    #[must_use]
    pub const fn wide_dest(self) -> bool {
        self.has(WIDE_DEST)
    }

    /// The status-register flags the channel runs with.
    #[must_use]
    pub const fn cs_flags(self) -> u32 {
        self.0 & CS_FLAGS
    }

    /// Bytes every block length and the buffer's base are a multiple of.
    #[must_use]
    pub const fn unit(self) -> u32 {
        if self.has(WIDE_SOURCE | WIDE_DEST) {
            WIDE_ACCESS
        } else {
            NARROW_ACCESS
        }
    }

    /// Bytes one access to the device FIFO moves.
    #[must_use]
    pub const fn peripheral_access(self, direction: DmaDirection) -> u32 {
        let wide = match direction {
            DmaDirection::MemoryToDevice => self.wide_dest(),
            DmaDirection::DeviceToMemory => self.wide_source(),
        };
        if wide {
            WIDE_ACCESS
        } else {
            NARROW_ACCESS
        }
    }

    /// The longest block an engine moving at most `limit` bytes a block can
    /// be handed: a whole number of units.
    #[must_use]
    pub const fn max_block(self, limit: u32) -> u32 {
        limit - limit % self.unit()
    }

    /// The chain a buffer of `periods` periods of `period_bytes` needs, with
    /// blocks of at most `max_block` bytes.
    ///
    /// # Errors
    ///
    /// [`DriverError::LengthOutOfRange`] for an empty period, fewer periods
    /// than a cyclic transfer needs, a period off the unit, a buffer past
    /// four gibibytes, or more blocks than a chain holds.
    pub fn shape(
        self,
        period_bytes: u32,
        periods: u32,
        max_block: u32,
    ) -> Result<Shape, DriverError> {
        if period_bytes == 0
            || periods < DMA_CYCLIC_MIN_PERIODS
            || !period_bytes.is_multiple_of(self.unit())
        {
            return Err(DriverError::LengthOutOfRange);
        }
        let buffer_bytes = period_bytes
            .checked_mul(periods)
            .ok_or(DriverError::LengthOutOfRange)?;
        let per_period = period_bytes.div_ceil(max_block);
        let blocks = per_period
            .checked_mul(periods)
            .and_then(|blocks| usize::try_from(blocks).ok())
            .filter(|&blocks| blocks <= MAX_BLOCKS)
            .ok_or(DriverError::LengthOutOfRange)?;
        Ok(Shape {
            buffer_bytes,
            per_period,
            blocks,
        })
    }

    /// Validate a transfer shaped `params` for an engine moving at most
    /// `limit` bytes a block, answering the bytes one FIFO access moves.
    ///
    /// # Errors
    ///
    /// As [`Self::shape`], or [`DriverError::OutOfRange`] for a FIFO off its
    /// access alignment.
    pub fn admit(self, params: &CyclicParams, limit: u32) -> Result<u32, DriverError> {
        self.shape(params.period_bytes, params.periods, self.max_block(limit))?;
        let access = self.peripheral_access(params.direction);
        if !params.fifo.is_multiple_of(u64::from(access)) {
            return Err(DriverError::OutOfRange);
        }
        Ok(access)
    }
}

/// A cyclic chain's dimensions.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Shape {
    /// Bytes of the whole buffer.
    pub buffer_bytes: u32,
    /// Blocks each period is split into.
    pub per_period: u32,
    /// Blocks of the whole chain.
    pub blocks: usize,
}
