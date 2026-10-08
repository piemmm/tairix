//! A register-level model of the legacy engines that fetches control blocks
//! from a simulated memory the way the silicon does.
//!
//! It holds the driver to the hardware's rules as assertions: a block must be
//! aligned, reserved words zero, a LITE channel's block within its limit, and
//! every memory-side access inside the buffer the test says the channel owns.
//! A block fetched from memory already freed is a read error, as on metal.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::vec::Vec;

use tairix_abi::{DriverError, RegisterBlock};

use crate::engine::{BlockStore, BlockTable, CHANNEL_STRIDE};

pub const CS_ACTIVE: u32 = 1 << 0;
pub const CS_END: u32 = 1 << 1;
pub const CS_INT: u32 = 1 << 2;
pub const CS_WAITING: u32 = 1 << 6;
pub const CS_ERROR: u32 = 1 << 8;
pub const CS_ABORT: u32 = 1 << 30;
pub const CS_RESET: u32 = 1 << 31;
/// The read-write fields of `CS`: `ACTIVE` and the serving flags.
const CS_WRITABLE: u32 = CS_ACTIVE | 0x30FF_0000;

pub const TI_INTEN: u32 = 1 << 0;
pub const TI_WAIT_RESP: u32 = 1 << 3;
pub const TI_DEST_INC: u32 = 1 << 4;
pub const TI_DEST_WIDTH: u32 = 1 << 5;
pub const TI_DEST_DREQ: u32 = 1 << 6;
pub const TI_SRC_INC: u32 = 1 << 8;
pub const TI_SRC_WIDTH: u32 = 1 << 9;
pub const TI_SRC_DREQ: u32 = 1 << 10;

pub const DEBUG_READ_ERROR: u32 = 1 << 2;
const DEBUG_ERRORS: u32 = 0b111;
const DEBUG_LITE: u32 = 1 << 28;

pub const LITE_MAX_BLOCK: u32 = 65_532;

pub const CS: usize = 0x00;
pub const CONBLK_AD: usize = 0x04;
const TI: usize = 0x08;
const SOURCE_AD: usize = 0x0C;
const DEST_AD: usize = 0x10;
const TXFR_LEN: usize = 0x14;
const NEXTCONBK: usize = 0x1C;
pub const DEBUG: usize = 0x20;

/// Where the first chain is carved.
const TABLE_BASE: u64 = 0xC020_0000;
const TABLE_STEP: u64 = 0x1000;

/// One step the device or the endpoint took, in the order it happened.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Trace {
    /// A register write.
    Write {
        channel: usize,
        register: usize,
        value: u32,
    },
    /// The endpoint dropped its mapping of the buffer at this bus address.
    Released { bus: u64 },
    /// A chain carved at this bus address was freed.
    Freed { bus: u64 },
}

pub type Timeline = Rc<RefCell<Vec<Trace>>>;

/// A block as the channel loaded it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Block {
    pub info: u32,
    pub source: u32,
    pub dest: u32,
    pub len: u32,
    pub next: u32,
}

struct Memory {
    tables: BTreeMap<u64, Vec<u8>>,
    next: u64,
    carved: usize,
    refuse: bool,
    timeline: Timeline,
}

/// How the test has told a channel to behave; a reset keeps it.
#[derive(Clone, Default)]
struct Behaviour {
    lite: bool,
    defer_fetch: bool,
    hold_between_blocks: bool,
    drain_reads: u32,
    region: Option<(u64, u64)>,
}

#[derive(Default)]
struct ChannelModel {
    behaviour: Behaviour,
    writable: u32,
    latched: u32,
    conblk: u32,
    info: u32,
    source: u32,
    dest: u32,
    remaining: u32,
    next: u32,
    errors: u32,
    fetched: bool,
    draining: u32,
    loaded: Vec<Block>,
    interrupts: u32,
}

/// The model: a node's register window plus the memory its chains live in.
pub struct Model {
    channels: RefCell<Vec<ChannelModel>>,
    memory: Rc<RefCell<Memory>>,
    timeline: Timeline,
}

impl Model {
    /// A node of `count` channels, the ones in `lite` being LITE engines.
    pub fn new(count: usize, lite: u64) -> Self {
        let channels = (0..count)
            .map(|index| ChannelModel {
                behaviour: Behaviour {
                    lite: lite & (1 << index) != 0,
                    ..Behaviour::default()
                },
                ..ChannelModel::default()
            })
            .collect();
        let timeline: Timeline = Rc::new(RefCell::new(Vec::new()));
        Self {
            channels: RefCell::new(channels),
            memory: Rc::new(RefCell::new(Memory {
                tables: BTreeMap::new(),
                next: TABLE_BASE,
                carved: 0,
                refuse: false,
                timeline: Rc::clone(&timeline),
            })),
            timeline,
        }
    }

    /// The legacy Pi 4 node: eleven channels, 7–10 LITE.
    pub fn pi4() -> Self {
        Self::new(11, 0b111_1000_0000)
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

    /// Leave each next block unfetched until the next [`Self::advance`].
    pub fn hold_between_blocks(&self, channel: usize) {
        self.channels.borrow_mut()[channel]
            .behaviour
            .hold_between_blocks = true;
    }

    /// Keep `WAITING_FOR_OUTSTANDING_WRITES` up for `reads` reads after a
    /// pause.
    pub fn drain_reads(&self, channel: usize, reads: u32) {
        self.channels.borrow_mut()[channel].behaviour.drain_reads = reads;
    }

    /// Refuse every further carve.
    pub fn refuse_carves(&self) {
        self.memory.borrow_mut().refuse = true;
    }

    pub fn carved(&self) -> usize {
        self.memory.borrow().carved
    }

    pub fn live_tables(&self) -> usize {
        self.memory.borrow().tables.len()
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

    /// Point `channel`'s live source address somewhere of the test's choosing.
    pub fn stray_source(&self, channel: usize, source: u32) {
        self.channels.borrow_mut()[channel].source = source;
    }

    /// Force `CS`'s `ACTIVE` bit, as the hardware clearing it itself would.
    pub fn set_active(&self, channel: usize, active: bool) {
        let mut channels = self.channels.borrow_mut();
        if active {
            channels[channel].writable |= CS_ACTIVE;
        } else {
            channels[channel].writable &= !CS_ACTIVE;
        }
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

    /// Whether any register of `channel` was ever written.
    pub fn touched(&self, channel: usize) -> bool {
        self.timeline
            .borrow()
            .iter()
            .any(|trace| matches!(*trace, Trace::Write { channel: c, .. } if c == channel))
    }

    /// Move `bytes` through `channel`, paced as its request line would.
    pub fn advance(&self, channel: usize, mut bytes: u32) {
        let mut channels = self.channels.borrow_mut();
        let model = &mut channels[channel];
        while bytes > 0 {
            if model.writable & CS_ACTIVE == 0 || model.conblk == 0 || model.errors != 0 {
                return;
            }
            if !model.fetched && !self.fetch(model) {
                return;
            }
            let step = bytes.min(model.remaining);
            if model.info & TI_SRC_INC != 0 {
                model.source += step;
            }
            if model.info & TI_DEST_INC != 0 {
                model.dest += step;
            }
            model.remaining -= step;
            bytes -= step;
            if model.remaining == 0 {
                model.latched |= CS_END;
                if model.info & TI_INTEN != 0 {
                    model.latched |= CS_INT;
                    model.interrupts += 1;
                }
                model.conblk = model.next;
                model.fetched = false;
                if model.conblk == 0 {
                    model.writable &= !CS_ACTIVE;
                    return;
                }
                if !model.behaviour.hold_between_blocks && !self.fetch(model) {
                    return;
                }
            }
        }
    }

    /// Load the block at `CONBLK_AD`, answering whether the fetch succeeded.
    fn fetch(&self, model: &mut ChannelModel) -> bool {
        let address = u64::from(model.conblk);
        assert_eq!(address % 32, 0, "a control block must be 32-byte aligned");
        let memory = self.memory.borrow();
        let Some(words) = memory
            .tables
            .range(..=address)
            .next_back()
            .and_then(|(&base, bytes)| {
                let offset = usize::try_from(address - base).ok()?;
                bytes.get(offset..offset + 32)
            })
            .map(|bytes| {
                let mut words = [0u32; 8];
                for (word, chunk) in words.iter_mut().zip(bytes.as_chunks::<4>().0) {
                    *word = u32::from_le_bytes(*chunk);
                }
                words
            })
        else {
            model.errors |= DEBUG_READ_ERROR;
            model.writable &= !CS_ACTIVE;
            return false;
        };
        let [info, source, dest, len, stride, next, reserved_a, reserved_b] = words;
        assert_eq!(
            (stride, reserved_a, reserved_b),
            (0, 0, 0),
            "reserved words"
        );
        assert_ne!(len, 0, "an empty block");
        if model.behaviour.lite {
            assert!(
                len <= LITE_MAX_BLOCK,
                "a LITE channel was handed a {len}-byte block"
            );
        }
        if let Some((start, end)) = model.behaviour.region {
            let memory_side = if info & TI_SRC_INC != 0 { source } else { dest };
            let (from, to) = (
                u64::from(memory_side),
                u64::from(memory_side) + u64::from(len),
            );
            assert!(
                from >= start && to <= end,
                "a block reaches {from:#x}..{to:#x}, outside its buffer {start:#x}..{end:#x}"
            );
        }
        model.info = info;
        model.source = source;
        model.dest = dest;
        model.remaining = len;
        model.next = next;
        model.fetched = true;
        model.loaded.push(Block {
            info,
            source,
            dest,
            len,
            next,
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

impl RegisterBlock for Model {
    fn read32(&self, offset: usize) -> Result<u32, DriverError> {
        let (channel, register) = self.locate(offset)?;
        let mut channels = self.channels.borrow_mut();
        let model = &mut channels[channel];
        Ok(match register {
            CS => {
                let waiting = if model.draining > 0 {
                    model.draining -= 1;
                    CS_WAITING
                } else {
                    0
                };
                let error = if model.errors != 0 { CS_ERROR } else { 0 };
                model.writable | model.latched | waiting | error
            }
            CONBLK_AD => model.conblk,
            TI => model.info,
            SOURCE_AD => model.source,
            DEST_AD => model.dest,
            TXFR_LEN => model.remaining,
            NEXTCONBK => model.next,
            DEBUG => model.errors | if model.behaviour.lite { DEBUG_LITE } else { 0 },
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
                    value & CS_ABORT,
                    0,
                    "the driver pauses and resets, never aborts"
                );
                if value & CS_RESET != 0 {
                    *model = ChannelModel {
                        behaviour: model.behaviour.clone(),
                        loaded: core::mem::take(&mut model.loaded),
                        interrupts: model.interrupts,
                        ..ChannelModel::default()
                    };
                    return Ok(());
                }
                model.latched &= !(value & (CS_INT | CS_END));
                let was_active = model.writable & CS_ACTIVE != 0;
                model.writable = value & CS_WRITABLE;
                let active = model.writable & CS_ACTIVE != 0;
                if was_active && !active && model.fetched {
                    model.draining = model.behaviour.drain_reads;
                }
                if !was_active && active && model.conblk != 0 && !model.fetched {
                    if model.behaviour.defer_fetch {
                        model.behaviour.defer_fetch = false;
                    } else {
                        self.fetch(model);
                    }
                }
            }
            CONBLK_AD => {
                assert_eq!(
                    model.writable & CS_ACTIVE,
                    0,
                    "a block address set while active"
                );
                model.conblk = value;
            }
            DEBUG => model.errors &= !(value & DEBUG_ERRORS),
            _ => panic!("the driver wrote read-only register {register:#x}"),
        }
        Ok(())
    }

    fn block_len(&self) -> usize {
        self.channels.borrow().len() * CHANNEL_STRIDE
    }
}

/// Carves chains from the model's memory.
pub struct ModelStore {
    memory: Rc<RefCell<Memory>>,
}

impl BlockStore for ModelStore {
    type Table = ModelTable;

    fn carve(&self, bytes: usize) -> Result<ModelTable, DriverError> {
        let mut memory = self.memory.borrow_mut();
        if memory.refuse {
            return Err(DriverError::OutOfMemory);
        }
        let base = memory.next;
        memory.next += TABLE_STEP;
        memory.carved += 1;
        memory.tables.insert(base, std::vec![0u8; bytes]);
        Ok(ModelTable {
            base,
            memory: Rc::clone(&self.memory),
            withheld: false,
        })
    }
}

/// One carve; freed when dropped unless withheld, after which a fetch from it
/// is a read error.
pub struct ModelTable {
    base: u64,
    memory: Rc<RefCell<Memory>>,
    withheld: bool,
}

impl BlockTable for ModelTable {
    fn bus_address(&self) -> u64 {
        self.base
    }

    fn store(&mut self, offset: usize, block: &[u32; 8]) -> Result<(), DriverError> {
        let mut memory = self.memory.borrow_mut();
        let table = memory
            .tables
            .get_mut(&self.base)
            .ok_or(DriverError::OutOfRange)?;
        let bytes = table
            .get_mut(offset..offset + 32)
            .ok_or(DriverError::OutOfRange)?;
        for (chunk, word) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(block) {
            *chunk = word.to_le_bytes();
        }
        Ok(())
    }

    fn withhold(&mut self) {
        self.withheld = true;
    }
}

impl Drop for ModelTable {
    fn drop(&mut self) {
        if self.withheld {
            return;
        }
        let mut memory = self.memory.borrow_mut();
        memory.tables.remove(&self.base);
        memory
            .timeline
            .borrow_mut()
            .push(Trace::Freed { bus: self.base });
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
        if self.armed.get() && offset % CHANNEL_STRIDE == CS && value == CS_RESET {
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
