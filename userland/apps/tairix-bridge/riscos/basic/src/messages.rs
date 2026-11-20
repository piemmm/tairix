//! BASIC's errors: BASIC V's numbers, each message the BBC BASIC Reference
//! Manual's text where it gives one and TAIRiX Bridge's own words otherwise.

/// An error BASIC raises.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Error {
    // 0: errors no `ON ERROR` can trap.
    StackCorrupt,
    NoErrorStatus,
    BadInCore,
    BadListo,
    LineTooLong,
    RenumberTooHigh,
    NoStep,
    ReferenceListed,
    NoRoom,
    NoRenumberRoom,
    Stopped,
    BadProgram,
    // 1 to 3: the assembler's.
    NoSuchMnemonic,
    AssemblerLimit,
    BadOffset,
    BadImmediate,
    BadShift,
    BadRegister,
    DuplicateRegister,
    // 4 onwards.
    MissingEquals,
    MissingEqualsInFor,
    Mistake,
    MissingComma,
    NeedArray,
    NeedNumericArray,
    NeedNumber,
    NeedStringArray,
    NeedString,
    NeedStringVariable,
    ArraysMismatch,
    ArraySize,
    ArrayParameterType,
    SwapArrays,
    NotInFunction,
    LowDollar,
    MissingQuote,
    Redimension,
    BadDim,
    NegativeDim,
    DimNeedsArray,
    ImpossibleDimension,
    MissingDimEnd,
    MatrixSame,
    InsufficientMemory,
    NoDimRoom,
    NoDimensionRoom,
    LocalOutside,
    NotInProcedure,
    ArrayReference,
    Undimensioned,
    UnknownArray,
    UnknownArrayInDim,
    SubscriptCount,
    SubscriptRange,
    Syntax,
    Escape,
    DivisionByZero,
    StringTooLong,
    TooBig,
    ArcRange,
    NegativeRoot,
    LogRange,
    AccuracyLost,
    ExponentRange,
    ArrayHere,
    NoSuchVariable,
    MissingBracket,
    MissingSquare,
    MissingBrace,
    MissingCloseBrace,
    BadBinary,
    BadHex,
    HexTooLarge,
    NoSuchRoutine,
    BadCall,
    BadArguments,
    BadArrayArgument,
    BadReturnArgument,
    NotInFor,
    CannotMatchFor,
    BadForVariable,
    ZeroStep,
    MissingTo,
    NotInSubroutine,
    OnSyntax,
    OnRange,
    NoSuchLine,
    NoDataPointer,
    OutOfData,
    NotInRepeat,
    TooDeep,
    MissingHash,
    NotInWhile,
    MissingEndcase,
    CaseNotLast,
    MissingOf,
    MissingEndif,
    BadMouseVariable,
    TooManyInputs,
    TooManyOutputs,
    CannotInstall,
    BadLibrary,
    NoLibraryRoom,
}

impl Error {
    /// Its number, which `ERR` gives.
    #[must_use]
    pub(crate) const fn number(self) -> u32 {
        use Error as E;
        match self {
            E::StackCorrupt
            | E::NoErrorStatus
            | E::BadInCore
            | E::BadListo
            | E::LineTooLong
            | E::RenumberTooHigh
            | E::NoStep
            | E::ReferenceListed
            | E::NoRoom
            | E::NoRenumberRoom
            | E::Stopped
            | E::BadProgram => 0,
            E::NoSuchMnemonic => 1,
            E::AssemblerLimit | E::BadOffset | E::BadImmediate | E::BadShift => 2,
            E::BadRegister | E::DuplicateRegister => 3,
            E::MissingEquals | E::MissingEqualsInFor | E::Mistake => 4,
            E::MissingComma => 5,
            E::NeedArray
            | E::NeedNumericArray
            | E::NeedNumber
            | E::NeedStringArray
            | E::NeedString
            | E::NeedStringVariable
            | E::ArraysMismatch
            | E::ArraySize
            | E::ArrayParameterType
            | E::SwapArrays => 6,
            E::NotInFunction => 7,
            E::LowDollar => 8,
            E::MissingQuote => 9,
            E::Redimension
            | E::BadDim
            | E::NegativeDim
            | E::DimNeedsArray
            | E::ImpossibleDimension
            | E::MissingDimEnd
            | E::MatrixSame => 10,
            E::InsufficientMemory | E::NoDimRoom | E::NoDimensionRoom => 11,
            E::LocalOutside => 12,
            E::NotInProcedure => 13,
            E::ArrayReference | E::Undimensioned | E::UnknownArray | E::UnknownArrayInDim => 14,
            E::SubscriptCount | E::SubscriptRange => 15,
            E::Syntax => 16,
            E::Escape => 17,
            E::DivisionByZero => 18,
            E::StringTooLong => 19,
            E::TooBig | E::ArcRange => 20,
            E::NegativeRoot => 21,
            E::LogRange => 22,
            E::AccuracyLost => 23,
            E::ExponentRange => 24,
            E::ArrayHere | E::NoSuchVariable => 26,
            E::MissingBracket | E::MissingSquare | E::MissingBrace | E::MissingCloseBrace => 27,
            E::BadBinary | E::BadHex | E::HexTooLarge => 28,
            E::NoSuchRoutine => 29,
            E::BadCall => 30,
            E::BadArguments | E::BadArrayArgument | E::BadReturnArgument => 31,
            E::NotInFor => 32,
            E::CannotMatchFor => 33,
            E::BadForVariable => 34,
            E::ZeroStep => 35,
            E::MissingTo => 36,
            E::NotInSubroutine => 38,
            E::OnSyntax => 39,
            E::OnRange => 40,
            E::NoSuchLine => 41,
            E::NoDataPointer | E::OutOfData => 42,
            E::NotInRepeat => 43,
            E::TooDeep => 44,
            E::MissingHash => 45,
            E::NotInWhile => 46,
            E::MissingEndcase => 47,
            E::CaseNotLast | E::MissingOf => 48,
            E::MissingEndif => 49,
            E::BadMouseVariable => 50,
            E::TooManyInputs | E::TooManyOutputs => 51,
            E::CannotInstall | E::BadLibrary | E::NoLibraryRoom => 52,
        }
    }

    /// Its message.
    #[must_use]
    #[expect(clippy::too_many_lines, reason = "one arm per message")]
    pub(crate) const fn text(self) -> &'static str {
        use Error as E;
        match self {
            E::StackCorrupt => "Corruption of stack",
            E::NoErrorStatus => "Error control status not found on stack for RESTORE ERROR",
            E::BadInCore => "Incorrect in-core file description",
            E::BadListo => "Invalid LISTO option",
            E::LineTooLong => "Line too long",
            E::RenumberTooHigh => {
                "Line numbers larger than 65279 would be generated by this renumber"
            }
            E::NoStep => "Line numbers cannot step by nought",
            E::ReferenceListed => "LIST/TWIN found line number reference",
            E::NoRoom => "No room",
            E::NoRenumberRoom => "No room to do this renumber",
            E::Stopped => "Stopped",
            E::BadProgram => "Bad program",
            E::NoSuchMnemonic => "No such mnemonic",
            E::AssemblerLimit => "Assembler limit reached",
            E::BadOffset => "Bad address offset",
            E::BadImmediate => "Bad immediate constant",
            E::BadShift => "Bad shift",
            E::BadRegister => "Bad register",
            E::DuplicateRegister => "Duplicate register in multiply",
            E::MissingEquals => "Missing =",
            E::MissingEqualsInFor => "Missing = in FOR statement",
            E::Mistake => "Mistake",
            E::MissingComma => "Missing ,",
            E::NeedArray => "Type mismatch: array needed",
            E::NeedNumericArray => "Type mismatch: numeric array needed",
            E::NeedNumber => "Type mismatch: number needed",
            E::NeedStringArray => "Type mismatch: string array needed",
            E::NeedString => "Type mismatch: string needed",
            E::NeedStringVariable => "Type mismatch: string variable needed",
            E::ArraysMismatch => "Type mismatch between arrays",
            E::ArraySize => "Can't assign to array of this size",
            E::ArrayParameterType => "Array type mismatch as parameter",
            E::SwapArrays => "Can't SWAP arrays of different types",
            E::NotInFunction => "Not in a function",
            E::LowDollar => "Too low a value for $<number>",
            E::MissingQuote => "Missing \"",
            E::Redimension => "Arrays cannot be redimensioned",
            E::BadDim => "Bad DIM statement",
            E::NegativeDim => "Can't DIM negative amount",
            E::DimNeedsArray => "DIM() function needs an array",
            E::ImpossibleDimension => "Impossible dimension",
            E::MissingDimEnd => "No end of dimension list )",
            E::MatrixSame => "No room to do matrix multiple with source(s) the same as destination",
            E::InsufficientMemory => "Attempt to allocate insufficient memory",
            E::NoDimRoom => "No room for this DIM",
            E::NoDimensionRoom => "No room for this dimension",
            E::LocalOutside => "Items can only be made local in a function or procedure",
            E::NotInProcedure => "Not in a procedure",
            E::ArrayReference => "Reference array incorrect",
            E::Undimensioned => "Undimensioned array",
            E::UnknownArray => "Unknown array",
            E::UnknownArrayInDim => "Unknown array in DIM() function",
            E::SubscriptCount => "Incorrect number of subscripts",
            E::SubscriptRange => "Subscript out of range",
            E::Syntax => "Syntax error",
            E::Escape => "Escape",
            E::DivisionByZero => "Division by zero",
            E::StringTooLong => "String too long",
            E::TooBig => "Number too big",
            E::ArcRange => "Number too big for arc Sine or arc Cosine",
            E::NegativeRoot => "Negative root",
            E::LogRange => "Logarithm range",
            E::AccuracyLost => "Accuracy lost in Sine/Cosine/Tangent",
            E::ExponentRange => "Exponent range",
            E::ArrayHere => "Can't use array reference here",
            E::NoSuchVariable => "Unknown or missing variable",
            E::MissingBracket => "Missing )",
            E::MissingSquare => "Missing ]",
            E::MissingBrace => "Missing {",
            E::MissingCloseBrace => "Missing }",
            E::BadBinary => "Bad Binary",
            E::BadHex => "Bad Hex",
            E::HexTooLarge => "Hex number too large",
            E::NoSuchRoutine => "No such function/procedure",
            E::BadCall => "Bad call of function/procedure",
            E::BadArguments => "Arguments of function/procedure incorrect",
            E::BadArrayArgument => "Invalid array actual parameter",
            E::BadReturnArgument => "Invalid RETURN actual parameter",
            E::NotInFor => "Not in a FOR loop",
            E::CannotMatchFor => "Can't match FOR",
            E::BadForVariable => "Bad FOR control variable",
            E::ZeroStep => "The step cannot be zero",
            E::MissingTo => "Missing TO",
            E::NotInSubroutine => "Not in a subroutine",
            E::OnSyntax => "ON syntax",
            E::OnRange => "ON range",
            E::NoSuchLine => "No such line",
            E::NoDataPointer => "DATA pointer not found on stack for RESTORE DATA",
            E::OutOfData => "Out of data",
            E::NotInRepeat => "Not in a REPEAT loop",
            E::TooDeep => "Too many nested structures",
            E::MissingHash => "Missing #",
            E::NotInWhile => "Not in a WHILE loop",
            E::MissingEndcase => "Missing ENDCASE",
            E::CaseNotLast => "CASE..OF statement must be the last thing on a line",
            E::MissingOf => "OF missing from CASE statement",
            E::MissingEndif => "Missing ENDIF",
            E::BadMouseVariable => "Bad MOUSE variable",
            E::TooManyInputs => "Too many input expressions for SYS",
            E::TooManyOutputs => "Too many output variables for SYS",
            E::CannotInstall => "Can't install library",
            E::BadLibrary => "Bad program used as function/procedure library",
            E::NoLibraryRoom => "No room for library",
        }
    }

    /// The error a numeric fault is, none for a floating-point exception,
    /// which is the floating-point emulator's error.
    #[must_use]
    pub(crate) const fn of_fault(fault: crate::Fault) -> Option<Self> {
        use crate::Fault as F;
        Some(match fault {
            F::DivisionByZero => Self::DivisionByZero,
            F::TooBig => Self::TooBig,
            F::ArcRange => Self::ArcRange,
            F::NegativeRoot => Self::NegativeRoot,
            F::LogRange => Self::LogRange,
            F::AccuracyLost => Self::AccuracyLost,
            F::ExponentRange => Self::ExponentRange,
            F::Floating(_) => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Error;

    #[test]
    fn a_numeric_fault_is_the_error_its_number_names() {
        use crate::Fault as F;
        for fault in [
            F::DivisionByZero,
            F::TooBig,
            F::ArcRange,
            F::NegativeRoot,
            F::LogRange,
            F::AccuracyLost,
            F::ExponentRange,
        ] {
            let error = Error::of_fault(fault).expect("BASIC's own");
            assert_eq!(Some(error.number()), fault.number(), "{fault:?}");
        }
    }
}
