//! The legacy engines' registers and control blocks (`brcm,bcm2835-dma`).
//!
//! A node's register window holds whole channel blocks at a `0x100` stride,
//! the node's channel `n` being the `n`-th. Every address a block or register
//! holds is 32 bits wide.
//!
//! Reference: BCM2711 ARM Peripherals, chapter 4 (DMA Controller).

use core::num::NonZeroU32;

use tairix_abi::driver::dma::DmaReach;
use tairix_abi::driver::dmaengine::{
    CyclicParams, CyclicTransfer, DmaChannel, DmaChannelEvent, DmaDirection, DmaEngine, Halted,
};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::{DriverError, RegisterBlock};

use crate::chain::{segments, within, BlockStore, BlockTable, BLOCK_BYTES, BLOCK_LEN};
use crate::request::{Serving, BURST_LENGTH};
use crate::{channel_count, CHANNEL_SLOTS, CHANNEL_STRIDE};

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

/// The most bytes a LITE channel's block moves. Every channel is held to it,
/// so whether a shape is admitted never depends on which channel serves it.
const LITE_MAX_BLOCK: u32 = 65_532;

/// Status reads a pause may take to drain its outstanding writes before the
/// reset goes ahead regardless.
const DRAIN_BUDGET: u32 = 1_000;

/// Reads a reset is given to leave the channel idle before it is taken to
/// have been ignored.
const RESET_BUDGET: u32 = 1_000;

/// The engines' address registers are 32 bits wide.
pub const REACH: DmaReach = DmaReach::of::<32>();

/// `address` as a 32-bit bus address, with `span` bytes from it still within
/// the engines' reach.
fn bus_address(address: u64, span: u64) -> Result<u32, DriverError> {
    u32::try_from(within(address, span, REACH)?).map_err(|_| DriverError::OutOfRange)
}

/// The transfer information a block carries, before its interrupt flag.
const fn transfer_info(serving: Serving, direction: DmaDirection) -> u32 {
    let mut info = serving.dreq() << TI_PERMAP_SHIFT;
    if serving.waits_for_writes() {
        info |= TI_WAIT_RESP;
    }
    if serving.wide_source() {
        info |= TI_SRC_WIDTH;
    }
    if serving.wide_dest() {
        info |= TI_DEST_WIDTH;
    }
    if serving.bursts() {
        info |= BURST_LENGTH << TI_BURST_LENGTH_SHIFT;
    }
    info | match direction {
        DmaDirection::MemoryToDevice => TI_DEST_DREQ | TI_SRC_INC,
        DmaDirection::DeviceToMemory => TI_SRC_DREQ | TI_DEST_INC,
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
        let max_block = serving.max_block(LITE_MAX_BLOCK);
        let shape = serving.shape(transfer.period_bytes, transfer.periods, max_block)?;
        let access = serving.peripheral_access(transfer.direction);
        let buffer = bus_address(transfer.buffer, u64::from(shape.buffer_bytes))?;
        let fifo = bus_address(transfer.fifo, u64::from(access))?;
        if !buffer.is_multiple_of(serving.unit()) || !fifo.is_multiple_of(access) {
            return Err(DriverError::OutOfRange);
        }
        let table_bytes = shape.blocks * BLOCK_BYTES;
        let mut table = self.store.carve(table_bytes)?;
        let table_base = bus_address(table.bus_address(), table_bytes as u64)?;
        if !table_base.is_multiple_of(BLOCK_LEN) {
            return Err(DriverError::OutOfRange);
        }

        let info = transfer_info(serving, transfer.direction);
        // Offsets stay below `buffer_bytes`, so no address below can pass the
        // reach `bus_address` checked the buffer's end against.
        for segment in segments(&shape, transfer.period_bytes, max_block) {
            let memory = buffer + segment.offset;
            let (source, dest) = match transfer.direction {
                DmaDirection::MemoryToDevice => (memory, fifo),
                DmaDirection::DeviceToMemory => (fifo, memory),
            };
            let interrupt = if segment.ends_period { TI_INTEN } else { 0 };
            let next = table_base + segment.next;
            table.store(
                segment.slot,
                &[info | interrupt, source, dest, segment.len, 0, next, 0, 0],
            )?;
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
        let count = channel_count(regs.block_len())?;
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
        Serving::decode(line)?.admit(params, LITE_MAX_BLOCK)
    }
}
