//! Arm `SMMUv3`: one System MMU, driven through a stream table, stage 2
//! translation where the unit has it and stage 1 otherwise, its command queue
//! and its event queue.
//!
//! The unit is taken over aborting every transaction while it is disabled
//! (`GBPA.ABORT`), with a stream table of invalid entries, so once translation
//! is on every device's DMA is aborted and recorded (`C_BAD_STE`) until its
//! stream is attached; firmware's reserved windows are attached before
//! [`IommuUnit::enable`]. Every removal is confirmed by a `CMD_SYNC` before it
//! is reported done.
//!
//! Reference: Arm System Memory Management Unit Architecture Specification,
//! SMMU architecture version 3 (Arm IHI 0070). The design and its staging are
//! `plans/IOMMU.md` IOM15.

#![no_std]
#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod format;
mod regs;

#[cfg(test)]
mod model;
#[cfg(test)]
mod tests;

pub use tairix_kernel_iommu_api::Registers;

use core::cell::Cell;

use tairix_arch_api::PageTableFrames;
use tairix_collections::HashMap;
use tairix_hash::BuildFastHash;
use tairix_inline::ArrayVec;
use tairix_kernel_iommu_api::{
    drain_in_batches, reach_bits, wait_for, Access, Binding, Bindings, Block, Clock, Command,
    CommandQueue, Completion, DomainId, Fault, FaultBatch, FaultRoute, Ids, IoPageTable,
    IommuError, IommuUnit, QueueRegisters, Reach, Stage, Table, TableMemory, UnitProfile,
    FAULT_QUEUE_RECORDS, MAX_ROOT_ORDER, TABLE_BYTES,
};
use tairix_sync::SpinLock;

use crate::format::{ArmTables, Entry, EVENT_WORDS, SPLIT, STE_WORDS};
use crate::regs::{Idr0, Idr1, Idr5};

/// The match key discovery gives an `SMMUv3` and the kernel binds this family
/// to: the one definition both sides use.
pub const COMPATIBLE: &[u8] = b"arm,smmu-v3";

/// The `interrupt-names` a unit's node may give the line it raises its
/// faults on, in preference: the one line every source shares, else the
/// event queue's.
pub const FAULT_INTERRUPTS: [&[u8]; 2] = [b"combined", b"eventq"];

/// The place among `names`, a unit's `interrupt-names` in order, of the line
/// it raises its faults on.
pub fn fault_interrupt<'n, I>(names: &I) -> Option<u32>
where
    I: Iterator<Item = &'n [u8]> + Clone,
{
    FAULT_INTERRUPTS.iter().find_map(|wanted| {
        names
            .clone()
            .position(|name| name == *wanted)
            .and_then(|place| u32::try_from(place).ok())
    })
}

/// VMID and ASID 0 are never handed out: neither names a domain of ours.
const FIRST_DOMAIN: u32 = 1;

/// `log2` of the commands the queue holds: one table frame of them.
const COMMAND_BITS: u32 = CommandQueue::SLOTS.trailing_zeros();

/// The commands the queue holds, as the unit's indices count them.
const COMMAND_SLOTS: u32 = 1 << COMMAND_BITS;

/// `log2` of the most events the queue is made to hold: as many as any
/// family's, enough to ride out a storm the fault budget then silences.
const EVENT_BITS: u32 = FAULT_QUEUE_RECORDS.trailing_zeros();

/// Stream-id bits a two-level table covers: a level-1 table of one MiB. A
/// stream past it is refused by the unit (`C_BAD_STREAMID`), never let
/// through.
const TWO_LEVEL_STREAM_BITS: u32 = 23;

/// Stream-id bits a linear table covers: one MiB of entries.
const LINEAR_STREAM_BITS: u32 = 14;

/// The widest IOVA a stage 2 walk starting at level 1 resolves: the most
/// tables a root may concatenate there.
const CONCATENATED_BITS: u32 = reach_bits(3) + MAX_ROOT_ORDER;

/// `log2` of the table frames `2^entries` entries of `entry_bytes` each span,
/// one at least.
const fn frame_order(entries: u32, entry_bytes: usize) -> u32 {
    (entries + entry_bytes.trailing_zeros()).saturating_sub(TABLE_BYTES.trailing_zeros())
}

/// One `SMMUv3`.
///
/// Its stream tables, queues and context descriptors are never freed:
/// nothing proves the unit stopped reading them.
pub struct Smmuv3Unit<'f, R: Registers> {
    memory: TableMemory<'f>,
    clock: &'f dyn Clock,
    stage: Stage,
    levels: u32,
    /// `log2` of the tables a stage 2 walk's first level concatenates.
    root_order: u32,
    input_bits: u32,
    /// The output size's encoding.
    output: u32,
    stream_bits: u32,
    event_bits: u32,
    /// The unit may stall a fault, so each entry must forbid it.
    stalls: bool,
    msi: bool,
    profile: UnitProfile,
    state: SpinLock<State<'f, R>>,
}

struct State<'f, R> {
    regs: R,
    streams: StreamTable,
    queue: CommandQueue,
    /// The command queue's producer index as last written, wrap included.
    prod: Cell<u32>,
    /// `SMMU_GERRORN` as last written: only this family writes it.
    acked: Cell<u32>,
    events: Block,
    /// The event queue's consumer index, wrap and overflow acknowledgement
    /// included.
    cons: u32,
    domains: HashMap<u16, DomainState<'f>, BuildFastHash>,
    bindings: Bindings,
    ids: Ids,
}

enum StreamTable {
    /// One entry per stream.
    Linear(Block),
    /// Level-1 descriptors, each leading to a table of `2^SPLIT` entries
    /// once a stream it covers is first written.
    TwoLevel {
        level1: Block,
        tables: HashMap<u32, Table, BuildFastHash>,
    },
}

/// Where one stream's entry lives.
enum Slot<'t> {
    Linear(&'t Block, usize),
    Table(&'t Table, usize),
}

struct DomainState<'f> {
    table: IoPageTable<'f, ArmTables>,
    /// A stage 1 domain's context descriptor.
    descriptor: Option<Table>,
}

/// The stage a unit translates at, the input its tables take, and where
/// their walk starts: the levels below the root, and the order of tables the
/// root concatenates.
struct Geometry {
    stage: Stage,
    input_bits: u32,
    levels: u32,
    root_order: u32,
}

impl Geometry {
    /// The geometry of a unit `idr0` describes, whose output is
    /// `output_bits` wide.
    fn of(idr0: Idr0, output_bits: u32) -> Result<Self, IommuError> {
        let stage = if idr0.stage2() {
            Stage::Second
        } else if idr0.stage1() {
            Stage::First
        } else {
            return Err(IommuError::OutOfRange);
        };
        // A stage 2 input is an intermediate physical address, no wider than
        // the output. Its walk starts at level 0 only on a 44-bit output or
        // wider, so a narrower one past three levels' reach starts at level 1
        // over the tables concatenated there.
        let input_bits = match stage {
            Stage::Second => output_bits,
            Stage::First => reach_bits(4),
        };
        let (levels, root_order) = match input_bits {
            bits if bits <= reach_bits(3) => (3, 0),
            bits if stage == Stage::Second && bits <= CONCATENATED_BITS => {
                (3, bits - reach_bits(3))
            }
            _ => (4, 0),
        };
        Ok(Self {
            stage,
            input_bits,
            levels,
            root_order,
        })
    }
}

impl<'f, R: Registers> Smmuv3Unit<'f, R> {
    /// Take over the unit behind `regs`, leaving it disabled and aborting
    /// every transaction, its stream table of invalid entries installed and
    /// its queues running, its interrupts masked. [`IommuUnit::enable`]
    /// starts translating.
    ///
    /// # Errors
    ///
    /// [`IommuError::OutOfRange`] for a unit this family cannot drive: one
    /// without AArch64 tables at a 4 KiB granule or a translation stage,
    /// walking big-endian, forcing stalls, with preset tables or queues, a
    /// command queue shorter than one frame, a register window short of both
    /// pages, or whose table and queue accesses do not snoop the CPUs' caches
    /// (reading its event queue would need them invalidated first).
    /// [`IommuError::Hardware`] for one in service-failure mode.
    /// [`IommuError::Exhausted`] when its tables cannot be had, and the
    /// unit's own errors or timeouts.
    pub fn new(
        regs: R,
        frames: &'f dyn PageTableFrames,
        clock: &'f dyn Clock,
    ) -> Result<Self, IommuError> {
        if regs.window_len() < regs::WINDOW {
            return Err(IommuError::OutOfRange);
        }
        let idr0 = Idr0(regs.read32(regs::IDR0)?);
        let idr1 = Idr1(regs.read32(regs::IDR1)?);
        let idr5 = Idr5(regs.read32(regs::IDR5)?);
        let (output, output_bits) = idr5
            .output(format::OUTPUT_BITS)
            .ok_or(IommuError::OutOfRange)?;
        let Geometry {
            stage,
            input_bits,
            levels,
            root_order,
        } = Geometry::of(idr0, output_bits)?;
        if !idr0.aarch64_tables()
            || !idr0.little_endian()
            || !idr0.coherent()
            || idr0.stall_model() == 0b10
            || !idr5.granule_4k()
            || idr1.preset()
            || idr1.cmdq_bits() < COMMAND_BITS
        {
            return Err(IommuError::OutOfRange);
        }
        let two_level = idr0.two_level() && idr1.stream_bits() > SPLIT;
        let stream_bits = idr1.stream_bits().min(if two_level {
            TWO_LEVEL_STREAM_BITS
        } else {
            LINEAR_STREAM_BITS
        });
        let event_bits = idr1.eventq_bits().min(EVENT_BITS);
        let id_bits = match stage {
            Stage::Second if idr0.vmid16() => 16,
            Stage::First if idr0.asid16() => 16,
            _ => 8,
        };
        let memory = TableMemory::new(frames, None);
        let ids = Ids::new(FIRST_DOMAIN, 1 << id_bits);
        let streams = StreamTable::new(&memory, two_level, stream_bits)?;
        let queue = match CommandQueue::new(&memory) {
            Ok(queue) => queue,
            Err(err) => {
                streams.release(&memory);
                return Err(err);
            }
        };
        let events = match memory.alloc_block(frame_order(event_bits, EVENT_WORDS * 8)) {
            Ok(events) => events,
            Err(err) => {
                streams.release(&memory);
                queue.release(&memory);
                return Err(err);
            }
        };
        let unit = Self {
            memory,
            clock,
            stage,
            levels,
            root_order,
            input_bits,
            output,
            stream_bits,
            event_bits,
            stalls: idr0.stall_model() == 0b00,
            msi: idr0.msi(),
            profile: UnitProfile {
                stage,
                reach: Reach {
                    input_bits,
                    output_bits,
                },
                reserved: &[],
            },
            state: SpinLock::new(State {
                regs,
                streams,
                queue,
                prod: Cell::new(0),
                acked: Cell::new(0),
                events,
                cons: 0,
                domains: HashMap::with_hasher(BuildFastHash::new()),
                bindings: Bindings::new(),
                ids,
            }),
        };
        // From here the unit may hold the tables' addresses, so a failure
        // keeps them.
        unit.take_over()?;
        Ok(unit)
    }

    fn take_over(&self) -> Result<(), IommuError> {
        let mut state = self.state.lock();
        let regs = &state.regs;
        // Firmware may have left the unit translating, or bypassing; from here
        // to the end of the hand-off every transaction is aborted instead. A
        // write while an update is still under way would be ignored.
        let updated = || Ok(regs.read32(regs::GBPA)? & regs::GBPA_UPDATE == 0);
        wait_for(self.clock, updated)?;
        regs.write32(regs::GBPA, regs::GBPA_ABORT | regs::GBPA_UPDATE)?;
        wait_for(self.clock, updated)?;
        self.control(regs, 0)?;
        regs.write32(regs::IRQ_CTRL, 0)?;
        wait_for(self.clock, || Ok(regs.read32(regs::IRQ_CTRLACK)? == 0))?;
        // An error firmware left active would be blamed on our first batch;
        // a unit failed in service translates nothing whatever is
        // acknowledged.
        let errors = regs.read32(regs::GERROR)?;
        if (errors ^ regs.read32(regs::GERRORN)?) & regs::GERROR_SFM != 0 {
            return Err(IommuError::Hardware);
        }
        regs.write32(regs::GERRORN, errors)?;
        state.acked.set(errors);
        regs.write32(regs::CR1, format::CR1)?;
        regs.write32(regs::CR2, regs::CR2_PTM | regs::CR2_RECINVSID)?;
        let (base, config) = state.streams.registers(self.stream_bits);
        regs.write64(regs::STRTAB_BASE, base | regs::BASE_ALLOCATE)?;
        regs.write32(regs::STRTAB_BASE_CFG, config)?;
        regs.write64(
            regs::CMDQ_BASE,
            state.queue.ring() | regs::BASE_ALLOCATE | u64::from(COMMAND_BITS),
        )?;
        regs.write32(regs::CMDQ_PROD, 0)?;
        regs.write32(regs::CMDQ_CONS, 0)?;
        regs.write64(
            regs::EVENTQ_BASE,
            state.events.phys() | regs::BASE_ALLOCATE | u64::from(self.event_bits),
        )?;
        regs.write32(regs::EVENTQ_PROD, 0)?;
        regs.write32(regs::EVENTQ_CONS, 0)?;
        self.control(regs, regs::CR0_CMDQEN | regs::CR0_EVENTQEN)?;
        // Nothing firmware's configuration left cached may be read again.
        self.run(&mut state, [format::cfgi_all(), format::tlbi_all()])
    }

    /// Write `SMMU_CR0` and wait for the unit to take it.
    fn control(&self, regs: &R, value: u32) -> Result<(), IommuError> {
        regs.write32(regs::CR0, value)?;
        wait_for(self.clock, || Ok(regs.read32(regs::CR0ACK)? == value))
    }

    /// Queue `commands` and a `CMD_SYNC` behind them, and return once the unit
    /// confirms every one is done.
    fn run(
        &self,
        state: &mut State<'f, R>,
        commands: impl IntoIterator<Item = Command>,
    ) -> Result<(), IommuError> {
        let State {
            regs,
            queue,
            prod,
            acked,
            ..
        } = state;
        let registers = Queue {
            regs,
            prod,
            acked,
            msi: self.msi,
        };
        let sync = match registers.completion() {
            Completion::Stored => format::sync_stored,
            Completion::Consumed => format::sync_consumed,
        };
        queue.run(&self.memory, self.clock, &registers, commands, sync)
    }

    /// What forgets everything the unit cached of `stream`'s configuration:
    /// its entry, the level-1 descriptor leading to it where that was just
    /// written, and at stage 1 the context descriptor it named.
    fn forget_stream(&self, stream: u32, linked: bool) -> ArrayVec<Command, 2> {
        let mut commands = ArrayVec::new();
        if self.stage == Stage::First {
            let _ = commands.try_push(format::cfgi_cd_all(stream));
        }
        let _ = commands.try_push(format::cfgi_ste(stream, !linked));
        commands
    }

    /// What forgets every translation the unit cached for domain `id`.
    fn forget_domain(&self, id: u16) -> Command {
        match self.stage {
            Stage::Second => format::tlbi_vmid(id),
            Stage::First => format::tlbi_asid(id),
        }
    }

    /// The entry translating a stream through domain `id`.
    fn entry_for(&self, id: u16, domain: &DomainState<'f>) -> Result<Entry, IommuError> {
        match (self.stage, &domain.descriptor) {
            (Stage::Second, _) => Ok(format::stage2_ste(format::Stage2 {
                vmid: id,
                root: domain.table.root(),
                input_bits: self.input_bits,
                levels: self.levels,
                output: self.output,
            })),
            (Stage::First, Some(descriptor)) => {
                Ok(format::stage1_ste(descriptor.phys(), self.stalls))
            }
            (Stage::First, None) => Err(IommuError::Hardware),
        }
    }

    fn check_stream(&self, stream: u32) -> Result<(), IommuError> {
        if u64::from(stream) >> self.stream_bits != 0 {
            return Err(IommuError::OutOfRange);
        }
        Ok(())
    }

    /// Write `entry` as `stream`'s: a translating entry's other words first,
    /// synced, then its word 0, so no fetch pairs a new word 0 with an old
    /// word. Whether a level-1 descriptor had to be written to reach it.
    fn write_entry(
        &self,
        state: &mut State<'f, R>,
        stream: u32,
        entry: &Entry,
    ) -> Result<bool, IommuError> {
        let (slot, linked) = state.streams.slot(&self.memory, stream)?;
        if format::translates(entry) {
            // The unit reads an entry 64 bits at a time in no set order, so
            // every fetch already under way is finished with the old words
            // before word 0 names the new ones.
            for (word, &value) in entry.iter().enumerate().skip(1) {
                slot.write(&self.memory, word, value)?;
            }
            slot.publish(&self.memory);
            tairix_dma_barrier::dma_wmb();
            let forget = self.forget_stream(stream, linked);
            self.run(state, forget)?;
        }
        let (slot, _) = state.streams.slot(&self.memory, stream)?;
        slot.write(&self.memory, 0, entry[0])?;
        slot.publish(&self.memory);
        Ok(linked)
    }

    /// Point `stream` at a blocking entry and confirm the unit forgot what it
    /// translated through, which it holds until then.
    fn block_stream(&self, state: &mut State<'f, R>, stream: u32) -> Result<(), IommuError> {
        let linked = self.write_entry(state, stream, &[0; STE_WORDS])?;
        state.bindings.unbind(stream);
        let forget = self.forget_stream(stream, linked);
        self.run(state, forget)?;
        state.bindings.release(stream);
        Ok(())
    }

    /// Move pending events into `batch`, oldest first, and answer whether
    /// more remain. An overflow is acknowledged: the events it dropped are
    /// gone.
    fn take_events(&self, batch: &mut FaultBatch) -> bool {
        let mut state = self.state.lock();
        let Ok(prod) = state.regs.read32(regs::EVENTQ_PROD) else {
            return false;
        };
        // The records the index announces are read only after it.
        tairix_dma_barrier::dma_rmb();
        let wrap = 1u32 << self.event_bits;
        let index = (wrap << 1) - 1;
        let mut cons = (state.cons & index) | (prod & regs::EVENTQ_OVERFLOW);
        while cons & index != prod & index && !batch.is_full() {
            let slot = (cons & (wrap - 1)) as usize * EVENT_WORDS;
            let mut record = [0u64; EVENT_WORDS];
            for (word, value) in record.iter_mut().enumerate() {
                match self.memory.read_block(&state.events, slot + word) {
                    Ok(read) => *value = read,
                    Err(_) => return false,
                }
            }
            if let Some(fault) = format::fault(&record) {
                let _ = batch.try_push(fault);
            }
            cons = (((cons & index) + 1) & index) | (cons & regs::EVENTQ_OVERFLOW);
        }
        state.cons = cons;
        // Every slot is read before it is handed back to the unit to fill.
        tairix_dma_barrier::dma_rmb();
        if state.regs.write32(regs::EVENTQ_CONS, cons).is_err() {
            return false;
        }
        // A record lost to an aborted queue write is gone; the queue runs on.
        if let Ok(errors) = state.regs.read32(regs::GERROR) {
            let acked = state.acked.get();
            if (errors ^ acked) & regs::GERROR_EVENTQ_ABT != 0 {
                let acked = acked ^ regs::GERROR_EVENTQ_ABT;
                if state.regs.write32(regs::GERRORN, acked).is_ok() {
                    state.acked.set(acked);
                }
            }
        }
        // A record landing after the drain read the index raises no
        // interrupt of its own, so the index is read again.
        state
            .regs
            .read32(regs::EVENTQ_PROD)
            .is_ok_and(|prod| cons & index != prod & index)
    }
}

/// The command queue's registers as the shared queue reads them: the slot
/// in the consumer index, and the producer index written with the wrap a
/// lap past slot 0 flips.
struct Queue<'r, R> {
    regs: &'r R,
    prod: &'r Cell<u32>,
    acked: &'r Cell<u32>,
    msi: bool,
}

impl<R: Registers> QueueRegisters for Queue<'_, R> {
    fn head(&self) -> Result<usize, IommuError> {
        Ok((self.regs.read32(regs::CMDQ_CONS)? & (COMMAND_SLOTS - 1)) as usize)
    }

    fn set_tail(&self, tail: usize) -> Result<(), IommuError> {
        let tail = u32::try_from(tail).map_err(|_| IommuError::Hardware)?;
        let last = self.prod.get();
        let lapped = if tail < last & (COMMAND_SLOTS - 1) {
            COMMAND_SLOTS
        } else {
            0
        };
        let next = tail | ((last & COMMAND_SLOTS) ^ lapped);
        self.regs.write32(regs::CMDQ_PROD, next)?;
        self.prod.set(next);
        Ok(())
    }

    /// A rejected command is replaced by a `CMD_SYNC`, and the error
    /// acknowledged, which lets the unit consume on.
    fn stopped(&self, queue: &CommandQueue, memory: &TableMemory<'_>) -> Result<bool, IommuError> {
        let acked = self.acked.get();
        let active = self.regs.read32(regs::GERROR)? ^ acked;
        if active & regs::GERROR_SFM != 0 {
            return Err(IommuError::Hardware);
        }
        if active & regs::GERROR_CMDQ == 0 {
            return Ok(false);
        }
        let head = (self.regs.read32(regs::CMDQ_CONS)? & (COMMAND_SLOTS - 1)) as usize;
        queue.replace(memory, head, format::sync_consumed(0, 0))?;
        tairix_dma_barrier::dma_wmb();
        self.regs
            .write32(regs::GERRORN, acked ^ regs::GERROR_CMDQ)?;
        self.acked.set(acked ^ regs::GERROR_CMDQ);
        Ok(true)
    }

    fn completion(&self) -> Completion {
        if self.msi {
            Completion::Stored
        } else {
            Completion::Consumed
        }
    }
}

impl StreamTable {
    fn new(
        memory: &TableMemory<'_>,
        two_level: bool,
        stream_bits: u32,
    ) -> Result<Self, IommuError> {
        if two_level {
            let descriptor = core::mem::size_of::<u64>();
            Ok(Self::TwoLevel {
                level1: memory.alloc_block(frame_order(stream_bits - SPLIT, descriptor))?,
                tables: HashMap::with_hasher(BuildFastHash::new()),
            })
        } else {
            Ok(Self::Linear(memory.alloc_block(frame_order(
                stream_bits,
                format::STE_BYTES,
            ))?))
        }
    }

    /// Give the table back: for one no unit was ever pointed at.
    fn release(self, memory: &TableMemory<'_>) {
        match self {
            Self::Linear(block) => memory.free_block(block),
            Self::TwoLevel { level1, tables } => {
                memory.free_block(level1);
                for (_, table) in tables {
                    memory.free(table);
                }
            }
        }
    }

    /// `SMMU_STRTAB_BASE`'s address and `SMMU_STRTAB_BASE_CFG`.
    fn registers(&self, stream_bits: u32) -> (u64, u32) {
        match self {
            Self::Linear(block) => (block.phys(), stream_bits),
            Self::TwoLevel { level1, .. } => (
                level1.phys(),
                regs::STRTAB_TWO_LEVEL | SPLIT << regs::STRTAB_SPLIT_SHIFT | stream_bits,
            ),
        }
    }

    /// Where `stream`'s entry lives, linking its second-level table the first
    /// time a stream it holds is written; whether one was linked.
    fn slot(
        &mut self,
        memory: &TableMemory<'_>,
        stream: u32,
    ) -> Result<(Slot<'_>, bool), IommuError> {
        match self {
            Self::Linear(block) => Ok((Slot::Linear(block, stream as usize * STE_WORDS), false)),
            Self::TwoLevel { level1, tables } => {
                let index = stream >> SPLIT;
                let mut linked = false;
                if !tables.contains_key(&index) {
                    tables.try_reserve(1).map_err(|_| IommuError::Exhausted)?;
                    let table = memory.alloc()?;
                    if let Err(err) =
                        memory.write_block(level1, index as usize, format::level1(table.phys()))
                    {
                        memory.free(table);
                        return Err(err);
                    }
                    memory.publish_block(level1, index as usize, 1);
                    let _ = tables.try_insert(index, table);
                    linked = true;
                }
                let table = tables.get(&index).ok_or(IommuError::Hardware)?;
                let first = (stream & ((1 << SPLIT) - 1)) as usize * STE_WORDS;
                Ok((Slot::Table(table, first), linked))
            }
        }
    }
}

impl Slot<'_> {
    fn write(&self, memory: &TableMemory<'_>, word: usize, value: u64) -> Result<(), IommuError> {
        match *self {
            Self::Linear(block, first) => memory.write_block(block, first + word, value),
            Self::Table(table, first) => memory.write(table, first + word, value),
        }
    }

    fn publish(&self, memory: &TableMemory<'_>) {
        match *self {
            Self::Linear(block, first) => memory.publish_block(block, first, STE_WORDS),
            Self::Table(table, first) => memory.publish(table, first, STE_WORDS),
        }
    }
}

impl<R: Registers> IommuUnit for Smmuv3Unit<'_, R> {
    fn profile(&self) -> UnitProfile {
        self.profile
    }

    fn enable(&self) -> Result<(), IommuError> {
        let state = self.state.lock();
        self.control(
            &state.regs,
            regs::CR0_CMDQEN | regs::CR0_EVENTQEN | regs::CR0_SMMUEN,
        )
    }

    fn create_domain(&self) -> Result<DomainId, IommuError> {
        let mut state = self.state.lock();
        state
            .domains
            .try_reserve(1)
            .map_err(|_| IommuError::Exhausted)?;
        let id = state.ids.take_sixteen_bits()?;
        let built = IoPageTable::new(
            ArmTables {
                stage: self.stage,
                root_order: self.root_order,
            },
            self.levels,
            self.memory,
            self.profile.reach,
        )
        .and_then(|table| {
            let descriptor = match self.stage {
                Stage::Second => None,
                Stage::First => Some(self.descriptor(id, table.root())?),
            };
            Ok(DomainState { table, descriptor })
        });
        match built {
            Ok(domain) => {
                let _ = state.domains.try_insert(id, domain);
                Ok(DomainId(u32::from(id)))
            }
            Err(err) => {
                state.ids.release(u32::from(id), true);
                Err(err)
            }
        }
    }

    fn destroy_domain(&self, domain: DomainId) -> Result<(), IommuError> {
        let id = domain.sixteen_bits()?;
        let mut state = self.state.lock();
        if !state.domains.contains_key(&id) {
            return Err(IommuError::OutOfRange);
        }
        if state.bindings.holders(u32::from(id)) != 0 {
            return Err(IommuError::DomainBusy);
        }
        // Nothing the unit cached for the id may outlive its tables, or
        // survive into the id's next owner.
        let forget = [self.forget_domain(id)];
        self.run(&mut state, forget)
            .map_err(|_| IommuError::Unconfirmed)?;
        if let Some(gone) = state.domains.remove(&id) {
            if let Some(descriptor) = gone.descriptor {
                self.memory.free(descriptor);
            }
        }
        state.ids.release(u32::from(id), true);
        Ok(())
    }

    fn attach(&self, stream: u32, domain: DomainId) -> Result<(), IommuError> {
        self.check_stream(stream)?;
        let id = domain.sixteen_bits()?;
        let mut state = self.state.lock();
        let entry = self.entry_for(id, state.domains.get(&id).ok_or(IommuError::OutOfRange)?)?;
        let Some(reserved) = state.bindings.prepare_attach(stream, u32::from(id))? else {
            return Ok(());
        };
        let linked = self.write_entry(&mut state, stream, &entry)?;
        // From the write on the unit may translate the stream through the
        // domain, so it holds the domain whether or not the unit confirms.
        state.bindings.hold(reserved, stream, u32::from(id));
        let forget = self.forget_stream(stream, linked);
        if let Err(err) = self.run(&mut state, forget) {
            // Taken back, and held until the unit confirms it forgot.
            let _ = self.block_stream(&mut state, stream);
            return Err(err);
        }
        Ok(())
    }

    fn block(&self, stream: u32) -> Result<(), IommuError> {
        self.check_stream(stream)?;
        let mut state = self.state.lock();
        // Only an attach ends silence.
        if !state.bindings.holds_domain(stream) {
            return Ok(());
        }
        self.block_stream(&mut state, stream)
    }

    fn silence(&self, stream: u32) -> Result<(), IommuError> {
        self.check_stream(stream)?;
        let mut state = self.state.lock();
        if state.bindings.get(stream) == Some(Binding::Silenced) {
            return Ok(());
        }
        let reserved = state.bindings.reserve()?;
        let linked = self.write_entry(&mut state, stream, &format::silent_ste())?;
        state.bindings.unbind(stream);
        state.bindings.silence(reserved, stream);
        let forget = self.forget_stream(stream, linked);
        self.run(&mut state, forget)?;
        state.bindings.release(stream);
        Ok(())
    }

    fn map(
        &self,
        domain: DomainId,
        iova: u64,
        phys: u64,
        len: u64,
        access: Access,
    ) -> Result<(), IommuError> {
        // A stage 1 entry cannot let a device write what it may not read.
        if self.stage == Stage::First && !access.read() {
            return Err(IommuError::OutOfRange);
        }
        let id = domain.sixteen_bits()?;
        let mut state = self.state.lock();
        let domain = state.domains.get_mut(&id).ok_or(IommuError::OutOfRange)?;
        // The unit caches no translation it faulted on, so a new mapping
        // needs no invalidation to be used.
        domain.table.map(iova, phys, len, access)
    }

    fn unmap(&self, domain: DomainId, iova: u64, len: u64) -> Result<(), IommuError> {
        let id = domain.sixteen_bits()?;
        let mut state = self.state.lock();
        state
            .domains
            .get_mut(&id)
            .ok_or(IommuError::OutOfRange)?
            .table
            .unmap(iova, len)
    }

    fn sync(&self, domain: DomainId) -> Result<(), IommuError> {
        let id = domain.sixteen_bits()?;
        let mut state = self.state.lock();
        if !state.domains.contains_key(&id) {
            return Err(IommuError::OutOfRange);
        }
        let forget = [self.forget_domain(id)];
        self.run(&mut state, forget)
            .map_err(|_| IommuError::Unconfirmed)?;
        if let Some(domain) = state.domains.get_mut(&id) {
            domain.table.release_retired();
        }
        Ok(())
    }

    /// Faults are the event queue's records; the global-error interrupt is
    /// raised with them, its errors read as each drain runs.
    fn route_faults(&self, route: FaultRoute) -> Result<(), IommuError> {
        if let FaultRoute::Message { address, .. } = route {
            if !self.msi || address & 0b11 != 0 || address >> regs::IRQ_ADDRESS_BITS != 0 {
                return Err(IommuError::OutOfRange);
            }
        }
        let state = self.state.lock();
        let regs = &state.regs;
        // An interrupt's configuration may change only while it is off.
        regs.write32(regs::IRQ_CTRL, 0)?;
        wait_for(self.clock, || Ok(regs.read32(regs::IRQ_CTRLACK)? == 0))?;
        match route {
            FaultRoute::Message { address, data } => {
                for (address_at, data_at, attribute_at) in [
                    (
                        regs::EVENTQ_IRQ_CFG0,
                        regs::EVENTQ_IRQ_CFG1,
                        regs::EVENTQ_IRQ_CFG2,
                    ),
                    (
                        regs::GERROR_IRQ_CFG0,
                        regs::GERROR_IRQ_CFG1,
                        regs::GERROR_IRQ_CFG2,
                    ),
                ] {
                    regs.write64(address_at, address)?;
                    regs.write32(data_at, data)?;
                    regs.write32(attribute_at, regs::MSI_DEVICE)?;
                }
            }
            // A message address of zero leaves the wired line signalling. The
            // event queue's line is fixed, named in the node by name.
            FaultRoute::Wired { .. } if self.msi => {
                regs.write64(regs::EVENTQ_IRQ_CFG0, 0)?;
                regs.write64(regs::GERROR_IRQ_CFG0, 0)?;
            }
            FaultRoute::Wired { .. } => {}
        }
        let enabled = regs::IRQ_EVENTQ | regs::IRQ_GERROR;
        regs.write32(regs::IRQ_CTRL, enabled)?;
        wait_for(
            self.clock,
            || Ok(regs.read32(regs::IRQ_CTRLACK)? == enabled),
        )
    }

    /// At most one queue's worth of events per call, each batch reaching
    /// `sink` with the unit unlocked, since what it does about a fault may be
    /// to call back in.
    fn drain_faults(&self, sink: &mut dyn FnMut(Fault)) -> bool {
        drain_in_batches(1 << self.event_bits, |batch| self.take_events(batch), sink)
    }
}

impl<R: Registers> Smmuv3Unit<'_, R> {
    /// A context descriptor translating through the tables at `root` under
    /// ASID `asid`, written whole before any entry can name it.
    fn descriptor(&self, asid: u16, root: u64) -> Result<Table, IommuError> {
        let table = self.memory.alloc()?;
        let words = format::context_descriptor(format::Stage1 {
            asid,
            root,
            input_bits: self.input_bits,
            output: self.output,
        });
        for (word, value) in words.into_iter().enumerate() {
            if let Err(err) = self.memory.write(&table, word, value) {
                self.memory.free(table);
                return Err(err);
            }
        }
        self.memory.publish(&table, 0, STE_WORDS);
        tairix_dma_barrier::dma_wmb();
        Ok(table)
    }
}
