//! A register-level model of the DMA4 engines that fetches control blocks
//! from a simulated memory the way the silicon does.
//!
//! It holds the driver to the hardware's rules as assertions: a block named by
//! its address shifted right by five, its reserved word zero, information
//! words carrying only an address byte, a burst, an increment and a 32- or
//! 128-bit size, every status write carrying the AXI protection, and every
//! memory-side access inside the buffer the test says the channel owns. A
//! block fetched from memory already freed is a block-fetch error.

use std::cell::RefCell;
use std::rc::Rc;
use std::vec::Vec;

use tairix_abi::{DriverError, RegisterBlock};

use crate::model_memory::Memory;
pub use crate::model_memory::{ModelStore, Timeline, Trace};
use crate::CHANNEL_STRIDE;

pub const CS_ACTIVE: u32 = 1 << 0;
pub const CS_END: u32 = 1 << 1;
pub const CS_INT: u32 = 1 << 2;
pub const CS_PROT: u32 = 0b11 << 8;
pub const CS_ERROR: u32 = 1 << 10;
pub const CS_TRANSACTIONS: u32 = 1 << 25;
const CS_ABORT: u32 = 1 << 30;
const CS_HALT: u32 = 1 << 31;
/// The read-write fields of `CS`: `ACTIVE`, the protection and the serving
/// flags.
const CS_WRITABLE: u32 = CS_ACTIVE | CS_PROT | 0x30FF_0000;

pub const TI_INTEN: u32 = 1 << 0;
pub const TI_WAIT_RESP: u32 = 1 << 2;
pub const TI_WAIT_RD_RESP: u32 = 1 << 3;
pub const TI_SRC_DREQ: u32 = 1 << 14;
pub const TI_DEST_DREQ: u32 = 1 << 15;
/// The fields of `TI` the driver may set: interrupt, the two waits, the
/// request map and the two request enables. 2D mode and wait cycles are not
/// its to use.
const TI_DEFINED: u32 =
    TI_INTEN | TI_WAIT_RESP | TI_WAIT_RD_RESP | 0x1F << 9 | TI_SRC_DREQ | TI_DEST_DREQ;

pub const INFO_INC: u32 = 1 << 12;
pub const INFO_SIZE_128: u32 = 2 << 13;
const INFO_SIZE: u32 = 0b11 << 13;
/// Address byte, burst, increment and size: an ignore or a 2D stride is not
/// the driver's to use.
const INFO_DEFINED: u32 = 0xFF | 0xF << 8 | INFO_INC | INFO_SIZE;

pub const DEBUG_READ_CB_ERROR: u32 = 1 << 3;
const DEBUG_ERRORS: u32 = 0b1111;
pub const DEBUG_RESET: u32 = 1 << 23;

/// The most bytes one block moves.
pub const MAX_BLOCK: u32 = 1 << 30;

pub const CS: usize = 0x00;
pub const CB: usize = 0x04;
pub const DEBUG: usize = 0x0C;
const TI: usize = 0x10;
const SRC: usize = 0x14;
const SRCI: usize = 0x18;
const DEST: usize = 0x1C;
const DESTI: usize = 0x20;
const LEN: usize = 0x24;
const NEXT_CB: usize = 0x28;

/// A block as the channel loaded it, its addresses whole.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Block {
    pub info: u32,
    pub source: u64,
    pub source_info: u32,
    pub dest: u64,
    pub dest_info: u32,
    pub len: u32,
    /// The next block's bus address.
    pub next: u64,
}

/// How the test has told a channel to behave; a reset keeps it.
#[derive(Clone, Default)]
struct Behaviour {
    defer_fetch: bool,
    drain_reads: u32,
    region: Option<(u64, u64)>,
}

#[derive(Default)]
struct ChannelModel {
    behaviour: Behaviour,
    writable: u32,
    latched: u32,
    cb: u32,
    info: u32,
    source: u64,
    source_info: u32,
    dest: u64,
    dest_info: u32,
    remaining: u32,
    next: u32,
    errors: u32,
    fetched: bool,
    draining: u32,
    loaded: Vec<Block>,
    interrupts: u32,
}

fn whole(low: u32, info: u32) -> u64 {
    u64::from(info & 0xFF) << 32 | u64::from(low)
}

/// The model: a node's register window plus the memory its chains live in.
pub struct Model {
    channels: RefCell<Vec<ChannelModel>>,
    memory: Rc<RefCell<Memory>>,
    timeline: Timeline,
}

impl Model {
    /// A node of `count` channels.
    pub fn new(count: usize) -> Self {
        let timeline: Timeline = Rc::new(RefCell::new(Vec::new()));
        Self {
            channels: RefCell::new((0..count).map(|_| ChannelModel::default()).collect()),
            memory: Memory::new(&timeline),
            timeline,
        }
    }

    /// The Pi 4's DMA4 node: four channels, the first the firmware's.
    pub fn pi4() -> Self {
        Self::new(4)
    }

    pub fn store(&self) -> ModelStore {
        ModelStore {
            memory: Rc::clone(&self.memory),
        }
    }

    pub fn timeline(&self) -> Timeline {
        Rc::clone(&self.timeline)
    }

    /// Hold the memory side of `channel` to `[bus, bus + len)`.
    pub fn own(&self, channel: usize, bus: u64, len: u64) {
        self.channels.borrow_mut()[channel].behaviour.region = Some((bus, bus + len));
    }

    /// Leave `channel`'s first fetch until the next [`Self::advance`].
    pub fn defer_fetch(&self, channel: usize) {
        self.channels.borrow_mut()[channel].behaviour.defer_fetch = true;
    }

    /// Keep `TRANSACTIONS` up for `reads` reads after a pause.
    pub fn drain_reads(&self, channel: usize, reads: u32) {
        self.channels.borrow_mut()[channel].behaviour.drain_reads = reads;
    }

    pub fn live_tables(&self) -> usize {
        self.memory.borrow().live_tables()
    }

    pub fn loaded(&self, channel: usize) -> Vec<Block> {
        self.channels.borrow()[channel].loaded.clone()
    }

    pub fn interrupts(&self, channel: usize) -> u32 {
        self.channels.borrow()[channel].interrupts
    }

    pub fn pending(&self, channel: usize) -> bool {
        self.channels.borrow()[channel].latched & CS_INT != 0
    }

    pub fn active(&self, channel: usize) -> bool {
        self.channels.borrow()[channel].writable & CS_ACTIVE != 0
    }

    /// Latch `bits` in `DEBUG` and halt the channel, as a bus error does.
    pub fn fault(&self, channel: usize, bits: u32) {
        let mut channels = self.channels.borrow_mut();
        channels[channel].errors |= bits;
        channels[channel].writable &= !CS_ACTIVE;
    }

    /// Writes to `channel`'s register `register`, in order.
    pub fn writes(&self, channel: usize, register: usize) -> Vec<u32> {
        self.timeline
            .borrow()
            .iter()
            .filter_map(|trace| match *trace {
                Trace::Write {
                    channel: c,
                    register: r,
                    value,
                } if c == channel && r == register => Some(value),
                _ => None,
            })
            .collect()
    }

    /// Move `bytes` through `channel`, paced as its request line would.
    pub fn advance(&self, channel: usize, mut bytes: u32) {
        let mut channels = self.channels.borrow_mut();
        let model = &mut channels[channel];
        while bytes > 0 {
            if model.writable & CS_ACTIVE == 0 || model.cb == 0 || model.errors != 0 {
                return;
            }
            if !model.fetched && !self.fetch(model) {
                return;
            }
            let step = bytes.min(model.remaining);
            if model.source_info & INFO_INC != 0 {
                model.source += u64::from(step);
            }
            if model.dest_info & INFO_INC != 0 {
                model.dest += u64::from(step);
            }
            model.remaining -= step;
            bytes -= step;
            if model.remaining == 0 {
                model.latched |= CS_END;
                if model.info & TI_INTEN != 0 {
                    model.latched |= CS_INT;
                    model.interrupts += 1;
                }
                model.cb = model.next;
                model.fetched = false;
                if model.cb == 0 {
                    model.writable &= !CS_ACTIVE;
                    return;
                }
                if !self.fetch(model) {
                    return;
                }
            }
        }
    }

    /// Load the block `CB` names, answering whether the fetch succeeded.
    fn fetch(&self, model: &mut ChannelModel) -> bool {
        let address = u64::from(model.cb) << 5;
        let Some(words) = self.memory.borrow().block(address) else {
            model.errors |= DEBUG_READ_CB_ERROR;
            model.writable &= !CS_ACTIVE;
            return false;
        };
        let [info, source, source_info, dest, dest_info, len, next, reserved] = words;
        assert_eq!(reserved, 0, "the reserved word");
        assert_eq!(info & !TI_DEFINED, 0, "transfer information {info:#x}");
        for side_info in [source_info, dest_info] {
            assert_eq!(
                side_info & !INFO_DEFINED,
                0,
                "side information {side_info:#x}"
            );
            let size = side_info & INFO_SIZE;
            assert!(
                size == 0 || size == INFO_SIZE_128,
                "a {size:#x} access size"
            );
        }
        assert_ne!(len, 0, "an empty block");
        assert!(len <= MAX_BLOCK, "a {len}-byte block");
        let (source, dest) = (whole(source, source_info), whole(dest, dest_info));
        if let Some((start, end)) = model.behaviour.region {
            let memory_side = if source_info & INFO_INC != 0 {
                source
            } else {
                dest
            };
            let (from, to) = (memory_side, memory_side + u64::from(len));
            assert!(
                from >= start && to <= end,
                "a block reaches {from:#x}..{to:#x}, outside its buffer {start:#x}..{end:#x}"
            );
        }
        model.info = info;
        model.source = source;
        model.source_info = source_info;
        model.dest = dest;
        model.dest_info = dest_info;
        model.remaining = len;
        model.next = next;
        model.fetched = true;
        model.loaded.push(Block {
            info,
            source,
            source_info,
            dest,
            dest_info,
            len,
            next: u64::from(next) << 5,
        });
        true
    }

    fn locate(&self, offset: usize) -> Result<(usize, usize), DriverError> {
        let channel = offset / CHANNEL_STRIDE;
        if !offset.is_multiple_of(4) || channel >= self.channels.borrow().len() {
            return Err(DriverError::OutOfRange);
        }
        Ok((channel, offset % CHANNEL_STRIDE))
    }
}

/// The low word of `address`.
fn low(address: u64) -> u32 {
    let [b0, b1, b2, b3, ..] = address.to_le_bytes();
    u32::from_le_bytes([b0, b1, b2, b3])
}

/// `info` with `address`'s top byte in its address field.
fn high_into(info: u32, address: u64) -> u32 {
    info & !0xFF | u32::from(address.to_le_bytes()[4])
}

impl RegisterBlock for Model {
    fn read32(&self, offset: usize) -> Result<u32, DriverError> {
        let (channel, register) = self.locate(offset)?;
        let mut channels = self.channels.borrow_mut();
        let model = &mut channels[channel];
        Ok(match register {
            CS => {
                let transactions = if model.draining > 0 {
                    model.draining -= 1;
                    CS_TRANSACTIONS
                } else {
                    0
                };
                let error = if model.errors != 0 { CS_ERROR } else { 0 };
                model.writable | model.latched | transactions | error
            }
            CB => model.cb,
            DEBUG => model.errors,
            TI => model.info,
            SRC => low(model.source),
            SRCI => high_into(model.source_info, model.source),
            DEST => low(model.dest),
            DESTI => high_into(model.dest_info, model.dest),
            LEN => model.remaining,
            NEXT_CB => model.next,
            _ => 0,
        })
    }

    fn write32(&self, offset: usize, value: u32) -> Result<(), DriverError> {
        let (channel, register) = self.locate(offset)?;
        self.timeline.borrow_mut().push(Trace::Write {
            channel,
            register,
            value,
        });
        let mut channels = self.channels.borrow_mut();
        let model = &mut channels[channel];
        match register {
            CS => {
                assert_eq!(
                    value & (CS_ABORT | CS_HALT),
                    0,
                    "the driver pauses and resets, never aborts or halts"
                );
                assert_eq!(
                    value & CS_PROT,
                    CS_PROT,
                    "every status write keeps the protection"
                );
                model.latched &= !(value & CS_INT);
                let was_active = model.writable & CS_ACTIVE != 0;
                model.writable = value & CS_WRITABLE;
                let active = model.writable & CS_ACTIVE != 0;
                if was_active && !active && model.fetched {
                    model.draining = model.behaviour.drain_reads;
                }
                if !was_active && active && model.cb != 0 && !model.fetched {
                    if model.behaviour.defer_fetch {
                        model.behaviour.defer_fetch = false;
                    } else {
                        self.fetch(model);
                    }
                }
            }
            CB => {
                assert_eq!(
                    model.writable & CS_ACTIVE,
                    0,
                    "a block address set while active"
                );
                model.cb = value;
            }
            DEBUG => {
                if value & DEBUG_RESET != 0 {
                    *model = ChannelModel {
                        behaviour: model.behaviour.clone(),
                        loaded: core::mem::take(&mut model.loaded),
                        interrupts: model.interrupts,
                        ..ChannelModel::default()
                    };
                    return Ok(());
                }
                model.errors &= !(value & DEBUG_ERRORS);
            }
            _ => panic!("the driver wrote read-only register {register:#x}"),
        }
        Ok(())
    }

    fn block_len(&self) -> usize {
        self.channels.borrow().len() * CHANNEL_STRIDE
    }
}

/// The model with its resets lost once `armed`: refused, as a bus that drops
/// a write would, or `ignored` — taken and without effect, as a channel that
/// will not reset does.
pub struct Unresettable<'m> {
    pub model: &'m Model,
    pub armed: core::cell::Cell<bool>,
    pub ignored: bool,
}

impl RegisterBlock for Unresettable<'_> {
    fn read32(&self, offset: usize) -> Result<u32, DriverError> {
        self.model.read32(offset)
    }

    fn write32(&self, offset: usize, value: u32) -> Result<(), DriverError> {
        if self.armed.get() && offset % CHANNEL_STRIDE == DEBUG && value & DEBUG_RESET != 0 {
            return if self.ignored {
                Ok(())
            } else {
                Err(DriverError::OutOfRange)
            };
        }
        self.model.write32(offset, value)
    }

    fn block_len(&self) -> usize {
        self.model.block_len()
    }
}
