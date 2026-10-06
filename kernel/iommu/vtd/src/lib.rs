//! Intel VT-d: one DMA remapping hardware unit, driven in legacy translation
//! mode with queued invalidation.
//!
//! The unit is brought up blocked — a root table with no context tables, so
//! every device's DMA faults — then firmware's reserved windows are attached,
//! then translation is enabled ([`VtdUnit::new`], [`VtdUnit::enable`]). From
//! then on a source id reaches memory only through the domain it is attached
//! to, and every removal is confirmed by an invalidation wait before it is
//! reported done.
//!
//! Reference: Intel Virtualization Technology for Directed I/O, Architecture
//! Specification, rev. 4.1. The design and its staging are `plans/IOMMU.md`.

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

use core::ops::Range;

use tairix_arch_api::PageTableFrames;
use tairix_collections::HashMap;
use tairix_hash::BuildFastHash;
use tairix_kernel_iommu_api::{
    drain_in_batches, reach_bits, wait_for, Access, Binding, Bindings, Block, Clock, CommandQueue,
    DomainId, Fault, FaultBatch, FaultReason, FaultRoute, Ids, InterruptRemapping, InterruptSource,
    InterruptTarget, IoPageTable, IommuError, IommuUnit, QueueRegisters, Reach, Remapped, Table,
    TableCoherence, TableMemory, UnitProfile, FAULT_QUEUE_RECORDS, IO_PAGE_SIZE, MESSAGE_WINDOW,
};

const _: () = assert!(
    regs::MOST_FAULT_RECORDS <= FAULT_QUEUE_RECORDS as usize,
    "a unit holds no more fault records than the containment bound allows"
);
use tairix_sync::SpinLock;

use crate::format::{Descriptor, SecondLevel};
use crate::regs::{Cap, Ecap};

/// The match key discovery gives a VT-d unit and the kernel binds this family
/// to: the one definition both sides use.
pub const COMPATIBLE: &[u8] = b"intel,vtd";

/// IOVA windows the fabric claims before translation.
static RESERVED: [Range<u64>; 1] = [MESSAGE_WINDOW];

/// Buses one segment has, and so root entries one root table holds.
const BUSES: usize = 256;

/// Domain id 0 is reserved: under caching mode the unit tags the invalid
/// translations it caches with it.
const FIRST_DOMAIN: u32 = 1;

/// Bits 51:12 of an entry name a frame, so no table can reach past them.
const ENTRY_ADDRESS_BITS: u32 = 52;

/// The fault record's fault bit, which software writes to clear.
const FAULT_F: u64 = 1 << 63;
/// Set in a fault record for a read (or atomic) request, clear for a write.
const FAULT_T1: u64 = 1 << 62;
/// Set in a fault record whose type field means something other than
/// read-or-write.
const FAULT_T2: u64 = 1 << 28;

/// A context entry that blocks its stream without recording its faults:
/// present, fault processing disabled.
const CONTEXT_FPD: u64 = 1 << 1;

/// One VT-d unit.
///
/// Its root, queue and context tables are never freed: nothing proves the
/// unit stopped reading them.
pub struct VtdUnit<'f, R: Registers> {
    memory: TableMemory<'f>,
    clock: &'f dyn Clock,
    cap: Cap,
    ecap: Ecap,
    iotlb: usize,
    levels: u32,
    profile: UnitProfile,
    state: SpinLock<State<'f, R>>,
}

struct State<'f, R> {
    regs: R,
    root: Table,
    queue: CommandQueue,
    /// The context table of each bus a stream was ever attached on.
    contexts: [Option<Table>; BUSES],
    /// Each domain's tables.
    domains: HashMap<u16, IoPageTable<'f, SecondLevel>, BuildFastHash>,
    /// What each source id translates through: its domain, or the silent
    /// table.
    bindings: Bindings,
    /// The table silenced streams point at, always empty, under an id of its
    /// own.
    silent: Option<Silent<'f>>,
    ids: Ids,
    /// The fault record after the last one drained.
    next_fault: usize,
    /// The interrupt remapping table, once prepared.
    remap: Option<Remap>,
    /// A context change the unit did not confirm, so it may still cache an
    /// entry whose domain id the next context published must not trust.
    stale_contexts: bool,
}

/// An interrupt remapping table and the entries handed out of it.
struct Remap {
    table: Block,
    extended: bool,
    ids: Ids,
}

struct Silent<'f> {
    id: u16,
    table: IoPageTable<'f, SecondLevel>,
}

impl<'f, R: Registers> VtdUnit<'f, R> {
    /// Take over the unit behind `regs`, leaving it blocked: translation
    /// disabled, a root table with no context tables installed, queued
    /// invalidation running, its fault interrupt masked.
    /// [`IommuUnit::enable`] starts translating.
    ///
    /// `coherence` writes tables back for a unit whose walker does not
    /// snoop. Tables name frames up to the entry format's reach: every frame
    /// is RAM, which the platform's DMA width covers by definition.
    ///
    /// # Errors
    ///
    /// [`IommuError::OutOfRange`] for a unit this family cannot drive: one
    /// without queued invalidation, without an address width it supports,
    /// whose registers reach past `regs`, or whose walker does not snoop when
    /// no `coherence` was given. [`IommuError::Exhausted`] when its tables
    /// cannot be had, and the unit's own errors or timeouts.
    pub fn new(
        regs: R,
        frames: &'f dyn PageTableFrames,
        coherence: Option<&'f dyn TableCoherence>,
        clock: &'f dyn Clock,
    ) -> Result<Self, IommuError> {
        let cap = Cap(regs.read64(regs::CAP)?);
        let ecap = Ecap(regs.read64(regs::ECAP)?);
        let faults_end = cap.fault_record_offset() + cap.fault_records() * regs::FAULT_RECORD_LEN;
        let iotlb = ecap.iotlb_offset();
        if !ecap.queued_invalidation()
            || (!ecap.coherent() && coherence.is_none())
            || faults_end > regs.window_len()
            || iotlb + regs::IOTLB_REG + 8 > regs.window_len()
            || regs.read32(regs::VER)? == 0
        {
            return Err(IommuError::OutOfRange);
        }
        let levels = [(0b0100, 4), (0b0010, 3), (0b1000, 5)]
            .into_iter()
            .find(|&(bit, _)| cap.sagaw() & bit != 0)
            .map(|(_, levels)| levels)
            .ok_or(IommuError::OutOfRange)?;
        let memory = TableMemory::new(frames, if ecap.coherent() { None } else { coherence });
        let profile = UnitProfile {
            stage: tairix_kernel_iommu_api::Stage::Second,
            reach: Reach {
                input_bits: reach_bits(levels).min(cap.mgaw()),
                output_bits: ENTRY_ADDRESS_BITS,
            },
            reserved: &RESERVED,
        };
        // No unit holds these yet, so a failure gives them back.
        let ids = Ids::new(FIRST_DOMAIN, cap.domains());
        let root = memory.alloc()?;
        let queue = match CommandQueue::new(&memory) {
            Ok(queue) => queue,
            Err(err) => {
                memory.free(root);
                return Err(err);
            }
        };
        let unit = Self {
            memory,
            clock,
            cap,
            ecap,
            iotlb,
            levels,
            profile,
            state: SpinLock::new(State {
                regs,
                root,
                queue,
                contexts: [const { None }; BUSES],
                domains: HashMap::with_hasher(BuildFastHash::new()),
                bindings: Bindings::new(),
                silent: None,
                ids,
                next_fault: 0,
                remap: None,
                stale_contexts: false,
            }),
        };
        // From here the unit may hold the tables' addresses, so a failure
        // keeps them.
        unit.take_over()?;
        Ok(unit)
    }

    fn take_over(&self) -> Result<(), IommuError> {
        let state = self.state.lock();
        // Firmware may have left translation and the queue running on tables
        // of its own; both stop before anything of ours is installed.
        let gsts = state.regs.read32(regs::GSTS)?;
        if gsts & regs::GSTS_TES != 0 {
            self.command(&state.regs, 0, regs::GCMD_TE, regs::GSTS_TES, 0)?;
        }
        // Interrupt remapping firmware left on would deliver through a table
        // of its own; ours replaces it before any source is unmasked.
        if gsts & regs::GSTS_IRES != 0 {
            self.command(&state.regs, 0, regs::GCMD_IRE, regs::GSTS_IRES, 0)?;
        }
        if gsts & regs::GSTS_QIES != 0 {
            self.command(&state.regs, 0, regs::GCMD_QIE, regs::GSTS_QIES, 0)?;
        }
        state.regs.write32(regs::FECTL, regs::FECTL_IM)?;
        // An error firmware left set would be blamed on our first batch, and a
        // status already set raises no fault event for the next one.
        state.regs.write32(regs::FSTS, regs::FSTS_ERRORS)?;
        state.regs.write64(regs::RTADDR, state.root.phys())?;
        self.command(
            &state.regs,
            regs::GCMD_SRTP,
            0,
            regs::GSTS_RTPS,
            regs::GSTS_RTPS,
        )?;
        // Until the queue runs, the register interface flushes whatever the
        // unit cached from an earlier root.
        state
            .regs
            .write64(regs::CCMD, regs::CCMD_ICC | regs::CCMD_GLOBAL)?;
        self.wait_register64(&state.regs, regs::CCMD, regs::CCMD_ICC)?;
        let mut flush = regs::IOTLB_IVT | regs::IOTLB_GLOBAL;
        if self.cap.drain_reads() {
            flush |= regs::IOTLB_DRAIN_READS;
        }
        if self.cap.drain_writes() {
            flush |= regs::IOTLB_DRAIN_WRITES;
        }
        let iotlb = self.iotlb + regs::IOTLB_REG;
        state.regs.write64(iotlb, flush)?;
        self.wait_register64(&state.regs, iotlb, regs::IOTLB_IVT)?;
        state.regs.write64(regs::IQT, 0)?;
        state.regs.write64(regs::IQA, state.queue.ring())?;
        self.command(
            &state.regs,
            regs::GCMD_QIE,
            0,
            regs::GSTS_QIES,
            regs::GSTS_QIES,
        )
    }

    /// Issue a global command: set `set`, clear `clear`, keep every other
    /// persistent command, and wait for `mask` of the status to read `want`.
    fn command(
        &self,
        regs: &R,
        set: u32,
        clear: u32,
        mask: u32,
        want: u32,
    ) -> Result<(), IommuError> {
        let persistent = regs.read32(regs::GSTS)? & regs::GSTS_PERSISTENT;
        regs.write32(regs::GCMD, (persistent | set) & !clear)?;
        self.wait_register(regs, regs::GSTS, mask, want)
    }

    fn wait_register(
        &self,
        regs: &R,
        offset: usize,
        mask: u32,
        want: u32,
    ) -> Result<(), IommuError> {
        wait_for(self.clock, || Ok(regs.read32(offset)? & mask == want))
    }

    fn wait_register64(&self, regs: &R, offset: usize, busy: u64) -> Result<(), IommuError> {
        wait_for(self.clock, || Ok(regs.read64(offset)? & busy == 0))
    }

    /// Queue `descriptors` and an invalidation wait behind them, and return
    /// once the unit has written the wait's status.
    fn invalidate(
        &self,
        state: &mut State<'f, R>,
        descriptors: &[Descriptor],
    ) -> Result<(), IommuError> {
        let State { regs, queue, .. } = state;
        queue.run(
            &self.memory,
            self.clock,
            &Queue(regs),
            descriptors.iter().copied(),
            format::wait,
        )
    }

    fn domain_iotlb(&self, domain: u16) -> Descriptor {
        format::iotlb_domain(domain, self.cap.drain_reads(), self.cap.drain_writes())
    }

    /// Link a context table for `bus` from the root the first time a stream
    /// on the bus is attached.
    fn link_context_table(&self, state: &mut State<'f, R>, bus: u8) -> Result<(), IommuError> {
        if state.contexts[usize::from(bus)].is_some() {
            return Ok(());
        }
        let table = self.memory.alloc()?;
        let slot = 2 * usize::from(bus);
        let linked = self.memory.write(&state.root, slot + 1, 0).and_then(|()| {
            self.memory.write(
                &state.root,
                slot,
                (table.phys() & format::ADDRESS) | format::PRESENT,
            )
        });
        if let Err(err) = linked {
            self.memory.free(table);
            return Err(err);
        }
        self.memory.publish(&state.root, slot, 2);
        state.contexts[usize::from(bus)] = Some(table);
        Ok(())
    }

    /// Write stream `source`'s context entry: the high word first, so the
    /// unit never sees a present entry with a stale domain.
    fn write_context(
        &self,
        state: &mut State<'f, R>,
        source: u16,
        low: u64,
        high: u64,
    ) -> Result<(), IommuError> {
        let [bus, devfn] = source.to_be_bytes();
        self.link_context_table(state, bus)?;
        let table = state.contexts[usize::from(bus)]
            .as_ref()
            .ok_or(IommuError::Hardware)?;
        let slot = 2 * usize::from(devfn);
        self.memory.write(table, slot + 1, high)?;
        tairix_dma_barrier::dma_wmb();
        self.memory.write(table, slot, low)?;
        self.memory.publish(table, slot, 2);
        Ok(())
    }

    /// Clear stream `source`'s context entry, low word first.
    fn clear_context(&self, state: &State<'f, R>, source: u16) -> Result<(), IommuError> {
        let [bus, devfn] = source.to_be_bytes();
        let Some(table) = state.contexts[usize::from(bus)].as_ref() else {
            return Ok(());
        };
        let slot = 2 * usize::from(devfn);
        self.memory.write(table, slot, 0)?;
        tairix_dma_barrier::dma_wmb();
        self.memory.write(table, slot + 1, 0)?;
        self.memory.publish(table, slot, 2);
        Ok(())
    }

    /// Make a not-present-to-present change visible: a unit in caching mode
    /// caches not-present entries (under domain id 0 for a context entry);
    /// otherwise only a unit that buffers writes needs a flush.
    fn publish_new(
        &self,
        state: &mut State<'f, R>,
        source: Option<u16>,
        domain: u16,
    ) -> Result<(), IommuError> {
        if core::mem::take(&mut state.stale_contexts) {
            if let Err(err) = self.invalidate(state, &[format::context_global()]) {
                state.stale_contexts = true;
                return Err(err);
            }
        }
        if self.cap.caching_mode() {
            let iotlb = self.domain_iotlb(domain);
            return match source {
                Some(source) => self.invalidate(state, &[format::context_device(0, source), iotlb]),
                None => self.invalidate(state, &[iotlb]),
            };
        }
        if self.cap.required_write_buffer_flush() {
            self.command(&state.regs, regs::GCMD_WBF, 0, regs::GSTS_WBFS, 0)?;
        }
        Ok(())
    }

    /// Move pending fault records into `batch`, oldest first, clearing each,
    /// within one sweep of the records. Whether records may remain.
    ///
    /// The unit writes consecutive records, so the pending ones run on from
    /// the record after the last one drained. `FSTS.FRI` is latched only when
    /// a run begins, so a drain that stopped part-way cannot resume from it;
    /// a run firmware left elsewhere is found by the sweep.
    fn take_faults(&self, records: usize, batch: &mut FaultBatch) -> bool {
        let mut state = self.state.lock();
        let Ok(fsts) = state.regs.read32(regs::FSTS) else {
            return false;
        };
        // Nothing more is recorded while the overflow stands.
        if fsts & regs::FSTS_PFO != 0 {
            let _ = state.regs.write32(regs::FSTS, regs::FSTS_PFO);
        }
        if fsts & regs::FSTS_PPF == 0 {
            return false;
        }
        let base = self.cap.fault_record_offset();
        let mut in_run = false;
        let mut took = false;
        for _ in 0..records {
            let offset = base + state.next_fault * regs::FAULT_RECORD_LEN;
            let Ok(high) = state.regs.read64(offset + 8) else {
                return false;
            };
            if high & FAULT_F != 0 {
                let low = state.regs.read64(offset).unwrap_or(0);
                if batch.try_push(decode_fault(low, high)).is_err() {
                    return true;
                }
                let _ = state.regs.write32(offset + 12, regs::low32(FAULT_F >> 32));
                in_run = true;
                took = true;
            } else if core::mem::take(&mut in_run) && !faults_pending(&state.regs) {
                return false;
            }
            state.next_fault = (state.next_fault + 1) % records;
        }
        // A sweep that took nothing while PPF stands is a unit misreporting,
        // not records a further drain could reach.
        took && faults_pending(&state.regs)
    }

    /// The id `binding`'s context entry was written under.
    fn tag(state: &State<'f, R>, binding: Binding) -> Option<u16> {
        match binding {
            Binding::Domain(id) => u16::try_from(id).ok(),
            Binding::Silenced => state.silent.as_ref().map(|silent| silent.id),
        }
    }

    /// Detach `source` from whatever it translates through, confirmed. One the
    /// unit cannot confirm keeps its domain, so the domain's tables outlive
    /// any walk the unit still holds and a later detach can try again; its
    /// silence ends with its context, confirmed or not.
    fn detach(&self, state: &mut State<'f, R>, source: u16) -> Result<(), IommuError> {
        let stream = u32::from(source);
        let id = match state.bindings.get(stream) {
            Some(binding) => Self::tag(state, binding).ok_or(IommuError::Hardware)?,
            // Cleared already: only the confirmation is owed.
            None => match state.bindings.held(stream) {
                Some(held) => u16::try_from(held).map_err(|_| IommuError::Hardware)?,
                None => return Ok(()),
            },
        };
        let descriptors = [format::context_device(id, source), self.domain_iotlb(id)];
        if state.bindings.get(stream).is_some() {
            self.clear_context(state, source)
                .map_err(|_| IommuError::Unconfirmed)?;
            state.bindings.unbind(stream);
            state.bindings.end_silence(stream);
        }
        if self.invalidate(state, &descriptors).is_err() {
            state.stale_contexts = true;
            return Err(IommuError::Unconfirmed);
        }
        state.bindings.release(stream);
        Ok(())
    }

    /// The silent table and its id, made the first time a stream is silenced.
    fn silent_root(&self, state: &mut State<'f, R>) -> Result<(u16, u64), IommuError> {
        if let Some(silent) = &state.silent {
            return Ok((silent.id, silent.table.root()));
        }
        let table = IoPageTable::new(
            SecondLevel {
                large_2m: false,
                large_1g: false,
            },
            self.levels,
            self.memory,
            self.profile.reach,
        )?;
        let id = state.ids.take_sixteen_bits()?;
        let root = table.root();
        state.silent = Some(Silent { id, table });
        Ok((id, root))
    }
}

/// A VT-d unit's invalidation queue registers.
struct Queue<'r, R>(&'r R);

impl<R: Registers> QueueRegisters for Queue<'_, R> {
    fn head(&self) -> Result<usize, IommuError> {
        Ok(regs::queue_index(self.0.read64(regs::IQH)?))
    }

    fn set_tail(&self, tail: usize) -> Result<(), IommuError> {
        self.0.write64(regs::IQT, (tail as u64) << 4)
    }

    /// A rejected descriptor stops the queue at it: replace it with a bare
    /// fence so the queue runs on, and clear the error.
    fn stopped(&self, queue: &CommandQueue, memory: &TableMemory<'_>) -> Result<bool, IommuError> {
        let errors =
            self.0.read32(regs::FSTS)? & (regs::FSTS_IQE | regs::FSTS_ICE | regs::FSTS_ITE);
        if errors == 0 {
            return Ok(false);
        }
        if errors & regs::FSTS_IQE != 0 {
            let head = regs::queue_index(self.0.read64(regs::IQH)?);
            queue.replace(memory, head, format::fence())?;
            tairix_dma_barrier::dma_wmb();
        }
        self.0.write32(regs::FSTS, errors)?;
        Ok(true)
    }
}

/// PPF: whether any fault record holds a fault.
fn faults_pending(regs: &impl Registers) -> bool {
    regs.read32(regs::FSTS)
        .is_ok_and(|fsts| fsts & regs::FSTS_PPF != 0)
}

fn source_id(stream: u32) -> Result<u16, IommuError> {
    u16::try_from(stream).map_err(|_| IommuError::OutOfRange)
}

/// Decode one fault recording register. A permission fault and a missing
/// entry report the same reasons (5 for a write, 6 for a read), and every
/// DMA carve is mapped read-write, so both read as unmapped. Reason 0xD is a
/// translated request, or a translation request, that the context entry's
/// untranslated-only type blocks. Reasons 0x20 to 0x27 are interrupt
/// requests, whose fault information names the entry they asked for.
fn decode_fault(low: u64, high: u64) -> Fault {
    let code = regs::field(high, 32, 8);
    let reason = match code {
        0x1 | 0x2 => FaultReason::Blocked,
        0x4..=0x6 => FaultReason::Unmapped,
        0x3 | 0x7..=0xC => FaultReason::Malformed,
        0xD => FaultReason::Translated,
        0x20..=0x27 => FaultReason::Interrupt,
        other => FaultReason::Other(u16::try_from(other).unwrap_or(u16::MAX)),
    };
    let iova = if reason == FaultReason::Interrupt {
        low >> 48
    } else {
        low & !(IO_PAGE_SIZE - 1)
    };
    Fault {
        stream: regs::field(high, 0, 16),
        iova,
        write: high & (FAULT_T1 | FAULT_T2) == 0,
        reason,
    }
}

/// The most entries a VT-d remapping table holds: its index is 16 bits.
const MAX_REMAP_ENTRIES: u32 = 1 << 16;
/// Entries one table frame holds: 16 bytes each.
const REMAP_ENTRIES_PER_FRAME: u32 = 256;

const _: () = assert!(
    IO_PAGE_SIZE / 16 == 256,
    "a frame holds 256 16-byte entries"
);

impl<R: Registers> VtdUnit<'_, R> {
    /// Write entry `entry` of `remap`'s table: the high word first, so the
    /// unit never reads a present entry with a stale source check.
    fn write_irte(&self, remap: &Remap, entry: u16, irte: format::Irte) -> Result<(), IommuError> {
        let slot = 2 * usize::from(entry);
        self.memory.write_block(&remap.table, slot + 1, irte[1])?;
        tairix_dma_barrier::dma_wmb();
        self.memory.write_block(&remap.table, slot, irte[0])?;
        self.memory.publish_block(&remap.table, slot, 2);
        Ok(())
    }

    /// Clear entry `entry` of `remap`'s table, present bit first.
    fn clear_irte(&self, remap: &Remap, entry: u16) -> Result<(), IommuError> {
        let slot = 2 * usize::from(entry);
        self.memory.write_block(&remap.table, slot, 0)?;
        tairix_dma_barrier::dma_wmb();
        self.memory.write_block(&remap.table, slot + 1, 0)?;
        self.memory.publish_block(&remap.table, slot, 2);
        Ok(())
    }
}

impl<R: Registers> InterruptRemapping for VtdUnit<'_, R> {
    fn supports_extended(&self) -> bool {
        self.ecap.extended_interrupts()
    }

    fn prepare_remapping(&self, extended: bool, entries: u32) -> Result<(), IommuError> {
        if !self.ecap.interrupt_remapping() || (extended && !self.ecap.extended_interrupts()) {
            return Err(IommuError::OutOfRange);
        }
        let entries = entries
            .clamp(REMAP_ENTRIES_PER_FRAME, MAX_REMAP_ENTRIES)
            .next_power_of_two();
        let order = (entries / REMAP_ENTRIES_PER_FRAME).trailing_zeros();
        let ids = Ids::new(0, entries);
        let mut state = self.state.lock();
        if state.remap.is_some() {
            return Err(IommuError::OutOfRange);
        }
        let table = self.memory.alloc_block(order)?;
        // The size field names 2^(S + 1) entries.
        let size = u64::from(entries.trailing_zeros() - 1);
        let mode = if extended { regs::IRTA_EIME } else { 0 };
        state
            .regs
            .write64(regs::IRTA, (table.phys() & format::ADDRESS) | mode | size)?;
        // The table's frames stay the unit's from here whatever follows: it
        // may already hold their address.
        let remap = Remap {
            table,
            extended,
            ids,
        };
        self.command(
            &state.regs,
            regs::GCMD_SIRTP,
            0,
            regs::GSTS_IRTPS,
            regs::GSTS_IRTPS,
        )?;
        state.remap = Some(remap);
        self.invalidate(&mut state, &[format::iec_global()])
    }

    fn remap_interrupt(
        &self,
        source: InterruptSource,
        target: InterruptTarget,
    ) -> Result<Remapped, IommuError> {
        let mut state = self.state.lock();
        let remap = state.remap.as_mut().ok_or(IommuError::OutOfRange)?;
        let irte = format::irte(source, target, remap.extended).ok_or(IommuError::OutOfRange)?;
        let entry = remap
            .ids
            .take()
            .and_then(|entry| u16::try_from(entry).ok())
            .ok_or(IommuError::Exhausted)?;
        let written = state
            .remap
            .as_ref()
            .ok_or(IommuError::Hardware)
            .and_then(|remap| self.write_irte(remap, entry, irte));
        let confirmed =
            written.and_then(|()| self.invalidate(&mut state, &[format::iec_entry(entry)]));
        if confirmed.is_err() {
            // The unit may hold the entry already, so it is taken back and
            // never handed out again.
            if let Some(remap) = state.remap.as_ref() {
                let _ = self.clear_irte(remap, entry);
            }
            let _ = self.invalidate(&mut state, &[format::iec_entry(entry)]);
            if let Some(remap) = state.remap.as_mut() {
                remap.ids.release(u32::from(entry), false);
            }
            return Err(IommuError::Unconfirmed);
        }
        Ok(Remapped {
            entry: u32::from(entry),
            address: format::remappable_msi_address(entry),
            data: 0,
            redirection: format::remappable_redirection(entry, target.vector, target.level),
        })
    }

    fn release_interrupt(&self, entry: u32) -> Result<(), IommuError> {
        let entry = u16::try_from(entry).map_err(|_| IommuError::NotMapped)?;
        let mut state = self.state.lock();
        let remap = state.remap.as_ref().ok_or(IommuError::NotMapped)?;
        if !remap.ids.is_live(u32::from(entry)) {
            return Err(IommuError::NotMapped);
        }
        let cleared = self.clear_irte(remap, entry);
        let confirmed = cleared
            .and_then(|()| self.invalidate(&mut state, &[format::iec_entry(entry)]))
            .is_ok();
        if let Some(remap) = state.remap.as_mut() {
            remap.ids.release(u32::from(entry), confirmed);
        }
        if confirmed {
            Ok(())
        } else {
            Err(IommuError::Unconfirmed)
        }
    }

    /// Compatibility-format interrupts are blocked from here on: every
    /// interrupt a device raises names an entry, or is refused.
    fn enable_remapping(&self) -> Result<(), IommuError> {
        let state = self.state.lock();
        if state.remap.is_none() {
            return Err(IommuError::OutOfRange);
        }
        // One command per write. Compatibility-format interrupts are refused
        // first, which changes nothing until remapping is on, so none slips
        // past it once it is.
        self.command(&state.regs, 0, regs::GCMD_CFI, regs::GSTS_CFIS, 0)?;
        self.command(
            &state.regs,
            regs::GCMD_IRE,
            0,
            regs::GSTS_IRES,
            regs::GSTS_IRES,
        )
    }

    fn disable_remapping(&self) -> Result<(), IommuError> {
        let state = self.state.lock();
        if state.remap.is_none() {
            return Err(IommuError::OutOfRange);
        }
        self.command(&state.regs, 0, regs::GCMD_IRE, regs::GSTS_IRES, 0)
    }
}

impl<R: Registers> IommuUnit for VtdUnit<'_, R> {
    fn profile(&self) -> UnitProfile {
        self.profile
    }

    fn interrupt_remapping(&self) -> Option<&dyn InterruptRemapping> {
        self.ecap
            .interrupt_remapping()
            .then_some(self as &dyn InterruptRemapping)
    }

    /// Enable translation, then retire firmware's protected memory regions,
    /// which translation now supersedes.
    fn enable(&self) -> Result<(), IommuError> {
        let state = self.state.lock();
        self.command(
            &state.regs,
            regs::GCMD_TE,
            0,
            regs::GSTS_TES,
            regs::GSTS_TES,
        )?;
        if self.cap.protected_low_memory() || self.cap.protected_high_memory() {
            let pmen = state.regs.read32(regs::PMEN)?;
            if pmen & regs::PMEN_EPM != 0 {
                state.regs.write32(regs::PMEN, pmen & !regs::PMEN_EPM)?;
                self.wait_register(&state.regs, regs::PMEN, regs::PMEN_PRS, 0)?;
            }
        }
        Ok(())
    }

    fn create_domain(&self) -> Result<DomainId, IommuError> {
        let table = IoPageTable::new(
            SecondLevel {
                large_2m: self.cap.large_page_2m(),
                large_1g: self.cap.large_page_1g(),
            },
            self.levels,
            self.memory,
            self.profile.reach,
        )?;
        let mut state = self.state.lock();
        state
            .domains
            .try_reserve(1)
            .map_err(|_| IommuError::Exhausted)?;
        let id = state.ids.take_sixteen_bits()?;
        let _ = state.domains.try_insert(id, table);
        Ok(DomainId(u32::from(id)))
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
        let flush = [self.domain_iotlb(id)];
        self.invalidate(&mut state, &flush)
            .map_err(|_| IommuError::Unconfirmed)?;
        state.domains.remove(&id);
        state.ids.release(u32::from(id), true);
        Ok(())
    }

    fn attach(&self, stream: u32, domain: DomainId) -> Result<(), IommuError> {
        let source = source_id(stream)?;
        let id = domain.sixteen_bits()?;
        let mut state = self.state.lock();
        let root = state.domains.get(&id).ok_or(IommuError::OutOfRange)?.root();
        let Some(reserved) = state.bindings.prepare_attach(stream, u32::from(id))? else {
            return Ok(());
        };
        // A silenced stream may take an owner: it is blocked either way.
        self.detach(&mut state, source)?;
        self.write_context(
            &mut state,
            source,
            format::context_low(root),
            format::context_high(id, self.levels),
        )?;
        state.bindings.hold(reserved, stream, u32::from(id));
        if let Err(err) = self.publish_new(&mut state, Some(source), id) {
            // The unit may already walk the entry, so it is taken back and
            // stays counted until that is confirmed.
            self.detach(&mut state, source)?;
            return Err(err);
        }
        Ok(())
    }

    fn block(&self, stream: u32) -> Result<(), IommuError> {
        let source = source_id(stream)?;
        let mut state = self.state.lock();
        if state.bindings.get(stream) == Some(Binding::Silenced) {
            return Ok(());
        }
        self.detach(&mut state, source)
    }

    fn silence(&self, stream: u32) -> Result<(), IommuError> {
        let source = source_id(stream)?;
        let mut state = self.state.lock();
        if state.bindings.get(stream) == Some(Binding::Silenced) {
            return Ok(());
        }
        let reserved = state.bindings.reserve()?;
        let (id, root) = self.silent_root(&mut state)?;
        self.detach(&mut state, source)?;
        self.write_context(
            &mut state,
            source,
            format::context_low(root) | CONTEXT_FPD,
            format::context_high(id, self.levels),
        )?;
        state.bindings.silence(reserved, stream);
        self.publish_new(&mut state, Some(source), id)
    }

    fn map(
        &self,
        domain: DomainId,
        iova: u64,
        phys: u64,
        len: u64,
        access: Access,
    ) -> Result<(), IommuError> {
        let id = domain.sixteen_bits()?;
        let mut state = self.state.lock();
        state
            .domains
            .get_mut(&id)
            .ok_or(IommuError::OutOfRange)?
            .map(iova, phys, len, access)?;
        if let Err(err) = self.publish_new(&mut state, None, id) {
            // The caller frees the frames once this fails, so no leaf may stay
            // behind to reach them.
            let taken_back = state
                .domains
                .get_mut(&id)
                .is_some_and(|table| table.unmap(iova, len).is_ok());
            return Err(if taken_back {
                err
            } else {
                IommuError::Unconfirmed
            });
        }
        Ok(())
    }

    fn unmap(&self, domain: DomainId, iova: u64, len: u64) -> Result<(), IommuError> {
        let id = domain.sixteen_bits()?;
        self.state
            .lock()
            .domains
            .get_mut(&id)
            .ok_or(IommuError::OutOfRange)?
            .unmap(iova, len)
    }

    fn sync(&self, domain: DomainId) -> Result<(), IommuError> {
        let id = domain.sixteen_bits()?;
        let mut state = self.state.lock();
        if !state.domains.contains_key(&id) {
            return Err(IommuError::OutOfRange);
        }
        let flush = [self.domain_iotlb(id)];
        self.invalidate(&mut state, &flush)?;
        if let Some(table) = state.domains.get_mut(&id) {
            table.release_retired();
        }
        Ok(())
    }

    /// Unmasking delivers a fault event the mask held pending. A VT-d unit
    /// raises its faults only as a message.
    fn route_faults(&self, route: FaultRoute) -> Result<(), IommuError> {
        let FaultRoute::Message { address, data } = route else {
            return Err(IommuError::OutOfRange);
        };
        let state = self.state.lock();
        state.regs.write32(regs::FEDATA, data)?;
        state.regs.write32(regs::FEADDR, regs::low32(address))?;
        state
            .regs
            .write32(regs::FEUADDR, regs::low32(address >> 32))?;
        state.regs.write32(regs::FECTL, 0)
    }

    /// At most one ring's worth of records per call. The fault event is raised
    /// only when PPF sets, so a call that stops with records left says so.
    /// Each batch reaches `sink` with the unit unlocked, since what it does
    /// about a fault may be to call back in.
    fn drain_faults(&self, sink: &mut dyn FnMut(Fault)) -> bool {
        let records = self.cap.fault_records();
        drain_in_batches(records, |batch| self.take_faults(records, batch), sink)
    }
}
