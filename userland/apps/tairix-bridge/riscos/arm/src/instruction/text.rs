//! The assembler's text for an instruction, as BBC BASIC's assembler and
//! RISC OS's Debugger spell it: the condition before an older mnemonic's
//! suffix (`LDREQB`, `ADDEQS`), stack mnemonics for a block transfer on R13,
//! and `&` for hexadecimal.

use core::fmt::{self, Display, Formatter, Write as _};

use super::op::{
    ArithmeticOp, BarrierOp, BitfieldOp, BlockMode, BlockOp, BranchOp, CallOp, CoIndex, FpaOperand,
    FpaRegisterOp, HintOp, Index, Instruction, LongOp, MultiplyOp, Offset, Op, Operand, SaturateOp,
    Shift, Source, SwapOp,
};
use super::Set;

mod simdfp;
use crate::a32;
use crate::cond::Condition;
use crate::fpa::{Format, Precision, Rounding};
use crate::isa::{Feature, Isa};
use crate::reg::Reg;

/// An instruction's text, placed at an address so a branch shows its target.
#[derive(Clone, Copy, Debug)]
pub struct Text {
    pub(super) instruction: Instruction,
    pub(super) address: u32,
    pub(super) set: Set,
}

/// The column the operands start in after a mnemonic shorter than it.
const MNEMONIC_WIDTH: usize = 8;

/// A number as an immediate: small ones in decimal, others in hexadecimal.
struct Number(u32);

impl Display for Number {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if self.0 < 10 {
            write!(f, "{}", self.0)
        } else {
            write!(f, "&{:X}", self.0)
        }
    }
}

/// An offset with its sign.
struct Signed {
    up: bool,
    value: u32,
}

impl Display for Signed {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if !self.up {
            f.write_char('-')?;
        }
        Number(self.value).fmt(f)
    }
}

/// An FPA operand: a register, or a constant as `#0.5` spells one.
struct FpaOperandText(FpaOperand);

impl Display for FpaOperandText {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self.0 {
            FpaOperand::Register(n) => write!(f, "F{n}"),
            FpaOperand::Constant(6) => f.write_str("#0.5"),
            FpaOperand::Constant(7) => f.write_str("#10"),
            FpaOperand::Constant(n) => write!(f, "#{n}"),
        }
    }
}

const fn precision_suffix(precision: Precision) -> &'static str {
    match precision {
        Precision::Single => "S",
        Precision::Double => "D",
        Precision::Extended => "E",
    }
}

const fn rounding_suffix(rounding: Rounding) -> &'static str {
    match rounding {
        Rounding::Nearest => "",
        Rounding::PlusInfinity => "P",
        Rounding::MinusInfinity => "M",
        Rounding::Zero => "Z",
    }
}

const fn format_suffix(format: Format) -> &'static str {
    match format {
        Format::Single => "S",
        Format::Double => "D",
        Format::Extended => "E",
        Format::Packed => "P",
    }
}

/// A coprocessor transfer's address: its offset in words written as bytes,
/// or an unindexed transfer's option.
fn address(out: &mut Buffer, rn: Reg, up: bool, index: CoIndex, offset: u8) -> fmt::Result {
    let bytes = Signed {
        up,
        value: 4 * u32::from(offset),
    };
    match index {
        CoIndex::Offset => write!(out, "[{rn},#{bytes}]"),
        CoIndex::PreIndexed => write!(out, "[{rn},#{bytes}]!"),
        CoIndex::PostIndexed => write!(out, "[{rn}],#{bytes}"),
        CoIndex::Unindexed => write!(out, "[{rn}],{{{offset}}}"),
    }
}

struct ShiftText(Shift);

impl Display for ShiftText {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self.0 {
            Shift::Lsl(0) => Ok(()),
            Shift::Lsl(n) => write!(f, ",LSL #{n}"),
            Shift::Lsr(n) => write!(f, ",LSR #{n}"),
            Shift::Asr(n) => write!(f, ",ASR #{n}"),
            Shift::Ror(n) => write!(f, ",ROR #{n}"),
            Shift::Rrx => f.write_str(",RRX"),
        }
    }
}

struct OperandText(Operand);

impl Display for OperandText {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self.0 {
            Operand::Immediate(immediate) => write!(f, "#{}", Number(immediate.value())),
            Operand::Expanded(expanded) => write!(f, "#{}", Number(expanded.value())),
            Operand::Literal(value) => write!(f, "#{}", Number(u32::from(value))),
            Operand::Register { rm, shift } => write!(f, "{rm}{}", ShiftText(shift)),
            Operand::Shifted { rm, kind, rs } => write!(f, "{rm},{} {rs}", kind.name()),
        }
    }
}

/// A single transfer's address: `[Rn,#4]`, `[Rn,#4]!` or `[Rn],#4`.
struct Address {
    rn: Reg,
    up: bool,
    offset: Offset,
    index: Index,
}

impl Display for Address {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let offset = OffsetText {
            up: self.up,
            offset: self.offset,
        };
        let empty = self.up && self.offset == Offset::Immediate(0);
        match self.index {
            Index::Offset if empty => write!(f, "[{}]", self.rn),
            Index::Offset => write!(f, "[{},{offset}]", self.rn),
            Index::PreIndexed => write!(f, "[{},{offset}]!", self.rn),
            Index::PostIndexed => write!(f, "[{}],{offset}", self.rn),
        }
    }
}

struct OffsetText {
    up: bool,
    offset: Offset,
}

impl Display for OffsetText {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self.offset {
            Offset::Immediate(value) => write!(
                f,
                "#{}",
                Signed {
                    up: self.up,
                    value: u32::from(value),
                }
            ),
            Offset::Register { rm, shift } => {
                let sign = if self.up { "" } else { "-" };
                write!(f, "{sign}{rm}{}", ShiftText(shift))
            }
        }
    }
}

fn half(top: bool) -> char {
    if top {
        'T'
    } else {
        'B'
    }
}

/// Splits a transfer's mnemonic at its suffix, which the condition precedes:
/// `LDR` and `B` for `LDRB`.
fn transfer_parts(name: &'static str) -> (&'static str, &'static str) {
    name.split_at_checked(3).unwrap_or((name, ""))
}

fn block_suffix(op: BlockOp, mode: BlockMode, stack: bool) -> &'static str {
    match (stack, op, mode) {
        (false, _, BlockMode::IncrementAfter) => "IA",
        (false, _, BlockMode::IncrementBefore) => "IB",
        (false, _, BlockMode::DecrementAfter) => "DA",
        (false, _, BlockMode::DecrementBefore) => "DB",
        (true, BlockOp::Ldm, BlockMode::IncrementAfter)
        | (true, BlockOp::Stm, BlockMode::DecrementBefore) => "FD",
        (true, BlockOp::Ldm, BlockMode::IncrementBefore)
        | (true, BlockOp::Stm, BlockMode::DecrementAfter) => "ED",
        (true, BlockOp::Ldm, BlockMode::DecrementAfter)
        | (true, BlockOp::Stm, BlockMode::IncrementBefore) => "FA",
        (true, BlockOp::Ldm, BlockMode::DecrementBefore)
        | (true, BlockOp::Stm, BlockMode::IncrementAfter) => "EA",
    }
}

fn barrier_option(option: u8) -> Option<&'static str> {
    Some(match option {
        15 => "SY",
        14 => "ST",
        13 => "LD",
        11 => "ISH",
        10 => "ISHST",
        9 => "ISHLD",
        7 => "NSH",
        6 => "NSHST",
        5 => "NSHLD",
        3 => "OSH",
        2 => "OSHST",
        1 => "OSHLD",
        _ => return None,
    })
}

/// Text built up in place: a mnemonic or its operands, neither of which is
/// ever longer than this.
struct Buffer {
    bytes: [u8; 64],
    len: usize,
    /// Whether a write ran past the bytes: only an instruction built with
    /// fields no encoding holds is that long.
    overflowed: bool,
}

impl Buffer {
    const fn new() -> Self {
        Self {
            bytes: [0; 64],
            len: 0,
            overflowed: false,
        }
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }

    fn push(&mut self, part: &str) -> fmt::Result {
        self.write_str(part)
    }
}

impl fmt::Write for Buffer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let end = self.len + s.len();
        match self.bytes.get_mut(self.len..end) {
            Some(slot) => {
                slot.copy_from_slice(s.as_bytes());
                self.len = end;
            }
            None => self.overflowed = true,
        }
        Ok(())
    }
}

impl Text {
    /// Writes an FPA form its syntax has no spelling for as the generic
    /// coprocessor operation its word is, as the RISC OS Debugger shows it.
    fn generic(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let generic = a32::encode(&self.instruction)
            .ok()
            .and_then(|word| a32::decode(word, Isa::ALL.without(Feature::Fpa)));
        match generic {
            Some(generic) => generic.text(self.set, self.address).fmt(f),
            None => f.write_str("(unrepresentable)"),
        }
    }

    /// The PC as the instruction reads it.
    const fn pc(&self) -> u32 {
        self.address.wrapping_add(match self.set {
            Set::A32 => 8,
            Set::T32 => 4,
        })
    }
}

impl Display for Text {
    #[allow(clippy::too_many_lines)] // One arm per form, each its own syntax.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let Instruction { cond, op } = self.instruction;
        let c = cond.suffix();
        let c = if self.set == Set::A32 && cond == Condition::Nv && a32::unconditional(op.kind()) {
            ""
        } else {
            c
        };
        let mut m = Buffer::new();
        let mut operands = Buffer::new();
        match op {
            Op::SimdFp(op) => simdfp::write(&op, c, &mut m, &mut operands)?,
            Op::Data {
                op,
                s,
                rd,
                rn,
                operand,
            } => {
                m.push(op.name())?;
                m.push(c)?;
                m.push(if s { "S" } else { "" })?;
                write!(operands, "{rd},{rn},{}", OperandText(operand))?;
            }
            Op::Compare {
                op,
                rd,
                rn,
                operand,
            } => {
                m.push(op.name())?;
                m.push(c)?;
                m.push(if rd == Reg::PC { "P" } else { "" })?;
                write!(operands, "{rn},{}", OperandText(operand))?;
            }
            Op::Move { op, s, rd, operand } => {
                m.push(op.name())?;
                m.push(c)?;
                m.push(if s { "S" } else { "" })?;
                write!(operands, "{rd},{}", OperandText(operand))?;
            }
            Op::Wide { op, rd, value } => {
                m.push(op.name())?;
                m.push(c)?;
                write!(operands, "{rd},#{}", Number(u32::from(value)))?;
            }
            Op::Multiply {
                op,
                s,
                x,
                y,
                rd,
                rn,
                rm,
                ra,
            } => {
                m.push(op.name())?;
                match op {
                    MultiplyOp::Smla | MultiplyOp::Smul => write!(m, "{}{}", half(x), half(y))?,
                    MultiplyOp::Smlaw | MultiplyOp::Smulw => {
                        m.push(if y { "T" } else { "B" })?;
                    }
                    MultiplyOp::Smlad
                    | MultiplyOp::Smuad
                    | MultiplyOp::Smlsd
                    | MultiplyOp::Smusd => {
                        m.push(if x { "X" } else { "" })?;
                    }
                    MultiplyOp::Smmla | MultiplyOp::Smmul | MultiplyOp::Smmls => {
                        m.push(if x { "R" } else { "" })?;
                    }
                    _ => {}
                }
                m.push(c)?;
                m.push(if s { "S" } else { "" })?;
                write!(operands, "{rd},{rn},{rm}")?;
                if let Some(ra) = ra {
                    write!(operands, ",{ra}")?;
                }
            }
            Op::MultiplyLong {
                op,
                s,
                x,
                y,
                lo,
                hi,
                rn,
                rm,
            } => {
                m.push(op.name())?;
                match op {
                    LongOp::SmlalHalves => write!(m, "{}{}", half(x), half(y))?,
                    LongOp::Smlald | LongOp::Smlsld => m.push(if x { "X" } else { "" })?,
                    _ => {}
                }
                m.push(c)?;
                m.push(if s { "S" } else { "" })?;
                write!(operands, "{lo},{hi},{rn},{rm}")?;
            }
            Op::Parallel {
                prefix,
                lanes,
                rd,
                rn,
                rm,
            } => {
                m.push(prefix.name())?;
                m.push(lanes.name())?;
                m.push(c)?;
                write!(operands, "{rd},{rn},{rm}")?;
            }
            Op::Arithmetic { op, rd, rn, rm } => {
                m.push(op.name())?;
                m.push(c)?;
                match op {
                    ArithmeticOp::Qadd
                    | ArithmeticOp::Qsub
                    | ArithmeticOp::Qdadd
                    | ArithmeticOp::Qdsub => write!(operands, "{rd},{rm},{rn}")?,
                    _ => write!(operands, "{rd},{rn},{rm}")?,
                }
            }
            Op::Unary { op, rd, rm } => {
                m.push(op.name())?;
                m.push(c)?;
                write!(operands, "{rd},{rm}")?;
            }
            Op::Saturate {
                op,
                rd,
                saturate,
                rn,
                shift,
            } => {
                m.push(op.name())?;
                m.push(c)?;
                let position = match op {
                    SaturateOp::Ssat | SaturateOp::Ssat16 => u32::from(saturate) + 1,
                    SaturateOp::Usat | SaturateOp::Usat16 => u32::from(saturate),
                };
                write!(operands, "{rd},#{position},{rn}{}", ShiftText(shift))?;
            }
            Op::Extend {
                op,
                rd,
                rn,
                rm,
                rotation,
            } => {
                m.push(op.name())?;
                m.push(c)?;
                write!(operands, "{rd},")?;
                if let Some(rn) = rn {
                    write!(operands, "{rn},")?;
                }
                write!(operands, "{rm}")?;
                if rotation != 0 {
                    write!(operands, ",ROR #{}", 8 * u32::from(rotation))?;
                }
            }
            Op::Pack {
                op,
                rd,
                rn,
                rm,
                shift,
            } => {
                m.push(op.name())?;
                m.push(c)?;
                write!(operands, "{rd},{rn},{rm}{}", ShiftText(shift))?;
            }
            Op::Bitfield {
                op,
                rd,
                rn,
                lsb,
                top,
            } => {
                m.push(op.name())?;
                m.push(c)?;
                let width = match op {
                    BitfieldOp::Sbfx | BitfieldOp::Ubfx => i32::from(top) + 1,
                    BitfieldOp::Bfi | BitfieldOp::Bfc => i32::from(top) - i32::from(lsb) + 1,
                };
                write!(operands, "{rd},")?;
                if let Some(rn) = rn {
                    write!(operands, "{rn},")?;
                }
                write!(operands, "#{lsb},#{width}")?;
            }
            Op::Status { spsr, rd } => {
                m.push("MRS")?;
                m.push(c)?;
                write!(operands, "{rd},{}", if spsr { "SPSR" } else { "CPSR" })?;
            }
            Op::StatusWrite {
                spsr,
                fields,
                source,
            } => {
                m.push("MSR")?;
                m.push(c)?;
                write!(operands, "{}_", if spsr { "SPSR" } else { "CPSR" })?;
                for (bit, name) in [(8, 'f'), (4, 's'), (2, 'x'), (1, 'c')] {
                    if fields & bit != 0 {
                        operands.write_char(name)?;
                    }
                }
                match source {
                    Source::Immediate(immediate) => {
                        write!(operands, ",#{}", Number(immediate.value()))?;
                    }
                    Source::Register(rm) => write!(operands, ",{rm}")?,
                }
            }
            Op::Branch { op, offset } => {
                m.push(op.name())?;
                m.push(c)?;
                let pc = if self.set == Set::T32 && op == BranchOp::Blx {
                    self.pc() & !3
                } else {
                    self.pc()
                };
                write!(operands, "&{:08X}", pc.wrapping_add_signed(offset))?;
            }
            Op::CompareBranch {
                nonzero,
                rn,
                offset,
            } => {
                m.push(if nonzero { "CBNZ" } else { "CBZ" })?;
                write!(operands, "{rn},&{:08X}", self.pc().wrapping_add(offset))?;
            }
            Op::IfThen { firstcond, mask } => {
                m.push("IT")?;
                // Each instruction after the first takes the next mask bit
                // down: then when it matches the condition's low bit.
                let length = 4 - u32::from(mask & 15).trailing_zeros().min(3);
                for bit in (4 - length..3).rev() {
                    let then = u32::from(mask >> (bit + 1)) & 1 == firstcond.bits() & 1;
                    m.push(if then { "T" } else { "E" })?;
                }
                operands.write_str(match firstcond {
                    Condition::Al => "AL",
                    other => other.suffix(),
                })?;
            }
            Op::TableBranch { half, rn, rm } => {
                m.push(if half { "TBH" } else { "TBB" })?;
                write!(operands, "[{rn},{rm}{}]", if half { ",LSL #1" } else { "" })?;
            }
            Op::Exchange { op, rm } => {
                m.push(op.name())?;
                m.push(c)?;
                write!(operands, "{rm}")?;
            }
            Op::Call { op, value } => {
                m.push(op.name())?;
                m.push(c)?;
                match op {
                    CallOp::Swi => write!(operands, "&{value:06X}")?,
                    _ => write!(operands, "#{}", Number(value))?,
                }
            }
            Op::Hint { op, value } => {
                m.push(op.name())?;
                m.push(c)?;
                if matches!(op, HintOp::Dbg | HintOp::Hint) {
                    write!(operands, "#{}", Number(u32::from(value)))?;
                }
            }
            Op::Barrier { op, option } => {
                m.push(op.name())?;
                match (op, barrier_option(option)) {
                    (BarrierOp::Ssbb | BarrierOp::Pssbb, _) => {}
                    (BarrierOp::Isb, _) if option != 15 => write!(operands, "#{option}")?,
                    (_, Some(name)) => operands.write_str(name)?,
                    (_, None) => write!(operands, "#{option}")?,
                }
            }
            Op::ClearExclusive => m.push("CLREX")?,
            Op::ExceptionReturn => {
                m.push("ERET")?;
                m.push(c)?;
            }
            Op::Transfer {
                op,
                rt,
                rn,
                up,
                offset,
                index,
            } => {
                let (base, suffix) = transfer_parts(op.name());
                m.push(base)?;
                m.push(c)?;
                m.push(suffix)?;
                write!(
                    operands,
                    "{rt},{}",
                    Address {
                        rn,
                        up,
                        offset,
                        index,
                    }
                )?;
            }
            Op::Dual {
                op,
                rt,
                rt2,
                rn,
                up,
                offset,
                index,
            } => {
                let (base, suffix) = transfer_parts(op.name());
                m.push(base)?;
                m.push(c)?;
                m.push(suffix)?;
                write!(
                    operands,
                    "{rt},{rt2},{}",
                    Address {
                        rn,
                        up,
                        offset,
                        index,
                    }
                )?;
            }
            Op::Preload { op, rn, up, offset } => {
                m.push(op.name())?;
                write!(
                    operands,
                    "{}",
                    Address {
                        rn,
                        up,
                        offset,
                        index: Index::Offset,
                    }
                )?;
            }
            Op::Block {
                op,
                rn,
                mode,
                writeback,
                user,
                list,
            } => {
                m.push(op.name())?;
                m.push(c)?;
                m.push(block_suffix(op, mode, rn == Reg::SP))?;
                write!(
                    operands,
                    "{rn}{},{list}{}",
                    if writeback { "!" } else { "" },
                    if user { "^" } else { "" }
                )?;
            }
            Op::Swap { op, rt, rt2, rn } => {
                m.push("SWP")?;
                m.push(c)?;
                m.push(if op == SwapOp::Swpb { "B" } else { "" })?;
                write!(operands, "{rt},{rt2},[{rn}]")?;
            }
            Op::Exclusive {
                op,
                rd,
                rt,
                rt2,
                rn,
                offset,
            } => {
                m.push(op.name())?;
                m.push(c)?;
                if let Some(rd) = rd {
                    write!(operands, "{rd},")?;
                }
                write!(operands, "{rt},")?;
                if let Some(rt2) = rt2 {
                    write!(operands, "{rt2},")?;
                }
                if offset == 0 {
                    write!(operands, "[{rn}]")?;
                } else {
                    write!(operands, "[{rn},#{}]", Number(u32::from(offset)))?;
                }
            }
            Op::ChangeState {
                imod,
                change_mode,
                interrupts,
                mode,
            } => {
                m.push(match imod {
                    2 => "CPSIE",
                    3 => "CPSID",
                    _ => "CPS",
                })?;
                if imod & 2 != 0 {
                    for (bit, name) in [(4, 'A'), (2, 'I'), (1, 'F')] {
                        if interrupts & bit != 0 {
                            operands.write_char(name)?;
                        }
                    }
                    if change_mode {
                        operands.write_char(',')?;
                    }
                }
                if change_mode {
                    write!(operands, "#{}", Number(u32::from(mode)))?;
                }
            }
            Op::SetEndian { big } => {
                m.push("SETEND")?;
                operands.write_str(if big { "BE" } else { "LE" })?;
            }
            Op::SaveState {
                mode,
                writeback,
                bank,
            } => {
                m.push("SRS")?;
                m.push(block_suffix(BlockOp::Stm, mode, false))?;
                write!(
                    operands,
                    "SP{},#{}",
                    if writeback { "!" } else { "" },
                    Number(u32::from(bank))
                )?;
            }
            Op::ReturnFromException {
                rn,
                mode,
                writeback,
            } => {
                m.push("RFE")?;
                m.push(block_suffix(BlockOp::Ldm, mode, false))?;
                write!(operands, "{rn}{}", if writeback { "!" } else { "" })?;
            }
            Op::Coprocessor {
                op,
                cp,
                opc1,
                crd,
                crn,
                crm,
                opc2,
            } => {
                m.push(op.name())?;
                m.push(c)?;
                write!(operands, "CP{cp},{opc1},C{crd},C{crn},C{crm},{opc2}")?;
            }
            Op::CoprocessorRegister {
                op,
                cp,
                opc1,
                rt,
                crn,
                crm,
                opc2,
            } => {
                m.push(op.name())?;
                m.push(c)?;
                write!(operands, "CP{cp},{opc1},{rt},C{crn},C{crm},{opc2}")?;
            }
            Op::CoprocessorRegisters {
                op,
                cp,
                opc1,
                rt,
                rt2,
                crm,
            } => {
                m.push(op.name())?;
                m.push(c)?;
                write!(operands, "CP{cp},{opc1},{rt},{rt2},C{crm}")?;
            }
            Op::FpaDyadic {
                op,
                precision,
                rounding,
                fd,
                fn_,
                fm,
            } => {
                m.push(op.name())?;
                m.push(c)?;
                m.push(precision_suffix(precision))?;
                m.push(rounding_suffix(rounding))?;
                write!(operands, "F{fd},F{fn_},{}", FpaOperandText(fm))?;
            }
            Op::FpaMonadic {
                op,
                precision,
                rounding,
                fd,
                fm,
            } => {
                m.push(op.name())?;
                m.push(c)?;
                m.push(precision_suffix(precision))?;
                m.push(rounding_suffix(rounding))?;
                write!(operands, "F{fd},{}", FpaOperandText(fm))?;
            }
            Op::FpaRegister {
                op,
                precision,
                rounding,
                fn_,
                fm,
                rd,
            } => {
                m.push(op.name())?;
                m.push(c)?;
                match op {
                    FpaRegisterOp::Flt => {
                        m.push(precision_suffix(precision))?;
                        m.push(rounding_suffix(rounding))?;
                        write!(operands, "F{fn_},{rd}")?;
                    }
                    FpaRegisterOp::Fix => {
                        m.push(rounding_suffix(rounding))?;
                        write!(operands, "{rd},{}", FpaOperandText(fm))?;
                    }
                    FpaRegisterOp::Wfs | FpaRegisterOp::Rfs => write!(operands, "{rd}")?,
                }
            }
            Op::FpaCompare {
                op, fn_, fm, rd, ..
            } => {
                if rd != Reg::PC {
                    // The flags to a register: no FPA syntax spells it.
                    return self.generic(f);
                }
                m.push(op.name())?;
                m.push(c)?;
                write!(operands, "F{fn_},{}", FpaOperandText(fm))?;
            }
            Op::FpaTransfer {
                op,
                format,
                fd,
                rn,
                up,
                index,
                offset,
            } => {
                if index == CoIndex::Unindexed {
                    return self.generic(f);
                }
                m.push(op.name())?;
                m.push(c)?;
                m.push(format_suffix(format))?;
                write!(operands, "F{fd},")?;
                address(&mut operands, rn, up, index, offset)?;
            }
            Op::FpaMultiple {
                op,
                fd,
                count,
                rn,
                up,
                index,
                offset,
            } => {
                if index == CoIndex::Unindexed {
                    return self.generic(f);
                }
                m.push(op.name())?;
                m.push(c)?;
                write!(operands, "F{fd},{count},")?;
                address(&mut operands, rn, up, index, offset)?;
            }
            Op::CoprocessorTransfer {
                op,
                cp,
                crd,
                rn,
                long,
                up,
                index,
                offset,
            } => {
                m.push(op.name())?;
                m.push(c)?;
                m.push(if long { "L" } else { "" })?;
                write!(operands, "CP{cp},C{crd},")?;
                address(&mut operands, rn, up, index, offset)?;
            }
        }
        if m.overflowed || operands.overflowed {
            return f.write_str("(unrepresentable)");
        }
        let mnemonic = m.as_str();
        let text = operands.as_str();
        if text.is_empty() {
            f.write_str(mnemonic)
        } else {
            // A space at least, however long the mnemonic.
            write!(
                f,
                "{mnemonic:<width$}{text}",
                width = MNEMONIC_WIDTH.max(mnemonic.len() + 1)
            )
        }
    }
}
