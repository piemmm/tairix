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
use tairix_kernel_iommu_api::{
    drain_in_batches, reach_bits, wait_for, Access, Binding, Bindings, Block, Clock, CommandQueue,
    DomainId, DomainMap, Fault, FaultBatch, FaultReason, FaultRoute, FrameRun, Ids,
    InterruptRemapping, InterruptSource, InterruptTarget, Invalidator, IoPageTable, IommuError,
    IommuUnit, PageSpan, QueueRegisters, Reach, Remapped, Table, TableCoherence, TableMemory,
    UnitProfile, FAULT_QUEUE_RECORDS, IO_PAGE_SIZE, MESSAGE_WINDOW,
};

const _: () = assert!(
    regs::MOST_FAULT_RECORDS <= FAULT_QUEUE_RECORDS as usize,
    "a unit holds no more fault records than the containment bound allows"
);
use tairix_sync::{SpinLock, SpinLockGuard};

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
/// unit stopped reading them. Its state is split so a domain's maps and
/// syncs wait on neither another domain nor a stream's attach: the
/// lifecycle lock, then the domain map and a domain, then the queue's ring,
/// then the register window, never the reverse.
pub struct VtdUnit<'f, R: Registers> {
    memory: TableMemory<'f>,
    clock: &'f dyn Clock,
    cap: Cap,
    ecap: Ecap,
    iotlb: usize,
    levels: u32,
    profile: UnitProfile,
    regs: Regs<R>,
    queue: CommandQueue,
    lifecycle: SpinLock<Lifecycle<'f>>,
    domains: DomainMap<IoPageTable<'f, SecondLevel>>,
    /// The fault record after the last one drained.
    next_fault: SpinLock<usize>,
}

/// The register window, held for one access or one handshake, so a
/// read-modify-write is never interleaved with another.
struct Regs<R>(SpinLock<R>);

impl<R> Regs<R> {
    fn lock(&self) -> SpinLockGuard<'_, R> {
        self.0.lock()
    }
}

/// What attaching a stream, silencing one and remapping interrupts change.
struct Lifecycle<'f> {
    root: Table,
    /// The context table of each bus a stream was ever attached on.
    contexts: [Option<Table>; BUSES],
    /// What each source id translates through: its domain, or the silent
    /// table.
    bindings: Bindings,
    /// The table silenced streams point at, always empty, under an id of its
    /// own.
    silent: Option<Silent<'f>>,
    ids: Ids,
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
    /// Take over the unit behind `regs`: translation off — so a device's DMA
    /// passes untranslated until [`IommuUnit::enable`] starts translating
    /// (`plans/OPEN-DEFECTS.md`) — over a root table with no context tables
    /// installed, queued invalidation running, its fault interrupt masked.
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
            tables: tairix_kernel_iommu_api::Tables::Walked(tairix_kernel_iommu_api::Stage::Second),
            reach: Reach {
                input_bits: reach_bits(levels).min(cap.mgaw()),
                output_bits: ENTRY_ADDRESS_BITS,
            },
            reserved: &RESERVED,
            write_only: true,
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
            regs: Regs(SpinLock::new(regs)),
            queue,
            lifecycle: SpinLock::new(Lifecycle {
                root,
                contexts: [const { None }; BUSES],
                bindings: Bindings::new(),
                silent: None,
                ids,
                remap: None,
                stale_contexts: false,
            }),
            domains: DomainMap::new(),
            next_fault: SpinLock::new(0),
        };
        // From here the unit may hold the tables' addresses, so a failure
        // keeps them.
        unit.take_over()?;
        Ok(unit)
    }

    fn take_over(&self) -> Result<(), IommuError> {
        let root = self.lifecycle.lock().root.phys();
        let regs = self.regs.lock();
        // Firmware may have left translation and the queue running on tables
        // of its own; both stop before anything of ours is installed.
        let gsts = regs.read32(regs::GSTS)?;
        if gsts & regs::GSTS_TES != 0 {
            self.command(&regs, 0, regs::GCMD_TE, regs::GSTS_TES, 0)?;
        }
        // Interrupt remapping firmware left on would deliver through a table
        // of its own; ours replaces it before any source is unmasked.
        if gsts & regs::GSTS_IRES != 0 {
            self.command(&regs, 0, regs::GCMD_IRE, regs::GSTS_IRES, 0)?;
        }
        if gsts & regs::GSTS_QIES != 0 {
            self.command(&regs, 0, regs::GCMD_QIE, regs::GSTS_QIES, 0)?;
        }
        regs.write32(regs::FECTL, regs::FECTL_IM)?;
        // An error firmware left set would be blamed on our first batch, and a
        // status already set raises no fault event for the next one.
        regs.write32(regs::FSTS, regs::FSTS_ERRORS)?;
        regs.write64(regs::RTADDR, root)?;
        self.command(&regs, regs::GCMD_SRTP, 0, regs::GSTS_RTPS, regs::GSTS_RTPS)?;
        // Until the queue runs, the register interface flushes whatever the
        // unit cached from an earlier root.
        regs.write64(regs::CCMD, regs::CCMD_ICC | regs::CCMD_GLOBAL)?;
        self.wait_register64(&regs, regs::CCMD, regs::CCMD_ICC)?;
        let mut flush = regs::IOTLB_IVT | regs::IOTLB_GLOBAL;
        if self.cap.drain_reads() {
            flush |= regs::IOTLB_DRAIN_READS;
        }
        if self.cap.drain_writes() {
            flush |= regs::IOTLB_DRAIN_WRITES;
        }
        let iotlb = self.iotlb + regs::IOTLB_REG;
        regs.write64(iotlb, flush)?;
        self.wait_register64(&regs, iotlb, regs::IOTLB_IVT)?;
        regs.write64(regs::IQT, 0)?;
        regs.write64(regs::IQA, self.queue.ring())?;
        self.command(&regs, regs::GCMD_QIE, 0, regs::GSTS_QIES, regs::GSTS_QIES)
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

    fn invalidator(&self) -> Invalidator<'_> {
        Invalidator {
            queue: &self.queue,
            memory: self.memory,
            clock: self.clock,
            regs: &self.regs,
            completion: format::wait,
        }
    }

    /// Queue `descriptors` and an invalidation wait behind them, and return
    /// once the unit has written the wait's status.
    fn invalidate(&self, descriptors: &[Descriptor]) -> Result<(), IommuError> {
        self.invalidator().run(descriptors.iter().copied())
    }

    fn domain_iotlb(&self, domain: u16) -> Descriptor {
        format::iotlb_domain(domain, self.cap.drain_reads(), self.cap.drain_writes())
    }

    /// The invalidation reaching `[iova, iova + len)` of `domain` and the walk
    /// caches above it: page-selective where the unit can name the span, else
    /// the whole domain. Whether it is page-selective.
    fn range_iotlb(
        &self,
        domain: u16,
        iova: u64,
        len: u64,
    ) -> Result<(Descriptor, bool), IommuError> {
        let (base, order) = PageSpan::of(iova, len)?.covering_block();
        if self.cap.page_selective() && order <= self.cap.max_address_mask() {
            let drain = (self.cap.drain_reads(), self.cap.drain_writes());
            Ok((
                format::iotlb_pages(domain, base, order, drain.0, drain.1),
                true,
            ))
        } else {
            Ok((self.domain_iotlb(domain), false))
        }
    }

    /// Link a context table for `bus` from the root the first time a stream
    /// on the bus is attached.
    fn link_context_table(&self, life: &mut Lifecycle<'f>, bus: u8) -> Result<(), IommuError> {
        if life.contexts[usize::from(bus)].is_some() {
            return Ok(());
        }
        let table = self.memory.alloc()?;
        let slot = 2 * usize::from(bus);
        let linked = self.memory.write(&life.root, slot + 1, 0).and_then(|()| {
            self.memory.write(
                &life.root,
                slot,
                (table.phys() & format::ADDRESS) | format::PRESENT,
            )
        });
        if let Err(err) = linked {
            self.memory.free(table);
            return Err(err);
        }
        self.memory.publish(&life.root, slot, 2);
        life.contexts[usize::from(bus)] = Some(table);
        Ok(())
    }

    /// Write stream `source`'s context entry: the high word first, so the
    /// unit never sees a present entry with a stale domain.
    fn write_context(
        &self,
        life: &mut Lifecycle<'f>,
        source: u16,
        low: u64,
        high: u64,
    ) -> Result<(), IommuError> {
        let [bus, devfn] = source.to_be_bytes();
        self.link_context_table(life, bus)?;
        let table = life.contexts[usize::from(bus)]
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
    fn clear_context(&self, life: &Lifecycle<'f>, source: u16) -> Result<(), IommuError> {
        let [bus, devfn] = source.to_be_bytes();
        let Some(table) = life.contexts[usize::from(bus)].as_ref() else {
            return Ok(());
        };
        let slot = 2 * usize::from(devfn);
        self.memory.write(table, slot, 0)?;
        tairix_dma_barrier::dma_wmb();
        self.memory.write(table, slot + 1, 0)?;
        self.memory.publish(table, slot, 2);
        Ok(())
    }

    /// Install into `domain`'s tables what `install` maps from `iova`, and
    /// publish it once, flushing only what it installed; a refusal takes it
    /// back, since the caller frees the frames once this fails.
    fn publish_mapped(
        &self,
        domain: DomainId,
        iova: u64,
        install: impl FnOnce(&mut IoPageTable<'f, SecondLevel>) -> Result<u64, IommuError>,
    ) -> Result<(), IommuError> {
        let id = domain.sixteen_bits()?;
        self.domains.map_published(
            u32::from(id),
            iova,
            |table| table,
            install,
            |mapped| {
                self.range_iotlb(id, iova, mapped)
                    .and_then(|(iotlb, _)| self.publish_mapping(iotlb))
            },
        )
    }

    /// Make a new mapping visible: a unit in caching mode caches not-present
    /// entries, so `iotlb` flushes them; otherwise only a unit that buffers
    /// writes needs a flush.
    fn publish_mapping(&self, iotlb: Descriptor) -> Result<(), IommuError> {
        if self.cap.caching_mode() {
            return self.invalidate(&[iotlb]);
        }
        self.flush_write_buffer()
    }

    /// Make `source`'s new context entry visible, as [`Self::publish_mapping`]
    /// does a mapping (a caching unit caches a not-present context entry
    /// under domain id 0), every context entry flushed first where an earlier
    /// change went unconfirmed.
    fn publish_context(
        &self,
        life: &mut Lifecycle<'f>,
        source: u16,
        iotlb: Descriptor,
    ) -> Result<(), IommuError> {
        if core::mem::take(&mut life.stale_contexts) {
            if let Err(err) = self.invalidate(&[format::context_global()]) {
                life.stale_contexts = true;
                return Err(err);
            }
        }
        if self.cap.caching_mode() {
            return self.invalidate(&[format::context_device(0, source), iotlb]);
        }
        self.flush_write_buffer()
    }

    fn flush_write_buffer(&self) -> Result<(), IommuError> {
        if self.cap.required_write_buffer_flush() {
            let regs = self.regs.lock();
            self.command(&regs, regs::GCMD_WBF, 0, regs::GSTS_WBFS, 0)?;
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
        let mut next_fault = self.next_fault.lock();
        let Ok(fsts) = self.regs.lock().read32(regs::FSTS) else {
            return false;
        };
        // Nothing more is recorded while the overflow stands.
        if fsts & regs::FSTS_PFO != 0 {
            let _ = self.regs.lock().write32(regs::FSTS, regs::FSTS_PFO);
        }
        if fsts & regs::FSTS_PPF == 0 {
            return false;
        }
        let base = self.cap.fault_record_offset();
        let mut in_run = false;
        let mut took = false;
        for _ in 0..records {
            // A record at a time, so a long sweep never holds up the queue.
            let regs = self.regs.lock();
            let offset = base + *next_fault * regs::FAULT_RECORD_LEN;
            let Ok(high) = regs.read64(offset + 8) else {
                return false;
            };
            if high & FAULT_F != 0 {
                let low = regs.read64(offset).unwrap_or(0);
                if batch.try_push(decode_fault(low, high)).is_err() {
                    return true;
                }
                let _ = regs.write32(offset + 12, regs::low32(FAULT_F >> 32));
                in_run = true;
                took = true;
            } else if core::mem::take(&mut in_run) && !faults_pending(&*regs) {
                return false;
            }
            *next_fault = (*next_fault + 1) % records;
        }
        // A sweep that took nothing while PPF stands is a unit misreporting,
        // not records a further drain could reach.
        took && faults_pending(&*self.regs.lock())
    }

    /// The id `binding`'s context entry was written under.
    fn tag(life: &Lifecycle<'f>, binding: Binding) -> Option<u16> {
        match binding {
            Binding::Domain(id) => u16::try_from(id).ok(),
            Binding::Silenced => life.silent.as_ref().map(|silent| silent.id),
        }
    }

    /// Detach `source` from whatever it translates through, confirmed. One the
    /// unit cannot confirm keeps its domain, so the domain's tables outlive
    /// any walk the unit still holds and a later detach can try again; its
    /// silence ends with its context, confirmed or not.
    fn detach(&self, life: &mut Lifecycle<'f>, source: u16) -> Result<(), IommuError> {
        let stream = u32::from(source);
        let id = match life.bindings.get(stream) {
            Some(binding) => Self::tag(life, binding).ok_or(IommuError::Hardware)?,
            // Cleared already: only the confirmation is owed.
            None => match life.bindings.held(stream) {
                Some(held) => u16::try_from(held).map_err(|_| IommuError::Hardware)?,
                None => return Ok(()),
            },
        };
        let descriptors = [format::context_device(id, source), self.domain_iotlb(id)];
        if life.bindings.get(stream).is_some() {
            self.clear_context(life, source)
                .map_err(|_| IommuError::Unconfirmed)?;
            life.bindings.unbind(stream);
            life.bindings.end_silence(stream);
        }
        if self.invalidate(&descriptors).is_err() {
            life.stale_contexts = true;
            return Err(IommuError::Unconfirmed);
        }
        life.bindings.release(stream);
        Ok(())
    }

    /// The silent table and its id, made the first time a stream is silenced.
    fn silent_root(&self, life: &mut Lifecycle<'f>) -> Result<(u16, u64), IommuError> {
        if let Some(silent) = &life.silent {
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
        let id = life.ids.take_sixteen_bits()?;
        let root = table.root();
        life.silent = Some(Silent { id, table });
        Ok((id, root))
    }
}

/// A VT-d unit's invalidation queue registers.
impl<R: Registers> QueueRegisters for Regs<R> {
    fn head(&self) -> Result<usize, IommuError> {
        Ok(regs::queue_index(self.lock().read64(regs::IQH)?))
    }

    fn set_tail(&self, tail: usize) -> Result<(), IommuError> {
        self.lock().write64(regs::IQT, (tail as u64) << 4)
    }

    /// Where a descriptor was rejected, or a completion or device-TLB
    /// invalidation failed, the queue stops at its head.
    fn stopped_at(&self) -> Result<Option<usize>, IommuError> {
        let regs = self.lock();
        if regs.read32(regs::FSTS)? & QUEUE_ERRORS == 0 {
            return Ok(None);
        }
        Ok(Some(regs::queue_index(regs.read64(regs::IQH)?)))
    }

    /// A rejected descriptor is replaced with a bare fence so the queue runs
    /// on, and the error cleared.
    fn resume(
        &self,
        queue: &CommandQueue,
        memory: &TableMemory<'_>,
        slot: usize,
    ) -> Result<(), IommuError> {
        let regs = self.lock();
        let errors = regs.read32(regs::FSTS)? & QUEUE_ERRORS;
        if errors & regs::FSTS_IQE != 0 {
            queue.replace(memory, slot, format::fence())?;
            tairix_dma_barrier::dma_wmb();
        }
        regs.write32(regs::FSTS, errors)
    }
}

/// The errors that stop the invalidation queue.
const QUEUE_ERRORS: u32 = regs::FSTS_IQE | regs::FSTS_ICE | regs::FSTS_ITE;

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
        let mut life = self.lifecycle.lock();
        if life.remap.is_some() {
            return Err(IommuError::OutOfRange);
        }
        let table = self.memory.alloc_block(order)?;
        // The size field names 2^(S + 1) entries.
        let size = u64::from(entries.trailing_zeros() - 1);
        let mode = if extended { regs::IRTA_EIME } else { 0 };
        {
            let regs = self.regs.lock();
            regs.write64(regs::IRTA, (table.phys() & format::ADDRESS) | mode | size)?;
            // The table's frames stay the unit's from here whatever follows:
            // it may already hold their address.
            self.command(
                &regs,
                regs::GCMD_SIRTP,
                0,
                regs::GSTS_IRTPS,
                regs::GSTS_IRTPS,
            )?;
        }
        life.remap = Some(Remap {
            table,
            extended,
            ids,
        });
        self.invalidate(&[format::iec_global()])
    }

    fn remap_interrupt(
        &self,
        source: InterruptSource,
        target: InterruptTarget,
    ) -> Result<Remapped, IommuError> {
        let mut life = self.lifecycle.lock();
        let remap = life.remap.as_mut().ok_or(IommuError::OutOfRange)?;
        let irte = format::irte(source, target, remap.extended).ok_or(IommuError::OutOfRange)?;
        let entry = remap
            .ids
            .take()
            .and_then(|entry| u16::try_from(entry).ok())
            .ok_or(IommuError::Exhausted)?;
        let confirmed = self
            .write_irte(remap, entry, irte)
            .and_then(|()| self.invalidate(&[format::iec_entry(entry)]));
        if confirmed.is_err() {
            // The unit may hold the entry already, so it is taken back and
            // never handed out again.
            let _ = self.clear_irte(remap, entry);
            let _ = self.invalidate(&[format::iec_entry(entry)]);
            remap.ids.release(u32::from(entry), false);
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
        let mut life = self.lifecycle.lock();
        let remap = life.remap.as_mut().ok_or(IommuError::NotMapped)?;
        if !remap.ids.is_live(u32::from(entry)) {
            return Err(IommuError::NotMapped);
        }
        let confirmed = self
            .clear_irte(remap, entry)
            .and_then(|()| self.invalidate(&[format::iec_entry(entry)]))
            .is_ok();
        remap.ids.release(u32::from(entry), confirmed);
        if confirmed {
            Ok(())
        } else {
            Err(IommuError::Unconfirmed)
        }
    }

    /// Compatibility-format interrupts are blocked from here on: every
    /// interrupt a device raises names an entry, or is refused.
    fn enable_remapping(&self) -> Result<(), IommuError> {
        let life = self.lifecycle.lock();
        if life.remap.is_none() {
            return Err(IommuError::OutOfRange);
        }
        let regs = self.regs.lock();
        // One command per write. Compatibility-format interrupts are refused
        // first, which changes nothing until remapping is on, so none slips
        // past it once it is.
        self.command(&regs, 0, regs::GCMD_CFI, regs::GSTS_CFIS, 0)?;
        self.command(&regs, regs::GCMD_IRE, 0, regs::GSTS_IRES, regs::GSTS_IRES)
    }

    fn disable_remapping(&self) -> Result<(), IommuError> {
        let life = self.lifecycle.lock();
        if life.remap.is_none() {
            return Err(IommuError::OutOfRange);
        }
        self.command(&self.regs.lock(), 0, regs::GCMD_IRE, regs::GSTS_IRES, 0)
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
        let regs = self.regs.lock();
        self.command(&regs, regs::GCMD_TE, 0, regs::GSTS_TES, regs::GSTS_TES)?;
        if self.cap.protected_low_memory() || self.cap.protected_high_memory() {
            let pmen = regs.read32(regs::PMEN)?;
            if pmen & regs::PMEN_EPM != 0 {
                regs.write32(regs::PMEN, pmen & !regs::PMEN_EPM)?;
                self.wait_register(&regs, regs::PMEN, regs::PMEN_PRS, 0)?;
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
        let mut life = self.lifecycle.lock();
        let id = life.ids.take_sixteen_bits()?;
        if let Err((err, _table)) = self.domains.insert(u32::from(id), table) {
            life.ids.release(u32::from(id), true);
            return Err(err);
        }
        Ok(DomainId(u32::from(id)))
    }

    fn destroy_domain(&self, domain: DomainId) -> Result<(), IommuError> {
        let id = domain.sixteen_bits()?;
        let mut life = self.lifecycle.lock();
        if !self.domains.contains(u32::from(id)) {
            return Err(IommuError::OutOfRange);
        }
        if life.bindings.holders(u32::from(id)) != 0 {
            return Err(IommuError::DomainBusy);
        }
        // Nothing the unit cached for the id may outlive its tables, or
        // survive into the id's next owner.
        self.invalidate(&[self.domain_iotlb(id)])
            .map_err(|_| IommuError::Unconfirmed)?;
        self.domains.remove(u32::from(id));
        life.ids.release(u32::from(id), true);
        Ok(())
    }

    fn attach(&self, stream: u32, domain: DomainId) -> Result<(), IommuError> {
        let source = source_id(stream)?;
        let id = domain.sixteen_bits()?;
        let mut life = self.lifecycle.lock();
        let root = self.domains.with(u32::from(id), |table| Ok(table.root()))?;
        let Some(reserved) = life.bindings.prepare_attach(stream, u32::from(id))? else {
            return Ok(());
        };
        // A silenced stream may take an owner: it is blocked either way.
        self.detach(&mut life, source)?;
        self.write_context(
            &mut life,
            source,
            format::context_low(root),
            format::context_high(id, self.levels),
        )?;
        life.bindings.hold(reserved, stream, u32::from(id));
        let iotlb = self.domain_iotlb(id);
        if let Err(err) = self.publish_context(&mut life, source, iotlb) {
            // The unit may already walk the entry, so it is taken back and
            // stays counted until that is confirmed.
            self.detach(&mut life, source)?;
            return Err(err);
        }
        Ok(())
    }

    fn block(&self, stream: u32) -> Result<(), IommuError> {
        let source = source_id(stream)?;
        let mut life = self.lifecycle.lock();
        if life.bindings.get(stream) == Some(Binding::Silenced) {
            return Ok(());
        }
        self.detach(&mut life, source)
    }

    fn silence(&self, stream: u32) -> Result<(), IommuError> {
        let source = source_id(stream)?;
        let mut life = self.lifecycle.lock();
        if life.bindings.get(stream) == Some(Binding::Silenced) {
            return Ok(());
        }
        let reserved = life.bindings.reserve()?;
        let (id, root) = self.silent_root(&mut life)?;
        self.detach(&mut life, source)?;
        self.write_context(
            &mut life,
            source,
            format::context_low(root) | CONTEXT_FPD,
            format::context_high(id, self.levels),
        )?;
        life.bindings.silence(reserved, stream);
        let iotlb = self.domain_iotlb(id);
        self.publish_context(&mut life, source, iotlb)
    }

    fn map(
        &self,
        domain: DomainId,
        iova: u64,
        phys: u64,
        len: u64,
        access: Access,
    ) -> Result<(), IommuError> {
        self.publish_mapped(domain, iova, |table| {
            table.map(iova, phys, len, access).map(|()| len)
        })
    }

    /// Every run installed, then published once.
    fn map_runs(
        &self,
        domain: DomainId,
        iova: u64,
        runs: &[FrameRun],
        access: Access,
    ) -> Result<(), IommuError> {
        self.publish_mapped(domain, iova, |table| table.map_runs(iova, runs, access))
    }

    fn unmap(&self, domain: DomainId, iova: u64, len: u64) -> Result<(), IommuError> {
        let id = domain.sixteen_bits()?;
        self.domains
            .with(u32::from(id), |table| table.unmap(iova, len))
    }

    fn sync(&self, domain: DomainId) -> Result<(), IommuError> {
        let id = domain.sixteen_bits()?;
        let flush = [self.domain_iotlb(id)];
        self.domains.confirm(
            u32::from(id),
            |table| table,
            &self.invalidator(),
            |_| Ok((flush, None)),
        )
    }

    /// Page-selective where the unit has it and one mask covers the range,
    /// paging-structure entries included; the domain otherwise.
    fn sync_range(&self, domain: DomainId, iova: u64, len: u64) -> Result<(), IommuError> {
        let id = domain.sixteen_bits()?;
        let (flush, by_pages) = self.range_iotlb(id, iova, len)?;
        self.domains.confirm(
            u32::from(id),
            |table| table,
            &self.invalidator(),
            |_| Ok(([flush], by_pages.then_some((iova, len)))),
        )
    }

    /// Unmasking delivers a fault event the mask held pending. A VT-d unit
    /// raises its faults only as a message.
    fn route_faults(&self, route: FaultRoute) -> Result<(), IommuError> {
        let FaultRoute::Message { address, data } = route else {
            return Err(IommuError::OutOfRange);
        };
        let regs = self.regs.lock();
        regs.write32(regs::FEDATA, data)?;
        regs.write32(regs::FEADDR, regs::low32(address))?;
        regs.write32(regs::FEUADDR, regs::low32(address >> 32))?;
        regs.write32(regs::FECTL, 0)
    }

    fn unroute_faults(&self) -> Result<(), IommuError> {
        let regs = self.regs.lock();
        regs.write32(regs::FECTL, regs::FECTL_IM)?;
        if regs.read32(regs::FECTL)? & regs::FECTL_IM == 0 {
            return Err(IommuError::Hardware);
        }
        Ok(())
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
