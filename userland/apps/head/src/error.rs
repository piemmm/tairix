//! The outcomes of parsing and running a `head` command.

use alloc::string::String;
use core::fmt;
use tairix_abi::Errno;
use tairix_util::getopt::Refusal;
use tairix_util::quote::{quote, quote_if_needed};

/// Why a `head` invocation did not complete.
///
/// Usage failures mirror the GNU tool's diagnostics; the runtime failure
/// leans on the frozen [`Errno`] for the wire-level cause so it invents no
/// parallel error set. A *per-file* read failure is not a variant here: the
/// client reports it on the diagnostic stream and continues with the next
/// operand, exactly as the GNU tool does.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HeadError {
    /// An option `getopt` refuses, worded as it words it.
    Option(Refusal),
    /// The `-n`/`--lines` value was not a valid count; carries the text.
    InvalidLines(String),
    /// The `-c`/`--bytes` value was not a valid count; carries the text.
    InvalidBytes(String),
    /// A digit (or other letter the obsolete form does not accept)
    /// trailed an option, e.g. `head -5` given anywhere but as the first
    /// argument; carries the offending character.
    InvalidTrailing(char),
    /// Writing to standard output (or the diagnostic stream) failed.
    /// Carries the underlying [`Errno`].
    Output(Errno),
}

impl fmt::Display for HeadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::InvalidLines(text) => {
                write!(f, "invalid number of lines: {text}", text = quote(text))
            }
            Self::InvalidBytes(text) => {
                write!(f, "invalid number of bytes: {text}", text = quote(text))
            }
            Self::InvalidTrailing(flag) => write!(
                f,
                "invalid trailing option -- {}",
                quote_if_needed(flag.encode_utf8(&mut [0; 4]))
            ),
            Self::Output(errno) => write!(f, "write error: {errno}"),
        }
    }
}
