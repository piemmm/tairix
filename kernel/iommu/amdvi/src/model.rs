//! A register-level model of one AMD-Vi unit, written from the specification
//! rather than from the unit's code: its register file, the device table and
//! v1 page-table walks it performs in memory, a device table cache, an IOTLB
//! and a remapping entry cache that keep what they cached until a command
//! removes it, the command buffer it fetches from memory, and its event log.

extern crate std;

use std::collections::BTreeMap;

use tairix_kernel_iommu_api::conformance::{InterruptProbe, TranslationProbe};
use tairix_kernel_iommu_api::hostmem::HostFrames;
use tairix_kernel_iommu_api::{InterruptTarget, IommuError, Registers};
use tairix_sync::SpinLock;

use crate::regs;

const ADDRESS: u64 = 0x000F_FFFF_FFFF_F000;
const PAGE: u64 = 0x1000;
const READ: u64 = 1 << 61;
const WRITE: u64 = 1 << 62;

/// `INVALIDATE_IOMMU_ALL`, in the extended feature register.
pub(crate) const FEATURE_INVALIDATE_ALL: u64 = 1 << 6;
/// XT and GA: 32-bit destinations through 128-bit entries.
pub(crate) const FEATURE_EXTENDED: u64 = (1 << 2) | (1 << 7);
/// No host translation.
pub(crate) const FEATURE_NO_HOST: u64 = 0b11 << 10;

pub(crate) const EVENT_ILLEGAL_DEVICE: u64 = 1;
pub(crate) const EVENT_PAGE_FAULT: u64 = 2;
pub(crate) const EVENT_ILLEGAL_COMMAND: u64 = 5;
pub(crate) const EVENT_INVALID_REQUEST: u64 = 8;
const FLAG_INTERRUPT: u64 = 1 << 3;
const FLAG_WRITE: u64 = 1 << 5;
const FLAG_PERMISSION: u64 = 1 << 6;
/// An invalid-request event's type: a pretranslated transaction from a device
/// whose entry does not let it translate.
const INVALID_PRETRANSLATED: u64 = 0b001 << 9;

pub(crate) const OPCODE_PAGES: u64 = 3;
pub(crate) const OPCODE_DEVICE: u64 = 2;
pub(crate) const OPCODE_INTERRUPTS: u64 = 5;
pub(crate) const OPCODE_ALL: u64 = 8;

/// A cached translation: the physical page, and whether it may be read and
/// written.
type Translation = (u64, bool, bool);

/// Ways the model can be told to misbehave.
#[derive(Copy, Clone, Default)]
pub(crate) struct Quirks {
    /// Reject the next command of this opcode as illegal.
    pub reject_next: Option<u64>,
    /// Never store a completion wait's token.
    pub ignore_waits: bool,
    /// Lose every device table entry invalidation, keeping what was cached.
    pub lose_device_invalidations: bool,
}

/// What kind of unit the model is, beyond the specification's minimum.
#[derive(Copy, Clone, Default)]
pub(crate) struct Kind {
    /// Run commands only while translation is on, as QEMU does.
    pub commands_need_translation: bool,
    /// Cache translations found absent until a page invalidation removes
    /// them.
    pub caches_misses: bool,
}

struct State {
    regs: BTreeMap<usize, u64>,
    command_head: usize,
    event_tail: usize,
    halted: bool,
    quirks: Quirks,
    devices: BTreeMap<u16, [u64; 4]>,
    iotlb: BTreeMap<(u16, u64), Option<Translation>>,
    entries: BTreeMap<(u16, u64), [u64; 2]>,
    /// Commands run, by opcode.
    processed: BTreeMap<u64, usize>,
}

pub(crate) struct Model<'f> {
    frames: &'f HostFrames,
    features: u64,
    kind: Kind,
    state: SpinLock<State>,
}

fn field(word: u64, shift: u32, width: u32) -> u64 {
    (word >> shift) & ((1 << width) - 1)
}

fn low16(word: u64) -> u16 {
    let [low, high, ..] = word.to_le_bytes();
    u16::from_le_bytes([low, high])
}

impl<'f> Model<'f> {
    /// A unit with four-level host translation and `features` beside.
    pub(crate) fn new(frames: &'f HostFrames, features: u64) -> Self {
        Self::of_kind(frames, features, Kind::default())
    }

    /// [`Self::new`], of `kind`.
    pub(crate) fn of_kind(frames: &'f HostFrames, features: u64, kind: Kind) -> Self {
        Self {
            frames,
            features,
            kind,
            state: SpinLock::new(State {
                regs: BTreeMap::new(),
                command_head: 0,
                event_tail: 0,
                halted: false,
                quirks: Quirks::default(),
                devices: BTreeMap::new(),
                iotlb: BTreeMap::new(),
                entries: BTreeMap::new(),
                processed: BTreeMap::new(),
            }),
        }
    }

    pub(crate) fn quirk(&self, quirks: Quirks) {
        self.state.lock().quirks = quirks;
    }

    /// Whether the IOTLB holds `domain`'s translation of the page at `iova`.
    pub(crate) fn caches(&self, domain: u16, iova: u64) -> bool {
        self.state
            .lock()
            .iotlb
            .get(&(domain, iova & !(PAGE - 1)))
            .is_some_and(Option::is_some)
    }

    /// Leave the model as firmware might: translating through a device table
    /// of its own, its command buffer and event log on, an exclusion range
    /// passing every device, and an overflow and an interrupt standing.
    pub(crate) fn firmware_left_running(&self) {
        let mut state = self.state.lock();
        state.regs.insert(regs::DEVICE_TABLE, 0xDEAD_0000);
        state.regs.insert(regs::EXCLUSION_BASE, 0x8000_0000 | 0b11);
        state.regs.insert(regs::EXCLUSION_LIMIT, 0x9000_0000);
        state.regs.insert(
            regs::CONTROL,
            regs::CONTROL_IOMMU | regs::CONTROL_COMMANDS | regs::CONTROL_EVENT_LOG,
        );
        state.regs.insert(
            regs::STATUS,
            regs::STATUS_EVENT_OVERFLOW | regs::STATUS_EVENT_INTERRUPT,
        );
    }

    pub(crate) fn register(&self, offset: usize) -> u64 {
        self.read(offset)
    }

    pub(crate) fn processed(&self, opcode: u64) -> usize {
        self.state
            .lock()
            .processed
            .get(&opcode)
            .copied()
            .unwrap_or(0)
    }

    pub(crate) fn translating(&self) -> bool {
        Self::control(&self.state.lock()) & regs::CONTROL_IOMMU != 0
    }

    /// Device `device`'s entry as it stands in memory.
    pub(crate) fn device_entry(&self, device: u16) -> Option<[u64; 4]> {
        let state = self.state.lock();
        self.walk_device(&state, device)
    }

    /// Log `count` records naming `device`, as faults arriving would.
    pub(crate) fn raise_events(&self, device: u16, count: usize) {
        let mut state = self.state.lock();
        for page in 0..count as u64 {
            self.log_event(&mut state, EVENT_PAGE_FAULT, device, 0, 0, page * PAGE);
        }
    }

    /// Move the event log's tail past a record that never lands, as a unit
    /// moving its tail before its record is written does.
    pub(crate) fn lose_next_record(&self) {
        let mut state = self.state.lock();
        let slots = Self::ring_slots(state.regs.get(&regs::EVENT_LOG).copied().unwrap_or(0));
        state.event_tail = (state.event_tail + 1) % slots;
    }

    fn control(state: &State) -> u64 {
        state.regs.get(&regs::CONTROL).copied().unwrap_or(0)
    }

    fn ring_slots(register: u64) -> usize {
        1 << field(register, 56, 4)
    }

    fn commands_run(&self, state: &State) -> bool {
        let control = Self::control(state);
        let gate = !self.kind.commands_need_translation || control & regs::CONTROL_IOMMU != 0;
        control & regs::CONTROL_COMMANDS != 0 && gate && !state.halted
    }

    fn status(&self, state: &State) -> u64 {
        let control = Self::control(state);
        let mut status = state.regs.get(&regs::STATUS).copied().unwrap_or(0) & regs::STATUS_CLEAR;
        if self.commands_run(state) {
            status |= regs::STATUS_COMMANDS_RUNNING;
        }
        if control & regs::CONTROL_EVENT_LOG != 0 && status & regs::STATUS_EVENT_OVERFLOW == 0 {
            status |= regs::STATUS_EVENT_LOG_RUNNING;
        }
        status
    }

    fn set_status(state: &mut State, bits: u64) {
        let status = state.regs.entry(regs::STATUS).or_insert(0);
        *status |= bits;
    }

    /// Write one event record as AMD rev. 3.08 §2.5 does: nothing while logging
    /// is off or an overflow stands, an overflow when the log is full.
    fn log_event(
        &self,
        state: &mut State,
        code: u64,
        device: u16,
        domain: u16,
        flags: u64,
        address: u64,
    ) {
        let control = Self::control(state);
        if control & regs::CONTROL_EVENT_LOG == 0
            || self.status(state) & regs::STATUS_EVENT_OVERFLOW != 0
        {
            return;
        }
        let log = state.regs.get(&regs::EVENT_LOG).copied().unwrap_or(0);
        let slots = Self::ring_slots(log);
        let head = regs::ring_index(
            state.regs.get(&regs::EVENT_HEAD).copied().unwrap_or(0),
            slots,
        );
        let next = (state.event_tail + 1) % slots;
        if next == head {
            Self::set_status(
                state,
                regs::STATUS_EVENT_OVERFLOW | regs::STATUS_EVENT_INTERRUPT,
            );
            return;
        }
        let at = (log & ADDRESS) + 16 * state.event_tail as u64;
        self.frames.store_word(
            at,
            u64::from(device) | (u64::from(domain) << 32) | (flags << 48) | (code << 60),
        );
        self.frames.store_word(at + 8, address);
        state.event_tail = next;
        Self::set_status(state, regs::STATUS_EVENT_INTERRUPT);
    }

    fn halt(&self, state: &mut State, at: u64) {
        state.halted = true;
        self.log_event(state, EVENT_ILLEGAL_COMMAND, 0, 0, 0, at);
    }

    fn run_commands(&self, state: &mut State) {
        let buffer = state.regs.get(&regs::COMMAND_BUFFER).copied().unwrap_or(0);
        let slots = Self::ring_slots(buffer);
        let tail = regs::ring_index(
            state.regs.get(&regs::COMMAND_TAIL).copied().unwrap_or(0),
            slots,
        );
        while state.command_head != tail && self.commands_run(state) {
            let at = (buffer & ADDRESS) + 16 * state.command_head as u64;
            let low = self.frames.word(at).unwrap_or(0);
            let high = self.frames.word(at + 8).unwrap_or(0);
            let opcode = low >> 60;
            if state.quirks.reject_next == Some(opcode) {
                state.quirks.reject_next = None;
                self.halt(state, at);
                return;
            }
            match opcode {
                1 => {
                    if low & 1 != 0 && !state.quirks.ignore_waits {
                        let store = (low & 0xFFFF_FFF8) | (field(low, 32, 20) << 32);
                        self.frames.store_word(store, high);
                    }
                }
                OPCODE_DEVICE => {
                    if !state.quirks.lose_device_invalidations {
                        state.devices.remove(&low16(low));
                    }
                }
                OPCODE_PAGES => {
                    let domain = low16(low >> 32);
                    let (base, size) = if high & 1 == 0 {
                        (high & ADDRESS, PAGE)
                    } else {
                        let ones = (high >> 12).trailing_ones();
                        let size = 1u64.checked_shl(12 + ones + 1).unwrap_or(0);
                        (high & ADDRESS & !size.wrapping_sub(1), size)
                    };
                    state.iotlb.retain(|&(cached, page), _| {
                        cached != domain || page < base || (size != 0 && page - base >= size)
                    });
                }
                OPCODE_INTERRUPTS => {
                    let device = low16(low);
                    state.entries.retain(|&(cached, _), _| cached != device);
                }
                OPCODE_ALL if self.features & FEATURE_INVALIDATE_ALL != 0 => {
                    state.devices.clear();
                    state.iotlb.clear();
                    state.entries.clear();
                }
                _ => {
                    self.halt(state, at);
                    return;
                }
            }
            *state.processed.entry(opcode).or_insert(0) += 1;
            state.command_head = (state.command_head + 1) % slots;
        }
    }

    fn walk_device(&self, state: &State, device: u16) -> Option<[u64; 4]> {
        let table = state.regs.get(&regs::DEVICE_TABLE).copied().unwrap_or(0);
        let entries = (field(table, 0, 9) + 1) * PAGE / 32;
        if u64::from(device) >= entries {
            return None;
        }
        let at = (table & ADDRESS) + 32 * u64::from(device);
        let mut entry = [0; 4];
        for (index, word) in entry.iter_mut().enumerate() {
            *word = self.frames.word(at + 8 * index as u64)?;
        }
        Some(entry)
    }

    /// Device `device`'s entry, from the cache once read.
    fn device(&self, state: &mut State, device: u16) -> Option<[u64; 4]> {
        if let Some(&cached) = state.devices.get(&device) {
            return Some(cached);
        }
        let entry = self.walk_device(state, device)?;
        state.devices.insert(device, entry);
        Some(entry)
    }

    /// Walk `levels` of v1 tables from `root`: a directory names the level
    /// of the table it points at, a leaf level 0, and access is what every
    /// entry on the way allows.
    fn walk_pages(&self, root: u64, levels: u32, iova: u64) -> Option<Translation> {
        let mut table = root;
        let mut level = levels;
        let (mut read, mut write) = (true, true);
        while level > 0 {
            let shift = 12 + 9 * (level - 1);
            let index = usize::try_from((iova >> shift) & 0x1FF).ok()?;
            let entry = self.frames.entry(table, index)?;
            if entry & 1 == 0 {
                return None;
            }
            read &= entry & READ != 0;
            write &= entry & WRITE != 0;
            match field(entry, 9, 3) {
                0 => {
                    let span = (1u64 << shift) - 1;
                    return Some(((entry & ADDRESS & !span) | (iova & span), read, write));
                }
                next if next == u64::from(level - 1) => {
                    table = entry & ADDRESS;
                    level -= 1;
                }
                _ => return None,
            }
        }
        None
    }

    fn page_fault(
        &self,
        state: &mut State,
        entry: [u64; 4],
        device: u16,
        flags: u64,
        address: u64,
    ) {
        // SA: no page fault is recorded for the device.
        if entry[1] & (1 << 34) == 0 {
            self.log_event(
                state,
                EVENT_PAGE_FAULT,
                device,
                low16(entry[1]),
                flags,
                address,
            );
        }
    }

    fn write(&self, offset: usize, value: u64) {
        let mut state = self.state.lock();
        match offset {
            regs::CONTROL => {
                let before = self.commands_run(&state);
                state.regs.insert(offset, value);
                if value & regs::CONTROL_COMMANDS == 0 {
                    state.halted = false;
                }
                if !before {
                    self.run_commands(&mut state);
                }
            }
            regs::COMMAND_TAIL => {
                state.regs.insert(offset, value);
                self.run_commands(&mut state);
            }
            regs::COMMAND_HEAD => {
                let slots =
                    Self::ring_slots(state.regs.get(&regs::COMMAND_BUFFER).copied().unwrap_or(0));
                state.command_head = regs::ring_index(value, slots);
            }
            regs::EVENT_TAIL => {
                let slots =
                    Self::ring_slots(state.regs.get(&regs::EVENT_LOG).copied().unwrap_or(0));
                state.event_tail = regs::ring_index(value, slots);
            }
            regs::STATUS => {
                let status = state.regs.entry(regs::STATUS).or_insert(0);
                *status &= !(value & regs::STATUS_CLEAR);
            }
            _ => {
                state.regs.insert(offset, value);
            }
        }
    }

    fn read(&self, offset: usize) -> u64 {
        let state = self.state.lock();
        match offset {
            regs::FEATURES => self.features,
            regs::STATUS => self.status(&state),
            regs::COMMAND_HEAD => regs::ring_offset(state.command_head),
            regs::EVENT_TAIL => regs::ring_offset(state.event_tail),
            _ => state.regs.get(&offset).copied().unwrap_or(0),
        }
    }

    /// Remapping entry `index` of the table `interrupts` names, from the cache
    /// once read: 128 bits each in guest-APIC mode, 32 otherwise.
    fn remapping_entry(
        &self,
        state: &mut State,
        device: u16,
        interrupts: u64,
        index: u64,
    ) -> Option<[u64; 2]> {
        if let Some(&cached) = state.entries.get(&(device, index)) {
            return Some(cached);
        }
        let table = interrupts & 0x000F_FFFF_FFFF_FFC0;
        let entry = if Self::control(state) & regs::CONTROL_GUEST_APIC != 0 {
            let at = table + 16 * index;
            [self.frames.word(at)?, self.frames.word(at + 8)?]
        } else {
            let word = self.frames.word(table + 8 * (index / 2))?;
            [(word >> (32 * (index % 2))) & 0xFFFF_FFFF, 0]
        };
        state.entries.insert((device, index), entry);
        Some(entry)
    }
}

impl Registers for &Model<'_> {
    fn read32(&self, offset: usize) -> Result<u32, IommuError> {
        let _ = offset;
        Err(IommuError::Hardware)
    }

    fn write32(&self, offset: usize, value: u32) -> Result<(), IommuError> {
        let _ = (offset, value);
        Err(IommuError::Hardware)
    }

    fn read64(&self, offset: usize) -> Result<u64, IommuError> {
        if offset + 8 > regs::WINDOW || !offset.is_multiple_of(8) {
            return Err(IommuError::Hardware);
        }
        Ok(self.read(offset))
    }

    fn write64(&self, offset: usize, value: u64) -> Result<(), IommuError> {
        if offset + 8 > regs::WINDOW || !offset.is_multiple_of(8) {
            return Err(IommuError::Hardware);
        }
        self.write(offset, value);
        Ok(())
    }

    fn window_len(&self) -> usize {
        regs::WINDOW
    }
}

impl TranslationProbe for Model<'_> {
    fn access(&self, stream: u32, iova: u64, write: bool) -> Option<u64> {
        let mut state = self.state.lock();
        if Self::control(&state) & regs::CONTROL_IOMMU == 0 {
            return Some(iova);
        }
        let device = u16::try_from(stream).ok()?;
        let page = iova & !(PAGE - 1);
        let rw = if write { FLAG_WRITE } else { 0 };
        let Some(entry) = self.device(&mut state, device) else {
            self.log_event(&mut state, EVENT_ILLEGAL_DEVICE, device, 0, rw, page);
            return None;
        };
        if entry[0] & 1 == 0 {
            return Some(iova);
        }
        if entry[0] & 0b10 == 0 {
            self.page_fault(&mut state, entry, device, rw, page);
            return None;
        }
        let levels = u32::try_from(field(entry[0], 9, 3)).ok()?;
        let (dte_read, dte_write) = (entry[0] & READ != 0, entry[0] & WRITE != 0);
        if levels == 0 || levels == 7 {
            // Mode 0 passes untranslated under IR and IW; this family never
            // writes it, and 7 is reserved.
            self.log_event(
                &mut state,
                EVENT_ILLEGAL_DEVICE,
                device,
                low16(entry[1]),
                rw,
                page,
            );
            return None;
        }
        if iova >> (12 + 9 * levels) != 0 {
            self.page_fault(&mut state, entry, device, rw, page);
            return None;
        }
        let domain = low16(entry[1]);
        let key = (domain, page);
        let cached = state.iotlb.get(&key).copied();
        let translation = cached.unwrap_or_else(|| {
            let walked = self.walk_pages(entry[0] & ADDRESS, levels, page);
            if walked.is_some() || self.kind.caches_misses {
                state.iotlb.insert(key, walked);
            }
            walked
        });
        let Some((phys, read, writable)) = translation else {
            self.page_fault(&mut state, entry, device, rw, page);
            return None;
        };
        if (write && writable && dte_write) || (!write && read && dte_read) {
            Some(phys + (iova - page))
        } else {
            self.page_fault(&mut state, entry, device, rw | FLAG_PERMISSION, page);
            None
        }
    }

    /// A translated request from a device whose entry leaves the IOTLB off
    /// is an invalid device request.
    fn translated(&self, stream: u32, address: u64, write: bool) -> Option<u64> {
        let mut state = self.state.lock();
        if Self::control(&state) & regs::CONTROL_IOMMU == 0 {
            return Some(address);
        }
        let device = u16::try_from(stream).ok()?;
        let rw = if write { FLAG_WRITE } else { 0 };
        let entry = self.device(&mut state, device);
        if entry.is_none_or(|entry| entry[1] & (1 << 32) == 0) {
            self.log_event(
                &mut state,
                EVENT_INVALID_REQUEST,
                device,
                0,
                rw | INVALID_PRETRANSLATED,
                address & !(PAGE - 1),
            );
        }
        None
    }
}

/// An interrupt request as rev. 3.08 §2.2.5 handles it: untouched while the
/// unit is off or the device's entry leaves interrupts unmapped; with
/// remapping control `0b10`, through entry `data[10:0]` of the device's
/// table, which must lie inside it and be enabled; refused, with a page
/// fault naming the message address, otherwise.
impl InterruptProbe for Model<'_> {
    fn interrupt(&self, source: u16, address: u64, data: u32) -> Option<InterruptTarget> {
        let mut state = self.state.lock();
        let level = data & (1 << 15) != 0;
        let passed = InterruptTarget {
            vector: data.to_le_bytes()[0],
            destination: u32::try_from(field(address, 12, 8)).unwrap_or(0),
            level,
        };
        if Self::control(&state) & regs::CONTROL_IOMMU == 0 {
            return Some(passed);
        }
        let entry = self.device(&mut state, source)?;
        let interrupts = entry[2];
        if entry[0] & 1 == 0 || interrupts & 1 == 0 {
            return Some(passed);
        }
        let refuse = |model: &Self, state: &mut State| {
            // IG: unmapped interrupts are dropped unrecorded.
            if interrupts & (1 << 5) == 0 {
                model.log_event(
                    state,
                    EVENT_PAGE_FAULT,
                    source,
                    low16(entry[1]),
                    FLAG_INTERRUPT | FLAG_WRITE,
                    address,
                );
            }
            None
        };
        match field(interrupts, 60, 2) {
            0b01 => return Some(passed),
            0b10 => {}
            _ => return refuse(self, &mut state),
        }
        let index = u64::from(data & 0x7FF);
        if index >= 1 << field(interrupts, 1, 4) {
            return refuse(self, &mut state);
        }
        let Some([low, high]) = self.remapping_entry(&mut state, source, interrupts, index) else {
            return refuse(self, &mut state);
        };
        if low & 1 == 0 || field(low, 2, 3) > 1 || low & (1 << 7) != 0 {
            return refuse(self, &mut state);
        }
        let control = Self::control(&state);
        let (vector, destination) = if control & regs::CONTROL_GUEST_APIC != 0 {
            let low_destination = field(low, 8, 24);
            let destination = if control & regs::CONTROL_X2APIC != 0 {
                low_destination | (field(high, 56, 8) << 24)
            } else {
                low_destination & 0xFF
            };
            (field(high, 0, 8), destination)
        } else {
            (field(low, 16, 8), field(low, 8, 8))
        };
        Some(InterruptTarget {
            vector: u8::try_from(vector).ok()?,
            destination: u32::try_from(destination).ok()?,
            level,
        })
    }
}
