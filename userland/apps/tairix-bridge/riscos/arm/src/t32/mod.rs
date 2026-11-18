//! The T32 instruction set: Thumb and Thumb-2. One table holds every
//! encoding; decoding, encoding, the assembler's text and the interpreter's
//! dispatch are all derived from it, as A32's are.
//!
//! A T32 instruction is one halfword or two. Its condition comes from the
//! `IT` block it stands in, which also changes what some encodings mean, so
//! decoding takes the instruction's [`Position`].

mod check;
mod form;
mod table;

use crate::cond::Condition;
use crate::encoding::layout::Field;
use crate::instruction::{CallOp, EncodeError, Instruction, Op};
use crate::isa::{Feature, Isa};
use crate::word::low16;

use form::Shape;
use table::Row;

/// Where an instruction stands relative to an `IT` block, which gives it its
/// condition and limits what it may be.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Position {
    /// Outside any block.
    Outside,
    /// In a block, before its last instruction, under this condition.
    Inside(Condition),
    /// A block's last instruction, under this condition.
    Last(Condition),
}

impl Position {
    /// Where the instruction the IT state `itstate` describes stands: its
    /// top four bits hold the condition, its low four what is left of the
    /// block.
    #[must_use]
    pub const fn of(itstate: u8) -> Self {
        let cond = Condition::field(itstate as u32 >> 4);
        match itstate & 15 {
            0 => Self::Outside,
            0b1000 => Self::Last(cond),
            _ => Self::Inside(cond),
        }
    }

    /// Whether the instruction is in a block.
    #[must_use]
    pub const fn in_block(self) -> bool {
        !matches!(self, Self::Outside)
    }

    /// The condition the block gives the instruction: `AL` outside one.
    #[must_use]
    pub const fn condition(self) -> Condition {
        match self {
            Self::Outside => Condition::Al,
            Self::Inside(cond) | Self::Last(cond) => cond,
        }
    }
}

/// Whether `first` begins a 32-bit instruction.
#[must_use]
pub const fn is_wide(first: u16) -> bool {
    first >> 11 >= 0b11101
}

/// A decoded instruction, its size, and whether its effect is defined.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Decoded {
    /// The instruction.
    pub instruction: Instruction,
    /// Whether it is 32 bits: two halfwords.
    pub wide: bool,
    /// Whether the architecture leaves its effect UNPREDICTABLE where it
    /// stands: its should-be bits, its registers, or its place in an `IT`
    /// block. What depends on the mode or the data is judged as it executes.
    pub unpredictable: bool,
}

/// The instruction whose first halfword is `first`, followed by `second`,
/// on a core with `isa` at `position`, or `None` when it is undefined there.
/// A 16-bit instruction ignores `second`.
#[must_use]
pub fn decode(first: u16, second: u16, isa: Isa, position: Position) -> Option<Decoded> {
    if is_wide(first) {
        let word = (u32::from(first) << 16) | u32::from(second);
        if let Some(a32_word) = simdfp_word(word, isa) {
            return decode_simdfp(a32_word, isa, position);
        }
        if isa.has(Feature::V6T2) && is_coprocessor(word) {
            return decode_coprocessor(word, isa, position);
        }
    }
    let detail = decode_detail(first, second, isa, position)?;
    let row = detail.row;
    Some(Decoded {
        instruction: detail.instruction,
        wide: row.wide,
        unpredictable: !detail.canonical
            || check::unpredictable(row, &detail.instruction, position),
    })
}

/// The A32 word whose floating-point or Advanced SIMD encoding the 32-bit
/// T32 `word` shares, where the core has the unit whose space it lies in:
/// Advanced SIMD's data processing and element transfers move their prefix,
/// and VFP's encodings are A32's with the condition `AL`, or unconditional.
const fn simdfp_word(word: u32, isa: Isa) -> Option<u32> {
    let simd = isa.has(Feature::Simd);
    let vfp = isa.claims_coprocessor(word >> 8);
    match word >> 24 {
        0xEF if simd => Some(0xF200_0000 | (word & 0x00FF_FFFF)),
        0xFF if simd => Some(0xF300_0000 | (word & 0x00FF_FFFF)),
        0xF9 if simd && word & (1 << 20) == 0 => Some(0xF400_0000 | (word & 0x00FF_FFFF)),
        0xEC..=0xEE | 0xFC..=0xFE if vfp => Some(word),
        _ => None,
    }
}

/// Whether the 32-bit T32 `word` lies in the coprocessor space, which A32's
/// encodings share with the condition `AL`, or unconditional.
const fn is_coprocessor(word: u32) -> bool {
    matches!(word >> 24, 0xEC..=0xEE | 0xFC..=0xFE)
}

/// The T32 word an A32 floating-point, Advanced SIMD or coprocessor encoding
/// is.
const fn simdfp_thumb(word: u32) -> Option<u32> {
    match word >> 24 {
        0xF2 => Some(0xEF00_0000 | (word & 0x00FF_FFFF)),
        0xF3 => Some(0xFF00_0000 | (word & 0x00FF_FFFF)),
        0xF4 => Some(0xF900_0000 | (word & 0x00FF_FFFF)),
        0xEC..=0xEE | 0xFC..=0xFE => Some(word),
        _ => None,
    }
}

/// A floating-point or Advanced SIMD instruction, decoded by A32's rows and
/// placed by the `IT` block it stands in.
fn decode_simdfp(a32_word: u32, isa: Isa, position: Position) -> Option<Decoded> {
    let decoded = crate::a32::decode_simdfp(a32_word, isa)?;
    let Op::SimdFp(op) = decoded.instruction.op else {
        return None;
    };
    let placed = position.in_block() && (position.condition() == Condition::Nv || decoded.outside);
    Some(Decoded {
        instruction: Instruction {
            cond: position.condition(),
            op: decoded.instruction.op,
        },
        wide: true,
        unpredictable: !decoded.canonical || placed || crate::encoding::check::simdfp(&op, true),
    })
}

/// A generic coprocessor instruction, decoded by A32's rows and placed by the
/// `IT` block it stands in.
fn decode_coprocessor(word: u32, isa: Isa, position: Position) -> Option<Decoded> {
    let decoded = crate::a32::decode_coprocessor(word, isa)?;
    let op = decoded.instruction.op;
    Some(Decoded {
        instruction: Instruction {
            cond: position.condition(),
            op,
        },
        wide: true,
        unpredictable: !decoded.canonical
            || check::placed(false, &op, position)
            || check::coprocessor(&op),
    })
}

/// A decoding, the row that made it, and whether the bits were canonical.
#[derive(Clone, Copy, Debug)]
struct Detail {
    instruction: Instruction,
    row: &'static Row,
    /// Whether the should-be bits held their values and a source named
    /// twice was named alike.
    canonical: bool,
}

fn decode_detail(first: u16, second: u16, isa: Isa, position: Position) -> Option<Detail> {
    let (rows, word) = if is_wide(first) {
        let word = (u32::from(first) << 16) | u32::from(second);
        (table::wide_rows(word), word)
    } else {
        (table::narrow_rows(first), u32::from(first))
    };
    rows.iter()
        .filter(|row| {
            row.layout.matches(word)
                && isa.has(row.since)
                && !(row.kind.generic_coprocessor() && isa.claims_coprocessor(word >> 8))
        })
        .find_map(|row| decode_row(row, word, position))
}

/// The instruction `row` decodes `word` to at `position`.
fn decode_row(row: &'static Row, word: u32, position: Position) -> Option<Detail> {
    let fields = row.layout.extract(word);
    let op = form::build(
        row.shape,
        row.kind,
        &row.layout,
        &fields,
        position.in_block(),
    )?;
    let cond = match (op, position) {
        // A breakpoint is unconditional even in a block.
        (
            Op::Call {
                op: CallOp::Bkpt, ..
            },
            _,
        ) => Condition::Al,
        (_, Position::Outside) if row.layout.field(Field::Cond) != 0 => {
            Condition::field(fields.get(Field::Cond))
        }
        (_, position) => position.condition(),
    };
    let twice_agrees =
        row.shape != Shape::Twice || fields.get(Field::Rm) >> 4 == fields.get(Field::Rm) & 15;
    Some(Detail {
        instruction: Instruction { cond, op },
        row,
        canonical: word & row.layout.should_mask == row.layout.should_bits && twice_agrees,
    })
}

/// A T32 instruction's halfwords.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Encoding {
    /// A 16-bit instruction.
    Narrow(u16),
    /// A 32-bit instruction: its first halfword, then its second.
    Wide(u16, u16),
}

/// The encoding sizes an encoder may choose from, as UAL's qualifiers ask
/// for them.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Size {
    /// The narrowest that holds the instruction.
    Narrowest,
    /// 16 bits only: `.N`.
    Narrow,
    /// 32 bits only: `.W`.
    Wide,
}

/// The encoding of `instruction` at `position`, of `size`, which decodes back
/// to it exactly. An encoding whose effect is defined is preferred to one the
/// architecture leaves UNPREDICTABLE where it stands.
pub fn encode(
    instruction: &Instruction,
    position: Position,
    size: Size,
) -> Result<Encoding, EncodeError> {
    if matches!(instruction.op, Op::SimdFp(_)) || instruction.op.kind().generic_coprocessor() {
        return encode_by_a32(instruction, position, size);
    }
    let kind = instruction.op.kind();
    let mut too_large = false;
    let mut unpredictable = None;
    // A reserved hint's fields are ignored, so it is no instruction's encoding.
    let candidates = table::all().filter(|row| {
        row.kind == kind
            && row.shape != Shape::Nop
            && match size {
                Size::Narrowest => true,
                Size::Narrow => !row.wide,
                Size::Wide => row.wide,
            }
    });
    for row in candidates {
        let mut fields = form::fields(row.shape, &row.layout, &instruction.op);
        fields.set(Field::Cond, instruction.cond.bits());
        let Some(word) = row.layout.deposit(&fields) else {
            too_large = true;
            continue;
        };
        let (encoding, first, second) = if row.wide {
            let first = low16(word >> 16);
            let second = low16(word);
            (Encoding::Wide(first, second), first, second)
        } else {
            (Encoding::Narrow(low16(word)), low16(word), 0)
        };
        let Some(decoded) = decode(first, second, Isa::ALL, position) else {
            continue;
        };
        if decoded.instruction != *instruction {
            continue;
        }
        if !decoded.unpredictable {
            return Ok(encoding);
        }
        unpredictable.get_or_insert(encoding);
    }
    unpredictable.ok_or(if too_large {
        EncodeError::Range
    } else {
        EncodeError::Unencodable
    })
}

/// A floating-point, Advanced SIMD or generic coprocessor instruction's
/// encoding: A32's, its condition the one the `IT` block gives it.
fn encode_by_a32(
    instruction: &Instruction,
    position: Position,
    size: Size,
) -> Result<Encoding, EncodeError> {
    if size == Size::Narrow || instruction.cond != position.condition() {
        return Err(EncodeError::Unencodable);
    }
    let unconditional = crate::a32::unconditional(instruction.op.kind());
    let a32 = Instruction {
        cond: if unconditional {
            Condition::Nv
        } else {
            Condition::Al
        },
        op: instruction.op,
    };
    let word = if let Op::SimdFp(_) = instruction.op {
        crate::a32::encode(&a32)?
    } else {
        crate::a32::encode_coprocessor(&a32)?
    };
    let word = simdfp_thumb(word).ok_or(EncodeError::Unencodable)?;
    let (first, second) = (low16(word >> 16), low16(word));
    match decode(first, second, Isa::ALL, position) {
        Some(decoded) if decoded.instruction == *instruction => Ok(Encoding::Wide(first, second)),
        _ => Err(EncodeError::Unencodable),
    }
}

#[cfg(test)]
mod tests;
