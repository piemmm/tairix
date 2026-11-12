//! Instruction text in Acorn syntax: `&` for hexadecimal, `#` for immediate,
//! `A` for the accumulator, as the BBC's own assembler spells them.

use core::fmt;

use super::isa::{Mode, Opcode, OPCODES};

/// One instruction, decoded from the bytes at its address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Instruction {
    /// Where it is.
    pub pc: u16,
    /// What it is.
    pub opcode: Opcode,
    /// Its operand bytes, little-endian; bytes past its length are ignored.
    pub operand: [u8; 2],
}

impl Instruction {
    /// Decode the instruction whose opcode is `bytes[0]`, at `pc`.
    #[must_use]
    pub fn decode(pc: u16, bytes: [u8; 3]) -> Self {
        Self {
            pc,
            opcode: OPCODES[usize::from(bytes[0])],
            operand: [bytes[1], bytes[2]],
        }
    }

    /// Its size in bytes.
    #[must_use]
    pub const fn size(self) -> u16 {
        self.opcode.size()
    }

    fn relative_target(self) -> u16 {
        self.pc
            .wrapping_add(2)
            .wrapping_add_signed(i16::from(self.operand[0].cast_signed()))
    }
}

impl fmt::Display for Instruction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = self.opcode.mnemonic.name();
        let byte = self.operand[0];
        let word = u16::from_le_bytes(self.operand);
        match self.opcode.mode {
            Mode::Implied => write!(f, "{name}"),
            Mode::Accumulator => write!(f, "{name} A"),
            Mode::Immediate => write!(f, "{name} #&{byte:02X}"),
            Mode::ZeroPage => write!(f, "{name} &{byte:02X}"),
            Mode::ZeroPageX => write!(f, "{name} &{byte:02X},X"),
            Mode::ZeroPageY => write!(f, "{name} &{byte:02X},Y"),
            Mode::Absolute => write!(f, "{name} &{word:04X}"),
            Mode::AbsoluteX => write!(f, "{name} &{word:04X},X"),
            Mode::AbsoluteY => write!(f, "{name} &{word:04X},Y"),
            Mode::Indirect => write!(f, "{name} (&{word:04X})"),
            Mode::IndexedIndirect => write!(f, "{name} (&{byte:02X},X)"),
            Mode::IndirectIndexed => write!(f, "{name} (&{byte:02X}),Y"),
            Mode::Relative => write!(f, "{name} &{:04X}", self.relative_target()),
        }
    }
}
