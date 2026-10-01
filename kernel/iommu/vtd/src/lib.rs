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
#![forbid(unsafe_op_in_unsafe_fn)]

mod format;
mod regs;

#[cfg(test)]
mod model;
#[cfg(test)]
mod tests;

pub use regs::Registers;

use core::ops::Range;

use tairix_arch_api::{PageTableFrames, PAGE_TABLE_ENTRIES};
use tairix_collections::HashMap;
use tairix_hash::BuildFastHash;
use tairix_inline::ArrayVec;
use tairix_kernel_iommu_api::{
    Access, Clock, DomainId, Fault, FaultReason, IoPageTable, IommuError, IommuUnit,
    TableCoherence, UnitProfile, IO_PAGE_SIZE,
};
use tairix_sync::SpinLock;

use crate::format::{Descriptor, SecondLevel};
use crate::regs::{Cap, Ecap, QUEUE_SLOTS};

/// The match key discovery gives a VT-d unit and the kernel binds this family
/// to: the one definition both sides use.
pub const COMPATIBLE: &[u8] = b"intel,vtd";

/// The x86 interrupt address window: a write here is an interrupt request,
/// which the unit never translates, so no domain may hand out an IOVA in it.
static INTERRUPT_WINDOW: Range<u64> = 0xFEE0_0000..0xFEF0_0000;

/// How long the unit may take to finish a command or confirm an
/// invalidation before it is taken for broken.
const COMMAND_BUDGET_NS: u64 = 1_000_000_000;

/// Faults drained per hold of the unit's lock.
const FAULT_BATCH: usize = 32;

/// Buses one segment has, and so root entries one root table holds.
const BUSES: usize = 256;

/// Domain id 0 is reserved: under caching mode the unit tags the invalid
/// translations it caches with it.
const FIRST_DOMAIN: u16 = 1;

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
pub struct VtdUnit<'f, R: Registers> {
    frames: &'f dyn PageTableFrames,
    coherence: Option<&'f dyn TableCoherence>,
    clock: &'f dyn Clock,
    cap: Cap,
    iotlb: usize,
    levels: u32,
    profile: UnitProfile,
    state: SpinLock<State<'f, R>>,
}

struct State<'f, R> {
    regs: R,
    root: u64,
    queue: u64,
    status: u64,
    tail: usize,
    sequence: u32,
    /// The context table of each bus a stream was ever attached on.
    contexts: [Option<u64>; BUSES],
    domains: HashMap<u16, DomainState<'f>, BuildFastHash>,
    /// The domain each attached source id translates through; `None` for a
    /// source id silenced by [`IommuUnit::silence`].
    attached: HashMap<u16, Option<u16>, BuildFastHash>,
    /// The table silenced streams point at: always empty.
    silent: Option<IoPageTable<'f, SecondLevel>>,
    next_domain: u16,
    /// The fault record after the last one drained.
    next_fault: usize,
}

struct DomainState<'f> {
    table: IoPageTable<'f, SecondLevel>,
    streams: usize,
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
        let coherence = if ecap.coherent() { None } else { coherence };
        let profile = UnitProfile {
            input_bits: (IO_PAGE_SIZE.trailing_zeros() + 9 * levels).min(cap.mgaw()),
            output_bits: ENTRY_ADDRESS_BITS,
            reserved: core::slice::from_ref(&INTERRUPT_WINDOW),
        };
        let root = frames.alloc_table().ok_or(IommuError::Exhausted)?.phys;
        let queue = frames.alloc_table().ok_or(IommuError::Exhausted)?.phys;
        let status = frames.alloc_table().ok_or(IommuError::Exhausted)?.phys;
        let unit = Self {
            frames,
            coherence,
            clock,
            cap,
            iotlb,
            levels,
            profile,
            state: SpinLock::new(State {
                regs,
                root,
                queue,
                status,
                tail: 0,
                sequence: 0,
                contexts: [None; BUSES],
                domains: HashMap::with_hasher(BuildFastHash::new()),
                attached: HashMap::with_hasher(BuildFastHash::new()),
                silent: None,
                next_domain: FIRST_DOMAIN,
                next_fault: 0,
            }),
        };
        unit.publish(root, 0, PAGE_TABLE_ENTRIES);
        unit.take_over()?;
        Ok(unit)
    }

    /// Deliver the unit's fault event as the MSI `address`/`data`, and unmask
    /// it.
    ///
    /// # Errors
    ///
    /// The unit's refusal.
    pub fn route_faults(&self, address: u64, data: u32) -> Result<(), IommuError> {
        let state = self.state.lock();
        state.regs.write32(regs::FEDATA, data)?;
        state.regs.write32(regs::FEADDR, regs::low32(address))?;
        state
            .regs
            .write32(regs::FEUADDR, regs::low32(address >> 32))?;
        state.regs.write32(regs::FECTL, 0)
    }

    fn take_over(&self) -> Result<(), IommuError> {
        let mut state = self.state.lock();
        // Firmware may have left translation and the queue running on tables
        // of its own; both stop before anything of ours is installed.
        let gsts = state.regs.read32(regs::GSTS)?;
        if gsts & regs::GSTS_TES != 0 {
            self.command(&state.regs, 0, regs::GCMD_TE, regs::GSTS_TES, 0)?;
        }
        if gsts & regs::GSTS_QIES != 0 {
            self.command(&state.regs, 0, regs::GCMD_QIE, regs::GSTS_QIES, 0)?;
        }
        state.regs.write32(regs::FECTL, regs::FECTL_IM)?;
        state.regs.write64(regs::RTADDR, state.root)?;
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
        state.tail = 0;
        state.regs.write64(regs::IQT, 0)?;
        state.regs.write64(regs::IQA, state.queue)?;
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
        let deadline = self.clock.now_ns().saturating_add(COMMAND_BUDGET_NS);
        loop {
            if regs.read32(offset)? & mask == want {
                return Ok(());
            }
            if self.clock.now_ns() > deadline {
                return Err(IommuError::Unconfirmed);
            }
            core::hint::spin_loop();
        }
    }

    fn wait_register64(&self, regs: &R, offset: usize, busy: u64) -> Result<(), IommuError> {
        let deadline = self.clock.now_ns().saturating_add(COMMAND_BUDGET_NS);
        loop {
            if regs.read64(offset)? & busy == 0 {
                return Ok(());
            }
            if self.clock.now_ns() > deadline {
                return Err(IommuError::Unconfirmed);
            }
            core::hint::spin_loop();
        }
    }

    /// Queue `descriptors` and an invalidation wait behind them, and return
    /// once the unit has written the wait's status.
    fn invalidate(
        &self,
        state: &mut State<'f, R>,
        descriptors: &[Descriptor],
    ) -> Result<(), IommuError> {
        state.sequence = state.sequence.wrapping_add(1).max(1);
        let token = state.sequence;
        let wait = format::wait(token, state.status);
        let deadline = self.clock.now_ns().saturating_add(COMMAND_BUDGET_NS);
        for descriptor in descriptors.iter().chain(core::iter::once(&wait)) {
            let next = (state.tail + 1) % QUEUE_SLOTS;
            // The queue is full while the tail would catch the head.
            while regs::queue_index(state.regs.read64(regs::IQH)?) == next {
                if self.clock.now_ns() > deadline {
                    return Err(IommuError::Unconfirmed);
                }
                core::hint::spin_loop();
            }
            self.write_entry(state.queue, 2 * state.tail, descriptor[0])?;
            self.write_entry(state.queue, 2 * state.tail + 1, descriptor[1])?;
            state.tail = next;
        }
        tairix_dma_barrier::dma_wmb();
        state.regs.write64(regs::IQT, (state.tail as u64) << 4)?;
        loop {
            if regs::low32(self.read_entry(state.status, 0)?) == token {
                return Ok(());
            }
            let fsts = state.regs.read32(regs::FSTS)?;
            let errors = fsts & (regs::FSTS_IQE | regs::FSTS_ICE | regs::FSTS_ITE);
            if errors != 0 {
                self.recover_queue(state, errors)?;
                return Err(IommuError::Hardware);
            }
            if self.clock.now_ns() > deadline {
                return Err(IommuError::Unconfirmed);
            }
            core::hint::spin_loop();
        }
    }

    /// A rejected descriptor stops the queue at it: replace it with a bare
    /// fence so the queue runs on, and clear the error.
    fn recover_queue(&self, state: &mut State<'f, R>, errors: u32) -> Result<(), IommuError> {
        if errors & regs::FSTS_IQE != 0 {
            let head = regs::queue_index(state.regs.read64(regs::IQH)?);
            let fence = format::wait(0, state.status);
            self.write_entry(state.queue, 2 * head, fence[0] & !(1 << 5))?;
            self.write_entry(state.queue, 2 * head + 1, 0)?;
            tairix_dma_barrier::dma_wmb();
        }
        state.regs.write32(regs::FSTS, errors)
    }

    fn domain_iotlb(&self, domain: u16) -> Descriptor {
        format::iotlb_domain(domain, self.cap.drain_reads(), self.cap.drain_writes())
    }

    fn read_entry(&self, table: u64, index: usize) -> Result<u64, IommuError> {
        let entries = self.frames.table_at(table).ok_or(IommuError::Hardware)?;
        if index >= PAGE_TABLE_ENTRIES {
            return Err(IommuError::Hardware);
        }
        // SAFETY: `table_at` returned the live pointer to one of this unit's
        // own frames, and `index` is inside it.
        Ok(unsafe { core::ptr::addr_of!((*entries)[index]).read_volatile() })
    }

    fn write_entry(&self, table: u64, index: usize, value: u64) -> Result<(), IommuError> {
        let entries = self.frames.table_at(table).ok_or(IommuError::Hardware)?;
        if index >= PAGE_TABLE_ENTRIES {
            return Err(IommuError::Hardware);
        }
        // SAFETY: as `read_entry`; the unit's lock serialises every writer of
        // its own frames.
        unsafe { core::ptr::addr_of_mut!((*entries)[index]).write_volatile(value) };
        Ok(())
    }

    fn publish(&self, table: u64, first: usize, count: usize) {
        if let Some(coherence) = self.coherence {
            coherence.write_back(table + (first as u64) * 8, count * 8);
        }
    }

    /// The context table of `bus`, created and linked from the root the
    /// first time a stream on the bus is attached.
    fn context_table(&self, state: &mut State<'f, R>, bus: u8) -> Result<u64, IommuError> {
        if let Some(table) = state.contexts[usize::from(bus)] {
            return Ok(table);
        }
        let table = self.frames.alloc_table().ok_or(IommuError::Exhausted)?.phys;
        self.publish(table, 0, PAGE_TABLE_ENTRIES);
        let slot = 2 * usize::from(bus);
        self.write_entry(state.root, slot + 1, 0)?;
        self.write_entry(
            state.root,
            slot,
            (table & format::ADDRESS) | format::PRESENT,
        )?;
        self.publish(state.root, slot, 2);
        state.contexts[usize::from(bus)] = Some(table);
        Ok(table)
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
        let table = self.context_table(state, bus)?;
        let slot = 2 * usize::from(devfn);
        self.write_entry(table, slot + 1, high)?;
        tairix_dma_barrier::dma_wmb();
        self.write_entry(table, slot, low)?;
        self.publish(table, slot, 2);
        Ok(())
    }

    /// Clear stream `source`'s context entry, low word first.
    fn clear_context(&self, state: &mut State<'f, R>, source: u16) -> Result<(), IommuError> {
        let [bus, devfn] = source.to_be_bytes();
        let Some(table) = state.contexts[usize::from(bus)] else {
            return Ok(());
        };
        let slot = 2 * usize::from(devfn);
        self.write_entry(table, slot, 0)?;
        tairix_dma_barrier::dma_wmb();
        self.write_entry(table, slot + 1, 0)?;
        self.publish(table, slot, 2);
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

    /// The first free domain id from the rotating cursor, so a freed id is
    /// the last one handed out again.
    fn allocate_domain_id(&self, state: &State<'f, R>) -> Option<u16> {
        let first = u32::from(FIRST_DOMAIN);
        let span = self.cap.domains().checked_sub(first)?;
        let start = u32::from(state.next_domain).saturating_sub(first);
        (0..span)
            .filter_map(|step| u16::try_from(first + (start + step) % span).ok())
            .find(|id| !state.domains.contains_key(id))
    }

    /// Move pending fault records into `batch`, oldest first, clearing each,
    /// within one sweep of the records. Whether records may remain.
    ///
    /// The unit writes consecutive records, so the pending ones run on from
    /// the record after the last one drained. `FSTS.FRI` is latched only when
    /// a run begins, so a drain that stopped part-way cannot resume from it;
    /// a run firmware left elsewhere is found by the sweep.
    fn take_faults(&self, records: usize, batch: &mut ArrayVec<Fault, FAULT_BATCH>) -> bool {
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

    /// Detach `source` from whatever it is attached to, confirmed.
    fn detach(&self, state: &mut State<'f, R>, source: u16) -> Result<(), IommuError> {
        let Some(previous) = state.attached.get(&source).copied() else {
            return Ok(());
        };
        self.clear_context(state, source)?;
        let domain = previous.unwrap_or(0);
        let descriptors = [
            format::context_device(domain, source),
            self.domain_iotlb(domain),
        ];
        // The entry is gone from the tables whatever the queue says; the
        // bookkeeping follows the tables, and an unconfirmed flush is the
        // caller's to act on.
        state.attached.remove(&source);
        if let Some(id) = previous {
            if let Some(owner) = state.domains.get_mut(&id) {
                owner.streams = owner.streams.saturating_sub(1);
            }
        }
        self.invalidate(state, &descriptors)
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

fn domain_id(domain: DomainId) -> Result<u16, IommuError> {
    u16::try_from(domain.0).map_err(|_| IommuError::OutOfRange)
}

/// Decode one fault recording register. A permission fault and a missing
/// entry report the same reasons (5 for a write, 6 for a read), and every
/// DMA carve is mapped read-write, so both read as unmapped.
fn decode_fault(low: u64, high: u64) -> Fault {
    let reason = match regs::field(high, 32, 8) {
        0x1 | 0x2 => FaultReason::Blocked,
        0x4..=0x6 => FaultReason::Unmapped,
        0x3 | 0x7..=0xC => FaultReason::Malformed,
        other => FaultReason::Other(u16::try_from(other).unwrap_or(u16::MAX)),
    };
    Fault {
        stream: regs::field(high, 0, 16),
        iova: low & !(IO_PAGE_SIZE - 1),
        write: high & (FAULT_T1 | FAULT_T2) == 0,
        reason,
    }
}

impl<R: Registers> IommuUnit for VtdUnit<'_, R> {
    fn profile(&self) -> UnitProfile {
        self.profile
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
            self.frames,
            self.coherence,
        )?;
        let mut state = self.state.lock();
        let id = self
            .allocate_domain_id(&state)
            .ok_or(IommuError::Exhausted)?;
        state.next_domain = id.checked_add(1).unwrap_or(FIRST_DOMAIN).max(FIRST_DOMAIN);
        state
            .domains
            .try_insert(id, DomainState { table, streams: 0 })
            .map_err(|_| IommuError::Exhausted)?;
        Ok(DomainId(u32::from(id)))
    }

    fn destroy_domain(&self, domain: DomainId) -> Result<(), IommuError> {
        let id = domain_id(domain)?;
        let mut state = self.state.lock();
        match state.domains.get(&id) {
            None => return Err(IommuError::OutOfRange),
            Some(owner) if owner.streams != 0 => return Err(IommuError::DomainBusy),
            Some(_) => {}
        }
        // Nothing the unit cached for the id may outlive its tables, or
        // survive into the id's next owner.
        let flush = [self.domain_iotlb(id)];
        self.invalidate(&mut state, &flush)?;
        state.domains.remove(&id);
        Ok(())
    }

    fn attach(&self, stream: u32, domain: DomainId) -> Result<(), IommuError> {
        let source = source_id(stream)?;
        let id = domain_id(domain)?;
        let mut state = self.state.lock();
        let root = state
            .domains
            .get(&id)
            .ok_or(IommuError::OutOfRange)?
            .table
            .root();
        state
            .attached
            .try_reserve(1)
            .map_err(|_| IommuError::Exhausted)?;
        match state.attached.get(&source) {
            Some(Some(_)) => return Err(IommuError::StreamBusy),
            // A silenced stream may take an owner: it is blocked either way.
            Some(None) => self.detach(&mut state, source)?,
            None => {}
        }
        self.write_context(
            &mut state,
            source,
            format::context_low(root),
            format::context_high(id, self.levels),
        )?;
        let _ = state.attached.try_insert(source, Some(id));
        if let Some(owner) = state.domains.get_mut(&id) {
            owner.streams += 1;
        }
        self.publish_new(&mut state, Some(source), id)
    }

    fn block(&self, stream: u32) -> Result<(), IommuError> {
        let source = source_id(stream)?;
        let mut state = self.state.lock();
        if state.attached.get(&source) == Some(&None) {
            return Ok(());
        }
        self.detach(&mut state, source)
    }

    fn silence(&self, stream: u32) -> Result<(), IommuError> {
        let source = source_id(stream)?;
        let mut state = self.state.lock();
        if state.attached.get(&source) == Some(&None) {
            return Ok(());
        }
        state
            .attached
            .try_reserve(1)
            .map_err(|_| IommuError::Exhausted)?;
        if state.silent.is_none() {
            let table = IoPageTable::new(
                SecondLevel {
                    large_2m: false,
                    large_1g: false,
                },
                self.levels,
                self.frames,
                self.coherence,
            )?;
            state.silent = Some(table);
        }
        let root = state.silent.as_ref().map_or(0, IoPageTable::root);
        self.detach(&mut state, source)?;
        self.write_context(
            &mut state,
            source,
            format::context_low(root) | CONTEXT_FPD,
            format::context_high(0, self.levels),
        )?;
        let _ = state.attached.try_insert(source, None);
        self.publish_new(&mut state, Some(source), 0)
    }

    fn map(
        &self,
        domain: DomainId,
        iova: u64,
        phys: u64,
        len: u64,
        access: Access,
    ) -> Result<(), IommuError> {
        let id = domain_id(domain)?;
        let end = phys.checked_add(len).ok_or(IommuError::OutOfRange)?;
        if end > 1u64 << self.profile.output_bits.min(63) {
            return Err(IommuError::OutOfRange);
        }
        let mut state = self.state.lock();
        state
            .domains
            .get_mut(&id)
            .ok_or(IommuError::OutOfRange)?
            .table
            .map(iova, phys, len, access)?;
        self.publish_new(&mut state, None, id)
    }

    fn unmap(&self, domain: DomainId, iova: u64, len: u64) -> Result<(), IommuError> {
        let id = domain_id(domain)?;
        self.state
            .lock()
            .domains
            .get_mut(&id)
            .ok_or(IommuError::OutOfRange)?
            .table
            .unmap(iova, len)
    }

    fn sync(&self, domain: DomainId) -> Result<(), IommuError> {
        let id = domain_id(domain)?;
        let mut state = self.state.lock();
        if !state.domains.contains_key(&id) {
            return Err(IommuError::OutOfRange);
        }
        let flush = [self.domain_iotlb(id)];
        self.invalidate(&mut state, &flush)?;
        if let Some(owner) = state.domains.get_mut(&id) {
            owner.table.release_retired();
        }
        Ok(())
    }

    /// At most one ring's worth of records per call. The fault event is raised
    /// only when PPF sets, so a call that stops with records left says so.
    /// Each batch reaches `sink` with the unit unlocked, since what it does
    /// about a fault may be to call back in.
    fn drain_faults(&self, sink: &mut dyn FnMut(Fault)) -> bool {
        let records = self.cap.fault_records();
        for _ in 0..records.div_ceil(FAULT_BATCH) {
            let mut batch = ArrayVec::<Fault, FAULT_BATCH>::new();
            let more = self.take_faults(records, &mut batch);
            for fault in batch {
                sink(fault);
            }
            if !more {
                return false;
            }
        }
        true
    }
}
