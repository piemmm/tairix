//! The FPA's instructions, and the generic coprocessor instructions: their
//! suffixes and operands.

use tairix_riscos_arm::cond::Condition;
use tairix_riscos_arm::fpa::{Format, Precision, Rounding};
use tairix_riscos_arm::instruction::{
    CoIndex, CoRegisterOp, CoTransferOp, CoprocessorOp, FpaMultipleOp, FpaOperand, FpaRegisterOp,
    Instruction, Op,
};
use tairix_riscos_arm::reg::Reg;

use super::core::{encoded, encoded_coprocessor, start};
use super::mnemonic::{Form, Word};
use super::parse::{Parse, Parser, Want};
use crate::machine::in_name;
use crate::messages::Error;

/// The eight constants an FPA operand may be, as doubles' bits, by index.
const CONSTANTS: [u64; 8] = [
    0,
    0x3FF0_0000_0000_0000,
    0x4000_0000_0000_0000,
    0x4008_0000_0000_0000,
    0x4010_0000_0000_0000,
    0x4014_0000_0000_0000,
    0x3FE0_0000_0000_0000,
    0x4024_0000_0000_0000,
];

/// The word of an FPA or generic coprocessor instruction of `form`.
#[expect(clippy::too_many_lines, reason = "one arm per form")]
pub(super) fn instruction(
    p: &mut Parser<'_>,
    word: &mut Word,
    form: Form,
    cond: Condition,
) -> Parse<u32> {
    let op = match form {
        Form::FpaDyadic(op) => {
            let (precision, rounding) = precision_and_rounding(word)?;
            start(p, word)?;
            let fd = register(p)?;
            p.comma()?;
            let fn_ = register(p)?;
            p.comma()?;
            let fm = operand(p)?;
            Op::FpaDyadic {
                op,
                precision,
                rounding,
                fd,
                fn_,
                fm,
            }
        }
        Form::FpaMonadic(op) => {
            let (precision, rounding) = precision_and_rounding(word)?;
            start(p, word)?;
            let fd = register(p)?;
            p.comma()?;
            let fm = operand(p)?;
            Op::FpaMonadic {
                op,
                precision,
                rounding,
                fd,
                fm,
            }
        }
        Form::Flt => {
            let (precision, rounding) = precision_and_rounding(word)?;
            start(p, word)?;
            let fn_ = register(p)?;
            p.comma()?;
            let rd = p.register()?;
            Op::FpaRegister {
                op: FpaRegisterOp::Flt,
                precision,
                rounding,
                fn_,
                fm: FpaOperand::Register(0),
                rd,
            }
        }
        Form::Fix => {
            let rounding = rounding(word);
            start(p, word)?;
            let rd = p.register()?;
            p.comma()?;
            let fm = FpaOperand::Register(register(p)?);
            Op::FpaRegister {
                op: FpaRegisterOp::Fix,
                precision: Precision::Single,
                rounding,
                fn_: 0,
                fm,
                rd,
            }
        }
        Form::FpaStatus { write } => {
            start(p, word)?;
            let rd = p.register()?;
            let op = if write {
                FpaRegisterOp::Wfs
            } else {
                FpaRegisterOp::Rfs
            };
            Op::FpaRegister {
                op,
                precision: Precision::Single,
                rounding: Rounding::Nearest,
                fn_: 0,
                fm: FpaOperand::Register(0),
                rd,
            }
        }
        Form::FpaControl { write } => {
            start(p, word)?;
            let rt = p.register()?;
            let op = if write {
                CoRegisterOp::Mcr
            } else {
                CoRegisterOp::Mrc
            };
            let op = Op::CoprocessorRegister {
                op,
                cp: 1,
                opc1: 2,
                rt,
                crn: 0,
                crm: 0,
                opc2: 0,
            };
            return encoded_coprocessor(&Instruction { cond, op });
        }
        Form::FpaCompare(op) => {
            start(p, word)?;
            let fn_ = register(p)?;
            p.comma()?;
            let fm = operand(p)?;
            Op::FpaCompare {
                op,
                precision: Precision::Single,
                fn_,
                fm,
                rd: Reg::PC,
            }
        }
        Form::FpaTransfer(op) => {
            let format = format(word)?;
            start(p, word)?;
            let fd = register(p)?;
            p.comma()?;
            let address = address(p, false)?;
            let (rn, up, index, offset) = address.fixed()?;
            Op::FpaTransfer {
                op,
                format,
                fd,
                rn,
                up,
                index,
                offset,
            }
        }
        Form::FpaMultiple(op) => return multiple(p, word, cond, op),
        Form::Coprocessor(op) => {
            start(p, word)?;
            let cp = number(p)?;
            p.comma()?;
            let opc1 = opcode(p, 15)?;
            p.comma()?;
            let crd = co_register(p)?;
            p.comma()?;
            let crn = co_register(p)?;
            p.comma()?;
            let crm = co_register(p)?;
            let opc2 = if p.take(b',') { opcode(p, 7)? } else { 0 };
            let cond = if op == CoprocessorOp::Cdp2 {
                Condition::Nv
            } else {
                cond
            };
            let op = Op::Coprocessor {
                op,
                cp,
                opc1,
                crd,
                crn,
                crm,
                opc2,
            };
            return encoded_coprocessor(&Instruction { cond, op });
        }
        Form::CoRegister(op) => {
            start(p, word)?;
            let cp = number(p)?;
            p.comma()?;
            let opc1 = opcode(p, 7)?;
            p.comma()?;
            let rt = p.register()?;
            p.comma()?;
            let crn = co_register(p)?;
            p.comma()?;
            let crm = co_register(p)?;
            let opc2 = if p.take(b',') { opcode(p, 7)? } else { 0 };
            let cond = if matches!(op, CoRegisterOp::Mcr2 | CoRegisterOp::Mrc2) {
                Condition::Nv
            } else {
                cond
            };
            let op = Op::CoprocessorRegister {
                op,
                cp,
                opc1,
                rt,
                crn,
                crm,
                opc2,
            };
            return encoded_coprocessor(&Instruction { cond, op });
        }
        Form::CoRegisters(op) => {
            start(p, word)?;
            let cp = number(p)?;
            p.comma()?;
            let opc1 = opcode(p, 15)?;
            p.comma()?;
            let rt = p.register()?;
            p.comma()?;
            let rt2 = p.register()?;
            p.comma()?;
            let crm = co_register(p)?;
            let op = Op::CoprocessorRegisters {
                op,
                cp,
                opc1,
                rt,
                rt2,
                crm,
            };
            return encoded_coprocessor(&Instruction { cond, op });
        }
        Form::CoTransfer(op) => return co_transfer(p, word, cond, op),
        _ => return Err(Error::NoSuchMnemonic.into()),
    };
    encoded(&Instruction { cond, op }, 0)
}

/// The precision a dyadic or monadic operation or `FLT` must name, then any
/// rounding; "No such mnemonic" where none is named.
fn precision_and_rounding(word: &mut Word) -> Parse<(Precision, Rounding)> {
    let precision = if word.take("S") {
        Precision::Single
    } else if word.take("D") {
        Precision::Double
    } else if word.take("E") {
        Precision::Extended
    } else {
        return Err(Error::NoSuchMnemonic.into());
    };
    Ok((precision, rounding(word)))
}

/// The rounding next in `word`: to nearest where none is named.
fn rounding(word: &mut Word) -> Rounding {
    if word.take("P") {
        Rounding::PlusInfinity
    } else if word.take("M") {
        Rounding::MinusInfinity
    } else if word.take("Z") {
        Rounding::Zero
    } else {
        Rounding::Nearest
    }
}

/// The memory format a transfer must name.
fn format(word: &mut Word) -> Parse<Format> {
    Ok(if word.take("S") {
        Format::Single
    } else if word.take("D") {
        Format::Double
    } else if word.take("E") {
        Format::Extended
    } else if word.take("P") {
        Format::Packed
    } else {
        return Err(Error::NoSuchMnemonic.into());
    })
}

/// An FPA register: F0 to F7 by name, a name to F9 refused, or a factor.
fn register(p: &mut Parser<'_>) -> Parse<u8> {
    p.spaces();
    let n = if p.peek().eq_ignore_ascii_case(&b'F')
        && p.byte(1).is_ascii_digit()
        && !in_name(p.byte(2))
    {
        let n = p.byte(1) - b'0';
        p.advance(2);
        i32::from(n)
    } else {
        p.int(Want::Factor)?
    };
    u8::try_from(n)
        .ok()
        .filter(|&n| n < 8)
        .ok_or_else(|| Error::BadRegister.into())
}

/// An FPA operand: a register, or `#` and one of the eight constants; under
/// suppressed errors any other constant is the first.
fn operand(p: &mut Parser<'_>) -> Parse<FpaOperand> {
    if !p.take(b'#') {
        return Ok(FpaOperand::Register(register(p)?));
    }
    let number = p.number(Want::Expression)?;
    let bits = p.arith.double_bits(number);
    let zero = bits == 1 << 63;
    match (0..)
        .zip(CONSTANTS)
        .find(|&(_, c)| c == bits || (c == 0 && zero))
    {
        Some((index, _)) => Ok(FpaOperand::Constant(index)),
        None if p.context.suppress => Ok(FpaOperand::Constant(0)),
        None => Err(Error::BadImmediate.into()),
    }
}

/// A coprocessor: `CP` and its number, or a factor.
fn number(p: &mut Parser<'_>) -> Parse<u8> {
    p.spaces();
    let n = if p.peek().eq_ignore_ascii_case(&b'C')
        && p.byte(1).eq_ignore_ascii_case(&b'P')
        && p.byte(2).is_ascii_digit()
    {
        p.advance(2);
        let mut n = 0i32;
        while p.peek().is_ascii_digit() {
            n = n
                .saturating_mul(10)
                .saturating_add(i32::from(p.peek() - b'0'));
            p.advance(1);
        }
        n
    } else {
        p.int(Want::Factor)?
    };
    u8::try_from(n)
        .ok()
        .filter(|&n| n < 16)
        .ok_or_else(|| Error::BadRegister.into())
}

/// A coprocessor's register: `C` and its number to 15, or a factor.
fn co_register(p: &mut Parser<'_>) -> Parse<u8> {
    p.spaces();
    let n = if p.peek().eq_ignore_ascii_case(&b'C') {
        match p.digits_to_fifteen(1) {
            Some((n, len)) => {
                p.advance(len);
                n.cast_signed()
            }
            None => p.int(Want::Factor)?,
        }
    } else {
        p.int(Want::Factor)?
    };
    u8::try_from(n)
        .ok()
        .filter(|&n| n < 16)
        .ok_or_else(|| Error::BadRegister.into())
}

/// An opcode from 0 to `most`.
fn opcode(p: &mut Parser<'_>, most: u8) -> Parse<u8> {
    u8::try_from(p.int(Want::Expression)?)
        .ok()
        .filter(|&n| n <= most)
        .ok_or_else(|| Error::BadImmediate.into())
}

/// A coprocessor transfer's address, as read.
struct CoAddress {
    rn: Reg,
    up: bool,
    index: CoIndex,
    offset: u8,
    /// Given as `[Rn]` alone.
    bare: bool,
}

impl CoAddress {
    /// The address an FPA transfer takes, which has no option form.
    fn fixed(&self) -> Parse<(Reg, bool, CoIndex, u8)> {
        if self.index == CoIndex::Unindexed {
            return Err(Error::Syntax.into());
        }
        Ok((self.rn, self.up, self.index, self.offset))
    }
}

/// A byte offset as a coprocessor transfer's words: a whole number of them
/// to 255; zero for any other under suppressed errors.
fn offset_words(p: &Parser<'_>, value: i32) -> Parse<(bool, u8)> {
    let magnitude = value.unsigned_abs();
    let up = value >= 0;
    let words = Some(magnitude)
        .filter(|magnitude| magnitude.is_multiple_of(4))
        .and_then(|magnitude| u8::try_from(magnitude / 4).ok());
    match words {
        Some(words) => Ok((up, words)),
        None if p.context.suppress => Ok((up, 0)),
        None => Err(Error::BadOffset.into()),
    }
}

/// A coprocessor transfer's address: in brackets with an offset, its write
/// back or an option, or an expression the PC reaches.
fn address(p: &mut Parser<'_>, options: bool) -> Parse<CoAddress> {
    if !p.take(b'[') {
        let target = p.int(Want::Expression)?;
        let distance = target
            .cast_unsigned()
            .wrapping_sub(p.context.address.wrapping_add(8))
            .cast_signed();
        let (up, offset) = offset_words(p, distance)?;
        return Ok(CoAddress {
            rn: Reg::PC,
            up,
            index: CoIndex::Offset,
            offset,
            bare: false,
        });
    }
    let rn = p.register()?;
    if p.take(b']') {
        if !p.take(b',') {
            return Ok(CoAddress {
                rn,
                up: true,
                index: CoIndex::Offset,
                offset: 0,
                bare: true,
            });
        }
        if options && p.take(b'{') {
            let option = u8::try_from(p.int(Want::Expression)?).map_err(|_| Error::BadImmediate)?;
            p.expect(b'}', Error::MissingCloseBrace)?;
            return Ok(CoAddress {
                rn,
                up: true,
                index: CoIndex::Unindexed,
                offset: option,
                bare: false,
            });
        }
        let value = p.hash_int()?;
        let (up, offset) = offset_words(p, value)?;
        return Ok(CoAddress {
            rn,
            up,
            index: CoIndex::PostIndexed,
            offset,
            bare: false,
        });
    }
    p.comma()?;
    let value = p.hash_int()?;
    let (up, offset) = offset_words(p, value)?;
    p.expect(b']', Error::MissingSquare)?;
    let index = if p.writeback() {
        CoIndex::PreIndexed
    } else {
        CoIndex::Offset
    };
    Ok(CoAddress {
        rn,
        up,
        index,
        offset,
        bare: false,
    })
}

/// `LFM` and `SFM`: a stack mode after the condition implies the offset of
/// the registers transferred; the count is from 1 to 4.
fn multiple(p: &mut Parser<'_>, word: &mut Word, cond: Condition, op: FpaMultipleOp) -> Parse<u32> {
    let load = op == FpaMultipleOp::Lfm;
    let stacked = if word.take("FD") {
        Some(if load { (false, true) } else { (true, false) })
    } else if word.take("EA") {
        Some(if load { (true, false) } else { (false, true) })
    } else {
        None
    };
    start(p, word)?;
    let fd = register(p)?;
    p.comma()?;
    let count = u8::try_from(p.int(Want::Expression)?)
        .ok()
        .filter(|n| (1..=4).contains(n))
        .ok_or(Error::Syntax)?;
    p.comma()?;
    let (rn, up, index, offset) = match stacked {
        Some((before, up)) => {
            p.expect(b'[', Error::Syntax)?;
            let rn = p.register()?;
            p.expect(b']', Error::MissingSquare)?;
            let writeback = p.writeback();
            (rn, up, CoIndex::of(before, writeback), 3 * count)
        }
        None => address(p, false)?.fixed()?,
    };
    encoded(
        &Instruction {
            cond,
            op: Op::FpaMultiple {
                op,
                fd,
                count,
                rn,
                up,
                index,
                offset,
            },
        },
        0,
    )
}

/// `LDC` and `STC`, and the unconditional `LDC2` and `STC2`, with `L` for
/// the long form. BASIC lays `LDC2` and `STC2` with an offset down without
/// the pre-index bit, and takes neither their write back nor post-index.
fn co_transfer(
    p: &mut Parser<'_>,
    word: &mut Word,
    cond: Condition,
    op: CoTransferOp,
) -> Parse<u32> {
    let long = word.take("L");
    start(p, word)?;
    let cp = number(p)?;
    p.comma()?;
    let crd = co_register(p)?;
    p.comma()?;
    let second = matches!(op, CoTransferOp::Ldc2 | CoTransferOp::Stc2);
    let CoAddress {
        rn,
        up,
        index,
        offset,
        bare,
    } = address(p, !second)?;
    if second && (index != CoIndex::Offset || rn == Reg::PC && !bare) {
        return Err(Error::Syntax.into());
    }
    let cond = if second { Condition::Nv } else { cond };
    let op = Op::CoprocessorTransfer {
        op,
        cp,
        crd,
        rn,
        long,
        up,
        index,
        offset,
    };
    let word = encoded_coprocessor(&Instruction { cond, op })?;
    Ok(if second && !bare {
        word & !(1 << 24)
    } else {
        word
    })
}
