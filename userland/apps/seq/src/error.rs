//! The outcomes of parsing and running a `seq` command.

use alloc::string::String;
use core::fmt;
use tairix_util::quote::quote;

/// Why a `seq` invocation did not complete.
///
/// Usage failures mirror the GNU tool's diagnostics word for word; the one
/// runtime failure is the output stream refusing bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SeqError {
    /// An unrecognised long option; carries the token.
    UnknownLong(String),
    /// An unrecognised short option; carries the flag character.
    UnknownShort(char),
    /// `-f`/`-s` (or their long forms) were given without a value; carries
    /// the option's spelling for the diagnostic.
    MissingValue(&'static str),
    /// No operand was given.
    MissingOperand,
    /// More than three operands were given; carries the first extra one.
    ExtraOperand(String),
    /// An operand was not a floating point number; carries the text.
    InvalidNumber(String),
    /// An operand parsed to a NaN; carries the text.
    NotANumber(String),
    /// The INCREMENT operand was zero; carries the text.
    ZeroIncrement(String),
    /// `-f` was combined with `-w`.
    FormatWithEqualWidth,
    /// The `-f` format has no `%` directive; carries the format.
    FormatNoDirective(String),
    /// The `-f` format ends inside its one directive; carries the format.
    FormatEndsInPercent(String),
    /// The `-f` format's conversion is not one of `efgaEFGA`; carries the
    /// format and the offending character.
    FormatUnknownDirective(String, char),
    /// The `-f` format has more than one `%` directive; carries the format.
    FormatTooManyDirectives(String),
    /// Writing to standard output failed.
    Output,
}

impl fmt::Display for SeqError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownLong(token) => {
                write!(f, "unrecognized option {token}", token = quote(token))
            }
            Self::UnknownShort(flag) => write!(
                f,
                "invalid option -- {}",
                quote(flag.encode_utf8(&mut [0; 4]))
            ),
            Self::MissingValue(option) => {
                write!(
                    f,
                    "option {option} requires an argument",
                    option = quote(option)
                )
            }
            Self::MissingOperand => write!(f, "missing operand"),
            Self::ExtraOperand(text) => write!(f, "extra operand {text}", text = quote(text)),
            Self::InvalidNumber(text) => {
                write!(
                    f,
                    "invalid floating point argument: {text}",
                    text = quote(text)
                )
            }
            Self::NotANumber(text) => {
                write!(
                    f,
                    "invalid 'not-a-number' argument: {text}",
                    text = quote(text)
                )
            }
            Self::ZeroIncrement(text) => {
                write!(
                    f,
                    "invalid Zero increment value: {text}",
                    text = quote(text)
                )
            }
            Self::FormatWithEqualWidth => write!(
                f,
                "format string may not be specified when printing equal width strings"
            ),
            Self::FormatNoDirective(fmt_str) => {
                write!(
                    f,
                    "format {fmt_str} has no % directive",
                    fmt_str = quote(fmt_str)
                )
            }
            Self::FormatEndsInPercent(fmt_str) => {
                write!(f, "format {fmt_str} ends in %", fmt_str = quote(fmt_str))
            }
            Self::FormatUnknownDirective(fmt_str, ch) => {
                write!(
                    f,
                    "format {fmt_str} has unknown %{ch} directive",
                    fmt_str = quote(fmt_str)
                )
            }
            Self::FormatTooManyDirectives(fmt_str) => {
                write!(
                    f,
                    "format {fmt_str} has too many % directives",
                    fmt_str = quote(fmt_str)
                )
            }
            Self::Output => write!(f, "write error"),
        }
    }
}
