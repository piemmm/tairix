//! The integer instructions' suffixes and operands, as RISC OS 5's BASIC
//! assembler reads them: the classic instructions in the old order, the
//! condition before a suffix, and those ARMv5 added on in UAL's.

use tairix_riscos_arm::a32;
use tairix_riscos_arm::cond::Condition;
use tairix_riscos_arm::instruction::{
    ArithmeticOp, BarrierOp, BitfieldOp, BranchOp, CallOp, CompareOp, DataOp, EncodeError,
    ExchangeOp, ExclusiveOp, ExtendOp, HintOp, Immediate, Instruction, LongOp, MoveOp, MultiplyOp,
    Op, Operand, PackOp, SaturateOp, Shift, ShiftType, Source, TransferOp,
};
use tairix_riscos_arm::reg::Reg;

use super::mnemonic::{Form, Word};
use super::parse::{reg_of, Parse, Parser, Want};
use crate::messages::Error;

/// The rotation BASIC's search overran to, laid down under suppressed errors
/// with the whole of an immediate no rotation spells.
const OVERRUN: u32 = 16 << 8;

/// The parser moved to where `word`'s operands begin.
pub(super) fn start(p: &mut Parser<'_>, word: &Word) -> Parse<()> {
    let offset = word.text_offset().ok_or(Error::NoSuchMnemonic)?;
    p.seek(offset);
    Ok(())
}

/// `instruction`'s word with `garbage`'s bits set: an operation A32 encodes
/// unconditionally takes no condition.
pub(super) fn encoded(instruction: &Instruction, garbage: u32) -> Parse<u32> {
    let mut instruction = *instruction;
    if a32::unconditional_op(&instruction.op) {
        if instruction.cond != Condition::Al {
            return Err(Error::Syntax.into());
        }
        instruction.cond = Condition::Nv;
    }
    a32::encode(&instruction)
        .map(|word| word | garbage)
        .map_err(encode_error)
}

/// A generic coprocessor `instruction`'s word.
pub(super) fn encoded_coprocessor(instruction: &Instruction) -> Parse<u32> {
    a32::encode_coprocessor(instruction).map_err(encode_error)
}

fn encode_error(error: EncodeError) -> super::parse::Halt {
    match error {
        EncodeError::Range => Error::BadImmediate.into(),
        EncodeError::Unencodable => Error::BadRegister.into(),
    }
}

/// The word of an integer instruction of `form`.
#[allow(clippy::too_many_lines)] // One arm per form, each its own syntax.
pub(super) fn instruction(
    p: &mut Parser<'_>,
    word: &mut Word,
    form: Form,
    cond: Condition,
) -> Parse<u32> {
    let op = match form {
        Form::Data(op) => return data(p, word, cond, op),
        Form::Compare(op) => return compare(p, word, cond, op),
        Form::Move(op) => {
            let s = word.take("S");
            start(p, word)?;
            let rd = p.register()?;
            p.comma()?;
            let (operand, garbage) = operand2(p)?;
            return encoded(
                &Instruction {
                    cond,
                    op: Op::Move { op, s, rd, operand },
                },
                garbage,
            );
        }
        Form::Wide(op) => {
            start(p, word)?;
            let rd = p.register()?;
            p.comma()?;
            let value = u16::try_from(p.hash_int()?).map_err(|_| Error::BadImmediate)?;
            Op::Wide { op, rd, value }
        }
        Form::Adr => return adr(p, word, cond),
        Form::Mul | Form::Mla | Form::Mls => return multiply(p, word, cond, form),
        Form::Long(op) => {
            let s = word.take("S");
            start(p, word)?;
            let [lo, hi, rn, rm] = registers(p)?;
            if lo == hi || lo == rn || hi == rn {
                return Err(Error::DuplicateRegister.into());
            }
            Op::MultiplyLong {
                op,
                s,
                x: false,
                y: false,
                lo,
                hi,
                rn,
                rm,
            }
        }
        Form::Halves { op, x, y } => {
            start(p, word)?;
            let accumulates = matches!(op, MultiplyOp::Smla | MultiplyOp::Smlaw);
            let rd = p.register()?;
            p.comma()?;
            let rn = p.register()?;
            p.comma()?;
            let rm = p.register()?;
            let ra = if accumulates {
                p.comma()?;
                Some(p.register()?)
            } else {
                None
            };
            Op::Multiply {
                op,
                s: false,
                x,
                y,
                rd,
                rn,
                rm,
                ra,
            }
        }
        Form::LongHalves { x, y } => {
            start(p, word)?;
            let [lo, hi, rn, rm] = registers(p)?;
            if lo == hi {
                return Err(Error::DuplicateRegister.into());
            }
            Op::MultiplyLong {
                op: LongOp::SmlalHalves,
                s: false,
                x,
                y,
                lo,
                hi,
                rn,
                rm,
            }
        }
        Form::Multiply { op, x } => {
            start(p, word)?;
            let accumulates = matches!(
                op,
                MultiplyOp::Smlad
                    | MultiplyOp::Smlsd
                    | MultiplyOp::Smmla
                    | MultiplyOp::Smmls
                    | MultiplyOp::Usada8
            );
            let rd = p.register()?;
            p.comma()?;
            let rn = p.register()?;
            p.comma()?;
            let rm = p.register()?;
            let ra = if accumulates {
                p.comma()?;
                Some(p.register()?)
            } else {
                None
            };
            Op::Multiply {
                op,
                s: false,
                x,
                y: false,
                rd,
                rn,
                rm,
                ra,
            }
        }
        Form::LongDual { op, x } => {
            start(p, word)?;
            let [lo, hi, rn, rm] = registers(p)?;
            Op::MultiplyLong {
                op,
                s: false,
                x,
                y: false,
                lo,
                hi,
                rn,
                rm,
            }
        }
        Form::Parallel(prefix, lanes) => {
            start(p, word)?;
            let [rd, rn, rm] = registers(p)?;
            Op::Parallel {
                prefix,
                lanes,
                rd,
                rn,
                rm,
            }
        }
        Form::Arithmetic(op) => return arithmetic(p, word, cond, op),
        Form::Unary(op) => {
            start(p, word)?;
            let rd = p.register()?;
            p.comma()?;
            let rm = p.register()?;
            Op::Unary { op, rd, rm }
        }
        Form::Saturate(op) => saturate(p, word, op)?,
        Form::Extend(op) => extend(p, word, op)?,
        Form::Pack(op) => pack(p, word, op)?,
        Form::Bitfield(op) => bitfield(p, word, op)?,
        Form::Mrs => {
            start(p, word)?;
            let rd = p.register()?;
            p.comma()?;
            p.spaces();
            let spsr = if p.whole_word("CPSR") || p.whole_word("APSR") {
                false
            } else if p.whole_word("SPSR") {
                true
            } else {
                return Err(Error::Syntax.into());
            };
            Op::Status { spsr, rd }
        }
        Form::Msr => return msr(p, word, cond),
        Form::Branch(op) => {
            start(p, word)?;
            let offset = branch_offset(p)?;
            Op::Branch { op, offset }
        }
        Form::Exchange(op) => exchange(p, word, cond, op)?,
        Form::Call(op) => {
            if op == CallOp::Hvc && cond != Condition::Al {
                return Err(Error::NoSuchMnemonic.into());
            }
            start(p, word)?;
            p.take(b'#');
            let value = call_value(p, op)?;
            if op == CallOp::Bkpt && cond != Condition::Al {
                // BASIC lays a conditional BKPT down as written, though the
                // architecture defines only an unconditional one.
                let word = encoded(
                    &Instruction {
                        cond: Condition::Al,
                        op: Op::Call { op, value },
                    },
                    0,
                )?;
                return Ok(word & 0x0FFF_FFFF | cond.bits() << 28);
            }
            Op::Call { op, value }
        }
        Form::Smi => {
            start(p, word)?;
            Op::Call {
                op: CallOp::Smc,
                value: call_value(p, CallOp::Smc)?,
            }
        }
        Form::Hint(op) => {
            start(p, word)?;
            let value = if op == HintOp::Dbg {
                p.take(b'#');
                u8::try_from(p.int(Want::Expression)?)
                    .ok()
                    .filter(|&v| v < 16)
                    .ok_or(Error::BadImmediate)?
            } else {
                0
            };
            Op::Hint { op, value }
        }
        Form::Nop => {
            start(p, word)?;
            Op::Move {
                op: MoveOp::Mov,
                s: false,
                rd: Reg::field(0),
                operand: Operand::Register {
                    rm: Reg::field(0),
                    shift: Shift::NONE,
                },
            }
        }
        Form::Barrier(op) => {
            start(p, word)?;
            Op::Barrier {
                op,
                option: barrier_option(p, op)?,
            }
        }
        Form::ClearExclusive => {
            start(p, word)?;
            Op::ClearExclusive
        }
        Form::ExceptionReturn => {
            start(p, word)?;
            Op::ExceptionReturn
        }
        Form::ChangeState(imod) => change_state(p, word, imod)?,
        Form::SetEndian => {
            start(p, word)?;
            p.spaces();
            let big = if p.word("BE") {
                true
            } else if p.word("LE") {
                false
            } else {
                return Err(Error::Syntax.into());
            };
            Op::SetEndian { big }
        }
        Form::SaveState(mode) => {
            start(p, word)?;
            if p.register()? != Reg::SP {
                return Err(Error::BadRegister.into());
            }
            let writeback = p.writeback();
            p.comma()?;
            let bank = u8::try_from(p.hash_int()?)
                .ok()
                .filter(|&m| m < 32)
                .ok_or(Error::BadImmediate)?;
            Op::SaveState {
                mode,
                writeback,
                bank,
            }
        }
        Form::ReturnFromException(mode) => {
            start(p, word)?;
            let rn = p.register()?;
            let writeback = p.writeback();
            Op::ReturnFromException {
                rn,
                mode,
                writeback,
            }
        }
        Form::Transfer { load } => return super::memory::transfer(p, word, cond, load),
        Form::Preload(op) => return super::memory::preload(p, word, op),
        Form::Block { load } => return super::memory::block(p, word, cond, load),
        Form::Stack { load } => return super::memory::stack(p, word, cond, load),
        Form::Swap => return super::memory::swap(p, word, cond),
        Form::Exclusive(op) => return super::memory::exclusive(p, word, cond, op),
        Form::Ordered { load } => {
            let op = match (load, word.take("B"), word.take("H")) {
                (true, true, _) => ExclusiveOp::Ldab,
                (true, _, true) => ExclusiveOp::Ldah,
                (true, ..) => ExclusiveOp::Lda,
                (false, true, _) => ExclusiveOp::Stlb,
                (false, _, true) => ExclusiveOp::Stlh,
                (false, ..) => ExclusiveOp::Stl,
            };
            return super::memory::exclusive_operands(p, word, cond, op);
        }
        _ => return Err(Error::NoSuchMnemonic.into()),
    };
    encoded(&Instruction { cond, op }, 0)
}

/// Data processing: `S`, then Rd, Rn and the second operand.
fn data(p: &mut Parser<'_>, word: &mut Word, cond: Condition, op: DataOp) -> Parse<u32> {
    let s = word.take("S");
    start(p, word)?;
    let rd = p.register()?;
    p.comma()?;
    let rn = p.register()?;
    p.comma()?;
    let (operand, garbage) = operand2(p)?;
    encoded(
        &Instruction {
            cond,
            op: Op::Data {
                op,
                s,
                rd,
                rn,
                operand,
            },
        },
        garbage,
    )
}

/// A comparison: `S` or `P`, then Rn and the second operand; `P` sets Rd's
/// field to R15 as the 26-bit cores read it.
fn compare(p: &mut Parser<'_>, word: &mut Word, cond: Condition, op: CompareOp) -> Parse<u32> {
    let psr = !word.take("S") && word.take("P");
    start(p, word)?;
    let rn = p.register()?;
    p.comma()?;
    let (operand, garbage) = operand2(p)?;
    let rd = if psr { Reg::PC } else { Reg::field(0) };
    encoded(
        &Instruction {
            cond,
            op: Op::Compare {
                op,
                rd,
                rn,
                operand,
            },
        },
        garbage,
    )
}

/// The second operand: `#` and a rotated immediate, or a register and any
/// shift of it; and the garbage a suppressed error leaves.
pub(super) fn operand2(p: &mut Parser<'_>) -> Parse<(Operand, u32)> {
    if p.take(b'#') {
        let value = p.int(Want::Expression)?.cast_unsigned();
        return immediate(p, value);
    }
    let rm = p.register()?;
    if !p.take(b',') {
        return Ok((
            Operand::Register {
                rm,
                shift: Shift::NONE,
            },
            0,
        ));
    }
    Ok((shifted(p, rm)?, 0))
}

/// `value` as a rotated immediate, or under suppressed errors the garbage
/// BASIC lays down for one no rotation spells.
pub(super) fn immediate(p: &Parser<'_>, value: u32) -> Parse<(Operand, u32)> {
    match Immediate::encode(value) {
        Some(imm) => Ok((Operand::Immediate(imm), 0)),
        None if p.context.suppress => Ok((
            Operand::Immediate(Immediate {
                value: 0,
                rotation: 0,
            }),
            value | OVERRUN,
        )),
        None => Err(Error::BadImmediate.into()),
    }
}

/// The kinds of shift a name gives.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shifting {
    By(ShiftType),
    Rrx,
}

/// A shift's name: `ASL` is `LSL`.
fn shift_name(p: &mut Parser<'_>) -> Parse<Shifting> {
    p.spaces();
    Ok(if p.word("LSL") || p.word("ASL") {
        Shifting::By(ShiftType::Lsl)
    } else if p.word("LSR") {
        Shifting::By(ShiftType::Lsr)
    } else if p.word("ASR") {
        Shifting::By(ShiftType::Asr)
    } else if p.word("ROR") {
        Shifting::By(ShiftType::Ror)
    } else if p.word("RRX") {
        Shifting::Rrx
    } else {
        return Err(Error::Syntax.into());
    })
}

/// `kind` by `amount`, or "Bad shift" where its field cannot hold it.
pub(super) fn immediate_shift(kind: ShiftType, amount: i32) -> Parse<Shift> {
    let amount = u8::try_from(amount).map_err(|_| Error::BadShift)?;
    let shift = match (kind, amount) {
        (ShiftType::Lsl, 0..=31) => Shift::Lsl(amount),
        (ShiftType::Lsr, 1..=32) => Shift::Lsr(amount),
        (ShiftType::Asr, 1..=32) => Shift::Asr(amount),
        (ShiftType::Ror, 1..=31) => Shift::Ror(amount),
        _ => return Err(Error::BadShift.into()),
    };
    Ok(shift)
}

/// `rm` shifted as the text after its comma says: by an immediate, by a
/// register, or `RRX`.
fn shifted(p: &mut Parser<'_>, rm: Reg) -> Parse<Operand> {
    let kind = match shift_name(p)? {
        Shifting::Rrx => {
            return Ok(Operand::Register {
                rm,
                shift: Shift::Rrx,
            })
        }
        Shifting::By(kind) => kind,
    };
    if p.take(b'#') {
        let amount = p.int(Want::Expression)?;
        return Ok(Operand::Register {
            rm,
            shift: immediate_shift(kind, amount)?,
        });
    }
    let rs = p.register()?;
    Ok(Operand::Shifted { rm, kind, rs })
}

/// A shift by an immediate after a comma, for an offset register: a shift by
/// a register is "Bad shift".
pub(super) fn offset_shift(p: &mut Parser<'_>) -> Parse<Shift> {
    let kind = match shift_name(p)? {
        Shifting::Rrx => return Ok(Shift::Rrx),
        Shifting::By(kind) => kind,
    };
    if !p.take(b'#') {
        return Err(Error::BadShift.into());
    }
    let amount = p.int(Want::Expression)?;
    immediate_shift(kind, amount)
}

/// `N` registers, each after the last's comma.
fn registers<const N: usize>(p: &mut Parser<'_>) -> Parse<[Reg; N]> {
    let mut registers = [Reg::field(0); N];
    for (index, register) in registers.iter_mut().enumerate() {
        if index > 0 {
            p.comma()?;
        }
        *register = p.register()?;
    }
    Ok(registers)
}

/// `MUL`, `MLA` and `MLS`: Rd may not be Rm in the first two.
fn multiply(p: &mut Parser<'_>, word: &mut Word, cond: Condition, form: Form) -> Parse<u32> {
    let s = form != Form::Mls && word.take("S");
    start(p, word)?;
    let rd = p.register()?;
    p.comma()?;
    let rn = p.register()?;
    p.comma()?;
    let rm = p.register()?;
    let ra = if form == Form::Mul {
        None
    } else {
        p.comma()?;
        Some(p.register()?)
    };
    if form != Form::Mls && rd == rn {
        return Err(Error::DuplicateRegister.into());
    }
    let op = match form {
        Form::Mul => MultiplyOp::Mul,
        Form::Mla => MultiplyOp::Mla,
        _ => MultiplyOp::Mls,
    };
    encoded(
        &Instruction {
            cond,
            op: Op::Multiply {
                op,
                s,
                x: false,
                y: false,
                rd,
                rn,
                rm,
                ra,
            },
        },
        0,
    )
}

/// The saturating additions, `SEL` and the checksums: the checksums take no
/// condition, and their sources only registers' R names.
fn arithmetic(
    p: &mut Parser<'_>,
    word: &mut Word,
    cond: Condition,
    op: ArithmeticOp,
) -> Parse<u32> {
    start(p, word)?;
    let rd = p.register()?;
    p.comma()?;
    let op_ = if op.checksum() {
        let rn = named(p)?;
        p.comma()?;
        let rm = named(p)?;
        Op::Arithmetic { op, rd, rn, rm }
    } else if op == ArithmeticOp::Sel {
        let rn = p.register()?;
        p.comma()?;
        let rm = p.register()?;
        Op::Arithmetic { op, rd, rn, rm }
    } else {
        let rm = p.register()?;
        p.comma()?;
        let rn = p.register()?;
        Op::Arithmetic { op, rd, rn, rm }
    };
    encoded(&Instruction { cond, op: op_ }, 0)
}

/// A register given only by an R name, as the checksums and the packs'
/// last operand take it: a variable's value otherwise, and a syntax error
/// where no name is there.
fn named(p: &mut Parser<'_>) -> Parse<Reg> {
    if let Some(reg) = (p.spaces(), p.register_name(false)).1 {
        return Ok(reg);
    }
    if !crate::machine::starts_name(p.peek()) {
        return Err(Error::Syntax.into());
    }
    let n = p.int(Want::Factor)?;
    reg_of(n)
}

/// The saturations: the bit position, then for `SSAT` and `USAT` a shift.
fn saturate(p: &mut Parser<'_>, word: &Word, op: SaturateOp) -> Parse<Op> {
    start(p, word)?;
    let rd = p.register()?;
    p.comma()?;
    let position = p.hash_int()?;
    let (least, most) = match op {
        SaturateOp::Ssat => (1, 32),
        SaturateOp::Usat => (0, 31),
        SaturateOp::Ssat16 => (1, 16),
        SaturateOp::Usat16 => (0, 15),
    };
    let saturate = u8::try_from(position)
        .ok()
        .filter(|position| (least..=most).contains(position))
        .ok_or(Error::BadImmediate)?
        - least;
    p.comma()?;
    let rn = p.register()?;
    let shift = if matches!(op, SaturateOp::Ssat | SaturateOp::Usat) && p.take(b',') {
        let shift = offset_shift(p)?;
        if !matches!(shift, Shift::Lsl(_) | Shift::Asr(_)) {
            return Err(Error::BadShift.into());
        }
        shift
    } else {
        Shift::NONE
    };
    Ok(Op::Saturate {
        op,
        rd,
        saturate,
        rn,
        shift,
    })
}

/// The extensions: an addend for the forms with one, then a rotation of
/// 8, 16 or 24.
fn extend(p: &mut Parser<'_>, word: &Word, op: ExtendOp) -> Parse<Op> {
    start(p, word)?;
    let adds = matches!(
        op,
        ExtendOp::Sxtab16
            | ExtendOp::Sxtab
            | ExtendOp::Sxtah
            | ExtendOp::Uxtab16
            | ExtendOp::Uxtab
            | ExtendOp::Uxtah
    );
    let rd = p.register()?;
    p.comma()?;
    let rn = if adds {
        let rn = p.register()?;
        p.comma()?;
        Some(rn)
    } else {
        None
    };
    let rm = p.register()?;
    let rotation = if p.take(b',') {
        p.spaces();
        if !p.word("ROR") {
            return Err(Error::BadShift.into());
        }
        match p.hash_int()? {
            0 => 0,
            8 => 1,
            16 => 2,
            24 => 3,
            _ => return Err(Error::BadShift.into()),
        }
    } else {
        0
    };
    Ok(Op::Extend {
        op,
        rd,
        rn,
        rm,
        rotation,
    })
}

/// The packs: `PKHTB` with no shift is `PKHBT` with its sources swapped.
fn pack(p: &mut Parser<'_>, word: &Word, op: PackOp) -> Parse<Op> {
    start(p, word)?;
    let rd = p.register()?;
    p.comma()?;
    let rn = p.register()?;
    p.comma()?;
    let rm = named(p)?;
    if !p.take(b',') {
        if op == PackOp::Pkhtb {
            return Ok(Op::Pack {
                op: PackOp::Pkhbt,
                rd,
                rn: rm,
                rm: rn,
                shift: Shift::NONE,
            });
        }
        return Ok(Op::Pack {
            op,
            rd,
            rn,
            rm,
            shift: Shift::NONE,
        });
    }
    let shift = offset_shift(p)?;
    let fits = match op {
        PackOp::Pkhbt => matches!(shift, Shift::Lsl(_)),
        PackOp::Pkhtb => matches!(shift, Shift::Asr(_)),
    };
    if !fits {
        return Err(Error::BadShift.into());
    }
    Ok(Op::Pack {
        op,
        rd,
        rn,
        rm,
        shift,
    })
}

/// The bitfield operations: the field's least significant bit and width.
fn bitfield(p: &mut Parser<'_>, word: &Word, op: BitfieldOp) -> Parse<Op> {
    start(p, word)?;
    let rd = p.register()?;
    p.comma()?;
    let rn = if op == BitfieldOp::Bfc {
        None
    } else {
        let rn = p.register()?;
        p.comma()?;
        Some(rn)
    };
    let lsb = p.hash_int()?;
    p.comma()?;
    let width = p.hash_int()?;
    // An insertion or a clear may be of no bits, an extraction not.
    let least = u8::from(matches!(op, BitfieldOp::Sbfx | BitfieldOp::Ubfx));
    let within = |n: i32, most: u8| u8::try_from(n).ok().filter(|&n| n <= most);
    let lsb = within(lsb, 31).ok_or(Error::BadImmediate)?;
    let width = within(width, 32 - lsb)
        .filter(|&width| width >= least)
        .ok_or(Error::BadImmediate)?;
    let top = match op {
        BitfieldOp::Sbfx | BitfieldOp::Ubfx => width - 1,
        // No bits at bit nought leave no top bit to lay down.
        BitfieldOp::Bfi | BitfieldOp::Bfc => {
            (lsb + width).checked_sub(1).ok_or(Error::BadImmediate)?
        }
    };
    Ok(Op::Bitfield {
        op,
        rd,
        rn,
        lsb,
        top,
    })
}

/// `MSR`: the status register and the fields written, then a register or an
/// immediate.
fn msr(p: &mut Parser<'_>, word: &Word, cond: Condition) -> Parse<u32> {
    start(p, word)?;
    p.spaces();
    let (spsr, fields) = if p.word("APSR") {
        if !p.take(b'_') {
            return Err(Error::BadRegister.into());
        }
        let mut fields = 0;
        if p.word("NZCVQ") {
            fields |= 8;
        }
        if p.word("G") {
            fields |= 4;
        }
        (false, fields)
    } else {
        let spsr = if p.word("CPSR") {
            false
        } else if p.word("SPSR") {
            true
        } else {
            return Err(Error::BadRegister.into());
        };
        if p.peek() != b'_' {
            return Err(Error::BadRegister.into());
        }
        p.advance(1);
        let mut fields = 0;
        loop {
            fields |= match p.peek().to_ascii_lowercase() {
                b'c' => 1,
                b'x' => 2,
                b's' => 4,
                b'f' => 8,
                _ => break,
            };
            p.advance(1);
        }
        (spsr, fields)
    };
    p.comma()?;
    if fields == 0 {
        return Err(Error::BadRegister.into());
    }
    let (source, garbage) = if p.take(b'#') {
        let value = p.int(Want::Expression)?.cast_unsigned();
        match immediate(p, value)? {
            (Operand::Immediate(imm), garbage) => (Source::Immediate(imm), garbage),
            _ => return Err(Error::BadImmediate.into()),
        }
    } else {
        (Source::Register(p.register()?), 0)
    };
    encoded(
        &Instruction {
            cond,
            op: Op::StatusWrite {
                spsr,
                fields,
                source,
            },
        },
        garbage,
    )
}

/// A call's number: a SWI's, a breakpoint's and an undefined instruction's
/// masked to their fields, the monitor's and the hypervisor's refused past
/// theirs, errors suppressed or not.
fn call_value(p: &mut Parser<'_>, op: CallOp) -> Parse<u32> {
    let most = match op {
        CallOp::Swi => return Ok(p.int(Want::Swi)?.cast_unsigned() & 0xFF_FFFF),
        CallOp::Bkpt | CallOp::Udf => return Ok(p.int(Want::Expression)?.cast_unsigned() & 0xFFFF),
        CallOp::Smc => 0xF,
        CallOp::Hvc => 0xFFFF,
    };
    u32::try_from(p.int(Want::Expression)?)
        .ok()
        .filter(|&value| value <= most)
        .ok_or_else(|| Error::BadImmediate.into())
}

/// A branch's target, as its offset from the PC the instruction reads:
/// rounded to a word as BASIC rounds one, and refused past 32 megabytes.
fn branch_offset(p: &mut Parser<'_>) -> Parse<i32> {
    let target = p.int(Want::Expression)?;
    let from = p.context.address.wrapping_add(8);
    let distance = target.cast_unsigned().wrapping_sub(from).cast_signed();
    let words = distance.wrapping_add(2) >> 2;
    if !(-(1 << 23)..(1 << 23)).contains(&words) {
        return Err(Error::BadOffset.into());
    }
    Ok(words << 2)
}

/// `BX`, `BXJ` and `BLX`: `BLX` without a condition to an address where its
/// operand names no register.
fn exchange(p: &mut Parser<'_>, word: &Word, cond: Condition, op: ExchangeOp) -> Parse<Op> {
    start(p, word)?;
    p.spaces();
    if op == ExchangeOp::Blx && cond == Condition::Al {
        if let Some(rm) = p.register_name(true) {
            return Ok(Op::Exchange { op, rm });
        }
        let target = p.int(Want::Expression)?;
        let from = p.context.address.wrapping_add(8);
        let distance = target.cast_unsigned().wrapping_sub(from).cast_signed();
        let halves = distance.wrapping_add(1) >> 1;
        if !(-(1 << 24)..(1 << 24)).contains(&halves) {
            return Err(Error::BadOffset.into());
        }
        return Ok(Op::Branch {
            op: BranchOp::Blx,
            offset: halves << 1,
        });
    }
    let rm = p.register()?;
    Ok(Op::Exchange { op, rm })
}

/// A barrier's option: its name, or full system where none is given.
fn barrier_option(p: &mut Parser<'_>, op: BarrierOp) -> Parse<u8> {
    const OPTIONS: [(&str, u8); 12] = [
        ("ISHST", 10),
        ("ISHLD", 9),
        ("NSHST", 6),
        ("NSHLD", 5),
        ("OSHST", 2),
        ("OSHLD", 1),
        ("ISH", 11),
        ("NSH", 7),
        ("OSH", 3),
        ("SY", 15),
        ("ST", 14),
        ("LD", 13),
    ];
    if matches!(op, BarrierOp::Ssbb | BarrierOp::Pssbb) {
        return Ok(0);
    }
    p.spaces();
    if matches!(p.peek(), b':' | b'\r' | b';' | b'\\' | crate::token::REM) {
        return Ok(15);
    }
    for (name, option) in OPTIONS {
        if (op != BarrierOp::Isb || option == 15) && p.word(name) {
            return Ok(option);
        }
    }
    Err(Error::Syntax.into())
}

/// `CPS`, `CPSIE` and `CPSID`: the interrupts named, then a mode.
fn change_state(p: &mut Parser<'_>, word: &Word, imod: u8) -> Parse<Op> {
    start(p, word)?;
    let mut interrupts = 0;
    if imod != 0 {
        p.spaces();
        loop {
            interrupts |= match p.peek().to_ascii_lowercase() {
                b'a' => 4,
                b'i' => 2,
                b'f' => 1,
                _ => break,
            };
            p.advance(1);
        }
        if !p.take(b',') {
            return Ok(Op::ChangeState {
                imod,
                change_mode: false,
                interrupts,
                mode: 0,
            });
        }
    }
    p.take(b'#');
    let mode = u8::try_from(p.int(Want::Expression)?)
        .ok()
        .filter(|&m| m < 32)
        .ok_or(Error::BadImmediate)?;
    Ok(Op::ChangeState {
        imod,
        change_mode: true,
        interrupts,
        mode,
    })
}

/// `ADR`: the address's distance from the PC as an `ADD` or `SUB` to R15.
fn adr(p: &mut Parser<'_>, word: &Word, cond: Condition) -> Parse<u32> {
    start(p, word)?;
    let rd = p.register()?;
    p.comma()?;
    let target = p.int(Want::Expression)?;
    let from = p.context.address.wrapping_add(8);
    let distance = target.cast_unsigned().wrapping_sub(from).cast_signed();
    let (op, magnitude) = if distance < 0 {
        (DataOp::Sub, distance.unsigned_abs())
    } else {
        (DataOp::Add, distance.cast_unsigned())
    };
    let (operand, garbage) = immediate(p, magnitude)?;
    encoded(
        &Instruction {
            cond,
            op: Op::Data {
                op,
                s: false,
                rd,
                rn: Reg::PC,
                operand,
            },
        },
        garbage,
    )
}

/// The transfer each suffix names, by whether it loads.
pub(super) const fn transfer_op(load: bool, suffix: &str) -> Option<TransferOp> {
    Some(match (load, suffix.as_bytes()) {
        (true, b"") => TransferOp::Ldr,
        (false, b"") => TransferOp::Str,
        (true, b"B") => TransferOp::Ldrb,
        (false, b"B") => TransferOp::Strb,
        (true, b"T") => TransferOp::Ldrt,
        (false, b"T") => TransferOp::Strt,
        (true, b"BT") => TransferOp::Ldrbt,
        (false, b"BT") => TransferOp::Strbt,
        (true, b"H") => TransferOp::Ldrh,
        (false, b"H") => TransferOp::Strh,
        (true, b"SB") => TransferOp::Ldrsb,
        (true, b"SH") => TransferOp::Ldrsh,
        (true, b"HT") => TransferOp::Ldrht,
        (false, b"HT") => TransferOp::Strht,
        (true, b"SBT") => TransferOp::Ldrsbt,
        (true, b"SHT") => TransferOp::Ldrsht,
        _ => return None,
    })
}
