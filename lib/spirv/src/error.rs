//! Why a module was refused, and why an encoder would not write one.

use core::fmt;

/// Why a module was refused, and the index of the word it was refused at.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DecodeError {
    /// The word at fault, counted from the module's first.
    pub word: u32,
    /// What was wrong with it.
    pub fault: Fault,
}

/// What was wrong with a module.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Fault {
    /// Larger than [`MAX_MODULE_BYTES`](crate::MAX_MODULE_BYTES).
    TooLarge,
    /// Not whole words, or shorter than the header.
    NotWords,
    /// No SPIR-V magic number, in either byte order.
    Magic,
    /// A version that is not 1.x, newer than the grammar, or written with
    /// its reserved bytes set.
    Version,
    /// A header's reserved schema word that is not zero.
    Schema,
    /// More than [`MAX_INSTRUCTIONS`](crate::MAX_INSTRUCTIONS) instructions.
    TooManyInstructions,
    /// An instruction of no words, or one running past the module's end.
    WordCount,
    /// An opcode the grammar does not recognise.
    UnknownOpcode,
    /// An operand the instruction requires is missing.
    MissingOperand,
    /// Words left over once the instruction's operands are read.
    TrailingWords,
    /// An id of zero, or not below the module's bound.
    IdOutOfBound,
    /// A value, or a bit, its kind does not have.
    UnknownEnumerant,
    /// A string with no terminator, padded with anything but zero, or not
    /// UTF-8.
    BadString,
    /// An id defined twice.
    DuplicateId,
    /// A type or value a literal's width is read from, not defined earlier.
    Unresolved,
    /// A literal whose type is not a numeric scalar of a width up to 64
    /// bits.
    NumericWidth,
    /// An extended instruction set the grammar does not recognise, or a set
    /// operand naming no import of one.
    UnknownExtInstSet,
    /// An extended instruction its set does not have.
    UnknownExtInst,
    /// An extended instruction given the wrong number of operands.
    ExtInstArity,
    /// The memory the decoded module needs could not be had.
    OutOfMemory,
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TooLarge => "the module is too large",
            Self::NotWords => "the module is not whole words with a header",
            Self::Magic => "no SPIR-V magic number",
            Self::Version => "a version this grammar does not describe",
            Self::Schema => "a reserved header word is not zero",
            Self::TooManyInstructions => "too many instructions",
            Self::WordCount => "an instruction's word count is zero or runs past the end",
            Self::UnknownOpcode => "an opcode the grammar does not recognise",
            Self::MissingOperand => "a required operand is missing",
            Self::TrailingWords => "words follow the instruction's last operand",
            Self::IdOutOfBound => "an id is zero or not below the bound",
            Self::UnknownEnumerant => "a value its kind does not have",
            Self::BadString => "a malformed string",
            Self::DuplicateId => "an id is defined twice",
            Self::Unresolved => "a literal's type is not defined before it",
            Self::NumericWidth => "a literal's type is not a numeric scalar",
            Self::UnknownExtInstSet => "an extended instruction set the grammar does not recognise",
            Self::UnknownExtInst => "an extended instruction its set does not have",
            Self::ExtInstArity => "an extended instruction given the wrong operands",
            Self::OutOfMemory => "the decoded module's memory could not be had",
        })
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "word {}: {}", self.word, self.fault)
    }
}

/// Why an encoder would not write an instruction.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EncodeError {
    /// More words than one instruction's word count can hold.
    TooLong,
    /// A string holding a nul, which would end it early.
    InteriorNul,
    /// An id this encoder did not issue.
    IdNotIssued,
    /// Every id below the largest bound has been issued.
    IdsExhausted,
    /// The module would pass [`MAX_MODULE_BYTES`](crate::MAX_MODULE_BYTES).
    TooLarge,
    /// The memory the module needs could not be had.
    OutOfMemory,
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TooLong => "the instruction is too long",
            Self::InteriorNul => "a string holds a nul",
            Self::IdNotIssued => "an id the encoder did not issue",
            Self::IdsExhausted => "every id has been issued",
            Self::TooLarge => "the module would be too large",
            Self::OutOfMemory => "the module's memory could not be had",
        })
    }
}
