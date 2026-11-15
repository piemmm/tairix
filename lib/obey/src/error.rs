//! The errors RISC OS raises from GSTrans, system variables, expressions and
//! argument parsing, each known by the message token RISC OS reports it under.

/// One of them. A RISC OS error block's number and text are the kernel's
/// message file's, found by [`Error::token`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Error {
    /// A string GSTrans cannot read: a control character, an escape before
    /// the string's end, or a quote never closed.
    BadString,
    /// A macro's value ends at a control character other than CR, LF or NUL.
    BadMacroValue,
    /// A new variable named with nothing, or with a wildcard.
    BadVariableName,
    /// A variable type `OS_SetVarVal` does not know.
    BadVariableType,
    /// The variables' store is full.
    NoRoomForVariable,
    /// No variable has the name.
    VariableNotFound,
    /// A code variable refused a value as too long.
    VariableTooLong,
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
    /// An `If` with no `Then`.
    NoThen,
    /// An `If`'s condition is a string, not an integer.
    ExpressionIsAString,
    /// The references a reading may expand ran out.
    ExpansionTooComplex,
    /// Memory for the work was refused.
    SystemHeapFull,
}

impl Error {
    /// The token RISC OS's kernel message file reports the error under.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::BadString => "BadString",
            Self::BadMacroValue => "BadMacVal",
            Self::BadVariableName => "BadVarNam",
            Self::BadVariableType => "BadVarType",
            Self::NoRoomForVariable => "VarNoRoom",
            Self::VariableNotFound => "VarCantFind",
            Self::VariableTooLong => "VarTooLong",
            Self::MismatchedBrackets => "BadBra",
            Self::ExpressionStackOverflow => "StkOFlo",
            Self::MissingOperand => "MissOpn",
            Self::MissingOperator => "MissOpr",
            Self::NotAnInteger => "BadInt",
            Self::StringTooLong => "StrOFlo",
            Self::UnknownOperand => "NaffItm",
            Self::DivisionByZero => "DivZero",
            Self::BadBase => "BadBase",
            Self::BadNumber => "BadNumb",
            Self::NumberTooBig => "NumbTooBig",
            Self::BufferOverflow => "BuffOverflow",
            Self::BadParameters => "BadParameters",
            Self::ArgumentRepeated => "ArgRepeated",
            Self::NoThen => "NoThen",
            Self::ExpressionIsAString => "IsString",
            Self::ExpansionTooComplex => "OscliTooHard",
            Self::SystemHeapFull => "SysHeapFull",
        }
    }
}
