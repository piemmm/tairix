//! The legacy engines' registers and control-block chains.
//!
//! A node's register window holds whole channel blocks at a `0x100` stride,
//! the node's channel `n` being the `n`-th. A channel walks a chain of 32-byte
//! control blocks it fetches from memory, so a cyclic transfer is a chain
//! whose last block names the first, with the interrupt enabled on the last
//! block of every period.

use core::num::NonZeroU32;

use tairix_abi::driver::dma::{DmaHost, DmaReach, DmaSlab};
use tairix_abi::driver::dmaengine::{
    CyclicParams, CyclicTransfer, DmaChannel, DmaChannelEvent, DmaDirection, DmaEngine, Halted,
    DMA_CYCLIC_MIN_PERIODS,
};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::{DriverError, RegisterBlock, PAGE_SIZE};

use crate::CHANNEL_SLOTS;

/// Bytes between two channels' register blocks.
pub const CHANNEL_STRIDE: usize = 0x100;

/// Control and status.
const CS: usize = 0x00;
/// Bus address of the control block the channel is on; zero when idle.
const CONBLK_AD: usize = 0x04;
/// Transfer information of the loaded block; zero until one is fetched.
const TI: usize = 0x08;
/// Live source address.
const SOURCE_AD: usize = 0x0C;
/// Live destination address.
const DEST_AD: usize = 0x10;
/// Error flags and the channel's identity.
const DEBUG: usize = 0x20;

const CS_ACTIVE: u32 = 1 << 0;
/// Write one to clear.
const CS_END: u32 = 1 << 1;
/// Write one to clear.
const CS_INT: u32 = 1 << 2;
const CS_WAITING_FOR_OUTSTANDING_WRITES: u32 = 1 << 6;
const CS_ERROR: u32 = 1 << 8;
/// Self-clearing.
const CS_RESET: u32 = 1 << 31;

const TI_INTEN: u32 = 1 << 0;
const TI_WAIT_RESP: u32 = 1 << 3;
const TI_DEST_INC: u32 = 1 << 4;
const TI_DEST_WIDTH: u32 = 1 << 5;
const TI_DEST_DREQ: u32 = 1 << 6;
const TI_SRC_INC: u32 = 1 << 8;
const TI_SRC_WIDTH: u32 = 1 << 9;
const TI_SRC_DREQ: u32 = 1 << 10;
const TI_BURST_LENGTH_SHIFT: u32 = 12;
const TI_PERMAP_SHIFT: u32 = 16;

/// `DEBUG`'s three error flags, each write-one-to-clear.
const DEBUG_ERRORS: u32 = 0b111;

/// The request line that paces the transfer.
const SPEC_DREQ: u32 = 0x1F;
/// AXI priority, panic priority, wait-for-outstanding-writes and no debug
/// pause, each at its own `CS` position.
const SPEC_CS_FLAGS: u32 = 0x30FF_0000;
const SPEC_WIDE_SOURCE: u32 = 1 << 24;
const SPEC_WIDE_DEST: u32 = 1 << 25;
const SPEC_NO_WAIT_RESP: u32 = 1 << 27;
const SPEC_BURST: u32 = 1 << 30;
/// Every bit the downstream binding defines; any other refuses the line.
const SPEC_DEFINED: u32 =
    SPEC_DREQ | SPEC_CS_FLAGS | SPEC_WIDE_SOURCE | SPEC_WIDE_DEST | SPEC_NO_WAIT_RESP | SPEC_BURST;
/// The burst length the binding's burst bit stands for.
const SPEC_BURST_LENGTH: u32 = 3;

const NARROW_ACCESS: u32 = 4;
const WIDE_ACCESS: u32 = 16;

/// The most bytes a LITE channel's block moves. Every channel is held to it,
/// so whether a shape is admitted never depends on which channel serves it.
const LITE_MAX_BLOCK: u32 = 65_532;

/// Bytes, and alignment, of one control block.
const BLOCK_LEN: u32 = 32;
const BLOCK_BYTES: usize = BLOCK_LEN as usize;

/// The most blocks one chain holds: a page of them, the smallest carve.
pub const MAX_BLOCKS: usize = PAGE_SIZE / BLOCK_BYTES;

/// Status reads a pause may take to drain its outstanding writes before the
/// reset goes ahead regardless.
const DRAIN_BUDGET: u32 = 1_000;

/// Reads a reset is given to leave the channel idle before it is taken to
/// have been ignored.
const RESET_BUDGET: u32 = 1_000;

/// The engines' address registers are 32 bits wide.
pub const REACH: DmaReach = DmaReach::of::<32>();

/// How a request line asks to be served: its one specifier cell, validated.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Serving(u32);

impl Serving {
    fn decode(line: &LinkRequest) -> Result<Self, DriverError> {
        let &[cell] = line.selector() else {
            return Err(DriverError::Unsupported);
        };
        // An unpaced request would run the chain flat out forever.
        if cell & !SPEC_DEFINED != 0 || cell & SPEC_DREQ == 0 {
            return Err(DriverError::Unsupported);
        }
        Ok(Self(cell))
    }

    const fn has(self, flag: u32) -> bool {
        self.0 & flag != 0
    }

    /// Bytes every block length and the buffer's base are a multiple of.
    const fn unit(self) -> u32 {
        if self.has(SPEC_WIDE_SOURCE | SPEC_WIDE_DEST) {
            WIDE_ACCESS
        } else {
            NARROW_ACCESS
        }
    }

    const fn peripheral_access(self, direction: DmaDirection) -> u32 {
        let wide = match direction {
            DmaDirection::MemoryToDevice => self.has(SPEC_WIDE_DEST),
            DmaDirection::DeviceToMemory => self.has(SPEC_WIDE_SOURCE),
        };
        if wide {
            WIDE_ACCESS
        } else {
            NARROW_ACCESS
        }
    }

    const fn max_block(self) -> u32 {
        LITE_MAX_BLOCK - LITE_MAX_BLOCK % self.unit()
    }

    const fn cs_flags(self) -> u32 {
        self.0 & SPEC_CS_FLAGS
    }

    const fn transfer_info(self, direction: DmaDirection) -> u32 {
        let mut info = (self.0 & SPEC_DREQ) << TI_PERMAP_SHIFT;
        if !self.has(SPEC_NO_WAIT_RESP) {
            info |= TI_WAIT_RESP;
        }
        if self.has(SPEC_WIDE_SOURCE) {
            info |= TI_SRC_WIDTH;
        }
        if self.has(SPEC_WIDE_DEST) {
            info |= TI_DEST_WIDTH;
        }
        if self.has(SPEC_BURST) {
            info |= SPEC_BURST_LENGTH << TI_BURST_LENGTH_SHIFT;
        }
        info | match direction {
            DmaDirection::MemoryToDevice => TI_DEST_DREQ | TI_SRC_INC,
            DmaDirection::DeviceToMemory => TI_SRC_DREQ | TI_DEST_INC,
        }
    }

    /// The chain a buffer of `periods` periods of `period_bytes` needs.
    fn shape(self, period_bytes: u32, periods: u32) -> Result<Shape, DriverError> {
        if period_bytes == 0
            || periods < DMA_CYCLIC_MIN_PERIODS
            || !period_bytes.is_multiple_of(self.unit())
        {
            return Err(DriverError::LengthOutOfRange);
        }
        let buffer_bytes = period_bytes
            .checked_mul(periods)
            .ok_or(DriverError::LengthOutOfRange)?;
        let per_period = period_bytes.div_ceil(self.max_block());
        let blocks = per_period
            .checked_mul(periods)
            .and_then(|blocks| usize::try_from(blocks).ok())
            .filter(|&blocks| blocks <= MAX_BLOCKS)
            .ok_or(DriverError::LengthOutOfRange)?;
        Ok(Shape {
            buffer_bytes,
            blocks,
        })
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Shape {
    buffer_bytes: u32,
    blocks: usize,
}

/// `address` as a 32-bit bus address, with `span` bytes from it still below
/// the engines' reach.
fn bus_address(address: u64, span: u64) -> Result<u32, DriverError> {
    match address.checked_add(span) {
        Some(end) if REACH.end().is_none_or(|limit| end <= limit) => {
            u32::try_from(address).map_err(|_| DriverError::OutOfRange)
        }
        _ => Err(DriverError::OutOfRange),
    }
}

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

/// A chain a channel runs, and what reading its position needs.
struct Chain<T> {
    table: T,
    serving: Serving,
    direction: DmaDirection,
    buffer: u32,
    buffer_bytes: u32,
}

/// One channel of the node's window.
pub struct Channel<'a, S: BlockStore> {
    regs: &'a dyn RegisterBlock,
    store: &'a S,
    base: usize,
    chain: Option<Chain<S::Table>>,
    running: bool,
}

impl<S: BlockStore> Drop for Channel<'_, S> {
    fn drop(&mut self) {
        // The chain goes with the channel: one that may still be fetching it
        // is reset first, and keeps it if the reset cannot be issued.
        if self.running && self.stop().is_err() {
            if let Some(chain) = self.chain.as_mut() {
                chain.table.withhold();
            }
        }
    }
}

impl<S: BlockStore> Channel<'_, S> {
    fn read(&self, register: usize) -> Result<u32, DriverError> {
        self.regs.read32(self.base + register)
    }

    fn write(&self, register: usize, value: u32) -> Result<(), DriverError> {
        self.regs.write32(self.base + register, value)
    }

    /// Pause a loaded channel and let its outstanding writes land, answering
    /// whether they did within the budget. An idle channel has none.
    fn pause(&self) -> Result<bool, DriverError> {
        // `ACTIVE` is not a reliable sign of an idle channel; a zero block
        // address is.
        if self.read(CONBLK_AD)? == 0 {
            return Ok(true);
        }
        self.write(CS, 0)?;
        for _ in 0..DRAIN_BUDGET {
            if self.read(CS)? & CS_WAITING_FOR_OUTSTANDING_WRITES == 0 {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether the channel reads back idle, as only a reset it took leaves it:
    /// no block loaded and not active.
    fn idles(&self) -> Result<bool, DriverError> {
        for _ in 0..RESET_BUDGET {
            if self.read(CONBLK_AD)? == 0 && self.read(CS)? & CS_ACTIVE == 0 {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

impl<S: BlockStore> DmaChannel for Channel<'_, S> {
    fn prepare(
        &mut self,
        line: &LinkRequest,
        transfer: &CyclicTransfer,
    ) -> Result<(), DriverError> {
        if self.running {
            return Err(DriverError::Busy);
        }
        let serving = Serving::decode(line)?;
        let shape = serving.shape(transfer.period_bytes, transfer.periods)?;
        let access = serving.peripheral_access(transfer.direction);
        let buffer = bus_address(transfer.buffer, u64::from(shape.buffer_bytes))?;
        let fifo = bus_address(transfer.fifo, u64::from(access))?;
        if !buffer.is_multiple_of(serving.unit()) || !fifo.is_multiple_of(access) {
            return Err(DriverError::OutOfRange);
        }
        let table_bytes =
            u32::try_from(shape.blocks * BLOCK_BYTES).map_err(|_| DriverError::LengthOutOfRange)?;
        let mut table = self.store.carve(shape.blocks * BLOCK_BYTES)?;
        let table_base = bus_address(table.bus_address(), u64::from(table_bytes))?;
        if !table_base.is_multiple_of(BLOCK_LEN) {
            return Err(DriverError::OutOfRange);
        }

        let info = serving.transfer_info(transfer.direction);
        let max_block = serving.max_block();
        let mut next = 0;
        // Offsets stay below `buffer_bytes`, so no address below can pass the
        // bus limit `bus_address` checked the buffer's end against.
        let mut period_offset = 0;
        for _ in 0..transfer.periods {
            let mut done = 0;
            while done < transfer.period_bytes {
                let len = (transfer.period_bytes - done).min(max_block);
                let memory = buffer + period_offset + done;
                done += len;
                let (source, dest) = match transfer.direction {
                    DmaDirection::MemoryToDevice => (memory, fifo),
                    DmaDirection::DeviceToMemory => (fifo, memory),
                };
                let interrupt = if done == transfer.period_bytes {
                    TI_INTEN
                } else {
                    0
                };
                let offset = next;
                next = (next + BLOCK_LEN) % table_bytes;
                table.store(
                    offset as usize,
                    &[
                        info | interrupt,
                        source,
                        dest,
                        len,
                        0,
                        table_base + next,
                        0,
                        0,
                    ],
                )?;
            }
            period_offset += transfer.period_bytes;
        }
        self.chain = Some(Chain {
            table,
            serving,
            direction: transfer.direction,
            buffer,
            buffer_bytes: shape.buffer_bytes,
        });
        Ok(())
    }

    fn start(&mut self) -> Result<(), DriverError> {
        let chain = self.chain.as_ref().ok_or(DriverError::NotFound)?;
        if self.running {
            return Err(DriverError::Busy);
        }
        let table =
            u32::try_from(chain.table.bus_address()).map_err(|_| DriverError::OutOfRange)?;
        self.write(CS, CS_RESET)?;
        self.write(CONBLK_AD, table)?;
        // The chain's stores must reach memory before the channel fetches it.
        tairix_dma_barrier::dma_wmb();
        self.write(CS, CS_ACTIVE | chain.serving.cs_flags())?;
        self.running = true;
        Ok(())
    }

    fn stop(&mut self) -> Result<Halted, DriverError> {
        let drained = matches!(self.pause(), Ok(true));
        self.write(CS, CS_RESET)?;
        // A channel that ignored the reset may still fetch its chain, which
        // nothing may then replace or free.
        if !self.idles()? {
            return Err(DriverError::DeviceFault);
        }
        self.running = false;
        let cleared = self.write(DEBUG, DEBUG_ERRORS).is_ok();
        Ok(if drained && cleared {
            Halted::Drained
        } else {
            Halted::Undrained
        })
    }

    fn release(&mut self) -> Result<Halted, DriverError> {
        let halted = self.stop()?;
        self.chain = None;
        Ok(halted)
    }

    fn position(&self) -> Result<u32, DriverError> {
        let chain = self.chain.as_ref().ok_or(DriverError::NotFound)?;
        if self.read(TI)? == 0 {
            return Ok(0);
        }
        let address = self.read(match chain.direction {
            DmaDirection::MemoryToDevice => SOURCE_AD,
            DmaDirection::DeviceToMemory => DEST_AD,
        })?;
        let offset = address
            .checked_sub(chain.buffer)
            .ok_or(DriverError::DeviceFault)?;
        match offset.cmp(&chain.buffer_bytes) {
            core::cmp::Ordering::Less => Ok(offset),
            // The last block has ended and the first is not yet loaded.
            core::cmp::Ordering::Equal => Ok(0),
            core::cmp::Ordering::Greater => Err(DriverError::DeviceFault),
        }
    }

    fn take_event(&mut self) -> Result<DmaChannelEvent, DriverError> {
        let status = self.read(CS)?;
        if status & CS_ERROR != 0 {
            let errors = self.read(DEBUG)? & DEBUG_ERRORS;
            let bits = NonZeroU32::new(CS_ERROR | errors).ok_or(DriverError::DeviceFault)?;
            return Ok(DmaChannelEvent::Faulted(bits));
        }
        if status & CS_INT == 0 {
            return Ok(DmaChannelEvent::Quiet);
        }
        // Every `CS` write sets the flags too, and must never start a channel
        // the hardware has itself stopped.
        let flags = self
            .chain
            .as_ref()
            .map_or(0, |chain| chain.serving.cs_flags());
        self.write(CS, CS_INT | CS_END | (status & CS_ACTIVE) | flags)?;
        Ok(DmaChannelEvent::Boundary)
    }
}

/// The legacy engines behind one node's register window.
pub struct Bcm2835Dma<'a, S: BlockStore> {
    channels: [Channel<'a, S>; CHANNEL_SLOTS],
    count: u8,
}

impl<'a, S: BlockStore> Bcm2835Dma<'a, S> {
    /// The engines behind `regs`, whose window holds whole channel blocks,
    /// carving chains from `store`. Touches no register.
    ///
    /// # Errors
    ///
    /// [`DriverError::LengthOutOfRange`] for a window that is empty or not
    /// whole channel blocks.
    pub fn new(regs: &'a dyn RegisterBlock, store: &'a S) -> Result<Self, DriverError> {
        let len = regs.block_len();
        if len == 0 || !len.is_multiple_of(CHANNEL_STRIDE) {
            return Err(DriverError::LengthOutOfRange);
        }
        let count = u8::try_from((len / CHANNEL_STRIDE).min(CHANNEL_SLOTS))
            .map_err(|_| DriverError::LengthOutOfRange)?;
        Ok(Self {
            channels: core::array::from_fn(|index| Channel {
                regs,
                store,
                base: index * CHANNEL_STRIDE,
                chain: None,
                running: false,
            }),
            count,
        })
    }
}

impl<'a, S: BlockStore> DmaEngine for Bcm2835Dma<'a, S> {
    type Channel = Channel<'a, S>;

    fn channel_count(&self) -> u8 {
        self.count
    }

    fn channel(&mut self, index: u8) -> Option<&mut Channel<'a, S>> {
        if index >= self.count {
            return None;
        }
        self.channels.get_mut(usize::from(index))
    }

    fn accept(&self, line: &LinkRequest) -> Result<(), DriverError> {
        Serving::decode(line).map(|_| ())
    }

    fn admit(&self, line: &LinkRequest, params: &CyclicParams) -> Result<u32, DriverError> {
        let serving = Serving::decode(line)?;
        serving.shape(params.period_bytes, params.periods)?;
        let access = serving.peripheral_access(params.direction);
        if !params.fifo.is_multiple_of(u64::from(access)) {
            return Err(DriverError::OutOfRange);
        }
        Ok(access)
    }
}
