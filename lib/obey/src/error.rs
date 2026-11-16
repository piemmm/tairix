//! The errors RISC OS raises from GSTrans, system variables, expressions,
//! argument parsing and the command line.

/// One of them. Each stands for one RISC OS error number, which the replay of
/// genuine RISC OS 5 in `tests/riscos5.rs` pins; the two bounds this crate
/// keeps on hostile text stand for none.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Error {
    /// A string GSTrans cannot read: a control character, an escape before
    /// the string's end, or a quote never closed.
    BadString,
    /// A macro's value ends at a control character other than CR, LF or NUL.
    BadMacroValue,
    /// A new variable named with nothing, or with a wildcard.
    BadVariableName,
    /// The variables' store is full.
    NoRoomForVariable,
    /// A `)` with no `(` open, or a `(` never closed.
    MismatchedBrackets,
    /// An expression holds more than its working stack does.
    ExpressionStackOverflow,
    /// An operator lacks a value on one of its sides.
    MissingOperand,
    /// Two values stand together with no operator between them.
    MissingOperator,
    /// A string where an integer is wanted is not one.
    NotAnInteger,
    /// A string grows past what an expression holds.
    StringTooLong,
    /// An expression names no variable that exists, or holds a character no
    /// item starts with.
    UnknownOperand,
    /// A division or remainder by zero.
    DivisionByZero,
    /// A number's base is outside 2 to 36.
    BadBase,
    /// A number has no digit where one must be, or ends where it may not.
    BadNumber,
    /// A number is larger than its reading allows.
    NumberTooBig,
    /// A result does not fit the room given for it.
    BufferOverflow,
    /// Arguments that do not fit their template.
    BadParameters,
    /// An argument given twice.
    ArgumentRepeated,
    /// A command line whose terminator is not within its longest.
    LineTooLong,
    /// A command's parameters that its syntax does not take: too few or too
    /// many, an `If` with no `Then`, or a string for its condition.
    Syntax,
    /// The references a reading may expand, or the `If`s a line may nest, ran
    /// out.
    ExpansionTooComplex,
    /// Memory for the work was refused.
    SystemHeapFull,
}

impl Error {
    /// Whether it is one of the bounds this crate keeps on hostile text,
    /// which no RISC OS error stands in for and every reading passes on.
    #[must_use]
    pub const fn contains(self) -> bool {
        matches!(self, Self::ExpansionTooComplex | Self::SystemHeapFull)
    }
}
