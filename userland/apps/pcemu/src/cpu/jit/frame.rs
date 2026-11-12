//! The frame: the processor's state as a translated block reads it on entry
//! and leaves it on every exit — little-endian fields at fixed offsets, so
//! the code addresses them from one base and the engine needs no layout it
//! cannot state. A block runs only from a step boundary in native mode with
//! TF clear and no queue byte held, so what it does not carry here (the
//! halt, the mode, the held bytes) is as it was. A block may leave FLAGS'
//! status bits as the operation that sets them, which [`Cpu::adopt`]
//! finishes.

use crate::cpu::decode::Rep;
use crate::cpu::regs::Seg;
use crate::cpu::{Bus, Cpu, Mode, Prefixes};

/// Each field's offset.
pub(crate) mod at {
    /// AX, CX, DX, BX, SP, BP, SI, DI.
    pub(crate) const GPR: i32 = 0x00;
    /// ES, CS, SS, DS.
    pub(crate) const SEG: i32 = 0x10;
    pub(crate) const IP: i32 = 0x18;
    pub(crate) const FLAGS: i32 = 0x1A;
    /// The 8086's last effective address and operand.
    pub(crate) const IND: i32 = 0x1C;
    pub(crate) const OPR: i32 = 0x1E;
    pub(crate) const CYCLES: i32 = 0x20;
    /// The cycle count a block stops at.
    pub(crate) const LIMIT: i32 = 0x28;
    /// The cycle at which DRAM refresh next takes the bus.
    pub(crate) const REFRESH: i32 = 0x30;
    /// The bus interface unit's fill and idle credit.
    pub(crate) const FILL: i32 = 0x38;
    pub(crate) const CREDIT: i32 = 0x3C;
    /// Wait states a prefix's fetch left for its instruction to charge.
    pub(crate) const STOLEN: i32 = 0x40;
    /// Interrupts inhibited for the next instruction.
    pub(crate) const INHIBIT: i32 = 0x44;
    /// The prefixes a unit left pending: segment and repeat (`NONE` for
    /// none), LOCK, the count, the first and last prefix's IP.
    pub(crate) const PREFIX_SEG: i32 = 0x46;
    pub(crate) const PREFIX_REP: i32 = 0x47;
    pub(crate) const PREFIX_LOCK: i32 = 0x48;
    pub(crate) const PREFIX_COUNT: i32 = 0x49;
    pub(crate) const PREFIX_FIRST: i32 = 0x4A;
    pub(crate) const PREFIX_LAST: i32 = 0x4C;
    /// Where the queue starts: the code segment's base and IP.
    pub(crate) const WINDOW_IP: i32 = 0x4E;
    pub(crate) const WINDOW_BASE: i32 = 0x50;
    /// The operation whose status flags are not yet in FLAGS: its code
    /// (`NONE` for none), carry in, and operands.
    pub(crate) const LAZY: i32 = 0x54;
    pub(crate) const LAZY_CARRY: i32 = 0x55;
    pub(crate) const LAZY_A: i32 = 0x56;
    pub(crate) const LAZY_B: i32 = 0x58;
}

/// The frame's bytes.
pub(crate) const FRAME_BYTES: usize = 0x60;

/// A field holding no prefix, or no pending operation.
pub(crate) const NONE: u8 = 0xFF;

/// The frame's bytes, as the code at its base sees them: aligned for its
/// widest field, so no load of one is split.
#[derive(Clone, Debug, PartialEq, Eq)]
#[repr(C, align(8))]
pub struct Frame([u8; FRAME_BYTES]);

impl Default for Frame {
    fn default() -> Self {
        Self([0; FRAME_BYTES])
    }
}

const fn place(offset: i32) -> usize {
    offset.unsigned_abs() as usize
}

impl Frame {
    /// The frame's bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }

    /// The frame's bytes, to change.
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }

    pub(crate) fn u8(&self, offset: i32) -> u8 {
        self.0[place(offset)]
    }

    pub(crate) fn u16(&self, offset: i32) -> u16 {
        let at = place(offset);
        u16::from_le_bytes([self.0[at], self.0[at + 1]])
    }

    pub(crate) fn u32(&self, offset: i32) -> u32 {
        let at = place(offset);
        u32::from_le_bytes([self.0[at], self.0[at + 1], self.0[at + 2], self.0[at + 3]])
    }

    pub(crate) fn u64(&self, offset: i32) -> u64 {
        let at = place(offset);
        let mut raw = [0u8; 8];
        raw.copy_from_slice(&self.0[at..at + 8]);
        u64::from_le_bytes(raw)
    }

    pub(crate) fn set_u8(&mut self, offset: i32, value: u8) {
        self.0[place(offset)] = value;
    }

    pub(crate) fn set_u16(&mut self, offset: i32, value: u16) {
        let at = place(offset);
        self.0[at..at + 2].copy_from_slice(&value.to_le_bytes());
    }

    pub(crate) fn set_u32(&mut self, offset: i32, value: u32) {
        let at = place(offset);
        self.0[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    pub(crate) fn set_u64(&mut self, offset: i32, value: u64) {
        let at = place(offset);
        self.0[at..at + 8].copy_from_slice(&value.to_le_bytes());
    }
}

const fn rep_code(rep: Rep) -> u8 {
    match rep {
        Rep::Z => 0,
        Rep::Nz => 1,
        Rep::Nc => 2,
        Rep::C => 3,
    }
}

const fn rep_of(code: u8) -> Option<Rep> {
    match code {
        0 => Some(Rep::Z),
        1 => Some(Rep::Nz),
        2 => Some(Rep::Nc),
        3 => Some(Rep::C),
        _ => None,
    }
}

impl Cpu {
    /// Whether a block may be entered now: at a step boundary that resumes no
    /// string instruction, in native mode, with TF clear, interrupts not
    /// inhibited, nothing halted, no queue byte held, nothing watching each
    /// instruction begin, and no interrupt the step would take first. The pins
    /// change only as devices act, at the cycle limit, and a block makes IF
    /// set only as it ends, so none comes due within it.
    #[must_use]
    pub fn can_enter_block<B: Bus>(&self, bus: &B) -> bool {
        self.prefixes.count == 0
            && !self.inhibit
            && self.mode == Mode::Native
            && !self.halted
            && self.waiting.is_none()
            && self.paused.is_none()
            && self.regs.flags & crate::cpu::flag::TF == 0
            && self.queue.holds_nothing()
            && self.biu.bus_cycles == 0
            && !bus.waiting()
            && !bus.watches_instructions()
            && !bus.nmi()
            && !(self.regs.flag(crate::cpu::flag::IF) && bus.intr())
    }

    /// The frame a block is entered with, the bus telling where refresh
    /// next falls and whether an interrupt waits on the pins.
    #[must_use]
    pub fn frame<B: Bus>(&self, bus: &B, limit: u64) -> Frame {
        let mut f = Frame::default();
        for (n, &value) in (0..).zip(&self.regs.gpr) {
            f.set_u16(at::GPR + 2 * n, value);
        }
        for (n, &value) in (0..).zip(&self.regs.seg) {
            f.set_u16(at::SEG + 2 * n, value);
        }
        f.set_u16(at::IP, self.regs.ip);
        f.set_u16(at::FLAGS, self.regs.flags);
        f.set_u16(at::IND, self.ind);
        f.set_u16(at::OPR, self.opr);
        f.set_u64(at::CYCLES, self.cycles);
        f.set_u64(at::LIMIT, limit);
        f.set_u64(at::REFRESH, bus.next_refresh());
        f.set_u32(at::FILL, u32::from(self.biu.fill));
        f.set_u32(at::CREDIT, self.biu.credit);
        f.set_u8(at::INHIBIT, u8::from(self.inhibit));
        f.set_u8(at::PREFIX_SEG, NONE);
        f.set_u8(at::PREFIX_REP, NONE);
        f.set_u8(at::LAZY, NONE);
        let (base, ip) = self.queue.window();
        f.set_u16(at::WINDOW_IP, ip);
        f.set_u32(at::WINDOW_BASE, base);
        f
    }

    /// Takes the state a block left in `frame`, its flags finished, and
    /// hands the bus the wait states a prefix's fetch left pending.
    pub fn adopt<B: Bus>(&mut self, bus: &mut B, frame: &Frame) {
        for (n, slot) in (0..).zip(&mut self.regs.gpr) {
            *slot = frame.u16(at::GPR + 2 * n);
        }
        for (n, slot) in (0..).zip(&mut self.regs.seg) {
            *slot = frame.u16(at::SEG + 2 * n);
        }
        self.regs.ip = frame.u16(at::IP);
        self.regs.flags = super::helper::status(
            frame.u8(at::LAZY),
            frame.u16(at::LAZY_A),
            frame.u16(at::LAZY_B),
            frame.u8(at::LAZY_CARRY) != 0,
            frame.u16(at::FLAGS),
        );
        self.ind = frame.u16(at::IND);
        self.opr = frame.u16(at::OPR);
        self.cycles = frame.u64(at::CYCLES);
        self.biu.fill = u8::try_from(frame.u32(at::FILL))
            .unwrap_or(self.queue.len())
            .min(self.queue.len());
        self.biu.credit = frame.u32(at::CREDIT).min(super::super::BUS_CYCLE - 1);
        self.biu.bus_cycles = 0;
        self.inhibit = frame.u8(at::INHIBIT) != 0;
        let count = frame.u8(at::PREFIX_COUNT);
        self.prefixes = if count == 0 {
            Prefixes::default()
        } else {
            Prefixes {
                segment: match frame.u8(at::PREFIX_SEG) {
                    NONE => None,
                    field => Some(Seg::from_field(field)),
                },
                rep: rep_of(frame.u8(at::PREFIX_REP)),
                lock: frame.u8(at::PREFIX_LOCK) != 0,
                count,
                first_ip: frame.u16(at::PREFIX_FIRST),
                last_ip: frame.u16(at::PREFIX_LAST),
            }
        };
        self.queue
            .set_window(frame.u32(at::WINDOW_BASE), frame.u16(at::WINDOW_IP));
        bus.steal(frame.u32(at::STOLEN));
    }
}

/// A prefix as its frame fields hold it.
pub(crate) const fn prefix_codes(segment: Option<Seg>, rep: Option<Rep>) -> (u8, u8) {
    let seg = match segment {
        Some(seg) => seg as u8,
        None => NONE,
    };
    let rep = match rep {
        Some(rep) => rep_code(rep),
        None => NONE,
    };
    (seg, rep)
}
