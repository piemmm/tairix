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
    let field = word >> 28;
    let unconditional_space = field == 0xF && !isa.has(Feature::ConditionNever);
    table::rows((word >> 25) as usize & 7)
        .filter(|row| {
            row.layout.matches(word)
                && isa.has(row.since)
                && (row.unconditional() || !unconditional_space)
                && !(row.kind.generic_coprocessor() && isa.claims_coprocessor(word >> 8))
        })
        .find_map(|row| {
            let fields = row.layout.extract(word);
            row.kind.build(&row.layout, &fields).map(|op| Decoded {
                instruction: Instruction {
                    cond: Condition::field(field),
                    op,
                },
                canonical: word & row.layout.should_mask == row.layout.should_bits,
            })
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
    let unconditional_space = word >> 28 == 0xF && !isa.has(Feature::ConditionNever);
    table::rows((word >> 25) as usize & 7)
        .filter(|row| {
            matches!(row.kind, crate::encoding::form::Kind::SimdFp(_))
                && row.layout.matches(word)
                && isa.has(row.since)
                && (row.unconditional() || !unconditional_space)
        })
        .find_map(|row| {
            let fields = row.layout.extract(word);
            row.kind
                .build(&row.layout, &fields)
                .map(|op| SimdFpDecoded {
                    instruction: Instruction {
                        cond: Condition::field(word >> 28),
                        op,
                    },
                    canonical: word & row.layout.should_mask == row.layout.should_bits,
                    outside: row.unconditional()
                        && matches!(row.since, Feature::FpV8 | Feature::Crypto),
                })
        })
}

/// The encoding of `instruction`, which decodes back to it exactly; an
/// unconditional instruction's condition is [`Condition::Nv`].
pub fn encode(instruction: &Instruction) -> Result<u32, EncodeError> {
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
