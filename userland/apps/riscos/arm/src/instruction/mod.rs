//! The instruction vocabulary both instruction sets decode to: an [`Op`] for
//! each form, carrying the operation its encodings select and the operands
//! their fields hold, and the assembler's text for it.

mod op;
mod text;

pub use op::*;
pub use text::Text;

/// An instruction set.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Set {
    /// ARM.
    A32,
    /// Thumb and Thumb-2.
    T32,
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
    /// The assembler's text for the instruction at `address` in `set`, which
    /// places a branch's target.
    #[must_use]
    pub const fn text(&self, set: Set, address: u32) -> Text {
        Text {
            instruction: *self,
            address,
            set,
        }
    }
}
