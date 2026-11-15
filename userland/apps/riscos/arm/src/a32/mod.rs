//! The A32 instruction set. One table holds every encoding; decoding,
//! encoding, the assembler's text and the interpreter's dispatch are all
//! derived from it.

mod check;
mod form;
mod layout;
mod op;
mod table;
mod text;

pub use op::*;
pub use text::Text;

use crate::cond::Condition;
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
        .iter()
        .filter(|row| {
            row.layout.matches(word)
                && isa.has(row.since)
                && (row.unconditional() || !unconditional_space)
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

/// Why an instruction has no encoding.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EncodeError {
    /// An operand is too large for its field.
    Range,
    /// No encoding holds the operands as given: a value only another
    /// instruction's encoding spells, or a combination the instruction
    /// does not have.
    Unencodable,
}

impl Instruction {
    /// The instruction's encoding, which decodes back to it exactly; an
    /// unconditional instruction's condition is [`Condition::Nv`].
    pub fn encode(&self) -> Result<u32, EncodeError> {
        let kind = self.op.kind();
        let mut fields = self.op.fields();
        fields.set(layout::Field::Cond, self.cond.bits());
        let mut too_large = false;
        for row in table::all().filter(|row| row.kind == kind) {
            let Some(word) = row.layout.deposit(&fields) else {
                too_large = true;
                continue;
            };
            let isa = if self.cond == Condition::Nv && !row.unconditional() {
                Isa::ALL.with(Feature::ConditionNever)
            } else {
                Isa::ALL
            };
            if decode(word, isa) == Some(*self) {
                return Ok(word);
            }
        }
        Err(if too_large {
            EncodeError::Range
        } else {
            EncodeError::Unencodable
        })
    }

    /// Whether the architecture leaves the instruction's effect
    /// UNPREDICTABLE on a core with `isa`, judged from its encoding alone:
    /// a register it may not name, a pair it may not repeat, a list it may
    /// not leave empty. What depends on the mode or the data is judged as it
    /// executes.
    #[must_use]
    pub fn unpredictable(&self, isa: Isa) -> bool {
        check::unpredictable(self, isa)
    }

    /// The assembler's text for the instruction at `address`, which places
    /// a branch's target.
    #[must_use]
    pub const fn text(&self, address: u32) -> Text {
        Text {
            instruction: *self,
            address,
        }
    }
}

#[cfg(test)]
mod tests;
