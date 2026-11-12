//! x86_64 machine instructions, and their encoding (Intel SDM volume 2).
//! Every jump is encoded with a 32-bit displacement, so an instruction's
//! length never depends on where its target lies.

use alloc::vec::Vec;

use crate::bits::low32;
use crate::lower::Label;
use crate::CompileError;

/// The general registers by their encoding number.
pub mod reg {
    pub const RAX: u8 = 0;
    pub const RCX: u8 = 1;
    pub const RDX: u8 = 2;
    pub const RBX: u8 = 3;
    pub const RSP: u8 = 4;
    pub const RBP: u8 = 5;
    pub const RSI: u8 = 6;
    pub const RDI: u8 = 7;
    pub const R8: u8 = 8;
    pub const R9: u8 = 9;
    pub const R10: u8 = 10;
    pub const R11: u8 = 11;
    pub const R12: u8 = 12;
    pub const R13: u8 = 13;
    pub const R14: u8 = 14;
    pub const R15: u8 = 15;
}

/// An arithmetic or logical operation of two operands, which sets the flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alu {
    /// `add`.
    Add,
    /// `or`.
    Or,
    /// `and`.
    And,
    /// `sub`.
    Sub,
    /// `xor`.
    Xor,
    /// `cmp`: a `sub` that keeps only the flags.
    Cmp,
}

impl Alu {
    /// The `/digit` of its immediate forms; its register form's opcode is
    /// eight times this, plus one.
    const fn digit(self) -> u8 {
        match self {
            Self::Add => 0,
            Self::Or => 1,
            Self::And => 4,
            Self::Sub => 5,
            Self::Xor => 6,
            Self::Cmp => 7,
        }
    }
}

/// A shift or rotate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shift {
    /// `rol`.
    Rol,
    /// `ror`.
    Ror,
    /// `shl`.
    Shl,
    /// `shr`.
    Shr,
    /// `sar`.
    Sar,
}

impl Shift {
    const fn digit(self) -> u8 {
        match self {
            Self::Rol => 0,
            Self::Ror => 1,
            Self::Shl => 4,
            Self::Shr => 5,
            Self::Sar => 7,
        }
    }
}

/// A BMI2 shift, which takes its count from any register and leaves the
/// flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShiftX {
    /// `shlx`.
    Shl,
    /// `shrx`.
    Shr,
    /// `sarx`.
    Sar,
}

impl ShiftX {
    /// VEX.pp: the prefix the encoding implies.
    const fn pp(self) -> u8 {
        match self {
            Self::Shl => 0b01,
            Self::Sar => 0b10,
            Self::Shr => 0b11,
        }
    }
}

/// A condition the flags are tested for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cc {
    /// Below: unsigned less.
    B = 2,
    /// Above or equal: unsigned greater or equal.
    Ae = 3,
    /// Equal: zero.
    E = 4,
    /// Not equal: not zero.
    Ne = 5,
    /// Below or equal.
    Be = 6,
    /// Above.
    A = 7,
    /// Less: signed.
    L = 12,
    /// Greater or equal: signed.
    Ge = 13,
    /// Less or equal: signed.
    Le = 14,
    /// Greater: signed.
    G = 15,
}

/// What an operation takes as its second operand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Src {
    /// A register.
    Reg(u8),
    /// A sign-extended 32-bit immediate.
    Imm(i32),
}

/// A shift's count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Count {
    /// The low bits of CL.
    Cl,
    /// An immediate, which the processor takes modulo the width.
    Imm(u8),
}

/// How many bytes a memory operand moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bytes {
    /// One.
    B1,
    /// Two.
    B2,
    /// Four.
    B4,
    /// Eight.
    B8,
}

/// One instruction. `wide` is a 64-bit operation; a 32-bit one clears the
/// destination's upper half.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inst {
    /// `mov dst, src`.
    Mov { wide: bool, dst: u8, src: u8 },
    /// `dst = imm`: of 32 bits zero-extended, or 64.
    MovImm { wide: bool, dst: u8, imm: u64 },
    /// `op dst, src`.
    Alu {
        op: Alu,
        wide: bool,
        dst: u8,
        src: Src,
    },
    /// `imul dst, src`.
    Imul { wide: bool, dst: u8, src: u8 },
    /// `op dst, count`.
    Shift {
        op: Shift,
        wide: bool,
        dst: u8,
        count: Count,
    },
    /// `op dst, src, count`.
    ShiftX {
        op: ShiftX,
        wide: bool,
        dst: u8,
        src: u8,
        count: u8,
    },
    /// `rorx dst, src, imm`: a rotate right that leaves the flags.
    Rorx {
        wide: bool,
        dst: u8,
        src: u8,
        imm: u8,
    },
    /// `neg dst`.
    Neg { wide: bool, dst: u8 },
    /// `not dst`.
    Not { wide: bool, dst: u8 },
    /// `test a, b`.
    Test { wide: bool, a: u8, b: u8 },
    /// `movzx dst32, src8/16`.
    Movzx { from: Bytes, dst: u8, src: u8 },
    /// `movsx dst, src8/16`, or `movsxd dst64, src32`.
    Movsx {
        from: Bytes,
        wide: bool,
        dst: u8,
        src: u8,
    },
    /// `setcc dst8`.
    Setcc { cc: Cc, dst: u8 },
    /// `cmovcc dst, src`.
    Cmov {
        cc: Cc,
        wide: bool,
        dst: u8,
        src: u8,
    },
    /// `div src` or `idiv src`: RDX:RAX by `src`.
    Div { signed: bool, wide: bool, src: u8 },
    /// `cdq` or `cqo`: RAX's sign into RDX.
    SignExtend { wide: bool },
    /// `dst = [base + disp]`, extended.
    Load {
        bytes: Bytes,
        signed: bool,
        wide: bool,
        dst: u8,
        base: u8,
        disp: i32,
    },
    /// `[base + disp] = src`'s low bytes.
    Store {
        bytes: Bytes,
        src: u8,
        base: u8,
        disp: i32,
    },
    /// `[base + disp] = imm`'s low bytes; eight of them sign-extended.
    StoreImm {
        bytes: Bytes,
        imm: i32,
        base: u8,
        disp: i32,
    },
    /// `push src`.
    Push(u8),
    /// `pop dst`.
    Pop(u8),
    /// `call src`.
    Call(u8),
    /// `ret`.
    Ret,
    /// `jmp label`.
    Jmp(Label),
    /// `jcc label`.
    Jcc(Cc, Label),
    /// The next instruction is at `label`.
    Bind(Label),
}

/// The instructions' bytes.
///
/// # Errors
///
/// [`CompileError::Malformed`] for a jump to a label never bound, and
/// [`CompileError::TooLarge`] for code a 32-bit displacement cannot span.
pub(crate) fn encode(insts: &[Inst], labels: usize) -> Result<Vec<u8>, CompileError> {
    // First the places of the labels, from the lengths, which no
    // displacement changes; then the bytes.
    let mut at: Vec<Option<u32>> = alloc::vec![None; labels];
    let mut scratch = Vec::new();
    let mut length = 0usize;
    for inst in insts {
        if let Inst::Bind(label) = *inst {
            let place = u32::try_from(length).map_err(|_| CompileError::TooLarge)?;
            *at.get_mut(label.0).ok_or(CompileError::Malformed)? = Some(place);
        }
        scratch.clear();
        one(inst, &mut scratch, 0, &|_| Some(0))?;
        length += scratch.len();
    }
    if i32::try_from(length).is_err() {
        return Err(CompileError::TooLarge);
    }
    let place = |label: Label| at.get(label.0).copied().flatten();
    let mut out = Vec::with_capacity(length);
    for inst in insts {
        let here = u32::try_from(out.len()).map_err(|_| CompileError::TooLarge)?;
        one(inst, &mut out, here, &place)?;
    }
    Ok(out)
}

fn rex(out: &mut Vec<u8>, wide: bool, r: u8, x: u8, b: u8, force: bool) {
    let byte =
        0x40 | (u8::from(wide) << 3) | ((r >> 3) & 1) << 2 | ((x >> 3) & 1) << 1 | ((b >> 3) & 1);
    if byte != 0x40 || force {
        out.push(byte);
    }
}

/// `ModRM` with a register operand in its r/m field.
fn modrm_reg(out: &mut Vec<u8>, reg: u8, rm: u8) {
    out.push(0xC0 | (reg & 7) << 3 | (rm & 7));
}

/// `ModRM`, and `SIB` and displacement where needed, for `[base + disp]`.
fn modrm_mem(out: &mut Vec<u8>, reg: u8, base: u8, disp: i32) {
    let low = base & 7;
    let (mode, short) = if disp == 0 && low != reg::RBP {
        (0b00, None)
    } else if let Ok(short) = i8::try_from(disp) {
        (0b01, Some(short))
    } else {
        (0b10, None)
    };
    out.push(mode << 6 | (reg & 7) << 3 | low);
    if low == reg::RSP {
        // SIB: no index, base as given.
        out.push(0x24);
    }
    match (mode, short) {
        (0b01, Some(short)) => out.push(short.cast_unsigned()),
        (0b10, _) => out.extend_from_slice(&disp.to_le_bytes()),
        _ => {}
    }
}

/// A three-byte VEX prefix for opcode map `map` with implied prefix `pp`,
/// then the opcode and a `ModRM` naming `reg` and `rm`; `vvvv` is the
/// register VEX.vvvv names.
fn vex3(
    out: &mut Vec<u8>,
    (map, pp, opcode): (u8, u8, u8),
    wide: bool,
    (reg, vvvv, rm): (u8, u8, u8),
) {
    out.push(0xC4);
    // R, X and B inverted; no index, so X is one.
    out.push((!reg >> 3 & 1) << 7 | 1 << 6 | (!rm >> 3 & 1) << 5 | map);
    out.push(u8::from(wide) << 7 | (!vvvv & 0xF) << 3 | pp);
    out.push(opcode);
    modrm_reg(out, reg, rm);
}

/// `op reg, rm` with a register in each.
fn rr(out: &mut Vec<u8>, wide: bool, opcode: &[u8], reg: u8, rm: u8, byte_regs: bool) {
    let force = byte_regs && (reg & 7 >= 4 && reg < 8 || rm & 7 >= 4 && rm < 8);
    rex(out, wide, reg, 0, rm, force);
    out.extend_from_slice(opcode);
    modrm_reg(out, reg, rm);
}

#[allow(clippy::too_many_lines)] // One exhaustive encoding match; splitting it would scatter the table.
fn one(
    inst: &Inst,
    out: &mut Vec<u8>,
    here: u32,
    place: &dyn Fn(Label) -> Option<u32>,
) -> Result<(), CompileError> {
    // A displacement counts from the end of its instruction: `here`, where
    // the instruction begins, and its opcode and four bytes on.
    let jump = |out: &mut Vec<u8>, opcode: &[u8], label: Label| -> Result<(), CompileError> {
        out.extend_from_slice(opcode);
        let target = place(label).ok_or(CompileError::Malformed)?;
        let end = here + if opcode.len() == 1 { 5 } else { 6 };
        out.extend_from_slice(&target.wrapping_sub(end).to_le_bytes());
        Ok(())
    };
    match *inst {
        Inst::Mov { wide, dst, src } => rr(out, wide, &[0x89], src, dst, false),
        Inst::MovImm { wide, dst, imm } => {
            if !wide || u32::try_from(imm).is_ok() {
                rex(out, false, 0, 0, dst, false);
                out.push(0xB8 + (dst & 7));
                out.extend_from_slice(&low32(imm).to_le_bytes());
            } else if let Ok(short) = i32::try_from(imm.cast_signed()) {
                rex(out, true, 0, 0, dst, false);
                out.push(0xC7);
                modrm_reg(out, 0, dst);
                out.extend_from_slice(&short.to_le_bytes());
            } else {
                rex(out, true, 0, 0, dst, false);
                out.push(0xB8 + (dst & 7));
                out.extend_from_slice(&imm.to_le_bytes());
            }
        }
        Inst::Alu { op, wide, dst, src } => match src {
            Src::Reg(src) => rr(out, wide, &[op.digit() * 8 + 1], src, dst, false),
            Src::Imm(imm) => {
                rex(out, wide, 0, 0, dst, false);
                if let Ok(short) = i8::try_from(imm) {
                    out.push(0x83);
                    modrm_reg(out, op.digit(), dst);
                    out.push(short.cast_unsigned());
                } else {
                    out.push(0x81);
                    modrm_reg(out, op.digit(), dst);
                    out.extend_from_slice(&imm.to_le_bytes());
                }
            }
        },
        Inst::Imul { wide, dst, src } => rr(out, wide, &[0x0F, 0xAF], dst, src, false),
        Inst::Shift {
            op,
            wide,
            dst,
            count,
        } => {
            rex(out, wide, 0, 0, dst, false);
            match count {
                Count::Cl => {
                    out.push(0xD3);
                    modrm_reg(out, op.digit(), dst);
                }
                Count::Imm(n) => {
                    out.push(0xC1);
                    modrm_reg(out, op.digit(), dst);
                    out.push(n);
                }
            }
        }
        Inst::ShiftX {
            op,
            wide,
            dst,
            src,
            count,
        } => vex3(out, (2, op.pp(), 0xF7), wide, (dst, count, src)),
        Inst::Rorx {
            wide,
            dst,
            src,
            imm,
        } => {
            // VEX.vvvv names no register: 1111, its inverse of zero.
            vex3(out, (3, 0b11, 0xF0), wide, (dst, 0, src));
            out.push(imm);
        }
        Inst::Neg { wide, dst } => {
            rex(out, wide, 0, 0, dst, false);
            out.push(0xF7);
            modrm_reg(out, 3, dst);
        }
        Inst::Not { wide, dst } => {
            rex(out, wide, 0, 0, dst, false);
            out.push(0xF7);
            modrm_reg(out, 2, dst);
        }
        Inst::Test { wide, a, b } => rr(out, wide, &[0x85], b, a, false),
        Inst::Movzx { from, dst, src } => {
            let (opcode, bytes) = if from == Bytes::B1 {
                (0xB6, true)
            } else {
                (0xB7, false)
            };
            rr(out, false, &[0x0F, opcode], dst, src, bytes);
        }
        Inst::Movsx {
            from,
            wide,
            dst,
            src,
        } => match from {
            Bytes::B1 => rr(out, wide, &[0x0F, 0xBE], dst, src, true),
            Bytes::B2 => rr(out, wide, &[0x0F, 0xBF], dst, src, false),
            Bytes::B4 | Bytes::B8 => rr(out, true, &[0x63], dst, src, false),
        },
        Inst::Setcc { cc, dst } => rr(out, false, &[0x0F, 0x90 + cc as u8], 0, dst, true),
        Inst::Cmov { cc, wide, dst, src } => {
            rr(out, wide, &[0x0F, 0x40 + cc as u8], dst, src, false);
        }
        Inst::Div { signed, wide, src } => {
            rex(out, wide, 0, 0, src, false);
            out.push(0xF7);
            modrm_reg(out, if signed { 7 } else { 6 }, src);
        }
        Inst::SignExtend { wide } => {
            rex(out, wide, 0, 0, 0, false);
            out.push(0x99);
        }
        Inst::Load {
            bytes,
            signed,
            wide,
            dst,
            base,
            disp,
        } => {
            let (wide, opcode): (bool, &[u8]) = match (bytes, signed) {
                (Bytes::B1, false) => (false, &[0x0F, 0xB6]),
                (Bytes::B1, true) => (wide, &[0x0F, 0xBE]),
                (Bytes::B2, false) => (false, &[0x0F, 0xB7]),
                (Bytes::B2, true) => (wide, &[0x0F, 0xBF]),
                (Bytes::B4, true) if wide => (true, &[0x63]),
                (Bytes::B4, _) => (false, &[0x8B]),
                (Bytes::B8, _) => (true, &[0x8B]),
            };
            rex(out, wide, dst, 0, base, false);
            out.extend_from_slice(opcode);
            modrm_mem(out, dst, base, disp);
        }
        Inst::Store {
            bytes,
            src,
            base,
            disp,
        } => {
            let wide = bytes == Bytes::B8;
            if bytes == Bytes::B2 {
                out.push(0x66);
            }
            let force = bytes == Bytes::B1 && src & 7 >= 4 && src < 8;
            rex(out, wide, src, 0, base, force);
            out.push(if bytes == Bytes::B1 { 0x88 } else { 0x89 });
            modrm_mem(out, src, base, disp);
        }
        Inst::StoreImm {
            bytes,
            imm,
            base,
            disp,
        } => {
            if bytes == Bytes::B2 {
                out.push(0x66);
            }
            rex(out, bytes == Bytes::B8, 0, 0, base, false);
            out.push(if bytes == Bytes::B1 { 0xC6 } else { 0xC7 });
            modrm_mem(out, 0, base, disp);
            let raw = imm.to_le_bytes();
            out.extend_from_slice(match bytes {
                Bytes::B1 => &raw[..1],
                Bytes::B2 => &raw[..2],
                Bytes::B4 | Bytes::B8 => &raw,
            });
        }
        Inst::Push(src) => {
            rex(out, false, 0, 0, src, false);
            out.push(0x50 + (src & 7));
        }
        Inst::Pop(dst) => {
            rex(out, false, 0, 0, dst, false);
            out.push(0x58 + (dst & 7));
        }
        Inst::Call(src) => {
            rex(out, false, 0, 0, src, false);
            out.push(0xFF);
            modrm_reg(out, 2, src);
        }
        Inst::Ret => out.push(0xC3),
        Inst::Jmp(label) => jump(out, &[0xE9], label)?,
        Inst::Jcc(cc, label) => jump(out, &[0x0F, 0x80 + cc as u8], label)?,
        Inst::Bind(_) => {}
    }
    Ok(())
}

#[cfg(test)]
#[path = "inst_tests.rs"]
mod tests;
