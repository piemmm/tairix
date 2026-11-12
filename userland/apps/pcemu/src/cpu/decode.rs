//! The one decoder: what a run of instruction bytes means to a model.
//!
//! A *unit* is either one prefix byte or one instruction. The 8086 family
//! takes no interrupt between a prefix and its instruction but allows any
//! number of prefixes, so prefixes decode one at a time and the caller
//! carries them, which keeps every decode step bounded.

use super::model::{CpuModel, Family};
use super::regs::Seg;
use crate::bits::{sext8, word};

/// A source of instruction bytes.
pub trait Fetch {
    /// The next byte of the instruction stream.
    fn fetch(&mut self) -> u8;
}

/// A byte slice as an instruction stream; past its end it reads zeros.
pub struct SliceFetch<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> SliceFetch<'a> {
    /// A stream over `bytes`.
    #[must_use]
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }
}

impl Fetch for SliceFetch<'_> {
    fn fetch(&mut self) -> u8 {
        let byte = self.bytes.get(self.at).copied().unwrap_or(0);
        self.at = self.at.saturating_add(1);
        byte
    }
}

/// The arithmetic and logic operations, numbered as group 1 encodes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alu {
    /// ADD.
    Add,
    /// OR.
    Or,
    /// ADC.
    Adc,
    /// SBB.
    Sbb,
    /// AND.
    And,
    /// SUB.
    Sub,
    /// XOR.
    Xor,
    /// CMP.
    Cmp,
}

impl Alu {
    const fn from_field(field: u8) -> Self {
        match field & 7 {
            0 => Self::Add,
            1 => Self::Or,
            2 => Self::Adc,
            3 => Self::Sbb,
            4 => Self::And,
            5 => Self::Sub,
            6 => Self::Xor,
            _ => Self::Cmp,
        }
    }
}

/// The group 2 operations, numbered as encoded. Field 6 is `SETMO` on the
/// 8086 family and an alias of `SHL` on the others.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shift {
    /// ROL.
    Rol,
    /// ROR.
    Ror,
    /// RCL.
    Rcl,
    /// RCR.
    Rcr,
    /// SHL/SAL.
    Shl,
    /// SHR.
    Shr,
    /// Field 6.
    Setmo,
    /// SAR.
    Sar,
}

impl Shift {
    const fn from_field(field: u8) -> Self {
        match field & 7 {
            0 => Self::Rol,
            1 => Self::Ror,
            2 => Self::Rcl,
            3 => Self::Rcr,
            4 => Self::Shl,
            5 => Self::Shr,
            6 => Self::Setmo,
            _ => Self::Sar,
        }
    }
}

/// Where a shift takes its count from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Count {
    /// One.
    One,
    /// CL.
    Cl,
    /// The immediate byte.
    Imm,
}

/// The group 3 operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unary {
    /// TEST with an immediate (fields 0 and 1).
    Test,
    /// NOT.
    Not,
    /// NEG.
    Neg,
    /// MUL.
    Mul,
    /// IMUL.
    Imul,
    /// DIV.
    Div,
    /// IDIV.
    Idiv,
}

/// A branch condition, numbered as the low nibble of `70h`–`7Fh` encodes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cond {
    /// Overflow.
    O,
    /// No overflow.
    No,
    /// Below (carry).
    B,
    /// Not below.
    Nb,
    /// Equal (zero).
    E,
    /// Not equal.
    Ne,
    /// Below or equal.
    Be,
    /// Above.
    A,
    /// Sign.
    S,
    /// No sign.
    Ns,
    /// Parity even.
    P,
    /// Parity odd.
    Np,
    /// Less.
    L,
    /// Greater or equal.
    Ge,
    /// Less or equal.
    Le,
    /// Greater.
    G,
}

impl Cond {
    const fn from_nibble(nibble: u8) -> Self {
        match nibble & 15 {
            0 => Self::O,
            1 => Self::No,
            2 => Self::B,
            3 => Self::Nb,
            4 => Self::E,
            5 => Self::Ne,
            6 => Self::Be,
            7 => Self::A,
            8 => Self::S,
            9 => Self::Ns,
            10 => Self::P,
            11 => Self::Np,
            12 => Self::L,
            13 => Self::Ge,
            14 => Self::Le,
            _ => Self::G,
        }
    }
}

/// A repeat prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rep {
    /// `F3`: REP, or REPE/REPZ for comparisons.
    Z,
    /// `F2`: REPNE/REPNZ.
    Nz,
    /// NEC `64`: repeat while carry is clear.
    Nc,
    /// NEC `65`: repeat while carry is set.
    C,
}

/// A prefix byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prefix {
    /// A segment override.
    Segment(Seg),
    /// A repeat prefix.
    Rep(Rep),
    /// LOCK (and `F1`, its 8086 alias).
    Lock,
}

/// The base of a memory operand's effective address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Base {
    /// BX + SI.
    BxSi,
    /// BX + DI.
    BxDi,
    /// BP + SI.
    BpSi,
    /// BP + DI.
    BpDi,
    /// SI.
    Si,
    /// DI.
    Di,
    /// BP.
    Bp,
    /// BX.
    Bx,
    /// The displacement alone.
    Absolute,
}

impl Base {
    /// The segment the address uses unless overridden.
    #[must_use]
    pub const fn default_segment(self) -> Seg {
        match self {
            Self::BpSi | Self::BpDi | Self::Bp => Seg::Ss,
            _ => Seg::Ds,
        }
    }
}

/// A memory operand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mem {
    /// What the displacement is added to.
    pub base: Base,
    /// The displacement, sign-extended to a word.
    pub disp: u16,
    /// Whether the encoding carries a displacement added to a base, which
    /// the 8086 charges for even when it is zero.
    pub displaced: bool,
}

/// A `ModRM` `r/m` operand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ea {
    /// A register, by encoding.
    Reg(u8),
    /// A memory location.
    Mem(Mem),
}

/// The NEC V20/V30 extended operations (`0F xx`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nec {
    /// TEST1: copy a bit's complement into ZF.
    Test1,
    /// CLR1: clear a bit.
    Clr1,
    /// SET1: set a bit.
    Set1,
    /// NOT1: complement a bit.
    Not1,
    /// ADD4S: packed-BCD string add.
    Add4s,
    /// SUB4S: packed-BCD string subtract.
    Sub4s,
    /// CMP4S: packed-BCD string compare.
    Cmp4s,
    /// ROL4: rotate a BCD digit left through AL.
    Rol4,
    /// ROR4: rotate a BCD digit right through AL.
    Ror4,
    /// INS: insert a bit field.
    InsBits,
    /// EXT: extract a bit field.
    ExtBits,
    /// BRKEM: enter 8080 emulation through an interrupt vector.
    Brkem,
    /// An unassigned second byte.
    Unassigned,
}

/// What a decoded unit does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// A prefix for the next instruction.
    Prefix(Prefix),
    /// ALU r/m, reg.
    AluRmReg(Alu),
    /// ALU reg, r/m.
    AluRegRm(Alu),
    /// ALU accumulator, immediate.
    AluAccImm(Alu),
    /// ALU r/m, immediate (group 1).
    AluRmImm(Alu),
    /// PUSH segment register.
    PushSeg(Seg),
    /// POP segment register (POP CS on the 8086 family).
    PopSeg(Seg),
    /// DAA.
    Daa,
    /// DAS.
    Das,
    /// AAA.
    Aaa,
    /// AAS.
    Aas,
    /// INC reg16.
    IncReg,
    /// DEC reg16.
    DecReg,
    /// PUSH reg16.
    PushReg,
    /// POP reg16.
    PopReg,
    /// A conditional short jump.
    Jcc(Cond),
    /// TEST r/m, reg.
    TestRmReg,
    /// XCHG r/m, reg.
    XchgRmReg,
    /// MOV r/m, reg.
    MovRmReg,
    /// MOV reg, r/m.
    MovRegRm,
    /// MOV r/m16, segment register.
    MovRmSeg,
    /// LEA.
    Lea,
    /// MOV segment register, r/m16.
    MovSegRm,
    /// POP r/m16.
    PopRm,
    /// XCHG AX, reg16 (NOP for AX).
    XchgAx,
    /// CBW.
    Cbw,
    /// CWD.
    Cwd,
    /// CALL far immediate.
    CallFar,
    /// WAIT.
    Wait,
    /// PUSHF.
    Pushf,
    /// POPF.
    Popf,
    /// SAHF.
    Sahf,
    /// LAHF.
    Lahf,
    /// MOV accumulator, `[offset]`.
    MovAccMem,
    /// MOV `[offset]`, accumulator.
    MovMemAcc,
    /// MOVS.
    Movs,
    /// CMPS.
    Cmps,
    /// TEST accumulator, immediate.
    TestAccImm,
    /// STOS.
    Stos,
    /// LODS.
    Lods,
    /// SCAS.
    Scas,
    /// MOV reg, immediate.
    MovRegImm,
    /// RET near (with `imm` popped when `pops`).
    Ret {
        /// Whether an immediate is added to SP.
        pops: bool,
    },
    /// LES.
    Les,
    /// LDS.
    Lds,
    /// MOV r/m, immediate.
    MovRmImm,
    /// RET far (with `imm` popped when `pops`).
    Retf {
        /// Whether an immediate is added to SP.
        pops: bool,
    },
    /// INT 3.
    Int3,
    /// INT immediate.
    Int,
    /// INTO.
    Into,
    /// IRET.
    Iret,
    /// A group 2 shift or rotate.
    Shift(Shift, Count),
    /// AAM.
    Aam,
    /// AAD.
    Aad,
    /// SALC.
    Salc,
    /// XLAT.
    Xlat,
    /// ESC: a coprocessor instruction (and NEC's `66`/`67`).
    Esc,
    /// LOOPNE.
    Loopne,
    /// LOOPE.
    Loope,
    /// LOOP.
    Loop,
    /// JCXZ.
    Jcxz,
    /// IN accumulator, immediate port.
    InImm,
    /// OUT immediate port, accumulator.
    OutImm,
    /// CALL near relative.
    CallNear,
    /// JMP near relative.
    JmpNear,
    /// JMP far immediate.
    JmpFar,
    /// JMP short relative.
    JmpShort,
    /// IN accumulator, DX.
    InDx,
    /// OUT DX, accumulator.
    OutDx,
    /// HLT.
    Hlt,
    /// CMC.
    Cmc,
    /// A group 3 operation.
    Unary(Unary),
    /// CLC.
    Clc,
    /// STC.
    Stc,
    /// CLI.
    Cli,
    /// STI.
    Sti,
    /// CLD.
    Cld,
    /// STD.
    Std,
    /// INC r/m.
    IncRm,
    /// DEC r/m.
    DecRm,
    /// CALL near r/m.
    CallRm,
    /// CALL far `[m]`.
    CallFarRm,
    /// JMP near r/m.
    JmpRm,
    /// JMP far `[m]`.
    JmpFarRm,
    /// PUSH r/m.
    PushRm,
    /// PUSHA.
    Pusha,
    /// POPA.
    Popa,
    /// BOUND.
    Bound,
    /// PUSH immediate.
    PushImm,
    /// IMUL reg, r/m, immediate.
    ImulImm,
    /// INS.
    Ins,
    /// OUTS.
    Outs,
    /// ENTER.
    Enter,
    /// LEAVE.
    Leave,
    /// A NEC extended operation; `imm` carries the bit number or field
    /// length when the encoding has one.
    Nec(Nec),
    /// A `ModRM` instruction that changes nothing (the V20's `63`).
    Nop,
    /// An unused opcode: interrupt 6 on the 80186 family.
    Invalid,
    /// The emulator's host call: stop the CPU and run Rust.
    HostCall,
}

/// One decoded unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Insn {
    /// The operation.
    pub op: Op,
    /// The primary opcode byte; for a NEC extended operation, the byte after
    /// `0F`.
    pub opcode: u8,
    /// Whether the operation is word-sized.
    pub w: bool,
    /// The `ModRM` `reg` field, or the register the opcode itself names.
    pub reg: u8,
    /// The `ModRM` `r/m` operand (`Reg(0)` when the instruction has none).
    pub ea: Ea,
    /// The `ModRM` byte, where one was decoded.
    pub modrm: Option<u8>,
    /// The immediate, relative displacement, offset or vector.
    pub imm: u16,
    /// The second immediate: a far pointer's segment or ENTER's level.
    pub imm2: u16,
    /// Bytes in the unit.
    pub len: u8,
    /// Bytes before any immediate, offset or relative operand: what must be
    /// fetched before execution can begin.
    pub head: u8,
}

/// Bytes a unit takes at most, its prefixes apart.
pub const LONGEST: usize = 7;

struct Counted<'a, F: Fetch> {
    inner: &'a mut F,
    len: u8,
    head: Option<u8>,
}

impl<F: Fetch> Counted<'_, F> {
    fn byte(&mut self) -> u8 {
        self.len = self.len.saturating_add(1);
        self.inner.fetch()
    }

    fn word(&mut self) -> u16 {
        let lo = self.byte();
        let hi = self.byte();
        word(lo, hi)
    }

    /// Marks where the trailing operand bytes begin.
    fn operands(&mut self) {
        self.head.get_or_insert(self.len);
    }

    fn imm(&mut self, w: bool) -> u16 {
        self.operands();
        if w {
            self.word()
        } else {
            u16::from(self.byte())
        }
    }

    fn imm8(&mut self) -> u8 {
        self.operands();
        self.byte()
    }

    fn imm16(&mut self) -> u16 {
        self.operands();
        self.word()
    }

    /// The `ModRM` byte, its `reg` field and its `r/m` operand.
    fn modrm(&mut self) -> (u8, u8, Ea) {
        let byte = self.byte();
        let md = byte >> 6;
        let reg = (byte >> 3) & 7;
        let rm = byte & 7;
        if md == 3 {
            return (byte, reg, Ea::Reg(rm));
        }
        let base = match rm {
            0 => Base::BxSi,
            1 => Base::BxDi,
            2 => Base::BpSi,
            3 => Base::BpDi,
            4 => Base::Si,
            5 => Base::Di,
            6 if md == 0 => Base::Absolute,
            6 => Base::Bp,
            _ => Base::Bx,
        };
        let disp = match md {
            0 if base == Base::Absolute => self.word(),
            0 => 0,
            1 => sext8(self.byte()),
            _ => self.word(),
        };
        let displaced = md != 0 && base != Base::Absolute;
        (
            byte,
            reg,
            Ea::Mem(Mem {
                base,
                disp,
                displaced,
            }),
        )
    }
}

/// Decodes one unit for `model`.
///
/// `host` says whether the unit starts in the emulator's own ROM, where the
/// encoding `FE 38 lo hi` is a host call; anywhere else it decodes as the
/// model's own `FE /7`.
#[allow(
    clippy::too_many_lines,
    reason = "one arm per opcode reads as the opcode map"
)]
pub fn decode<F: Fetch>(model: CpuModel, host: bool, fetch: &mut F) -> Insn {
    let mut s = Counted {
        inner: fetch,
        len: 0,
        head: None,
    };
    let opcode = s.byte();
    let mut insn = Insn {
        op: Op::Nop,
        opcode,
        w: opcode & 1 != 0,
        reg: 0,
        ea: Ea::Reg(0),
        modrm: None,
        imm: 0,
        imm2: 0,
        len: 0,
        head: 0,
    };
    let family = model.family();
    let set186 = model.has_186_set();
    insn.op = match opcode {
        0x00..=0x3F if opcode & 7 < 6 => alu_block(&mut s, &mut insn, opcode),
        0x06 | 0x0E | 0x16 | 0x1E => Op::PushSeg(Seg::from_field(opcode >> 3)),
        0x07 | 0x17 | 0x1F => Op::PopSeg(Seg::from_field(opcode >> 3)),
        0x0F => match family {
            Family::Intel8086 => Op::PopSeg(Seg::Cs),
            Family::Intel80186 => Op::Invalid,
            Family::Nec => nec_extended(&mut s, &mut insn),
        },
        0x26 | 0x2E | 0x36 | 0x3E => Op::Prefix(Prefix::Segment(Seg::from_field(opcode >> 3))),
        0x27 => Op::Daa,
        0x2F => Op::Das,
        0x37 => Op::Aaa,
        0x3F => Op::Aas,
        0x40..=0x5F => {
            insn.w = true;
            insn.reg = opcode & 7;
            match opcode >> 3 {
                8 => Op::IncReg,
                9 => Op::DecReg,
                10 => Op::PushReg,
                _ => Op::PopReg,
            }
        }
        0x60..=0x6F if !set186 => short_jump(&mut s, &mut insn, opcode),
        0x60 => Op::Pusha,
        0x61 => Op::Popa,
        0x62 => {
            insn.w = true;
            with_modrm(&mut s, &mut insn);
            Op::Bound
        }
        0x63 if family == Family::Nec => {
            with_modrm(&mut s, &mut insn);
            Op::Nop
        }
        0x64 if family == Family::Nec => Op::Prefix(Prefix::Rep(Rep::Nc)),
        0x65 if family == Family::Nec => Op::Prefix(Prefix::Rep(Rep::C)),
        0x66 | 0x67 if family == Family::Nec => {
            with_modrm(&mut s, &mut insn);
            Op::Esc
        }
        0x68 => {
            insn.w = true;
            insn.imm = s.imm16();
            Op::PushImm
        }
        0x6A => {
            insn.w = true;
            insn.imm = sext8(s.imm8());
            Op::PushImm
        }
        0x69 | 0x6B => {
            insn.w = true;
            with_modrm(&mut s, &mut insn);
            insn.imm = if opcode == 0x69 {
                s.imm16()
            } else {
                sext8(s.imm8())
            };
            Op::ImulImm
        }
        0x6C | 0x6D => Op::Ins,
        0x6E | 0x6F => Op::Outs,
        0x70..=0x7F => short_jump(&mut s, &mut insn, opcode),
        0x80..=0x83 => {
            insn.w = opcode & 1 != 0;
            with_modrm(&mut s, &mut insn);
            insn.imm = match opcode {
                0x81 => s.imm16(),
                0x83 => sext8(s.imm8()),
                _ => u16::from(s.imm8()),
            };
            Op::AluRmImm(Alu::from_field(insn.reg))
        }
        0x84 | 0x85 => modrm_op(&mut s, &mut insn, Op::TestRmReg),
        0x86 | 0x87 => modrm_op(&mut s, &mut insn, Op::XchgRmReg),
        0x88 | 0x89 => modrm_op(&mut s, &mut insn, Op::MovRmReg),
        0x8A | 0x8B => modrm_op(&mut s, &mut insn, Op::MovRegRm),
        0x8C => word_modrm_op(&mut s, &mut insn, Op::MovRmSeg),
        0x8D => word_modrm_op(&mut s, &mut insn, Op::Lea),
        0x8E => word_modrm_op(&mut s, &mut insn, Op::MovSegRm),
        0x8F => word_modrm_op(&mut s, &mut insn, Op::PopRm),
        0x90..=0x97 => {
            insn.w = true;
            insn.reg = opcode & 7;
            Op::XchgAx
        }
        0x98 => Op::Cbw,
        0x99 => Op::Cwd,
        0x9A => {
            insn.imm = s.imm16();
            insn.imm2 = s.imm16();
            Op::CallFar
        }
        0x9B => Op::Wait,
        0x9C => Op::Pushf,
        0x9D => Op::Popf,
        0x9E => Op::Sahf,
        0x9F => Op::Lahf,
        0xA0..=0xA3 => {
            insn.imm = s.imm16();
            if opcode < 0xA2 {
                Op::MovAccMem
            } else {
                Op::MovMemAcc
            }
        }
        0xA4 | 0xA5 => Op::Movs,
        0xA6 | 0xA7 => Op::Cmps,
        0xA8 | 0xA9 => {
            insn.imm = s.imm(insn.w);
            Op::TestAccImm
        }
        0xAA | 0xAB => Op::Stos,
        0xAC | 0xAD => Op::Lods,
        0xAE | 0xAF => Op::Scas,
        0xB0..=0xBF => {
            insn.w = opcode & 8 != 0;
            insn.reg = opcode & 7;
            insn.imm = s.imm(insn.w);
            Op::MovRegImm
        }
        0xC0 | 0xC1 if set186 => {
            with_modrm(&mut s, &mut insn);
            insn.imm = u16::from(s.imm8());
            Op::Shift(Shift::from_field(insn.reg), Count::Imm)
        }
        0xC0 | 0xC2 => {
            insn.imm = s.imm16();
            Op::Ret { pops: true }
        }
        0xC1 | 0xC3 => Op::Ret { pops: false },
        0xC4 => word_modrm_op(&mut s, &mut insn, Op::Les),
        0xC5 => word_modrm_op(&mut s, &mut insn, Op::Lds),
        0xC6 | 0xC7 => {
            with_modrm(&mut s, &mut insn);
            insn.imm = s.imm(insn.w);
            Op::MovRmImm
        }
        0xC8 if set186 => {
            insn.imm = s.imm16();
            insn.imm2 = u16::from(s.imm8());
            Op::Enter
        }
        0xC9 if set186 => Op::Leave,
        0xC8 | 0xCA => {
            insn.imm = s.imm16();
            Op::Retf { pops: true }
        }
        0xC9 | 0xCB => Op::Retf { pops: false },
        0xCC => Op::Int3,
        0xCD => {
            insn.imm = u16::from(s.imm8());
            Op::Int
        }
        0xCE => Op::Into,
        0xCF => Op::Iret,
        0xD0..=0xD3 => {
            with_modrm(&mut s, &mut insn);
            let count = if opcode & 2 == 0 {
                Count::One
            } else {
                Count::Cl
            };
            Op::Shift(Shift::from_field(insn.reg), count)
        }
        0xD4 => {
            insn.imm = u16::from(s.imm8());
            Op::Aam
        }
        0xD5 => {
            insn.imm = u16::from(s.imm8());
            Op::Aad
        }
        0xD6 if family == Family::Nec => Op::Xlat,
        0xD6 => Op::Salc,
        0xD7 => Op::Xlat,
        0xD8..=0xDF => {
            with_modrm(&mut s, &mut insn);
            Op::Esc
        }
        0xE0..=0xE3 => {
            insn.imm = sext8(s.imm8());
            match opcode {
                0xE0 => Op::Loopne,
                0xE1 => Op::Loope,
                0xE2 => Op::Loop,
                _ => Op::Jcxz,
            }
        }
        0xE4 | 0xE5 => {
            insn.imm = u16::from(s.imm8());
            Op::InImm
        }
        0xE6 | 0xE7 => {
            insn.imm = u16::from(s.imm8());
            Op::OutImm
        }
        0xE8 => {
            insn.imm = s.imm16();
            Op::CallNear
        }
        0xE9 => {
            insn.imm = s.imm16();
            Op::JmpNear
        }
        0xEA => {
            insn.imm = s.imm16();
            insn.imm2 = s.imm16();
            Op::JmpFar
        }
        0xEB => {
            insn.imm = sext8(s.imm8());
            Op::JmpShort
        }
        0xEC | 0xED => Op::InDx,
        0xEE | 0xEF => Op::OutDx,
        0xF1 if model.traps_unused() => Op::Invalid,
        0xF0 | 0xF1 => Op::Prefix(Prefix::Lock),
        0xF2 => Op::Prefix(Prefix::Rep(Rep::Nz)),
        0xF3 => Op::Prefix(Prefix::Rep(Rep::Z)),
        0xF4 => Op::Hlt,
        0xF5 => Op::Cmc,
        0xF6 | 0xF7 => {
            with_modrm(&mut s, &mut insn);
            let unary = match insn.reg {
                0 | 1 => Unary::Test,
                2 => Unary::Not,
                3 => Unary::Neg,
                4 => Unary::Mul,
                5 => Unary::Imul,
                6 => Unary::Div,
                _ => Unary::Idiv,
            };
            if unary == Unary::Test {
                insn.imm = s.imm(insn.w);
            }
            Op::Unary(unary)
        }
        0xF8 => Op::Clc,
        0xF9 => Op::Stc,
        0xFA => Op::Cli,
        0xFB => Op::Sti,
        0xFC => Op::Cld,
        0xFD => Op::Std,
        0xFE | 0xFF => group5(&mut s, &mut insn, model, host),
        _ => Op::Invalid,
    };
    insn.len = s.len;
    insn.head = s.head.unwrap_or(s.len);
    insn
}

fn with_modrm<F: Fetch>(s: &mut Counted<'_, F>, insn: &mut Insn) {
    let (byte, reg, ea) = s.modrm();
    insn.reg = reg;
    insn.ea = ea;
    insn.modrm = Some(byte);
}

fn modrm_op<F: Fetch>(s: &mut Counted<'_, F>, insn: &mut Insn, op: Op) -> Op {
    with_modrm(s, insn);
    op
}

fn word_modrm_op<F: Fetch>(s: &mut Counted<'_, F>, insn: &mut Insn, op: Op) -> Op {
    insn.w = true;
    modrm_op(s, insn, op)
}

fn alu_block<F: Fetch>(s: &mut Counted<'_, F>, insn: &mut Insn, opcode: u8) -> Op {
    let alu = Alu::from_field(opcode >> 3);
    match opcode & 7 {
        0 | 1 => modrm_op(s, insn, Op::AluRmReg(alu)),
        2 | 3 => modrm_op(s, insn, Op::AluRegRm(alu)),
        _ => {
            insn.imm = s.imm(insn.w);
            Op::AluAccImm(alu)
        }
    }
}

fn short_jump<F: Fetch>(s: &mut Counted<'_, F>, insn: &mut Insn, opcode: u8) -> Op {
    insn.imm = sext8(s.imm8());
    Op::Jcc(Cond::from_nibble(opcode))
}

fn group5<F: Fetch>(s: &mut Counted<'_, F>, insn: &mut Insn, model: CpuModel, host: bool) -> Op {
    with_modrm(s, insn);
    let host_form = Ea::Mem(Mem {
        base: Base::BxSi,
        disp: 0,
        displaced: false,
    });
    if host && insn.opcode == 0xFE && insn.reg == 7 && insn.ea == host_form {
        insn.imm = s.imm16();
        return Op::HostCall;
    }
    match insn.reg {
        0 => Op::IncRm,
        1 => Op::DecRm,
        7 if model.traps_unused() => Op::Invalid,
        2 => Op::CallRm,
        3 => Op::CallFarRm,
        4 => Op::JmpRm,
        5 => Op::JmpFarRm,
        _ => Op::PushRm,
    }
}

fn nec_extended<F: Fetch>(s: &mut Counted<'_, F>, insn: &mut Insn) -> Op {
    let second = s.byte();
    insn.opcode = second;
    insn.w = second & 1 != 0;
    let op = match second {
        0x10..=0x1F => {
            with_modrm(s, insn);
            if second & 8 != 0 {
                insn.imm = u16::from(s.imm8());
            }
            match (second >> 1) & 3 {
                0 => Nec::Test1,
                1 => Nec::Clr1,
                2 => Nec::Set1,
                _ => Nec::Not1,
            }
        }
        0x20 => Nec::Add4s,
        0x22 => Nec::Sub4s,
        0x26 => Nec::Cmp4s,
        0x28 | 0x2A => {
            insn.w = false;
            with_modrm(s, insn);
            if second == 0x28 {
                Nec::Rol4
            } else {
                Nec::Ror4
            }
        }
        0x31 | 0x33 | 0x39 | 0x3B => {
            insn.w = false;
            with_modrm(s, insn);
            if second & 8 != 0 {
                insn.imm = u16::from(s.imm8());
            }
            if second & 2 == 0 {
                Nec::InsBits
            } else {
                Nec::ExtBits
            }
        }
        0xFF => {
            insn.imm = u16::from(s.imm8());
            Nec::Brkem
        }
        _ => Nec::Unassigned,
    };
    Op::Nec(op)
}

crate::snapshot::state_enum!(Rep {
    0 => Z,
    1 => Nz,
    2 => Nc,
    3 => C,
});

#[cfg(test)]
#[path = "decode_tests.rs"]
mod tests;
