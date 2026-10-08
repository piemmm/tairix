//! A register-level model of one `SMMUv3`, written from the specification
//! rather than from the unit's code: its registers, the stream table it walks
//! in memory (linear or two-level), stage 1 and stage 2 table walks at a 4 KiB
//! granule, configuration and TLB caches that keep what they cached until a
//! command removes it, the command queue it consumes, and the event queue it
//! writes.

extern crate std;

use std::collections::BTreeMap;

use tairix_kernel_iommu_api::conformance::TranslationProbe;
use tairix_kernel_iommu_api::hostmem::HostFrames;
use tairix_kernel_iommu_api::{IommuError, Registers};
use tairix_sync::SpinLock;

use crate::regs;

const ADDRESS_51_6: u64 = 0x000F_FFFF_FFFF_FFC0;
const ADDRESS_51_5: u64 = 0x000F_FFFF_FFFF_FFE0;
const ADDRESS_51_4: u64 = 0x000F_FFFF_FFFF_FFF0;
const ADDRESS_51_2: u64 = 0x000F_FFFF_FFFF_FFFC;
const ADDRESS_47_12: u64 = 0x0000_FFFF_FFFF_F000;
const PAGE: u64 = 0x1000;
/// An event record's `RnW`: the access was a read.
const EVENT_READ: u64 = 1 << 35;
/// `SMMU_CMDQ_CONS.ERR`: an illegal command.
const CERROR_ILL: u32 = 1 << 24;

/// The opcodes and event types the model answers.
pub(crate) mod op {
    pub const CFGI_STE: u8 = 0x03;
    pub const CFGI_STE_RANGE: u8 = 0x04;
    pub const CFGI_CD: u8 = 0x05;
    pub const CFGI_CD_ALL: u8 = 0x06;
    pub const TLBI_NH_ASID: u8 = 0x11;
    pub const TLBI_NH_VA: u8 = 0x12;
    pub const TLBI_S12_VMALL: u8 = 0x28;
    pub const TLBI_S2_IPA: u8 = 0x2A;
    pub const TLBI_NSNH_ALL: u8 = 0x30;
    pub const SYNC: u8 = 0x46;
}

/// Event types.
pub(crate) mod event {
    pub const UNSUPPORTED: u64 = 0x01;
    pub const BAD_STREAMID: u64 = 0x02;
    pub const BAD_STE: u64 = 0x04;
    pub const TRANSL_FORBIDDEN: u64 = 0x07;
    pub const BAD_CD: u64 = 0x0A;
    pub const TRANSLATION: u64 = 0x10;
    pub const ADDR_SIZE: u64 = 0x11;
    pub const ACCESS: u64 = 0x12;
    pub const PERMISSION: u64 = 0x13;
}

/// What the modelled unit implements.
#[derive(Copy, Clone)]
pub(crate) struct Features {
    /// Stage 2; stage 1 otherwise.
    pub stage2: bool,
    /// Message-signalled `CMD_SYNC` completions.
    pub msi: bool,
    /// Two-level stream tables.
    pub two_level: bool,
    /// Stream-id bits.
    pub stream_bits: u32,
    /// The output size's encoding.
    pub output: u32,
}

impl Features {
    fn idr0(self) -> u32 {
        let stage = if self.stage2 { 1 } else { 1 << 1 };
        let two_level = if self.two_level { 0b01 << 27 } else { 0 };
        // AArch64 tables, coherent, 16-bit ASIDs and VMIDs, no stalls,
        // little-endian, aborting terminations.
        stage
            | 0b10 << 2
            | 1 << 4
            | 1 << 12
            | u32::from(self.msi) << 13
            | 1 << 18
            | 0b10 << 21
            | 0b01 << 24
            | 1 << 26
            | two_level
    }

    fn idr1(self) -> u32 {
        // Nineteen-bit command and event queues.
        self.stream_bits | 19 << 16 | 19 << 21
    }

    /// The output size, and the 4 KiB granule.
    fn idr5(self) -> u32 {
        self.output | 1 << 4
    }

    fn output_bits(self) -> u32 {
        [32, 36, 40, 42, 44, 48, 52, 56][self.output as usize & 0b111]
    }
}

/// Ways the model can be told to misbehave.
#[derive(Copy, Clone, Default)]
pub(crate) struct Quirks {
    /// Reject the next command with this opcode as illegal.
    pub reject: Option<u8>,
    /// Never get past a `CMD_SYNC`.
    pub stall_syncs: bool,
    /// Complete this many more `CMD_SYNC`s, then stall at the next.
    pub syncs_before_stall: Option<usize>,
    /// Abort the next `CMD_SYNC`'s completion message.
    pub abort_sync_message: bool,
}

/// A cached translation: the physical page, and whether it may be read and
/// written.
type Translation = (u64, bool, bool);

struct State {
    words: BTreeMap<usize, u64>,
    cr0: u32,
    gbpa: u32,
    /// Reads of `GBPA` still to report an update under way; a write meanwhile
    /// is ignored.
    gbpa_updating: u32,
    irq_ctrl: u32,
    /// Interrupt configuration writes made while an interrupt was on.
    configured_live: usize,
    gerror: u32,
    gerrorn: u32,
    cmdq_prod: u32,
    cmdq_cons: u32,
    evtq_prod: u32,
    evtq_cons: u32,
    quirks: Quirks,
    entries: BTreeMap<u32, [u64; 8]>,
    descriptors: BTreeMap<u32, [u64; 8]>,
    /// By tag (a VMID, or `0x1_0000` above an ASID) and page.
    tlb: BTreeMap<(u32, u64), Translation>,
    processed: BTreeMap<u8, usize>,
    /// A record to raise the moment software next hands slots back.
    on_consumed: Option<(u64, u32, u64)>,
}

pub(crate) struct Model<'f> {
    frames: &'f HostFrames,
    features: Features,
    state: SpinLock<State>,
}

/// A stage's walk: its tag in the TLB, its root, its input bits and levels,
/// and whether its faults are recorded.
struct Walk {
    tag: u32,
    root: u64,
    input_bits: u32,
    levels: u32,
    stage2: bool,
    record: bool,
}

impl<'f> Model<'f> {
    pub(crate) fn new(frames: &'f HostFrames, features: Features) -> Self {
        Self {
            frames,
            features,
            state: SpinLock::new(State {
                words: BTreeMap::new(),
                cr0: 0,
                gbpa: 0,
                gbpa_updating: 0,
                irq_ctrl: 0,
                configured_live: 0,
                gerror: 0,
                gerrorn: 0,
                cmdq_prod: 0,
                cmdq_cons: 0,
                evtq_prod: 0,
                evtq_cons: 0,
                quirks: Quirks::default(),
                on_consumed: None,
                entries: BTreeMap::new(),
                descriptors: BTreeMap::new(),
                tlb: BTreeMap::new(),
                processed: BTreeMap::new(),
            }),
        }
    }

    pub(crate) fn quirk(&self, quirks: Quirks) {
        self.state.lock().quirks = quirks;
    }

    /// Leave the model as firmware might: translating with its queues on,
    /// passing transactions through while disabled with an update of that
    /// still under way, interrupts raised, and a command-queue error standing.
    pub(crate) fn firmware_left_running(&self) {
        let mut state = self.state.lock();
        state.cr0 = regs::CR0_SMMUEN | regs::CR0_CMDQEN | regs::CR0_EVENTQEN;
        state.gbpa_updating = 3;
        state.irq_ctrl = regs::IRQ_EVENTQ | regs::IRQ_GERROR;
        state.gerror = regs::GERROR_CMDQ;
    }

    /// Put the unit in service-failure mode, which no acknowledgement ends.
    pub(crate) fn fail_service(&self) {
        self.state.lock().gerror |= regs::GERROR_SFM;
    }

    pub(crate) fn translating(&self) -> bool {
        self.state.lock().cr0 & regs::CR0_SMMUEN != 0
    }

    pub(crate) fn aborting(&self) -> bool {
        self.state.lock().gbpa & regs::GBPA_ABORT != 0
    }

    pub(crate) fn interrupts(&self) -> u32 {
        self.state.lock().irq_ctrl
    }

    pub(crate) fn configured_live(&self) -> usize {
        self.state.lock().configured_live
    }

    pub(crate) fn errors(&self) -> u32 {
        let state = self.state.lock();
        state.gerror ^ state.gerrorn
    }

    /// Whether the TLB holds the translation of the page at `iova` under
    /// `tag`: an ASID with bit 16 set, or a VMID.
    pub(crate) fn caches(&self, tag: u32, iova: u64) -> bool {
        self.state.lock().tlb.contains_key(&(tag, iova & !0xFFF))
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

    /// Raise a `kind` record for `stream` at `iova` as soon as software next
    /// writes the event queue's consumer index: one landing mid-drain.
    pub(crate) fn raise_when_consumed(&self, kind: u64, stream: u32, iova: u64) {
        self.state.lock().on_consumed = Some((kind, stream, iova));
    }

    /// Record `count` events of `kind` from `stream` at `iova`, as a device
    /// raising them would.
    pub(crate) fn raise(&self, count: usize, kind: u64, stream: u32, iova: u64) {
        let mut state = self.state.lock();
        for _ in 0..count {
            self.record(&mut state, kind, stream, iova, true);
        }
    }

    fn word_of(state: &State, offset: usize) -> u64 {
        state.words.get(&offset).copied().unwrap_or(0)
    }

    fn queue_bits(base: u64) -> u32 {
        (base & 0x1F) as u32
    }

    /// Write one event record as the unit does: dropped, with the overflow
    /// flag flipped, when the queue is full.
    fn record(&self, state: &mut State, kind: u64, stream: u32, iova: u64, write: bool) {
        if state.cr0 & regs::CR0_EVENTQEN == 0 {
            return;
        }
        let base = Self::word_of(state, regs::EVENTQ_BASE);
        let bits = Self::queue_bits(base);
        let wrap = 1u32 << bits;
        let index = (wrap << 1) - 1;
        let (prod, cons) = (state.evtq_prod & index, state.evtq_cons & index);
        if prod & (wrap - 1) == cons & (wrap - 1) && prod != cons {
            state.evtq_prod ^= regs::EVENTQ_OVERFLOW;
            return;
        }
        let at = (base & ADDRESS_51_5) + u64::from(prod & (wrap - 1)) * 32;
        let read = if write { 0 } else { EVENT_READ };
        for (offset, value) in [kind | u64::from(stream) << 32, read, iova, 0]
            .into_iter()
            .enumerate()
        {
            self.frames.store_word(at + 8 * offset as u64, value);
        }
        state.evtq_prod = ((prod + 1) & index) | (state.evtq_prod & regs::EVENTQ_OVERFLOW);
    }

    fn run_queue(&self, state: &mut State) {
        if state.cr0 & regs::CR0_CMDQEN == 0 {
            return;
        }
        let base = Self::word_of(state, regs::CMDQ_BASE);
        let bits = Self::queue_bits(base);
        let wrap = 1u32 << bits;
        let index = (wrap << 1) - 1;
        // An error halts consumption until software acknowledges it.
        while (state.gerror ^ state.gerrorn) & regs::GERROR_CMDQ == 0
            && state.cmdq_cons & index != state.cmdq_prod & index
        {
            let slot = state.cmdq_cons & (wrap - 1);
            let at = (base & ADDRESS_51_5) + u64::from(slot) * 16;
            let low = self.frames.word(at).unwrap_or(0);
            let high = self.frames.word(at + 8).unwrap_or(0);
            let opcode = (low & 0xFF) as u8;
            if state.quirks.reject == Some(opcode) {
                state.quirks.reject = None;
                state.cmdq_cons = (state.cmdq_cons & index) | CERROR_ILL;
                state.gerror ^= regs::GERROR_CMDQ;
                return;
            }
            let stream = u32::try_from(low >> 32).unwrap_or(u32::MAX);
            match opcode {
                op::CFGI_STE => {
                    state.entries.remove(&stream);
                }
                op::CFGI_STE_RANGE if high & 0x1F == 31 => state.entries.clear(),
                op::CFGI_STE_RANGE => {
                    let span = 1u32 << ((high & 0x1F) + 1);
                    let first = stream & !(span - 1);
                    state
                        .entries
                        .retain(|&sid, _| sid < first || sid - first >= span);
                }
                op::CFGI_CD | op::CFGI_CD_ALL => {
                    state.descriptors.remove(&stream);
                }
                op::TLBI_NH_ASID => {
                    let tag = 0x1_0000 | (low >> 48) as u32;
                    state.tlb.retain(|&(at, _), _| at != tag);
                }
                op::TLBI_S12_VMALL => {
                    let tag = ((low >> 32) & 0xFFFF) as u32;
                    state.tlb.retain(|&(at, _), _| at != tag);
                }
                op::TLBI_NH_VA | op::TLBI_S2_IPA => {
                    let tag = if opcode == op::TLBI_NH_VA {
                        0x1_0000 | (low >> 48) as u32
                    } else {
                        ((low >> 32) & 0xFFFF) as u32
                    };
                    let page = high & !0xFFF;
                    state.tlb.remove(&(tag, page));
                }
                op::TLBI_NSNH_ALL => state.tlb.clear(),
                op::SYNC
                    if state.quirks.stall_syncs || state.quirks.syncs_before_stall == Some(0) =>
                {
                    return;
                }
                op::SYNC => {
                    if let Some(left) = state.quirks.syncs_before_stall.as_mut() {
                        *left -= 1;
                    }
                    if (low >> 12) & 0b11 == 0b01 && self.features.msi {
                        if core::mem::take(&mut state.quirks.abort_sync_message) {
                            state.gerror ^= regs::GERROR_MSI_CMDQ_ABT;
                        } else {
                            self.frames.store_word(high & ADDRESS_51_2, low >> 32);
                        }
                    }
                }
                _ => {
                    state.cmdq_cons = (state.cmdq_cons & index) | CERROR_ILL;
                    state.gerror ^= regs::GERROR_CMDQ;
                    return;
                }
            }
            *state.processed.entry(opcode).or_insert(0) += 1;
            state.cmdq_cons = ((state.cmdq_cons & index) + 1) & index;
        }
    }

    /// `stream`'s stream table entry: the cached one, else read from memory
    /// and cached.
    fn entry(&self, state: &mut State, stream: u32) -> [u64; 8] {
        if let Some(entry) = state.entries.get(&stream) {
            return *entry;
        }
        let base = Self::word_of(state, regs::STRTAB_BASE) & ADDRESS_51_6;
        let config = u32::try_from(Self::word_of(state, regs::STRTAB_BASE_CFG)).unwrap_or(0);
        let at = if config & regs::STRTAB_TWO_LEVEL != 0 {
            let split = (config >> regs::STRTAB_SPLIT_SHIFT) & 0x1F;
            let level1 = self
                .frames
                .word(base + u64::from(stream >> split) * 8)
                .unwrap_or(0);
            let span = level1 & 0x1F;
            if span == 0 {
                None
            } else {
                Some((level1 & ADDRESS_51_6) + u64::from(stream & ((1 << split) - 1)) * 64)
            }
        } else {
            Some(base + u64::from(stream) * 64)
        };
        let mut entry = [0u64; 8];
        if let Some(at) = at {
            for (word, value) in entry.iter_mut().enumerate() {
                *value = self.frames.word(at + 8 * word as u64).unwrap_or(0);
            }
        }
        state.entries.insert(stream, entry);
        entry
    }

    fn descriptor(&self, state: &mut State, stream: u32, at: u64) -> [u64; 8] {
        if let Some(descriptor) = state.descriptors.get(&stream) {
            return *descriptor;
        }
        let mut descriptor = [0u64; 8];
        for (word, value) in descriptor.iter_mut().enumerate() {
            *value = self.frames.word(at + 8 * word as u64).unwrap_or(0);
        }
        state.descriptors.insert(stream, descriptor);
        descriptor
    }

    /// The walk `stream`'s entry configures, or how its access ends without
    /// one: passed through to `Err(Some(address))`, or aborted.
    fn walk_of(
        &self,
        state: &mut State,
        stream: u32,
        iova: u64,
        write: bool,
    ) -> Result<Walk, Option<u64>> {
        let config = u32::try_from(Self::word_of(state, regs::STRTAB_BASE_CFG)).unwrap_or(0);
        if u64::from(stream) >> (config & 0x3F) != 0 {
            if Self::word_of(state, regs::CR2) & u64::from(regs::CR2_RECINVSID) != 0 {
                self.record(state, event::BAD_STREAMID, stream, 0, write);
            }
            return Err(None);
        }
        let entry = self.entry(state, stream);
        if entry[0] & 1 == 0 {
            self.record(state, event::BAD_STE, stream, 0, write);
            return Err(None);
        }
        match (entry[0] >> 1) & 0b111 {
            0b000 => Err(None),
            0b100 => Err(Some(iova)),
            0b110 => {
                let t0sz = ((entry[2] >> 32) & 0x3F) as u32;
                let sl0 = ((entry[2] >> 38) & 0b11) as u32;
                let input_bits = 64 - t0sz;
                // A walk starts at level 0 only on a 44-bit output or wider,
                // and its first level concatenates at most sixteen tables.
                let concatenated = input_bits.saturating_sub(12 + 9 * (sl0 + 2));
                if (sl0 == 2 && self.features.output_bits() < 44) || concatenated > 4 {
                    self.record(state, event::BAD_STE, stream, 0, write);
                    return Err(None);
                }
                Ok(Walk {
                    tag: (entry[2] & 0xFFFF) as u32,
                    root: entry[3] & ADDRESS_51_4,
                    input_bits: 64 - t0sz,
                    levels: sl0 + 2,
                    stage2: true,
                    record: entry[2] & (1 << 58) != 0,
                })
            }
            0b101 => {
                let descriptor = self.descriptor(state, stream, entry[0] & ADDRESS_51_6);
                if descriptor[0] & (1 << 31) == 0 {
                    self.record(state, event::BAD_CD, stream, 0, write);
                    return Err(None);
                }
                let input_bits = 64 - (descriptor[0] & 0x3F) as u32;
                Ok(Walk {
                    tag: 0x1_0000 | (descriptor[0] >> 48) as u32,
                    root: descriptor[1] & ADDRESS_51_4,
                    input_bits,
                    levels: (input_bits - 12).div_ceil(9),
                    stage2: false,
                    record: descriptor[0] & (1 << 45) != 0,
                })
            }
            _ => {
                self.record(state, event::BAD_STE, stream, 0, write);
                Err(None)
            }
        }
    }

    /// Walk `walk`'s tables for `iova`: the page, and its read and write
    /// permission, or the event the walk faults with.
    fn translate(&self, walk: &Walk, iova: u64) -> Result<Translation, u64> {
        if iova >> walk.input_bits != 0 {
            return Err(event::ADDR_SIZE);
        }
        let mut table = walk.root;
        for level in (0..walk.levels).rev() {
            let shift = 12 + 9 * level;
            // The first level spans every table concatenated there.
            let index = if level == walk.levels - 1 {
                iova >> shift
            } else {
                (iova >> shift) & 0x1FF
            };
            let entry = self.frames.word(table + 8 * index).unwrap_or(0);
            let kind = entry & 0b11;
            let leaf = match (kind, level) {
                (0b11, 0) | (0b01, 1 | 2) => true,
                (0b11, _) => false,
                _ => return Err(event::TRANSLATION),
            };
            if !leaf {
                table = entry & ADDRESS_47_12;
                continue;
            }
            if entry & (1 << 10) == 0 {
                return Err(event::ACCESS);
            }
            let span = (1u64 << shift) - 1;
            let page = (entry & ADDRESS_47_12 & !span) | (iova & span & !(PAGE - 1));
            let (read, write) = if walk.stage2 {
                (entry & (1 << 6) != 0, entry & (1 << 7) != 0)
            } else {
                // A device's access is unprivileged; AP[2] makes it read-only.
                let reachable = entry & (1 << 6) != 0;
                (reachable, reachable && entry & (1 << 7) == 0)
            };
            return Ok((page, read, write));
        }
        Err(event::TRANSLATION)
    }
}

impl Registers for &Model<'_> {
    fn read32(&self, offset: usize) -> Result<u32, IommuError> {
        let mut state = self.state.lock();
        Ok(match offset {
            regs::IDR0 => self.features.idr0(),
            regs::IDR1 => self.features.idr1(),
            regs::IDR5 => self.features.idr5(),
            regs::CR0 | regs::CR0ACK => state.cr0,
            regs::GBPA if state.gbpa_updating > 0 => {
                state.gbpa_updating -= 1;
                state.gbpa | regs::GBPA_UPDATE
            }
            regs::GBPA => state.gbpa,
            regs::IRQ_CTRL | regs::IRQ_CTRLACK => state.irq_ctrl,
            regs::GERROR => state.gerror,
            regs::GERRORN => state.gerrorn,
            regs::CMDQ_PROD => state.cmdq_prod,
            regs::CMDQ_CONS => state.cmdq_cons,
            regs::EVENTQ_PROD => state.evtq_prod,
            regs::EVENTQ_CONS => state.evtq_cons,
            _ => u32::try_from(Model::word_of(&state, offset)).unwrap_or(u32::MAX),
        })
    }

    fn write32(&self, offset: usize, value: u32) -> Result<(), IommuError> {
        let mut state = self.state.lock();
        match offset {
            regs::CR0 => {
                state.cr0 = value;
                self.run_queue(&mut state);
            }
            regs::GBPA if state.gbpa_updating > 0 => {}
            // The update takes effect at once.
            regs::GBPA => state.gbpa = value & !regs::GBPA_UPDATE,
            regs::IRQ_CTRL => state.irq_ctrl = value,
            regs::GERRORN => {
                state.gerrorn = value;
                if (state.gerror ^ value) & regs::GERROR_CMDQ == 0 {
                    state.cmdq_cons &= !CERROR_ILL;
                }
                self.run_queue(&mut state);
            }
            regs::CMDQ_PROD => {
                state.cmdq_prod = value;
                self.run_queue(&mut state);
            }
            regs::CMDQ_CONS => state.cmdq_cons = value,
            regs::EVENTQ_PROD => state.evtq_prod = value,
            regs::EVENTQ_CONS => {
                state.evtq_cons = value;
                if let Some((kind, stream, iova)) = state.on_consumed.take() {
                    self.record(&mut state, kind, stream, iova, true);
                }
            }
            _ => {
                state.words.insert(offset, u64::from(value));
            }
        }
        Ok(())
    }

    fn read64(&self, offset: usize) -> Result<u64, IommuError> {
        Ok(Model::word_of(&self.state.lock(), offset))
    }

    fn write64(&self, offset: usize, value: u64) -> Result<(), IommuError> {
        let mut state = self.state.lock();
        if matches!(offset, regs::EVENTQ_IRQ_CFG0 | regs::GERROR_IRQ_CFG0) && state.irq_ctrl != 0 {
            state.configured_live += 1;
        }
        state.words.insert(offset, value);
        Ok(())
    }

    fn window_len(&self) -> usize {
        regs::WINDOW
    }
}

impl TranslationProbe for Model<'_> {
    fn access(&self, stream: u32, iova: u64, write: bool) -> Option<u64> {
        let mut state = self.state.lock();
        if state.cr0 & regs::CR0_SMMUEN == 0 {
            return (state.gbpa & regs::GBPA_ABORT == 0).then_some(iova);
        }
        let walk = match self.walk_of(&mut state, stream, iova, write) {
            Ok(walk) => walk,
            Err(through) => return through,
        };
        let page = iova & !(PAGE - 1);
        let cached = state.tlb.get(&(walk.tag, page)).copied();
        let translated = match cached {
            Some(hit) => Ok(hit),
            None => self.translate(&walk, iova),
        };
        let fault = match translated {
            Ok((frame, read, writable)) => {
                state.tlb.insert((walk.tag, page), (frame, read, writable));
                if (write && writable) || (!write && read) {
                    return Some(frame | (iova & (PAGE - 1)));
                }
                event::PERMISSION
            }
            Err(fault) => fault,
        };
        if walk.record {
            self.record(&mut state, fault, stream, iova, write);
        }
        None
    }

    /// A translated request is refused for every entry, as each sets
    /// `EATS` to zero.
    fn translated(&self, stream: u32, address: u64, write: bool) -> Option<u64> {
        let mut state = self.state.lock();
        if state.cr0 & regs::CR0_SMMUEN == 0 {
            return (state.gbpa & regs::GBPA_ABORT == 0).then_some(address);
        }
        if let Err(through) = self.walk_of(&mut state, stream, address, write) {
            return through;
        }
        self.record(&mut state, event::TRANSL_FORBIDDEN, stream, address, write);
        None
    }
}
