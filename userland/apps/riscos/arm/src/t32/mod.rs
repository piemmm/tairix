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
use crate::isa::Isa;

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
    let detail = decode_detail(first, second, isa, position)?;
    let row = detail.row;
    Some(Decoded {
        instruction: detail.instruction,
        wide: row.wide,
        unpredictable: !detail.canonical
            || check::unpredictable(row, &detail.instruction, position),
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
        .filter(|row| row.layout.matches(word) && isa.has(row.since))
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

impl Encoding {
    /// The instruction's size in bytes.
    #[must_use]
    pub const fn bytes(self) -> u32 {
        match self {
            Self::Narrow(_) => 2,
            Self::Wide(..) => 4,
        }
    }
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
            let first = halfword(word >> 16);
            let second = halfword(word);
            (Encoding::Wide(first, second), first, second)
        } else {
            (Encoding::Narrow(halfword(word)), halfword(word), 0)
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

/// The low halfword of `word`.
const fn halfword(word: u32) -> u16 {
    let bytes = word.to_le_bytes();
    u16::from_le_bytes([bytes[0], bytes[1]])
}

#[cfg(test)]
mod tests;
