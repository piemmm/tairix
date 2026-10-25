//! The outcomes of parsing and running a `seq` command.

use alloc::string::String;
use core::fmt;
use tairix_util::getopt::Refusal;
use tairix_util::quote::{quote, quote_if_needed};

/// Why a `seq` invocation did not complete.
///
/// Usage failures are the GNU tool's diagnostics word for word, save that a
/// control or other hidden character in a rejected directive is escaped
/// where GNU prints it raw; the one runtime failure is the output stream
/// refusing bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SeqError {
    /// An option `getopt` refuses, worded as it words it.
    Option(Refusal),
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
            Self::Option(refusal) => write!(f, "{refusal}"),
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
                let mut directive = [0; 8];
                directive[0] = b'%';
                let spelled = 1 + ch.encode_utf8(&mut directive[1..]).len();
                write!(
                    f,
                    "format {} has unknown {} directive",
                    quote(fmt_str),
                    quote_if_needed(core::str::from_utf8(&directive[..spelled]).unwrap_or("%"))
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

#[cfg(test)]
mod tests {
    use super::SeqError;
    use alloc::string::{String, ToString};
    use tairix_util::getopt::Refusal;

    #[test]
    fn an_unknown_directive_is_shown_with_its_hidden_characters_escaped() {
        let escape = SeqError::FormatUnknownDirective(String::from("%\u{1b}"), '\u{1b}');
        assert_eq!(
            escape.to_string(),
            "format '%'$'\\033' has unknown '%'$'\\033' directive"
        );
        let letter = SeqError::FormatUnknownDirective(String::from("%Q"), 'Q');
        assert_eq!(letter.to_string(), "format '%Q' has unknown %Q directive");
    }

    #[test]
    fn a_missing_value_is_named_as_getopt_names_it() {
        assert_eq!(
            SeqError::Option(Refusal::MissingShortValue('f')).to_string(),
            "option requires an argument -- 'f'"
        );
        assert_eq!(
            SeqError::Option(Refusal::MissingLongValue("separator")).to_string(),
            "option '--separator' requires an argument"
        );
    }
}
