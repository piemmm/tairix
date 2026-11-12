//! A64 machine instructions, and their encoding (Arm Architecture Reference
//! Manual, chapter C4). Register 31 is the stack pointer as an address's
//! base and in an immediate addition or subtraction, and the zero register
//! everywhere else. A conditional branch out of its one-mebibyte reach is
//! relaxed to the inverted branch over an unconditional one.

use alloc::vec::Vec;

use crate::lower::Label;
use crate::relax::{relax, Relaxable};
use crate::CompileError;

/// The general registers named, by their encoding number.
pub(crate) mod reg {
    /// The intra-procedure-call scratch registers, never allocated.
    pub(crate) const X15: u8 = 15;
    pub(crate) const X16: u8 = 16;
    pub(crate) const X17: u8 = 17;
    /// The frame pointer.
    pub(crate) const FP: u8 = 29;
    /// The link register.
    pub(crate) const LR: u8 = 30;
    /// Register 31 where it is the stack pointer.
    pub(crate) const SP: u8 = 31;
    /// Register 31 where it is the zero register.
    pub(crate) const ZR: u8 = 31;
}

/// A condition the flags are tested for, by its encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cond {
    Eq = 0,
    Ne = 1,
    /// Unsigned higher or same.
    Hs = 2,
    /// Unsigned lower.
    Lo = 3,
    /// Unsigned higher.
    Hi = 8,
    /// Unsigned lower or same.
    Ls = 9,
    Ge = 10,
    Lt = 11,
    Gt = 12,
    Le = 13,
}

impl Cond {
    /// The condition holding exactly where this one does not.
    pub(crate) const fn invert(self) -> Self {
        match self {
            Self::Eq => Self::Ne,
            Self::Ne => Self::Eq,
            Self::Hs => Self::Lo,
            Self::Lo => Self::Hs,
            Self::Hi => Self::Ls,
            Self::Ls => Self::Hi,
            Self::Ge => Self::Lt,
            Self::Lt => Self::Ge,
            Self::Gt => Self::Le,
            Self::Le => Self::Gt,
        }
    }
}

/// How many bytes a load or store moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Size {
    B,
    H,
    W,
    X,
}

impl Size {
    /// Its bytes, as a power of two.
    pub(crate) const fn log2(self) -> u32 {
        match self {
            Self::B => 0,
            Self::H => 1,
            Self::W => 2,
            Self::X => 3,
        }
    }
}

/// Where a load or store is, from its base register.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Offset {
    /// That many of its size on, up to 4095.
    Scaled(u16),
    /// That many bytes on, from -256 to 255.
    Unscaled(i16),
    /// The whole 64-bit value of a register on.
    Index(u8),
}

/// A logical operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Logic {
    And,
    Orr,
    Eor,
    /// `orr` of the second operand inverted; of a register only.
    Orn,
}

/// A data-processing operation of two registers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dp2 {
    Udiv,
    Sdiv,
    Lslv,
    Lsrv,
    Asrv,
    Rorv,
}

/// A logical immediate's encoding: its element size and rotated run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Bitmask {
    pub(crate) n: bool,
    pub(crate) immr: u8,
    pub(crate) imms: u8,
}

/// The logical immediate a `wide` operation encodes `imm` as, where one
/// does: a repeated element holding one rotated run of ones.
#[allow(clippy::similar_names)] // `immr` and `imms` are the architecture's own field names.
pub(crate) fn bitmask(imm: u64, wide: bool) -> Option<Bitmask> {
    let value = if wide {
        imm
    } else {
        let low = imm & 0xFFFF_FFFF;
        low | low << 32
    };
    if value == 0 || value == u64::MAX {
        return None;
    }
    let mut size = 64u32;
    while size > 2 {
        let half = size / 2;
        let mask = (1u64 << half) - 1;
        if value & mask != (value >> half) & mask {
            break;
        }
        size = half;
    }
    let mask = if size == 64 {
        u64::MAX
    } else {
        (1u64 << size) - 1
    };
    let element = value & mask;
    let ones = element.count_ones();
    let run = (1u64 << ones) - 1;
    let rotr = |v: u64, by: u32| {
        if by == 0 {
            v
        } else {
            ((v >> by) | (v << (size - by))) & mask
        }
    };
    let immr = (0..size).find(|&by| rotr(run, by) == element)?;
    let imms = (!(size - 1) << 1 & 0x3F) | (ones - 1);
    Some(Bitmask {
        n: size == 64,
        immr: crate::bits::low8(u64::from(immr)),
        imms: crate::bits::low8(u64::from(imms)),
    })
}

/// One instruction; `wide` is a 64-bit operation, a 32-bit one zeroing the
/// destination's upper half.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Inst {
    /// `movz rd, #imm, lsl #(16 * hw)`.
    Movz {
        wide: bool,
        rd: u8,
        imm: u16,
        hw: u8,
    },
    /// `movk rd, #imm, lsl #(16 * hw)`.
    Movk {
        wide: bool,
        rd: u8,
        imm: u16,
        hw: u8,
    },
    /// `movn rd, #imm, lsl #(16 * hw)`.
    Movn {
        wide: bool,
        rd: u8,
        imm: u16,
        hw: u8,
    },
    /// `add`/`sub` of an immediate, shifted twelve where `high`; setting the
    /// flags where `flags`. The registers may be SP, `rd` only where the
    /// flags are not set.
    AddImm {
        sub: bool,
        flags: bool,
        wide: bool,
        rd: u8,
        rn: u8,
        imm: u16,
        high: bool,
    },
    /// `add`/`sub` of registers, setting the flags where `flags`.
    AddReg {
        sub: bool,
        flags: bool,
        wide: bool,
        rd: u8,
        rn: u8,
        rm: u8,
    },
    /// `add`/`sub rd, rn, xm, uxtx`, of 64 bits, `rd` and `rn` the stack
    /// pointer where 31.
    AddSp { sub: bool, rd: u8, rn: u8, rm: u8 },
    /// A logical operation of registers.
    Logic {
        op: Logic,
        wide: bool,
        rd: u8,
        rn: u8,
        rm: u8,
    },
    /// A logical operation of an immediate; never `Orn`.
    LogicImm {
        op: Logic,
        wide: bool,
        rd: u8,
        rn: u8,
        mask: Bitmask,
    },
    /// A data-processing operation of two registers.
    Dp2 {
        op: Dp2,
        wide: bool,
        rd: u8,
        rn: u8,
        rm: u8,
    },
    /// `rd = ra + rn * rm`, or `ra - rn * rm` where `sub`.
    Madd {
        sub: bool,
        wide: bool,
        rd: u8,
        rn: u8,
        rm: u8,
        ra: u8,
    },
    /// `sbfm`/`ubfm rd, rn, #immr, #imms`.
    Bfm {
        signed: bool,
        wide: bool,
        rd: u8,
        rn: u8,
        immr: u8,
        imms: u8,
    },
    /// `extr rd, rn, rm, #lsb`.
    Extr {
        wide: bool,
        rd: u8,
        rn: u8,
        rm: u8,
        lsb: u8,
    },
    /// `csel rd, rn, rm, cond`, or `csinc` where `inc`.
    Csel {
        inc: bool,
        wide: bool,
        rd: u8,
        rn: u8,
        rm: u8,
        cond: Cond,
    },
    /// A load of `size` bytes, sign-extended where `signed` to 64 bits where
    /// `wide`, else to 32; zero-extended otherwise.
    Load {
        size: Size,
        signed: bool,
        wide: bool,
        rt: u8,
        rn: u8,
        offset: Offset,
    },
    /// A store of `rt`'s low `size` bytes.
    Store {
        size: Size,
        rt: u8,
        rn: u8,
        offset: Offset,
    },
    /// `stp rt, rt2, [sp, #-16]!`.
    PushPair { rt: u8, rt2: u8 },
    /// `ldp rt, rt2, [sp], #16`.
    PopPair { rt: u8, rt2: u8 },
    /// `blr rn`.
    Blr(u8),
    /// `ret`.
    Ret,
    /// `b label`.
    B(Label),
    /// `b.cond label`.
    BCond(Cond, Label),
    /// `cbz rt, label`.
    Cbz { wide: bool, rt: u8, label: Label },
    /// The next instruction is at `label`.
    Bind(Label),
}

/// The reach of an unconditional branch's 26-bit word offset.
const B_REACH: i64 = 1 << 27;
/// The reach of a conditional branch's 19-bit word offset.
const COND_REACH: i64 = 1 << 20;

const fn within(distance: i64, reach: i64) -> bool {
    distance % 4 == 0 && -reach <= distance && distance < reach
}

impl Relaxable for Inst {
    fn binds(&self) -> Option<Label> {
        match *self {
            Self::Bind(label) => Some(label),
            _ => None,
        }
    }

    fn target(&self) -> Option<Label> {
        match *self {
            Self::B(label) | Self::BCond(_, label) | Self::Cbz { label, .. } => Some(label),
            _ => None,
        }
    }

    fn length(&self, form: usize) -> usize {
        match self {
            Self::Bind(_) => 0,
            Self::BCond(..) | Self::Cbz { .. } if form == 1 => 8,
            _ => 4,
        }
    }

    fn forms(&self) -> usize {
        match self {
            Self::BCond(..) | Self::Cbz { .. } => 2,
            _ => 1,
        }
    }

    fn reaches(&self, form: usize, distance: i64) -> bool {
        match (self, form) {
            (Self::BCond(..) | Self::Cbz { .. }, 0) => within(distance, COND_REACH),
            // The unconditional branch of the long form is one word on.
            (Self::BCond(..) | Self::Cbz { .. }, _) => within(distance - 4, B_REACH),
            _ => within(distance, B_REACH),
        }
    }
}

/// The instructions' bytes, little-endian.
///
/// # Errors
///
/// [`CompileError::Malformed`] for a jump to a label never bound or a field
/// no encoding holds, and [`CompileError::TooLarge`] for a jump no form
/// reaches.
pub(crate) fn encode(insts: &[Inst], labels: usize) -> Result<Vec<u8>, CompileError> {
    let layout = relax(insts, labels)?;
    let mut out = Vec::with_capacity(layout.length);
    for (n, inst) in insts.iter().enumerate() {
        let distance = match inst.target() {
            Some(label) => layout.distance(n, label)?,
            None => 0,
        };
        for word in words(inst, layout.forms[n], distance)? {
            out.extend_from_slice(&word.to_le_bytes());
        }
    }
    Ok(out)
}

/// A field of `bits` bits holding `value`.
fn field(value: u32, bits: u32) -> Result<u32, CompileError> {
    if value >> bits == 0 {
        Ok(value)
    } else {
        Err(CompileError::Malformed)
    }
}

fn r(reg: u8) -> Result<u32, CompileError> {
    field(u32::from(reg), 5)
}

/// A branch offset of `bits` bits, in words.
fn offset(distance: i64, bits: u32) -> Result<u32, CompileError> {
    if !within(distance, 1 << (bits + 1)) {
        return Err(CompileError::TooLarge);
    }
    let words = distance / 4;
    Ok(crate::bits::low32(words.cast_unsigned()) & ((1 << bits) - 1))
}

const fn sf(wide: bool) -> u32 {
    if wide {
        1 << 31
    } else {
        0
    }
}

const fn logic_opc(op: Logic) -> u32 {
    match op {
        Logic::And => 0b00,
        Logic::Orr | Logic::Orn => 0b01,
        Logic::Eor => 0b10,
    }
}

fn mem(size: Size, opc: u32, rt: u8, rn: u8, offset: Offset) -> Result<u32, CompileError> {
    let common = size.log2() << 30 | opc << 22 | r(rn)? << 5 | r(rt)?;
    Ok(match offset {
        Offset::Scaled(imm) => 0x3900_0000 | common | field(u32::from(imm), 12)? << 10,
        Offset::Unscaled(imm) => {
            if !(-256..=255).contains(&imm) {
                return Err(CompileError::Malformed);
            }
            0x3800_0000 | common | (u32::from(imm.cast_unsigned()) & 0x1FF) << 12
        }
        Offset::Index(rm) => 0x3820_0800 | common | r(rm)? << 16 | 0b011 << 13,
    })
}

/// The words of `inst` in form `form`, `distance` bytes from its label.
#[allow(clippy::too_many_lines)] // One exhaustive encoding match; splitting it would scatter the table.
fn words(inst: &Inst, form: usize, distance: i64) -> Result<Vec<u32>, CompileError> {
    let one = |word| Ok(alloc::vec![word]);
    match *inst {
        Inst::Movz { wide, rd, imm, hw }
        | Inst::Movk { wide, rd, imm, hw }
        | Inst::Movn { wide, rd, imm, hw } => {
            if !wide && hw > 1 {
                return Err(CompileError::Malformed);
            }
            let base = match inst {
                Inst::Movn { .. } => 0x1280_0000,
                Inst::Movz { .. } => 0x5280_0000,
                _ => 0x7280_0000,
            };
            one(base | sf(wide) | field(u32::from(hw), 2)? << 21 | u32::from(imm) << 5 | r(rd)?)
        }
        Inst::AddImm {
            sub,
            flags,
            wide,
            rd,
            rn,
            imm,
            high,
        } => one(0x1100_0000
            | sf(wide)
            | u32::from(sub) << 30
            | u32::from(flags) << 29
            | u32::from(high) << 22
            | field(u32::from(imm), 12)? << 10
            | r(rn)? << 5
            | r(rd)?),
        Inst::AddReg {
            sub,
            flags,
            wide,
            rd,
            rn,
            rm,
        } => one(0x0B00_0000
            | sf(wide)
            | u32::from(sub) << 30
            | u32::from(flags) << 29
            | r(rm)? << 16
            | r(rn)? << 5
            | r(rd)?),
        Inst::AddSp { sub, rd, rn, rm } => {
            one(0x8B20_6000 | u32::from(sub) << 30 | r(rm)? << 16 | r(rn)? << 5 | r(rd)?)
        }
        Inst::Logic {
            op,
            wide,
            rd,
            rn,
            rm,
        } => one(0x0A00_0000
            | sf(wide)
            | logic_opc(op) << 29
            | u32::from(op == Logic::Orn) << 21
            | r(rm)? << 16
            | r(rn)? << 5
            | r(rd)?),
        Inst::LogicImm {
            op,
            wide,
            rd,
            rn,
            mask,
        } => {
            if op == Logic::Orn || (!wide && mask.n) {
                return Err(CompileError::Malformed);
            }
            one(0x1200_0000
                | sf(wide)
                | logic_opc(op) << 29
                | u32::from(mask.n) << 22
                | field(u32::from(mask.immr), 6)? << 16
                | field(u32::from(mask.imms), 6)? << 10
                | r(rn)? << 5
                | r(rd)?)
        }
        Inst::Dp2 {
            op,
            wide,
            rd,
            rn,
            rm,
        } => {
            let opcode = match op {
                Dp2::Udiv => 0b00_0010,
                Dp2::Sdiv => 0b00_0011,
                Dp2::Lslv => 0b00_1000,
                Dp2::Lsrv => 0b00_1001,
                Dp2::Asrv => 0b00_1010,
                Dp2::Rorv => 0b00_1011,
            };
            one(0x1AC0_0000 | sf(wide) | r(rm)? << 16 | opcode << 10 | r(rn)? << 5 | r(rd)?)
        }
        Inst::Madd {
            sub,
            wide,
            rd,
            rn,
            rm,
            ra,
        } => one(0x1B00_0000
            | sf(wide)
            | r(rm)? << 16
            | u32::from(sub) << 15
            | r(ra)? << 10
            | r(rn)? << 5
            | r(rd)?),
        Inst::Bfm {
            signed,
            wide,
            rd,
            rn,
            immr,
            imms,
        } => {
            let bits = if wide { 6 } else { 5 };
            let base = if signed { 0x1300_0000 } else { 0x5300_0000 };
            one(base
                | sf(wide)
                | u32::from(wide) << 22
                | field(u32::from(immr), bits)? << 16
                | field(u32::from(imms), bits)? << 10
                | r(rn)? << 5
                | r(rd)?)
        }
        Inst::Extr {
            wide,
            rd,
            rn,
            rm,
            lsb,
        } => {
            let bits = if wide { 6 } else { 5 };
            one(0x1380_0000
                | sf(wide)
                | u32::from(wide) << 22
                | r(rm)? << 16
                | field(u32::from(lsb), bits)? << 10
                | r(rn)? << 5
                | r(rd)?)
        }
        Inst::Csel {
            inc,
            wide,
            rd,
            rn,
            rm,
            cond,
        } => one(0x1A80_0000
            | sf(wide)
            | r(rm)? << 16
            | (cond as u32) << 12
            | u32::from(inc) << 10
            | r(rn)? << 5
            | r(rd)?),
        Inst::Load {
            size,
            signed,
            wide,
            rt,
            rn,
            offset,
        } => {
            let opc = match (signed, wide, size) {
                (false, _, _) | (true, _, Size::X) | (true, false, Size::W) => 0b01,
                (true, true, _) => 0b10,
                (true, false, _) => 0b11,
            };
            one(mem(size, opc, rt, rn, offset)?)
        }
        Inst::Store {
            size,
            rt,
            rn,
            offset,
        } => one(mem(size, 0b00, rt, rn, offset)?),
        Inst::PushPair { rt, rt2 } => {
            one(0xA980_0000 | 0x7E << 15 | r(rt2)? << 10 | u32::from(reg::SP) << 5 | r(rt)?)
        }
        Inst::PopPair { rt, rt2 } => {
            one(0xA8C0_0000 | 0x02 << 15 | r(rt2)? << 10 | u32::from(reg::SP) << 5 | r(rt)?)
        }
        Inst::Blr(rn) => one(0xD63F_0000 | r(rn)? << 5),
        Inst::Ret => one(0xD65F_03C0),
        Inst::B(_) => one(0x1400_0000 | offset(distance, 26)?),
        Inst::BCond(cond, _) if form == 0 => {
            one(0x5400_0000 | offset(distance, 19)? << 5 | cond as u32)
        }
        Inst::BCond(cond, _) => Ok(alloc::vec![
            0x5400_0000 | 2 << 5 | cond.invert() as u32,
            0x1400_0000 | offset(distance - 4, 26)?,
        ]),
        Inst::Cbz { wide, rt, .. } if form == 0 => {
            one(0x3400_0000 | sf(wide) | offset(distance, 19)? << 5 | r(rt)?)
        }
        Inst::Cbz { wide, rt, .. } => Ok(alloc::vec![
            0x3500_0000 | sf(wide) | 2 << 5 | r(rt)?,
            0x1400_0000 | offset(distance - 4, 26)?,
        ]),
        Inst::Bind(_) => Ok(Vec::new()),
    }
}

#[cfg(test)]
#[path = "inst_tests.rs"]
mod tests;
