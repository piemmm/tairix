//! The DMA4 engines' registers and control blocks (`brcm,bcm2711-dma`).
//!
//! A node's register window holds whole channel blocks at the legacy engines'
//! stride. A DMA4 channel addresses 40 bits: a block holds each address's low
//! word beside an information word carrying its top byte, the burst, the
//! increment and the access size, and a control block is named by its address
//! shifted right by five, the 32-byte alignment every block keeps.
//!
//! Reference: the BCM2711's DMA4 engines as the Raspberry Pi kernel's
//! `bcm2835-dma` driver programs them.

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
/// The loaded control block's address, shifted right by five; zero when idle.
const CB: usize = 0x04;
/// Error flags, the reset, and the channel's identity.
const DEBUG: usize = 0x0C;
/// Transfer information of the loaded block; zero until one is fetched.
const TI: usize = 0x10;
/// Live source address, low word.
const SRC: usize = 0x14;
/// Live source information: the address's top byte in bits 7:0.
const SRCI: usize = 0x18;
/// Live destination address, low word.
const DEST: usize = 0x1C;
/// Live destination information, as [`SRCI`].
const DESTI: usize = 0x20;

const CS_ACTIVE: u32 = 1 << 0;
/// Write one to clear.
const CS_INT: u32 = 1 << 2;
/// The AXI protection the channel's transactions carry.
const CS_PROT: u32 = 0b11 << 8;
const CS_ERROR: u32 = 1 << 10;
/// Transactions are still outstanding on the bus.
const CS_TRANSACTIONS: u32 = 1 << 25;

const TI_INTEN: u32 = 1 << 0;
const TI_WAIT_RESP: u32 = 1 << 2;
const TI_WAIT_RD_RESP: u32 = 1 << 3;
const TI_PERMAP_SHIFT: u32 = 9;
const TI_SRC_DREQ: u32 = 1 << 14;
const TI_DEST_DREQ: u32 = 1 << 15;

/// The information word's address byte.
const INFO_ADDRESS_HIGH: u32 = 0xFF;
const INFO_BURST_LENGTH_SHIFT: u32 = 8;
const INFO_INC: u32 = 1 << 12;
/// 128-bit accesses, in the size field at bits 14:13.
const INFO_SIZE_128: u32 = 2 << 13;

/// `DEBUG`'s write, FIFO, read and block-fetch error flags.
const DEBUG_ERRORS: u32 = 0b1111;
const DEBUG_RESET: u32 = 1 << 23;

/// The most bytes one block moves.
const MAX_BLOCK: u32 = 1 << 30;

/// Status reads a pause may take to let its outstanding transactions finish
/// before the reset goes ahead regardless.
const DRAIN_BUDGET: u32 = 1_000;

/// Reads a reset is given to leave the channel idle before it is taken to
/// have been ignored.
const RESET_BUDGET: u32 = 1_000;

/// What the engines' bus interface reaches, below the forty bits a block can
/// name.
pub const REACH: DmaReach = DmaReach::of::<36>();

/// The bits a control block's own address drops when a register or a block
/// names it.
const BLOCK_SHIFT: u32 = BLOCK_LEN.trailing_zeros();

/// The low word and the information word naming `address`: its top byte,
/// whether the side `increments`, accesses `wide` or 32 bits, and the burst.
fn side(address: u64, increments: bool, wide: bool, bursts: bool) -> (u32, u32) {
    let [b0, b1, b2, b3, high, ..] = address.to_le_bytes();
    let mut info = u32::from(high);
    if increments {
        info |= INFO_INC;
    }
    if wide {
        info |= INFO_SIZE_128;
    }
    if bursts {
        info |= BURST_LENGTH << INFO_BURST_LENGTH_SHIFT;
    }
    (u32::from_le_bytes([b0, b1, b2, b3]), info)
}

/// The transfer information a block carries, before its interrupt flag.
const fn transfer_info(serving: Serving, direction: DmaDirection) -> u32 {
    let mut info = serving.dreq() << TI_PERMAP_SHIFT;
    if serving.waits_for_writes() {
        info |= TI_WAIT_RESP;
    }
    info | match direction {
        DmaDirection::MemoryToDevice => TI_DEST_DREQ,
        DmaDirection::DeviceToMemory => TI_SRC_DREQ | TI_WAIT_RD_RESP,
    }
}

/// A control block's address as a register or a block names it.
fn block_name(address: u64) -> Result<u32, DriverError> {
    if !address.is_multiple_of(u64::from(BLOCK_LEN)) {
        return Err(DriverError::OutOfRange);
    }
    u32::try_from(address >> BLOCK_SHIFT).map_err(|_| DriverError::OutOfRange)
}

/// A chain a channel runs, and what reading its position needs.
struct Chain<T> {
    table: T,
    serving: Serving,
    direction: DmaDirection,
    buffer: u64,
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

    /// Pause a loaded channel and let its outstanding transactions finish,
    /// answering whether they did within the budget. An idle channel has none.
    fn pause(&self) -> Result<bool, DriverError> {
        // `ACTIVE` is not a reliable sign of an idle channel; a zero block
        // address is.
        if self.read(CB)? == 0 {
            return Ok(true);
        }
        self.write(CS, self.read(CS)? & !CS_ACTIVE)?;
        for _ in 0..DRAIN_BUDGET {
            if self.read(CS)? & CS_TRANSACTIONS == 0 {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Reset the channel: its status back to protection alone, then the reset
    /// its debug register holds.
    fn reset(&self) -> Result<(), DriverError> {
        self.write(CS, CS_PROT)?;
        self.write(DEBUG, self.read(DEBUG)? | DEBUG_RESET)
    }

    /// Whether the channel reads back idle, as only a reset it took leaves it:
    /// no block loaded and not active.
    fn idles(&self) -> Result<bool, DriverError> {
        for _ in 0..RESET_BUDGET {
            if self.read(CB)? == 0 && self.read(CS)? & CS_ACTIVE == 0 {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The bus address the channel's memory side has reached.
    fn live_address(&self, direction: DmaDirection) -> Result<u64, DriverError> {
        let (low, info) = match direction {
            DmaDirection::MemoryToDevice => (SRC, SRCI),
            DmaDirection::DeviceToMemory => (DEST, DESTI),
        };
        let high = u64::from(self.read(info)? & INFO_ADDRESS_HIGH);
        Ok(high << 32 | u64::from(self.read(low)?))
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
        let max_block = serving.max_block(MAX_BLOCK);
        let shape = serving.shape(transfer.period_bytes, transfer.periods, max_block)?;
        let access = serving.peripheral_access(transfer.direction);
        let buffer = within(transfer.buffer, u64::from(shape.buffer_bytes), REACH)?;
        let fifo = within(transfer.fifo, u64::from(access), REACH)?;
        if !buffer.is_multiple_of(u64::from(serving.unit()))
            || !fifo.is_multiple_of(u64::from(access))
        {
            return Err(DriverError::OutOfRange);
        }
        let table_bytes = shape.blocks * BLOCK_BYTES;
        let mut table = self.store.carve(table_bytes)?;
        let table_base = within(table.bus_address(), table_bytes as u64, REACH)?;
        block_name(table_base)?;

        let info = transfer_info(serving, transfer.direction);
        let (memory_wide, peripheral_wide) = match transfer.direction {
            DmaDirection::MemoryToDevice => (serving.wide_source(), serving.wide_dest()),
            DmaDirection::DeviceToMemory => (serving.wide_dest(), serving.wide_source()),
        };
        let peripheral = side(fifo, false, peripheral_wide, serving.bursts());
        for segment in segments(&shape, transfer.period_bytes, max_block) {
            let memory = side(
                buffer + u64::from(segment.offset),
                true,
                memory_wide,
                serving.bursts(),
            );
            let ((source, source_info), (dest, dest_info)) = match transfer.direction {
                DmaDirection::MemoryToDevice => (memory, peripheral),
                DmaDirection::DeviceToMemory => (peripheral, memory),
            };
            let interrupt = if segment.ends_period { TI_INTEN } else { 0 };
            let next = block_name(table_base + u64::from(segment.next))?;
            table.store(
                segment.slot,
                &[
                    info | interrupt,
                    source,
                    source_info,
                    dest,
                    dest_info,
                    segment.len,
                    next,
                    0,
                ],
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
        let table = block_name(chain.table.bus_address())?;
        self.reset()?;
        self.write(CB, table)?;
        // The chain's stores must reach memory before the channel fetches it.
        tairix_dma_barrier::dma_wmb();
        self.write(CS, CS_ACTIVE | CS_PROT | chain.serving.cs_flags())?;
        self.running = true;
        Ok(())
    }

    fn stop(&mut self) -> Result<Halted, DriverError> {
        let drained = matches!(self.pause(), Ok(true));
        self.reset()?;
        // A channel that ignored the reset may still fetch its chain, which
        // nothing may then replace or free.
        if !self.idles()? {
            return Err(DriverError::DeviceFault);
        }
        self.running = false;
        Ok(if drained {
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
        let offset = self
            .live_address(chain.direction)?
            .checked_sub(chain.buffer)
            .ok_or(DriverError::DeviceFault)?;
        match offset.cmp(&u64::from(chain.buffer_bytes)) {
            // Below the buffer's length, so within a `u32`.
            core::cmp::Ordering::Less => {
                u32::try_from(offset).map_err(|_| DriverError::DeviceFault)
            }
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
        self.write(CS, CS_INT | (status & CS_ACTIVE) | CS_PROT | flags)?;
        Ok(DmaChannelEvent::Boundary)
    }
}

/// The DMA4 engines behind one node's register window.
pub struct Bcm2711Dma<'a, S: BlockStore> {
    channels: [Channel<'a, S>; CHANNEL_SLOTS],
    count: u8,
}

impl<'a, S: BlockStore> Bcm2711Dma<'a, S> {
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

impl<'a, S: BlockStore> DmaEngine for Bcm2711Dma<'a, S> {
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
        Serving::decode(line)?.admit(params, MAX_BLOCK)
    }
}
