//! A register-level model of one VT-d unit, written from the specification
//! rather than from the unit's code: its register file, the root → context →
//! second-level walk it performs in memory, a context cache and an IOTLB that
//! keep what they cached until an invalidation removes it, the invalidation
//! queue it fetches from memory, and its fault recording registers.

extern crate std;

use std::collections::BTreeMap;

use tairix_kernel_iommu_api::conformance::{InterruptProbe, TranslationProbe};
use tairix_kernel_iommu_api::hostmem::HostFrames;
use tairix_kernel_iommu_api::{InterruptTarget, IommuError};
use tairix_sync::SpinLock;

use crate::regs;
use tairix_kernel_iommu_api::Registers;

const ADDRESS: u64 = 0x000F_FFFF_FFFF_F000;
const PAGE: u64 = 0x1000;
const READ_WRITE: u64 = 0b11;

/// A cached translation: the physical page, and whether it may be read and
/// written.
type Translation = (u64, bool, bool);

/// Register layout the model uses: the IOTLB registers at 0x300, fault
/// records from 0x400, one page of registers.
pub(crate) const IOTLB_OFFSET: usize = 0x300;
pub(crate) const FAULT_OFFSET: usize = 0x400;
pub(crate) const WINDOW: usize = 0x1000;

/// A capability register for the model: `domains_field` (ND), four fault
/// records at [`FAULT_OFFSET`], a 48-bit 4-level width, 2 MiB and 1 GiB
/// leaves, plus `extra` bits.
pub(crate) fn cap(domains_field: u64, extra: u64) -> u64 {
    cap_with_records(domains_field, 4, extra)
}

/// [`cap`] with `records` fault records, at most what the window holds past
/// [`FAULT_OFFSET`].
pub(crate) fn cap_with_records(domains_field: u64, records: u64, extra: u64) -> u64 {
    domains_field
        | (0b0100 << 8)
        | (47 << 16)
        | (((FAULT_OFFSET / 16) as u64) << 24)
        | (0b11 << 34)
        | ((records - 1) << 40)
        | extra
}

/// An extended capability register: queued invalidation, the IOTLB
/// registers at [`IOTLB_OFFSET`], and a snooping walker when `coherent`.
pub(crate) fn ecap(coherent: bool) -> u64 {
    u64::from(coherent) | (1 << 1) | (((IOTLB_OFFSET / 16) as u64) << 8)
}

/// Interrupt remapping, in an extended capability register.
pub(crate) const ECAP_IR: u64 = 1 << 3;
/// Extended interrupt mode: 32-bit x2APIC destinations.
pub(crate) const ECAP_EIM: u64 = 1 << 4;

/// Ways the model can be told to misbehave.
#[derive(Copy, Clone, Default)]
pub(crate) struct Quirks {
    /// Reject the next IOTLB descriptor as malformed.
    pub reject_next_iotlb: bool,
    /// Never write an invalidation wait's status.
    pub ignore_waits: bool,
    /// Lose every context-cache invalidation, keeping what was cached.
    pub lose_context_invalidations: bool,
}

#[derive(Copy, Clone)]
struct Context {
    domain: u16,
    table: u64,
    levels: u32,
    silent: bool,
}

struct State {
    regs: BTreeMap<usize, u64>,
    gsts: u32,
    root: Option<u64>,
    head: usize,
    quirks: Quirks,
    /// Cached context entries; `None` caches a not-present one (caching
    /// mode only).
    contexts: BTreeMap<u16, Option<Context>>,
    /// Cached translations by domain and page, with their read and write
    /// permission; `None` caches a miss (caching mode only).
    iotlb: BTreeMap<(u16, u64), Option<Translation>>,
    /// The record the next fault is written to: the specification's internal
    /// fault recording index.
    fault_index: usize,
    /// `GCMD` writes that changed more than one command at once, which the
    /// architecture leaves undefined and the model ignores.
    gcmd_overloaded: usize,
    /// FSTS.FRI: latched when a fault sets PPF, and only then.
    fault_first: usize,
    /// Descriptors processed, by type.
    processed: BTreeMap<u64, usize>,
    /// The remapping table address latched by the last table-pointer
    /// command.
    irta: Option<u64>,
    /// Cached remapping entries, by index, until an interrupt entry cache
    /// invalidation removes them.
    iec: BTreeMap<u16, [u64; 2]>,
}

pub(crate) struct Model<'f> {
    frames: &'f HostFrames,
    cap: u64,
    ecap: u64,
    state: SpinLock<State>,
}

impl<'f> Model<'f> {
    pub(crate) fn new(frames: &'f HostFrames, cap: u64, ecap: u64) -> Self {
        Self {
            frames,
            cap,
            ecap,
            state: SpinLock::new(State {
                regs: BTreeMap::new(),
                gsts: 0,
                root: None,
                head: 0,
                quirks: Quirks::default(),
                contexts: BTreeMap::new(),
                iotlb: BTreeMap::new(),
                fault_index: 0,
                gcmd_overloaded: 0,
                fault_first: 0,
                processed: BTreeMap::new(),
                irta: None,
                iec: BTreeMap::new(),
            }),
        }
    }

    /// `GCMD` writes that changed more than one command at once.
    pub(crate) fn gcmd_overloaded(&self) -> usize {
        self.state.lock().gcmd_overloaded
    }

    pub(crate) fn quirk(&self, quirks: Quirks) {
        self.state.lock().quirks = quirks;
    }

    /// Leave the model as firmware might: translating through a root of its
    /// own, queued invalidation on, protected memory enabled.
    pub(crate) fn firmware_left_running(&self) {
        let mut state = self.state.lock();
        state.gsts = regs::GSTS_TES
            | regs::GSTS_RTPS
            | regs::GSTS_QIES
            | regs::GSTS_IRES
            | regs::GSTS_IRTPS
            | regs::GSTS_CFIS;
        state.root = Some(0xDEAD_0000);
        state
            .regs
            .insert(regs::PMEN, u64::from(regs::PMEN_EPM | regs::PMEN_PRS));
    }

    /// Leave an invalidation queue error and a fault overflow standing, as
    /// firmware might.
    pub(crate) fn firmware_left_errors(&self) {
        let mut state = self.state.lock();
        let sticky = state.regs.entry(regs::FSTS).or_insert(0);
        *sticky |= u64::from(regs::FSTS_IQE | regs::FSTS_PFO);
    }

    /// The domain id stream `source`'s context entry carries, walked from
    /// memory.
    pub(crate) fn context_domain(&self, source: u16) -> Option<u16> {
        let state = self.state.lock();
        self.walk_context(&state, source)
            .ok()
            .flatten()
            .map(|context| context.domain)
    }

    /// Leave a fault of each of `streams` recorded from record `at`, as
    /// firmware might.
    pub(crate) fn firmware_left_faults(&self, at: usize, streams: &[u16]) {
        let mut state = self.state.lock();
        state.fault_index = at;
        for &stream in streams {
            self.record_fault(&mut state, stream, 0, true, 0x1);
        }
    }

    pub(crate) fn translating(&self) -> bool {
        self.state.lock().gsts & regs::GSTS_TES != 0
    }

    pub(crate) fn register(&self, offset: usize) -> u64 {
        self.state.lock().regs.get(&offset).copied().unwrap_or(0)
    }

    pub(crate) fn processed(&self, kind: u64) -> usize {
        self.state.lock().processed.get(&kind).copied().unwrap_or(0)
    }

    /// Whether the IOTLB holds `domain`'s translation of the page at `iova`.
    pub(crate) fn caches(&self, domain: u16, iova: u64) -> bool {
        self.state
            .lock()
            .iotlb
            .get(&(domain, iova & !(PAGE - 1)))
            .is_some_and(Option::is_some)
    }

    fn caching_mode(&self) -> bool {
        self.cap & (1 << 7) != 0
    }

    fn fault_records(&self) -> usize {
        regs::field_usize(self.cap, 40, 8) + 1
    }

    fn recorded(state: &State, index: usize) -> bool {
        state
            .regs
            .get(&(FAULT_OFFSET + index * 16 + 8))
            .copied()
            .unwrap_or(0)
            & (1 << 63)
            != 0
    }

    /// PPF: whether any record holds a fault.
    fn pending(&self, state: &State) -> bool {
        (0..self.fault_records()).any(|index| Self::recorded(state, index))
    }

    fn fsts(&self, state: &State) -> u32 {
        let sticky = regs::low32(state.regs.get(&regs::FSTS).copied().unwrap_or(0));
        if self.pending(state) {
            sticky | regs::FSTS_PPF | (u32::try_from(state.fault_first).unwrap_or(0) << 8)
        } else {
            sticky
        }
    }

    /// Record a fault as Intel VT-d rev. 4.1 §7.2.1 does: nothing while PFO is
    /// set, an overflow when the record at the internal index is still
    /// pending, and FRI latched only by the fault that sets PPF.
    fn record_fault(&self, state: &mut State, stream: u16, iova: u64, write: bool, reason: u64) {
        let fsts = state.regs.get(&regs::FSTS).copied().unwrap_or(0);
        if fsts & u64::from(regs::FSTS_PFO) != 0 {
            return;
        }
        let index = state.fault_index;
        if Self::recorded(state, index) {
            state
                .regs
                .insert(regs::FSTS, fsts | u64::from(regs::FSTS_PFO));
            return;
        }
        if !self.pending(state) {
            state.fault_first = index;
        }
        let high = (1 << 63) | (u64::from(!write) << 62) | (reason << 32) | u64::from(stream);
        state
            .regs
            .insert(FAULT_OFFSET + index * 16, iova & !(PAGE - 1));
        state.regs.insert(FAULT_OFFSET + index * 16 + 8, high);
        state.fault_index = (index + 1) % self.fault_records();
    }

    fn walk_context(&self, state: &State, stream: u16) -> Result<Option<Context>, u64> {
        let root = state.root.ok_or(1u64)?;
        let [bus, devfn] = stream.to_be_bytes();
        let root_entry = self.frames.entry(root, 2 * usize::from(bus)).unwrap_or(0);
        if root_entry & 1 == 0 {
            return Err(1);
        }
        let table = root_entry & ADDRESS;
        let low = self
            .frames
            .entry(table, 2 * usize::from(devfn))
            .unwrap_or(0);
        let high = self
            .frames
            .entry(table, 2 * usize::from(devfn) + 1)
            .unwrap_or(0);
        if low & 1 == 0 {
            return Ok(None);
        }
        let width = high & 0b111;
        if !(1..=3).contains(&width) || (low >> 2) & 0b11 != 0 {
            return Err(3);
        }
        Ok(Some(Context {
            domain: ((high >> 8) & 0xFFFF) as u16,
            table: low & ADDRESS,
            levels: width as u32 + 2,
            silent: low & 0b10 != 0,
        }))
    }

    /// Walk a second-level tree: bit 0 read, bit 1 write, bit 7 a leaf above
    /// level 0, bits 51:12 the address.
    fn walk_second_level(&self, context: &Context, iova: u64) -> Option<Translation> {
        let mut table = context.table;
        for level in (0..context.levels).rev() {
            let shift = 12 + 9 * level;
            let index = ((iova >> shift) & 0x1FF) as usize;
            let entry = self.frames.entry(table, index)?;
            if entry & READ_WRITE == 0 {
                return None;
            }
            if level == 0 || entry & (1 << 7) != 0 {
                let span_mask = (1u64 << shift) - 1;
                return Some((
                    (entry & ADDRESS & !span_mask) | (iova & span_mask),
                    entry & 1 != 0,
                    entry & 2 != 0,
                ));
            }
            table = entry & ADDRESS;
        }
        None
    }

    fn run_queue(&self, state: &mut State, tail: usize) {
        let queue = state.regs.get(&regs::IQA).copied().unwrap_or(0) & ADDRESS;
        // An invalidation queue error halts fetching until software clears it.
        let halted = |state: &State| {
            state.regs.get(&regs::FSTS).copied().unwrap_or(0) & u64::from(regs::FSTS_IQE) != 0
        };
        while state.head != tail && !halted(state) {
            let slot = state.head;
            let low = self.frames.entry(queue, 2 * slot).unwrap_or(0);
            let high = self.frames.entry(queue, 2 * slot + 1).unwrap_or(0);
            let kind = low & 0xF;
            let granularity = (low >> 4) & 0b11;
            let domain = ((low >> 16) & 0xFFFF) as u16;
            match kind {
                0x1 if state.quirks.lose_context_invalidations => {}
                0x1 => {
                    let source = ((low >> 32) & 0xFFFF) as u16;
                    match granularity {
                        0b01 => state.contexts.clear(),
                        0b10 => state
                            .contexts
                            .retain(|_, c| c.is_some_and(|c| c.domain != domain)),
                        _ => {
                            state.contexts.remove(&source);
                        }
                    }
                }
                0x2 => {
                    if core::mem::take(&mut state.quirks.reject_next_iotlb) {
                        let sticky = state.regs.entry(regs::FSTS).or_insert(0);
                        *sticky |= u64::from(regs::FSTS_IQE);
                        return;
                    }
                    let page_selective = self.cap & (1 << 39) != 0;
                    match granularity {
                        0b01 => state.iotlb.clear(),
                        0b11 if page_selective => {
                            let mask = high & 0x3F;
                            if mask > (self.cap >> 48) & 0x3F {
                                let sticky = state.regs.entry(regs::FSTS).or_insert(0);
                                *sticky |= u64::from(regs::FSTS_IQE);
                                return;
                            }
                            // The unit ignores the address bits the mask spans.
                            let size = PAGE << mask;
                            let base = high & !(size - 1);
                            state.iotlb.retain(|&(d, page), _| {
                                d != domain || page < base || page - base >= size
                            });
                        }
                        _ => state.iotlb.retain(|&(d, _), _| d != domain),
                    }
                }
                0x4 => {
                    if low & (1 << 4) == 0 {
                        state.iec.clear();
                    } else {
                        let index = ((low >> 32) & 0xFFFF) as u16;
                        let mask = (low >> 27) & 0x1F;
                        let first = index & !((1u16 << mask) - 1);
                        state
                            .iec
                            .retain(|&at, _| at < first || u64::from(at - first) >= 1 << mask);
                    }
                }
                0x5 => {
                    let status_write = low & (1 << 5) != 0;
                    if status_write && !state.quirks.ignore_waits {
                        let address = high & !0b11;
                        let page = address & !(PAGE - 1);
                        let index = ((address - page) / 8) as usize;
                        let old = self.frames.entry(page, index).unwrap_or(0);
                        let value = (old & !0xFFFF_FFFF) | (low >> 32);
                        self.frames.store(page, index, value);
                    }
                }
                _ => {
                    let sticky = state.regs.entry(regs::FSTS).or_insert(0);
                    *sticky |= u64::from(regs::FSTS_IQE);
                    return;
                }
            }
            *state.processed.entry(kind).or_insert(0) += 1;
            state.head = (state.head + 1) % regs::QUEUE_SLOTS;
        }
    }

    fn write(&self, offset: usize, value: u64) {
        let mut state = self.state.lock();
        match offset {
            regs::GCMD => {
                let value = regs::low32(value);
                let toggled = [
                    (regs::GCMD_TE, regs::GSTS_TES),
                    (regs::GCMD_QIE, regs::GSTS_QIES),
                    (regs::GCMD_IRE, regs::GSTS_IRES),
                    (regs::GCMD_CFI, regs::GSTS_CFIS),
                ]
                .iter()
                .filter(|&&(command, status)| (value & command != 0) != (state.gsts & status != 0))
                .count();
                let latched = [regs::GCMD_SRTP, regs::GCMD_SIRTP]
                    .iter()
                    .filter(|&&command| value & command != 0)
                    .count();
                if toggled + latched > 1 {
                    state.gcmd_overloaded += 1;
                    return;
                }
                if value & regs::GCMD_SRTP != 0 {
                    state.root =
                        Some(state.regs.get(&regs::RTADDR).copied().unwrap_or(0) & ADDRESS);
                    state.gsts |= regs::GSTS_RTPS;
                }
                if value & regs::GCMD_SIRTP != 0 {
                    state.irta = state.regs.get(&regs::IRTA).copied();
                    state.gsts |= regs::GSTS_IRTPS;
                }
                for (command, status) in [
                    (regs::GCMD_TE, regs::GSTS_TES),
                    (regs::GCMD_QIE, regs::GSTS_QIES),
                    (regs::GCMD_IRE, regs::GSTS_IRES),
                    (regs::GCMD_CFI, regs::GSTS_CFIS),
                ] {
                    if value & command != 0 {
                        if status == regs::GSTS_QIES && state.gsts & status == 0 {
                            state.head = 0;
                        }
                        state.gsts |= status;
                    } else {
                        state.gsts &= !status;
                    }
                }
                if state.gsts & (regs::GSTS_TES | regs::GSTS_QIES) == 0 {
                    state.fault_index = 0;
                }
            }
            regs::CCMD => {
                if value & regs::CCMD_ICC != 0 {
                    state.contexts.clear();
                }
                state.regs.insert(offset, value & !regs::CCMD_ICC);
            }
            regs::IQT => {
                state.regs.insert(offset, value);
                if state.gsts & regs::GSTS_QIES != 0 {
                    self.run_queue(&mut state, regs::queue_index(value));
                }
            }
            regs::FSTS => {
                let sticky = state.regs.entry(regs::FSTS).or_insert(0);
                *sticky &= !(value & 0x7F);
            }
            regs::PMEN => {
                let enabled = value & u64::from(regs::PMEN_EPM) != 0;
                let prs = u64::from(if enabled { regs::PMEN_PRS } else { 0 });
                state
                    .regs
                    .insert(offset, (value & u64::from(regs::PMEN_EPM)) | prs);
            }
            _ if offset == IOTLB_OFFSET + regs::IOTLB_REG => {
                if value & regs::IOTLB_IVT != 0 {
                    state.iotlb.clear();
                }
                state.regs.insert(offset, value & !regs::IOTLB_IVT);
            }
            _ if (FAULT_OFFSET..FAULT_OFFSET + self.fault_records() * 16).contains(&offset)
                && offset % 16 == 12 =>
            {
                if value & (1 << 31) != 0 {
                    let high = state.regs.entry(offset - 4).or_insert(0);
                    *high &= !(1 << 63);
                }
            }
            _ => {
                state.regs.insert(offset, value);
            }
        }
    }

    fn read(&self, offset: usize) -> u64 {
        let state = self.state.lock();
        match offset {
            regs::VER => 0x10,
            regs::CAP => self.cap,
            regs::ECAP => self.ecap,
            regs::GSTS => u64::from(state.gsts),
            regs::IQH => (state.head as u64) << 4,
            regs::FSTS => u64::from(self.fsts(&state)),
            _ => state.regs.get(&offset).copied().unwrap_or(0),
        }
    }
}

impl Registers for &Model<'_> {
    fn read32(&self, offset: usize) -> Result<u32, IommuError> {
        if offset + 4 > WINDOW {
            return Err(IommuError::Hardware);
        }
        Ok(regs::low32(self.read(offset)))
    }

    fn write32(&self, offset: usize, value: u32) -> Result<(), IommuError> {
        if offset + 4 > WINDOW {
            return Err(IommuError::Hardware);
        }
        self.write(offset, u64::from(value));
        Ok(())
    }

    fn read64(&self, offset: usize) -> Result<u64, IommuError> {
        if offset + 8 > WINDOW || !offset.is_multiple_of(8) {
            return Err(IommuError::Hardware);
        }
        Ok(self.read(offset))
    }

    fn write64(&self, offset: usize, value: u64) -> Result<(), IommuError> {
        if offset + 8 > WINDOW || !offset.is_multiple_of(8) {
            return Err(IommuError::Hardware);
        }
        self.write(offset, value);
        Ok(())
    }

    fn window_len(&self) -> usize {
        WINDOW
    }
}

impl Model<'_> {
    /// Record an interrupt request's fault: its source and the entry it
    /// named, in the fault information field.
    fn record_interrupt_fault(&self, state: &mut State, source: u16, entry: u64, reason: u64) {
        self.record_fault(state, source, 0, true, reason);
        let last = (state.fault_index + self.fault_records() - 1) % self.fault_records();
        if Self::recorded(state, last) {
            state.regs.insert(FAULT_OFFSET + last * 16, entry << 48);
        }
    }

    /// The remapping entry at `index` of the latched table, cached once read.
    fn irte(&self, state: &mut State, irta: u64, index: u16) -> Option<[u64; 2]> {
        if let Some(&cached) = state.iec.get(&index) {
            return Some(cached);
        }
        let at = (irta & ADDRESS) + u64::from(index) * 16;
        let entry = [self.frames.word(at)?, self.frames.word(at + 8)?];
        state.iec.insert(index, entry);
        Some(entry)
    }
}

/// An interrupt request as Intel VT-d rev. 4.1 §5.1 decodes it: with remapping off,
/// every request is a compatibility one; with it on, a compatibility request
/// is blocked (reason 0x25) unless CFI allows it, and a remappable one names
/// an entry that must be in the table (0x21), present (0x22), and admit the
/// request's source (0x26).
impl InterruptProbe for Model<'_> {
    fn interrupt(&self, source: u16, address: u64, data: u32) -> Option<InterruptTarget> {
        let mut state = self.state.lock();
        let compatibility = InterruptTarget {
            vector: data.to_le_bytes()[0],
            destination: u32::try_from((address >> 12) & 0xFF).unwrap_or(0),
            level: false,
        };
        if state.gsts & regs::GSTS_IRES == 0 {
            return Some(compatibility);
        }
        if address & (1 << 4) == 0 {
            if state.gsts & regs::GSTS_CFIS != 0 {
                return Some(compatibility);
            }
            self.record_interrupt_fault(&mut state, source, 0, 0x25);
            return None;
        }
        let handle = ((address >> 5) & 0x7FFF) | (((address >> 2) & 1) << 15);
        let index = if address & (1 << 3) != 0 {
            handle + u64::from(data & 0xFFFF)
        } else {
            handle
        };
        let irta = state.irta.unwrap_or(0);
        let size = 1u64 << ((irta & 0xF) + 1);
        let Ok(slot) = u16::try_from(index).map_err(|_| ()).and_then(|slot| {
            if index < size {
                Ok(slot)
            } else {
                Err(())
            }
        }) else {
            self.record_interrupt_fault(&mut state, source, index, 0x21);
            return None;
        };
        let Some([low, high]) = self.irte(&mut state, irta, slot) else {
            self.record_interrupt_fault(&mut state, source, index, 0x23);
            return None;
        };
        if low & 1 == 0 {
            self.record_interrupt_fault(&mut state, source, index, 0x22);
            return None;
        }
        let sid = (high & 0xFFFF) as u16;
        let admitted = match (high >> 18) & 0b11 {
            0b00 => true,
            0b01 => source == sid,
            0b10 => {
                let [first, last] = sid.to_be_bytes();
                (first..=last).contains(&source.to_be_bytes()[0])
            }
            _ => false,
        };
        if !admitted {
            self.record_interrupt_fault(&mut state, source, index, 0x26);
            return None;
        }
        let destination = if irta & regs::IRTA_EIME != 0 {
            (low >> 32) as u32
        } else {
            ((low >> 40) & 0xFF) as u32
        };
        Some(InterruptTarget {
            vector: ((low >> 16) & 0xFF) as u8,
            destination,
            level: low & (1 << 4) != 0,
        })
    }
}

impl TranslationProbe for Model<'_> {
    /// A translated request against a context entry of the untranslated-only
    /// type is blocked, reason 0xD (rev. 4.1 §7.1.3); a stream with no
    /// context faults as an untranslated request would.
    fn translated(&self, stream: u32, address: u64, write: bool) -> Option<u64> {
        let mut state = self.state.lock();
        if state.gsts & regs::GSTS_TES == 0 {
            return Some(address);
        }
        let source = u16::try_from(stream).ok()?;
        let page = address & !(PAGE - 1);
        match self.walk_context(&state, source) {
            Ok(Some(context)) if context.silent => {}
            Ok(Some(_)) => self.record_fault(&mut state, source, page, write, 0xD),
            Ok(None) => self.record_fault(&mut state, source, page, write, 2),
            Err(reason) => self.record_fault(&mut state, source, page, write, reason),
        }
        None
    }

    fn access(&self, stream: u32, iova: u64, write: bool) -> Option<u64> {
        let mut state = self.state.lock();
        if state.gsts & regs::GSTS_TES == 0 {
            return Some(iova);
        }
        let source = u16::try_from(stream).ok()?;
        let page = iova & !(PAGE - 1);
        let cached = state.contexts.get(&source).copied();
        let context = match cached {
            Some(context) => context,
            None => match self.walk_context(&state, source) {
                Ok(context) => {
                    if context.is_some() || self.caching_mode() {
                        state.contexts.insert(source, context);
                    }
                    context
                }
                Err(reason) => {
                    self.record_fault(&mut state, source, page, write, reason);
                    return None;
                }
            },
        };
        let Some(context) = context else {
            self.record_fault(&mut state, source, page, write, 2);
            return None;
        };
        if context.silent {
            return None;
        }
        if iova >> (12 + 9 * context.levels) != 0 {
            self.record_fault(&mut state, source, page, write, 4);
            return None;
        }
        let key = (context.domain, page);
        let hit = state.iotlb.get(&key).copied();
        let translation = hit.unwrap_or_else(|| {
            let walked = self.walk_second_level(&context, page);
            if walked.is_some() || self.caching_mode() {
                state.iotlb.insert(key, walked);
            }
            walked
        });
        match translation {
            Some((phys, read, writable)) if (write && writable) || (!write && read) => {
                Some(phys + (iova - page))
            }
            _ => {
                let reason = if write { 5 } else { 6 };
                self.record_fault(&mut state, source, page, write, reason);
                None
            }
        }
    }
}
