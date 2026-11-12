//! RV64 machine instructions of the base ISA, the M extension and the Zbb
//! rotations and extensions, and their encoding (RISC-V Unprivileged ISA,
//! chapters 2, 4, 7 and 28). Every
//! instruction is one 32-bit word; a branch out of its four-kibibyte reach is
//! relaxed to the inverted branch over a `jal`, and one out of a `jal`'s
//! mebibyte to the inverted branch over `auipc t0` and `jalr` — so a jump is
//! only ever made where `t0` holds nothing live.

use alloc::vec::Vec;

use crate::lower::Label;
use crate::relax::{relax, Relaxable};
use crate::CompileError;

/// The integer registers named, by number.
pub(crate) mod reg {
    pub(crate) const ZERO: u8 = 0;
    pub(crate) const RA: u8 = 1;
    pub(crate) const SP: u8 = 2;
    pub(crate) const T0: u8 = 5;
    pub(crate) const T1: u8 = 6;
    pub(crate) const T2: u8 = 7;
    /// The frame pointer.
    pub(crate) const S0: u8 = 8;
    pub(crate) const A0: u8 = 10;
    pub(crate) const T3: u8 = 28;
}

/// An operation of two registers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    Add,
    Sub,
    Sll,
    Slt,
    Sltu,
    Xor,
    Srl,
    Sra,
    Or,
    And,
    Mul,
    Div,
    Divu,
    Rem,
    Remu,
    /// Zbb.
    Rol,
    Ror,
}

impl Op {
    /// Whether it has a form of the low 32 bits.
    const fn has_word(self) -> bool {
        !matches!(
            self,
            Self::Slt | Self::Sltu | Self::Xor | Self::Or | Self::And
        )
    }

    /// Its `funct7` and `funct3`.
    const fn functs(self) -> (u32, u32) {
        match self {
            Self::Add => (0, 0),
            Self::Sub => (0x20, 0),
            Self::Sll => (0, 1),
            Self::Slt => (0, 2),
            Self::Sltu => (0, 3),
            Self::Xor => (0, 4),
            Self::Srl => (0, 5),
            Self::Sra => (0x20, 5),
            Self::Or => (0, 6),
            Self::And => (0, 7),
            Self::Mul => (1, 0),
            Self::Div => (1, 4),
            Self::Divu => (1, 5),
            Self::Rem => (1, 6),
            Self::Remu => (1, 7),
            Self::Rol => (0x30, 1),
            Self::Ror => (0x30, 5),
        }
    }
}

/// An operation of a register and an immediate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ImmOp {
    Addi,
    Slti,
    Sltiu,
    Xori,
    Ori,
    Andi,
    Slli,
    Srli,
    Srai,
    /// Zbb.
    Rori,
}

impl ImmOp {
    /// Whether it has a form of the low 32 bits.
    const fn has_word(self) -> bool {
        matches!(
            self,
            Self::Addi | Self::Slli | Self::Srli | Self::Srai | Self::Rori
        )
    }

    const fn shifts(self) -> bool {
        matches!(self, Self::Slli | Self::Srli | Self::Srai | Self::Rori)
    }

    /// The bits above a shift's amount in its immediate field: the same
    /// for the word form, whose amount is a bit narrower.
    const fn shift_bits(self) -> u32 {
        match self {
            Self::Srai => 0x400,
            Self::Rori => 0x600,
            _ => 0,
        }
    }
}

/// A Zbb extension of a register's low bits to the whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ext {
    /// `sext.b`.
    SextB,
    /// `sext.h`.
    SextH,
    /// `zext.h`.
    ZextH,
}

/// A branch's condition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cond {
    Eq,
    Ne,
    Lt,
    Ge,
    Ltu,
    Geu,
}

impl Cond {
    /// The condition holding exactly where this one does not.
    pub(crate) const fn invert(self) -> Self {
        match self {
            Self::Eq => Self::Ne,
            Self::Ne => Self::Eq,
            Self::Lt => Self::Ge,
            Self::Ge => Self::Lt,
            Self::Ltu => Self::Geu,
            Self::Geu => Self::Ltu,
        }
    }

    const fn funct3(self) -> u32 {
        match self {
            Self::Eq => 0,
            Self::Ne => 1,
            Self::Lt => 4,
            Self::Ge => 5,
            Self::Ltu => 6,
            Self::Geu => 7,
        }
    }
}

/// How many bytes a load or store moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Size {
    B,
    H,
    W,
    D,
}

impl Size {
    const fn funct3(self) -> u32 {
        match self {
            Self::B => 0,
            Self::H => 1,
            Self::W => 2,
            Self::D => 3,
        }
    }
}

/// One instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Inst {
    /// `op rd, rs1, rs2`, of the low 32 bits sign-extended where `word`.
    Reg {
        op: Op,
        word: bool,
        rd: u8,
        rs1: u8,
        rs2: u8,
    },
    /// `op rd, rs1, imm`: a twelve-bit immediate, or a shift's amount.
    Imm {
        op: ImmOp,
        word: bool,
        rd: u8,
        rs1: u8,
        imm: i16,
    },
    /// `lui rd, imm`: the twenty bits `imm` in the upper part of a 32-bit
    /// value, sign-extended.
    Lui { rd: u8, imm: u32 },
    /// `rd` the low bits of `rs1` the extension `ext` takes, extended.
    Ext { ext: Ext, rd: u8, rs1: u8 },
    /// A load of `size` bytes, sign-extended where `signed`, else zeroed.
    Load {
        size: Size,
        signed: bool,
        rd: u8,
        rs1: u8,
        offset: i16,
    },
    /// A store of `rs2`'s low `size` bytes.
    Store {
        size: Size,
        rs2: u8,
        rs1: u8,
        offset: i16,
    },
    /// `jalr rd, offset(rs1)`.
    Jalr { rd: u8, rs1: u8, offset: i16 },
    /// `b<cond> rs1, rs2, label`.
    Branch {
        cond: Cond,
        rs1: u8,
        rs2: u8,
        label: Label,
    },
    /// `jal zero, label`.
    Jump(Label),
    /// The next instruction is at `label`.
    Bind(Label),
}

/// The reaches of a branch, a `jal`, and `auipc` with `jalr`.
const B_REACH: i64 = 1 << 12;
const J_REACH: i64 = 1 << 20;
const FAR_REACH: i64 = 1 << 31;

const fn within(distance: i64, reach: i64) -> bool {
    distance % 2 == 0 && -reach <= distance && distance < reach
}

/// Whether `auipc` then `jalr` reach `distance` from the `auipc`.
const fn far(distance: i64) -> bool {
    distance % 2 == 0 && -FAR_REACH <= distance + 0x800 && distance + 0x800 < FAR_REACH
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
            Self::Branch { label, .. } | Self::Jump(label) => Some(label),
            _ => None,
        }
    }

    fn length(&self, form: usize) -> usize {
        match self {
            Self::Bind(_) => 0,
            _ => 4 * (form + 1),
        }
    }

    fn forms(&self) -> usize {
        match self {
            Self::Branch { .. } => 3,
            Self::Jump(_) => 2,
            _ => 1,
        }
    }

    fn reaches(&self, form: usize, distance: i64) -> bool {
        match (self, form) {
            (Self::Branch { .. }, 0) => within(distance, B_REACH),
            (Self::Branch { .. }, 1) => within(distance - 4, J_REACH),
            (Self::Branch { .. }, _) => far(distance - 4),
            (Self::Jump(_), 0) => within(distance, J_REACH),
            _ => far(distance),
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

fn r(reg: u8) -> Result<u32, CompileError> {
    if reg < 32 {
        Ok(u32::from(reg))
    } else {
        Err(CompileError::Malformed)
    }
}

/// A twelve-bit signed immediate's bits.
fn imm12(imm: i16) -> Result<u32, CompileError> {
    if (-2048..=2047).contains(&imm) {
        Ok(u32::from(imm.cast_unsigned()) & 0xFFF)
    } else {
        Err(CompileError::Malformed)
    }
}

const fn i_type(imm: u32, rs1: u32, funct3: u32, rd: u32, opcode: u32) -> u32 {
    imm << 20 | rs1 << 15 | funct3 << 12 | rd << 7 | opcode
}

fn branch_word(cond: Cond, rs1: u8, rs2: u8, distance: i64) -> Result<u32, CompileError> {
    if !within(distance, B_REACH) {
        return Err(CompileError::TooLarge);
    }
    let imm = crate::bits::low32(distance.cast_unsigned());
    Ok((imm >> 12 & 1) << 31
        | (imm >> 5 & 0x3F) << 25
        | r(rs2)? << 20
        | r(rs1)? << 15
        | cond.funct3() << 12
        | (imm >> 1 & 0xF) << 8
        | (imm >> 11 & 1) << 7
        | 0x63)
}

fn jal_word(distance: i64) -> Result<u32, CompileError> {
    if !within(distance, J_REACH) {
        return Err(CompileError::TooLarge);
    }
    let imm = crate::bits::low32(distance.cast_unsigned());
    Ok((imm >> 20 & 1) << 31
        | (imm >> 1 & 0x3FF) << 21
        | (imm >> 11 & 1) << 20
        | (imm >> 12 & 0xFF) << 12
        | 0x6F)
}

/// `auipc t0, hi; jalr zero, lo(t0)`, reaching `distance` from the `auipc`.
fn far_words(distance: i64) -> Result<[u32; 2], CompileError> {
    if !far(distance) {
        return Err(CompileError::TooLarge);
    }
    let hi = (distance + 0x800) >> 12;
    let lo = distance - (hi << 12);
    let hi = crate::bits::low32(hi.cast_unsigned()) & 0xF_FFFF;
    let lo = i16::try_from(lo).map_err(|_| CompileError::TooLarge)?;
    let t0 = u32::from(reg::T0);
    Ok([
        hi << 12 | t0 << 7 | 0x17,
        i_type(imm12(lo)?, t0, 0, 0, 0x67),
    ])
}

/// The words of `inst` in form `form`, `distance` bytes from its label.
#[allow(clippy::too_many_lines)] // One exhaustive encoding match; splitting it would scatter the table.
fn words(inst: &Inst, form: usize, distance: i64) -> Result<Vec<u32>, CompileError> {
    let one = |word| Ok(alloc::vec![word]);
    match *inst {
        Inst::Reg {
            op,
            word,
            rd,
            rs1,
            rs2,
        } => {
            if word && !op.has_word() {
                return Err(CompileError::Malformed);
            }
            let (funct7, funct3) = op.functs();
            let opcode = if word { 0x3B } else { 0x33 };
            one(funct7 << 25 | r(rs2)? << 20 | r(rs1)? << 15 | funct3 << 12 | r(rd)? << 7 | opcode)
        }
        Inst::Imm {
            op,
            word,
            rd,
            rs1,
            imm,
        } => {
            if word && !op.has_word() {
                return Err(CompileError::Malformed);
            }
            let bits = if op.shifts() {
                let width = if word { 32 } else { 64 };
                let amount = u32::try_from(imm)
                    .ok()
                    .filter(|&n| n < width)
                    .ok_or(CompileError::Malformed)?;
                amount | op.shift_bits()
            } else {
                imm12(imm)?
            };
            let funct3 = match op {
                ImmOp::Addi => 0,
                ImmOp::Slli => 1,
                ImmOp::Slti => 2,
                ImmOp::Sltiu => 3,
                ImmOp::Xori => 4,
                ImmOp::Srli | ImmOp::Srai | ImmOp::Rori => 5,
                ImmOp::Ori => 6,
                ImmOp::Andi => 7,
            };
            one(i_type(
                bits,
                r(rs1)?,
                funct3,
                r(rd)?,
                if word { 0x1B } else { 0x13 },
            ))
        }
        Inst::Lui { rd, imm } => {
            if imm >> 20 != 0 {
                return Err(CompileError::Malformed);
            }
            one(imm << 12 | r(rd)? << 7 | 0x37)
        }
        Inst::Ext { ext, rd, rs1 } => one(match ext {
            Ext::SextB => i_type(0x604, r(rs1)?, 1, r(rd)?, 0x13),
            Ext::SextH => i_type(0x605, r(rs1)?, 1, r(rd)?, 0x13),
            Ext::ZextH => 0x04 << 25 | r(rs1)? << 15 | 4 << 12 | r(rd)? << 7 | 0x3B,
        }),
        Inst::Load {
            size,
            signed,
            rd,
            rs1,
            offset,
        } => {
            if !signed && size == Size::D {
                return Err(CompileError::Malformed);
            }
            let funct3 = size.funct3() | if signed { 0 } else { 4 };
            one(i_type(imm12(offset)?, r(rs1)?, funct3, r(rd)?, 0x03))
        }
        Inst::Store {
            size,
            rs2,
            rs1,
            offset,
        } => {
            let imm = imm12(offset)?;
            one((imm >> 5) << 25
                | r(rs2)? << 20
                | r(rs1)? << 15
                | size.funct3() << 12
                | (imm & 0x1F) << 7
                | 0x23)
        }
        Inst::Jalr { rd, rs1, offset } => one(i_type(imm12(offset)?, r(rs1)?, 0, r(rd)?, 0x67)),
        Inst::Branch { cond, rs1, rs2, .. } => match form {
            0 => one(branch_word(cond, rs1, rs2, distance)?),
            1 => Ok(alloc::vec![
                branch_word(cond.invert(), rs1, rs2, 8)?,
                jal_word(distance - 4)?
            ]),
            _ => {
                let [auipc, jalr] = far_words(distance - 4)?;
                Ok(alloc::vec![
                    branch_word(cond.invert(), rs1, rs2, 12)?,
                    auipc,
                    jalr
                ])
            }
        },
        Inst::Jump(_) if form == 0 => one(jal_word(distance)?),
        Inst::Jump(_) => Ok(far_words(distance)?.to_vec()),
        Inst::Bind(_) => Ok(Vec::new()),
    }
}

#[cfg(test)]
#[path = "inst_tests.rs"]
mod tests;
