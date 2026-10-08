//! A register-level model of one RISC-V IOMMU, written from the
//! specification rather than from the unit's code: its registers, the device
//! directory it walks in memory at one, two or three levels, first and second
//! stage walks at a 4 KiB granule, context and translation caches that keep
//! what they cached until a command removes it, the command queue it
//! consumes, and the fault queue it writes.

extern crate std;

use std::collections::BTreeMap;
use std::vec::Vec;

use tairix_kernel_iommu_api::conformance::TranslationProbe;
use tairix_kernel_iommu_api::hostmem::HostFrames;
use tairix_kernel_iommu_api::{IommuError, Registers};
use tairix_sync::SpinLock;

use crate::format::CONTEXT_WORDS;
use crate::regs;

const PAGE: u64 = 0x1000;
const PPN: u64 = ((1 << 44) - 1) << 10;

/// The opcodes the model answers.
pub(crate) mod op {
    pub const IOTINVAL: u8 = 1;
    pub const IOFENCE: u8 = 2;
    pub const IODIR: u8 = 3;
}

/// Fault causes.
pub(crate) mod cause {
    pub const DMA_DISABLED: u64 = 256;
    pub const DDT_INVALID: u64 = 258;
    /// Transaction type disallowed, which a device id wider than the
    /// directory's mode reaches also raises.
    pub const DISALLOWED: u64 = 260;
    pub const READ_PAGE: u64 = 13;
    pub const WRITE_PAGE: u64 = 15;
    pub const READ_GUEST_PAGE: u64 = 21;
    pub const WRITE_GUEST_PAGE: u64 = 23;
}

/// Transaction types.
const UNTRANSLATED_READ: u64 = 2;
const UNTRANSLATED_WRITE: u64 = 3;
const TRANSLATED_READ: u64 = 6;
const TRANSLATED_WRITE: u64 = 7;

/// What the modelled unit implements.
#[derive(Copy, Clone)]
pub(crate) struct Features {
    /// The second stage; the first otherwise.
    pub second_stage: bool,
    /// Which of the stage's three modes it walks.
    pub modes: [bool; 3],
    /// How far it translates messages.
    pub msi: Msi,
    /// Which directory depths `ddtp` takes, one to three levels.
    pub depths: [bool; 3],
    /// How it raises interrupts.
    pub interrupts: Interrupts,
    /// Physical address bits.
    pub physical_bits: u32,
    /// Performance counters.
    pub hpm: bool,
}

/// How far the modelled unit translates devices' messages.
#[derive(Copy, Clone, Eq, PartialEq)]
pub(crate) enum Msi {
    /// Not at all: base-format contexts.
    None,
    /// Through MSI page tables of flat entries alone: extended contexts.
    Flat,
    /// Through entries naming memory-resident interrupt files too.
    Files,
}

/// How the modelled unit raises interrupts.
#[derive(Copy, Clone)]
pub(crate) enum Interrupts {
    /// By message only.
    Messages,
    /// On wires only.
    Wired,
    /// Either, as `fctl.WSI` says.
    Both,
}

impl Features {
    fn capabilities(self) -> u64 {
        let shift = if self.second_stage { 17 } else { 9 };
        let modes = self
            .modes
            .iter()
            .enumerate()
            .filter(|(_, on)| **on)
            .fold(0, |bits, (mode, _)| bits | 1 << (shift + mode));
        let interrupts = match self.interrupts {
            Interrupts::Messages => 0b00,
            Interrupts::Wired => 0b01,
            Interrupts::Both => 0b10,
        };
        0x10 | modes
            | u64::from(self.msi != Msi::None) << 22
            | u64::from(self.msi == Msi::Files) << 23
            | interrupts << 28
            | u64::from(self.hpm) << 30
            | u64::from(self.physical_bits) << 32
    }
}

/// Ways the model can be told to misbehave.
#[derive(Copy, Clone, Default)]
pub(crate) struct Quirks {
    /// Reject the next command with this opcode as illegal.
    pub reject: Option<u8>,
    /// Never get past a fence.
    pub stall_fences: bool,
    /// Complete this many more fences, then stall at the next.
    pub fences_before_stall: Option<usize>,
    /// Access memory big-endian, whatever is written.
    pub big_endian: bool,
    /// Translate guest addresses as a 32-bit guest's, whatever is written.
    pub gxl_stuck: bool,
    /// The largest `LOG2SZ-1` a queue base takes.
    pub queue_log2_max: Option<u64>,
}

/// A cached translation: the physical page, and whether it may be read and
/// written.
type Translation = (u64, bool, bool);

/// What a translation cache entry is tagged by: the stage, and its id.
type Tag = (bool, u32);

struct State {
    words: BTreeMap<usize, u64>,
    fctl: u32,
    ddtp: u64,
    cqcsr: u32,
    fqcsr: u32,
    pqcsr: u32,
    ipsr: u32,
    cqh: u32,
    fqt: u32,
    quirks: Quirks,
    /// `fctl` writes made while the unit was not off or a queue ran.
    fctl_live: usize,
    /// What a firmware's translation left cached and no command has removed:
    /// contexts, and translations of either stage.
    stale_contexts: bool,
    stale_stages: [bool; 2],
    /// A directory was made live while something stale was cached.
    exposed: bool,
    /// Contexts as loaded, valid or not, by device id.
    contexts: BTreeMap<u32, [u64; CONTEXT_WORDS]>,
    /// Each device a command forgot the context of, and whether the context
    /// it would read next was valid.
    forgotten: Vec<(u32, bool)>,
    tlb: BTreeMap<(Tag, u64), Translation>,
    processed: BTreeMap<u8, usize>,
}

pub(crate) struct Model<'f> {
    frames: &'f HostFrames,
    features: Features,
    state: SpinLock<State>,
}

/// A context's walk: its tag, its root, its input bits and levels, how many
/// entries its root holds, and whether its faults are recorded.
struct Walk {
    tag: Tag,
    root: u64,
    input_bits: u32,
    levels: u32,
    root_entries: u64,
    record: bool,
}

impl<'f> Model<'f> {
    pub(crate) fn new(frames: &'f HostFrames, features: Features) -> Self {
        Self {
            frames,
            features,
            state: SpinLock::new(State {
                words: BTreeMap::new(),
                fctl: 0,
                ddtp: 0,
                cqcsr: 0,
                fqcsr: 0,
                pqcsr: 0,
                ipsr: 0,
                cqh: 0,
                fqt: 0,
                quirks: Quirks::default(),
                fctl_live: 0,
                stale_contexts: false,
                stale_stages: [false; 2],
                exposed: false,
                contexts: BTreeMap::new(),
                forgotten: Vec::new(),
                tlb: BTreeMap::new(),
                processed: BTreeMap::new(),
            }),
        }
    }

    pub(crate) fn quirk(&self, quirks: Quirks) {
        let mut state = self.state.lock();
        if quirks.big_endian {
            state.fctl |= regs::FCTL_BE;
        }
        if quirks.gxl_stuck {
            state.fctl |= regs::FCTL_GXL;
        }
        state.quirks = quirks;
    }

    /// Leave the model as firmware might: passing everything through with
    /// its queues on, interrupts pending, a command error standing, guest
    /// addresses translated as a 32-bit guest's, and its counters counting.
    pub(crate) fn firmware_left_running(&self) {
        let mut state = self.state.lock();
        state.fctl |= regs::FCTL_GXL;
        state.ddtp = 1;
        state.cqcsr = regs::QUEUE_EN | regs::QUEUE_ON | regs::CQCSR_CMD_ILL;
        state.fqcsr = regs::QUEUE_EN | regs::QUEUE_ON;
        state.pqcsr = regs::QUEUE_EN | regs::QUEUE_ON;
        state.ipsr = regs::IPSR_ALL;
    }

    /// Leave cached what a firmware's translation would have: a valid
    /// context for `device`, and a translation tagged `tag` at the stage the
    /// unit implements.
    pub(crate) fn firmware_cached(&self, device: u32, tag: u32) {
        let mut state = self.state.lock();
        let mut context = [0; CONTEXT_WORDS];
        context[0] = 1;
        context[1] = 8 << 60 | u64::from(tag) << 44;
        state.contexts.insert(device, context);
        let second = self.features.second_stage;
        state.tlb.insert(((second, tag), 0), (0, true, true));
        state.stale_contexts = true;
        state.stale_stages[usize::from(second)] = true;
    }

    /// Whether a directory was made live while firmware's cached contexts or
    /// translations could still be used.
    pub(crate) fn stale_exposed(&self) -> bool {
        self.state.lock().exposed
    }

    /// Whether the context `device`'s each forget left the unit to read next
    /// was valid, oldest first, emptying the record.
    pub(crate) fn take_forgotten(&self, device: u32) -> Vec<bool> {
        let mut state = self.state.lock();
        let (theirs, others) = state
            .forgotten
            .drain(..)
            .partition::<Vec<_>, _>(|&(forgot, _)| forgot == device);
        state.forgotten = others;
        theirs.into_iter().map(|(_, valid)| valid).collect()
    }

    /// `fctl` writes made while the unit was not off or a queue ran.
    pub(crate) fn fctl_written_live(&self) -> usize {
        self.state.lock().fctl_live
    }

    pub(crate) fn directory_mode(&self) -> u64 {
        self.state.lock().ddtp & regs::DDTP_MODE
    }

    pub(crate) fn interrupt_pending(&self) -> bool {
        self.state.lock().ipsr & regs::IPSR_FIP != 0
    }

    /// Whether the TLB holds the translation of the page at `iova` under
    /// `id` at the second stage or the first.
    pub(crate) fn caches(&self, second_stage: bool, id: u32, iova: u64) -> bool {
        self.state
            .lock()
            .tlb
            .contains_key(&((second_stage, id), iova & !0xFFF))
    }

    pub(crate) fn processed(&self, opcode: u8) -> usize {
        self.state
            .lock()
            .processed
            .get(&opcode)
            .copied()
            .unwrap_or(0)
    }

    pub(crate) fn word(&self, offset: usize) -> u64 {
        self.state.lock().words.get(&offset).copied().unwrap_or(0)
    }

    pub(crate) fn fctl(&self) -> u32 {
        self.state.lock().fctl
    }

    pub(crate) fn queue_controls(&self) -> [u32; 3] {
        let state = self.state.lock();
        [state.cqcsr, state.fqcsr, state.pqcsr]
    }

    /// Record `count` faults of `cause` from `device` at `iova`, as a device
    /// raising them would.
    pub(crate) fn raise(&self, count: usize, cause: u64, device: u32, iova: u64, write: bool) {
        let mut state = self.state.lock();
        let kind = if write {
            UNTRANSLATED_WRITE
        } else {
            UNTRANSLATED_READ
        };
        for _ in 0..count {
            self.record(&mut state, cause, kind, device, iova);
        }
    }

    fn word_of(state: &State, offset: usize) -> u64 {
        state.words.get(&offset).copied().unwrap_or(0)
    }

    fn queue_size(base: u64) -> u32 {
        1 << ((base & 0x1F) + 1)
    }

    fn queue_ring(base: u64) -> u64 {
        (base & PPN) << 2
    }

    /// Write one fault record as the unit does: dropped, with the overflow
    /// flag set, when the queue is full, and every one dropped until software
    /// clears the flag.
    fn record(&self, state: &mut State, cause: u64, kind: u64, device: u32, iova: u64) {
        if state.fqcsr & regs::QUEUE_EN == 0 || state.fqcsr & regs::FQCSR_FQOF != 0 {
            return;
        }
        let base = Self::word_of(state, regs::FQB);
        let size = Self::queue_size(base);
        let head = u32::try_from(Self::word_of(state, regs::FQH)).unwrap_or(0) % size;
        if (state.fqt + 1) % size == head {
            state.fqcsr |= regs::FQCSR_FQOF;
            return;
        }
        let at = Self::queue_ring(base) + u64::from(state.fqt) * 32;
        let header = cause | kind << 34 | u64::from(device) << 40;
        for (offset, value) in [header, 0, iova, 0].into_iter().enumerate() {
            self.frames.store_word(at + 8 * offset as u64, value);
        }
        state.fqt = (state.fqt + 1) % size;
        if state.fqcsr & regs::QUEUE_IE != 0 {
            state.ipsr |= regs::IPSR_FIP;
        }
    }

    fn run_queue(&self, state: &mut State) {
        if state.cqcsr & regs::QUEUE_EN == 0 {
            return;
        }
        let base = Self::word_of(state, regs::CQB);
        let size = Self::queue_size(base);
        let tail = u32::try_from(Self::word_of(state, regs::CQT)).unwrap_or(0) % size;
        // An error halts consumption until software acknowledges it.
        while state.cqcsr & regs::CQCSR_CMD_ILL == 0 && state.cqh != tail {
            let at = Self::queue_ring(base) + u64::from(state.cqh) * 16;
            let low = self.frames.word(at).unwrap_or(0);
            let high = self.frames.word(at + 8).unwrap_or(0);
            let opcode = (low & 0x7F) as u8;
            let function = (low >> 7) & 0b111;
            if state.quirks.reject == Some(opcode) {
                state.quirks.reject = None;
                state.cqcsr |= regs::CQCSR_CMD_ILL;
                return;
            }
            match (opcode, function) {
                // Unsupported, a stage the unit lacks halts the queue as an
                // illegal command does.
                (op::IOTINVAL, 0 | 1) if (function == 1) != self.features.second_stage => {
                    state.cqcsr |= regs::CQCSR_CMD_ILL;
                    return;
                }
                (op::IOTINVAL, 0 | 1) => {
                    let second_stage = function == 1;
                    let (named, id) = if second_stage {
                        (low & (1 << 33) != 0, ((low >> 44) & 0xFFFF) as u32)
                    } else {
                        (low & (1 << 32) != 0, ((low >> 12) & 0xF_FFFF) as u32)
                    };
                    if low & (1 << 10) != 0 {
                        let page = (high >> 10) << 12;
                        state.tlb.retain(|&((stage, tag), at), _| {
                            stage != second_stage || (named && tag != id) || at != page
                        });
                    } else {
                        state.tlb.retain(|&((stage, tag), _), _| {
                            stage != second_stage || (named && tag != id)
                        });
                        if !named {
                            state.stale_stages[usize::from(second_stage)] = false;
                        }
                    }
                }
                (op::IOFENCE, 0)
                    if state.quirks.stall_fences || state.quirks.fences_before_stall == Some(0) =>
                {
                    return;
                }
                (op::IOFENCE, 0) => {
                    if let Some(left) = state.quirks.fences_before_stall.as_mut() {
                        *left -= 1;
                    }
                    if low & (1 << 10) != 0 {
                        self.frames.store_word(high << 2, low >> 32);
                    }
                }
                (op::IODIR, 0) => {
                    if low & (1 << 33) != 0 {
                        let device = (low >> 40) as u32;
                        state.contexts.remove(&device);
                        let valid = self
                            .context(state, device)
                            .is_ok_and(|next| next[0] & 1 != 0);
                        state.contexts.remove(&device);
                        state.forgotten.push((device, valid));
                    } else {
                        state.contexts.clear();
                        state.stale_contexts = false;
                    }
                }
                _ => {
                    state.cqcsr |= regs::CQCSR_CMD_ILL;
                    return;
                }
            }
            *state.processed.entry(opcode).or_insert(0) += 1;
            state.cqh = (state.cqh + 1) % size;
        }
    }

    /// `device`'s context: the cached one, else read through the directory
    /// and cached, or the cause the directory walk faults with.
    fn context(&self, state: &mut State, device: u32) -> Result<[u64; CONTEXT_WORDS], u64> {
        if let Some(context) = state.contexts.get(&device) {
            return Ok(*context);
        }
        let extended = self.features.msi != Msi::None;
        let leaf_bits = if extended { 6 } else { 7 };
        let words: usize = if extended { 8 } else { 4 };
        let levels = match state.ddtp & regs::DDTP_MODE {
            2 => 1,
            3 => 2,
            _ => 3,
        };
        if u64::from(device) >> (leaf_bits + 9 * (levels - 1)).min(24) != 0 {
            return Err(cause::DISALLOWED);
        }
        let mut table = (state.ddtp & PPN) << 2;
        for level in (1..levels).rev() {
            let index = u64::from((device >> (leaf_bits + 9 * (level - 1))) & 0x1FF);
            let entry = self.frames.word(table + index * 8).unwrap_or(0);
            if entry & 1 == 0 {
                return Err(cause::DDT_INVALID);
            }
            table = (entry & PPN) << 2;
        }
        let at = table + u64::from(device & ((1 << leaf_bits) - 1)) * (words as u64) * 8;
        let mut context = [0u64; CONTEXT_WORDS];
        for (word, value) in context.iter_mut().take(words).enumerate() {
            *value = self.frames.word(at + 8 * word as u64).unwrap_or(0);
        }
        state.contexts.insert(device, context);
        Ok(context)
    }

    /// The walk `device`'s context configures, or how its access ends without
    /// one: passed through to `Err(Some(address))`, or refused and recorded.
    fn walk_of(
        &self,
        state: &mut State,
        device: u32,
        iova: u64,
        kind: u64,
    ) -> Result<Walk, Option<u64>> {
        match state.ddtp & regs::DDTP_MODE {
            0 => {
                self.record(state, cause::DMA_DISABLED, kind, device, 0);
                return Err(None);
            }
            1 => return Err(Some(iova)),
            _ => {}
        }
        let context = match self.context(state, device) {
            Ok(context) => context,
            Err(fault) => {
                self.record(state, fault, kind, device, 0);
                return Err(None);
            }
        };
        if context[0] & 1 == 0 {
            self.record(state, cause::DDT_INVALID, kind, device, 0);
            return Err(None);
        }
        let record = context[0] & (1 << 4) == 0;
        let (stage2, stage1) = (context[1] >> 60, context[3] >> 60);
        let levels = |mode: u64| u32::try_from(mode).ok().map(|mode| mode - 5);
        match (stage2, stage1) {
            (0, 0) => Err(Some(iova)),
            (8..=10, 0) => {
                let levels = levels(stage2).unwrap_or(3);
                Ok(Walk {
                    tag: (true, ((context[1] >> 44) & 0xFFFF) as u32),
                    root: (context[1] & ((1 << 44) - 1)) << 12,
                    input_bits: 12 + 9 * levels + 2,
                    levels,
                    root_entries: 2048,
                    record,
                })
            }
            (0, 8..=10) => {
                let levels = levels(stage1).unwrap_or(3);
                Ok(Walk {
                    tag: (false, ((context[2] >> 12) & 0xF_FFFF) as u32),
                    root: (context[3] & ((1 << 44) - 1)) << 12,
                    input_bits: 12 + 9 * levels,
                    levels,
                    root_entries: 512,
                    record,
                })
            }
            _ => {
                self.record(state, 259, kind, device, 0);
                Err(None)
            }
        }
    }

    /// Walk `walk`'s tables for `iova`: the page, and its read and write
    /// permission, or `Err` for a page fault.
    fn translate(&self, walk: &Walk, iova: u64, write: bool) -> Result<Translation, ()> {
        // A second-stage address is zero-extended; a first-stage one is
        // sign-extended from its top bit.
        let top = if walk.tag.0 {
            iova >> walk.input_bits
        } else {
            let upper = iova >> (walk.input_bits - 1);
            if upper == u64::MAX >> (walk.input_bits - 1) {
                0
            } else {
                upper
            }
        };
        if top != 0 {
            return Err(());
        }
        let mut table = walk.root;
        for level in (0..walk.levels).rev() {
            let shift = 12 + 9 * level;
            let entries = if level == walk.levels - 1 {
                walk.root_entries
            } else {
                512
            };
            let index = (iova >> shift) & (entries - 1);
            let entry = self.frames.word(table + index * 8).unwrap_or(0);
            if entry & 1 == 0 {
                return Err(());
            }
            let frame = (entry & PPN) << 2;
            if entry & 0b1110 == 0 {
                if level == 0 {
                    return Err(());
                }
                table = frame;
                continue;
            }
            let span = (1u64 << shift) - 1;
            // A superpage's frame must be aligned to its span; the leaf must
            // be reachable unprivileged and accessed, and dirty to be written.
            if frame & span != 0 || entry & (1 << 4) == 0 || entry & (1 << 6) == 0 {
                return Err(());
            }
            if write && entry & (1 << 7) == 0 {
                return Err(());
            }
            let page = frame | (iova & span & !(PAGE - 1));
            return Ok((page, entry & (1 << 1) != 0, entry & (1 << 2) != 0));
        }
        Err(())
    }

    fn page_fault(walk: &Walk, write: bool) -> u64 {
        match (walk.tag.0, write) {
            (true, false) => cause::READ_GUEST_PAGE,
            (true, true) => cause::WRITE_GUEST_PAGE,
            (false, false) => cause::READ_PAGE,
            (false, true) => cause::WRITE_PAGE,
        }
    }
}

impl Registers for &Model<'_> {
    fn read32(&self, offset: usize) -> Result<u32, IommuError> {
        let state = self.state.lock();
        Ok(match offset {
            regs::FCTL => state.fctl,
            regs::CQH => state.cqh,
            regs::FQT => state.fqt,
            regs::CQCSR => state.cqcsr,
            regs::FQCSR => state.fqcsr,
            regs::PQCSR => state.pqcsr,
            regs::IPSR => state.ipsr,
            _ => u32::try_from(Model::word_of(&state, offset)).unwrap_or(u32::MAX),
        })
    }

    fn write32(&self, offset: usize, value: u32) -> Result<(), IommuError> {
        let mut state = self.state.lock();
        let errors = regs::CQCSR_ERRORS | regs::FQCSR_ERRORS;
        let queue = |now: u32, value: u32| {
            let running = if value & regs::QUEUE_EN != 0 {
                regs::QUEUE_ON
            } else {
                0
            };
            (value & (regs::QUEUE_EN | regs::QUEUE_IE)) | running | (now & errors & !value)
        };
        match offset {
            regs::FCTL => {
                let queues = [state.cqcsr, state.fqcsr, state.pqcsr];
                if state.ddtp & regs::DDTP_MODE != 0
                    || queues
                        .iter()
                        .any(|control| control & (regs::QUEUE_EN | regs::QUEUE_ON) != 0)
                {
                    // Unspecified by the architecture: ignored here, and counted.
                    state.fctl_live += 1;
                    return Ok(());
                }
                let mut fctl = value & (regs::FCTL_BE | regs::FCTL_WSI | regs::FCTL_GXL);
                if state.quirks.big_endian {
                    fctl |= regs::FCTL_BE;
                }
                if state.quirks.gxl_stuck {
                    fctl |= regs::FCTL_GXL;
                }
                match self.features.interrupts {
                    Interrupts::Messages => fctl &= !regs::FCTL_WSI,
                    Interrupts::Wired => fctl |= regs::FCTL_WSI,
                    Interrupts::Both => {}
                }
                state.fctl = fctl;
            }
            regs::IOCOUNTINH if !self.features.hpm => {}
            regs::CQCSR => {
                if value & regs::QUEUE_EN != 0 && state.cqcsr & regs::QUEUE_EN == 0 {
                    state.cqh = 0;
                }
                state.cqcsr = queue(state.cqcsr, value);
                self.run_queue(&mut state);
            }
            regs::FQCSR => {
                if value & regs::QUEUE_EN != 0 && state.fqcsr & regs::QUEUE_EN == 0 {
                    state.fqt = 0;
                }
                state.fqcsr = queue(state.fqcsr, value);
            }
            regs::PQCSR => state.pqcsr = queue(state.pqcsr, value),
            regs::IPSR => state.ipsr &= !value,
            regs::CQT => {
                state.words.insert(offset, u64::from(value));
                self.run_queue(&mut state);
            }
            _ => {
                state.words.insert(offset, u64::from(value));
            }
        }
        Ok(())
    }

    fn read64(&self, offset: usize) -> Result<u64, IommuError> {
        let state = self.state.lock();
        Ok(match offset {
            regs::CAPABILITIES => self.features.capabilities(),
            regs::DDTP => state.ddtp,
            _ => Model::word_of(&state, offset),
        })
    }

    fn write64(&self, offset: usize, value: u64) -> Result<(), IommuError> {
        let mut state = self.state.lock();
        match offset {
            regs::DDTP => {
                let mode = value & regs::DDTP_MODE;
                let taken = match mode {
                    0 | 1 => true,
                    2 => self.features.depths[0],
                    3 => self.features.depths[1],
                    4 => self.features.depths[2],
                    _ => false,
                };
                if taken {
                    state.ddtp = value & (PPN | regs::DDTP_MODE);
                    if mode >= 2 && (state.stale_contexts || state.stale_stages.contains(&true)) {
                        state.exposed = true;
                    }
                }
            }
            regs::CQB | regs::FQB => {
                let value = match state.quirks.queue_log2_max {
                    Some(max) if value & 0x1F > max => value & !0x1F | max,
                    _ => value,
                };
                state.words.insert(offset, value);
            }
            regs::ICVEC => {
                state.words.insert(offset, value & 0xFFFF);
            }
            _ => {
                state.words.insert(offset, value);
            }
        }
        Ok(())
    }

    fn window_len(&self) -> usize {
        regs::WINDOW
    }
}

impl Model<'_> {
    /// A message `stream` writes to `address` with identity `data`: where its
    /// context confines it into a file, the file's pending bit is set — with
    /// a read and a write, as a unit without atomic files does — and the
    /// notice the unit then sends is answered, [`None`] where the file has
    /// the identity disabled. `Err` for a write that is no confined message.
    pub(crate) fn message(
        &self,
        stream: u32,
        address: u64,
        data: u32,
    ) -> Result<Option<(u64, u32)>, ()> {
        let context = {
            let mut state = self.state.lock();
            self.context(&mut state, stream).map_err(|_| ())?
        };
        let (msiptp, pattern) = (context[4], context[6]);
        let valid = context[0] & 1 != 0;
        if !valid
            || msiptp >> 60 != 1
            || address & (PAGE - 1) != 0
            || address >> 12 != pattern
            || data > 2047
        {
            return Err(());
        }
        let table = (msiptp & ((1 << 44) - 1)) << 12;
        let (entry, notice) = (
            self.frames.word(table).ok_or(())?,
            self.frames.word(table + 8).ok_or(())?,
        );
        if entry & 1 == 0 || (entry >> 1) & 0b11 != 1 {
            return Err(());
        }
        let pending = (((entry >> 7) & ((1 << 47) - 1)) << 9) + u64::from(data / 64) * 16;
        let bit = 1 << (data % 64);
        let was = self.frames.word(pending).ok_or(())?;
        self.frames.store_word(pending, was | bit);
        let enabled = self.frames.word(pending + 8).ok_or(())? & bit != 0;
        let identity =
            u32::try_from((notice & 0x3FF) | ((notice >> 60) & 1) << 10).map_err(|_| ())?;
        let page = ((notice >> 10) & ((1 << 44) - 1)) << 12;
        Ok(enabled.then_some((page, identity)))
    }
}

impl TranslationProbe for Model<'_> {
    fn access(&self, stream: u32, iova: u64, write: bool) -> Option<u64> {
        let mut state = self.state.lock();
        let kind = if write {
            UNTRANSLATED_WRITE
        } else {
            UNTRANSLATED_READ
        };
        let walk = match self.walk_of(&mut state, stream, iova, kind) {
            Ok(walk) => walk,
            Err(through) => return through,
        };
        let page = iova & !(PAGE - 1);
        let cached = state.tlb.get(&(walk.tag, page)).copied();
        let translated = match cached {
            Some(hit) => Ok(hit),
            None => self.translate(&walk, iova, write),
        };
        if let Ok((frame, read, writable)) = translated {
            state.tlb.insert((walk.tag, page), (frame, read, writable));
            if (write && writable) || (!write && read) {
                return Some(frame | (iova & (PAGE - 1)));
            }
        }
        if walk.record {
            self.record(
                &mut state,
                Model::page_fault(&walk, write),
                kind,
                stream,
                iova,
            );
        }
        None
    }

    /// A translated request is refused for every context, none enabling ATS.
    fn translated(&self, stream: u32, address: u64, write: bool) -> Option<u64> {
        let mut state = self.state.lock();
        let kind = if write {
            TRANSLATED_WRITE
        } else {
            TRANSLATED_READ
        };
        if let Err(through) = self.walk_of(&mut state, stream, address, kind) {
            return through;
        }
        self.record(&mut state, cause::DISALLOWED, kind, stream, address);
        None
    }
}
