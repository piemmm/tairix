//! The A32 instruction set. One table holds every encoding; decoding,
//! encoding, the assembler's text and the interpreter's dispatch are all
//! derived from it.

mod check;
mod simdfp;
mod table;

pub(crate) use table::unconditional;

use crate::cond::Condition;
use crate::encoding::layout;
use crate::instruction::{EncodeError, Instruction};
use crate::isa::{Feature, Isa};

/// A decoded word and whether its should-be bits held their required
/// values; a word whose did not is UNPREDICTABLE.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Decoded {
    pub instruction: Instruction,
    pub canonical: bool,
}

/// The instruction `word` encodes on a core with `isa`, or `None` when it
/// is undefined there.
#[must_use]
pub fn decode(word: u32, isa: Isa) -> Option<Instruction> {
    decode_detail(word, isa).map(|decoded| decoded.instruction)
}

/// [`decode`], with whether the word was canonical.
pub(crate) fn decode_detail(word: u32, isa: Isa) -> Option<Decoded> {
    first_decoding(table::rows_matching(word), word, isa).map(|(_, instruction, canonical)| {
        Decoded {
            instruction,
            canonical,
        }
    })
}

/// The generic coprocessor instruction `word` is, by A32's rows for them
/// alone: the encodings T32's coprocessor space shares.
pub(crate) fn decode_coprocessor(word: u32, isa: Isa) -> Option<Decoded> {
    let rows = table::rows_matching(word).filter(|row| row.kind.generic_coprocessor());
    first_decoding(rows, word, isa).map(|(_, instruction, canonical)| Decoded {
        instruction,
        canonical,
    })
}

/// The first of `rows` a core with `isa` decodes `word` by: the row, what it
/// built and whether the word's should-be bits held their values.
fn first_decoding(
    rows: impl Iterator<Item = &'static table::Row>,
    word: u32,
    isa: Isa,
) -> Option<(&'static table::Row, Instruction, bool)> {
    let unconditional_space = word >> 28 == 0xF && !isa.has(Feature::ConditionNever);
    rows.filter(|row| {
        isa.has(row.since)
            && (row.unconditional() || !unconditional_space)
            && !(row.kind.generic_coprocessor() && isa.claims_coprocessor(word >> 8))
    })
    .find_map(|row| {
        let op = row.kind.build(&row.layout, &row.layout.extract(word))?;
        let instruction = Instruction {
            cond: Condition::field(word >> 28),
            op,
        };
        let canonical = word & row.layout.should_mask == row.layout.should_bits;
        Some((row, instruction, canonical))
    })
}

/// A floating-point or Advanced SIMD decoding, for T32, whose encodings of
/// them are A32's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct SimdFpDecoded {
    pub instruction: Instruction,
    /// Whether the should-be bits held their values.
    pub canonical: bool,
    /// Whether T32 forbids it in an `IT` block: ARMv8's unconditional
    /// additions.
    pub outside: bool,
}

/// The floating-point or Advanced SIMD instruction the A32 `word` encodes on
/// a core with `isa`; the other rows are not consulted.
pub(crate) fn decode_simdfp(word: u32, isa: Isa) -> Option<SimdFpDecoded> {
    first_decoding(table::simdfp_matching(word), word, isa).map(|(row, instruction, canonical)| {
        SimdFpDecoded {
            instruction,
            canonical,
            outside: row.unconditional() && matches!(row.since, Feature::FpV8 | Feature::Crypto),
        }
    })
}

/// The encoding of `instruction`, which decodes back to it exactly; an
/// unconditional instruction's condition is [`Condition::Nv`].
pub fn encode(instruction: &Instruction) -> Result<u32, EncodeError> {
    encode_among(instruction, decode)
}

/// The encoding of a generic coprocessor `instruction` that
/// [`decode_coprocessor`] decodes back to it: T32's, where no unit claims the
/// coprocessor space as A32's FPA does.
pub(crate) fn encode_coprocessor(instruction: &Instruction) -> Result<u32, EncodeError> {
    encode_among(instruction, |word, isa| {
        decode_coprocessor(word, isa).map(|decoded| decoded.instruction)
    })
}

/// The first encoding of `instruction` its rows give that `decode` reads back
/// to it.
fn encode_among(
    instruction: &Instruction,
    decode: impl Fn(u32, Isa) -> Option<Instruction>,
) -> Result<u32, EncodeError> {
    let kind = instruction.op.kind();
    let mut fields = instruction.op.fields();
    fields.set(layout::Field::Cond, instruction.cond.bits());
    let mut too_large = false;
    for row in table::all().filter(|row| row.kind == kind) {
        let Some(word) = row.layout.deposit(&fields) else {
            too_large = true;
            continue;
        };
        let isa = if instruction.cond == Condition::Nv && !row.unconditional() {
            Isa::ALL.with(Feature::ConditionNever)
        } else {
            Isa::ALL
        };
        if decode(word, isa) == Some(*instruction) {
            return Ok(word);
        }
    }
    Err(if too_large {
        EncodeError::Range
    } else {
        EncodeError::Unencodable
    })
}

/// Whether the architecture leaves the A32 `instruction`'s effect
/// UNPREDICTABLE on a core with `isa`, judged from its encoding alone: a
/// register it may not name, a pair it may not repeat, a list it may not
/// leave empty. What depends on the mode or the data is judged as it
/// executes.
#[must_use]
pub fn unpredictable(instruction: &Instruction, isa: Isa) -> bool {
    check::unpredictable(instruction, isa)
}

#[cfg(test)]
mod tests;
