//! The outcomes of parsing and running a `wc` command.

use alloc::string::String;
use core::fmt;
use tairix_abi::Errno;
use tairix_util::argv::{BadWord, WordMiss};
use tairix_util::getopt::Refusal;

/// Why a `wc` invocation did not complete.
///
/// Usage failures mirror the GNU tool's diagnostics; the runtime failure
/// leans on the frozen [`Errno`] for the wire-level cause so it invents no
/// parallel error set. A *per-file* read failure is not a variant here: the
/// client reports it on the diagnostic stream and continues with the next
/// operand, exactly as the GNU tool does.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WcError {
    /// An option `getopt` refuses, worded as it words it.
    Option(Refusal),
    /// The `--total` value named no one of `auto`, `always`, `only`,
    /// `never`; carries how, and the text.
    BadTotal(WordMiss, String),
    /// File operands were given together with `--files0-from`, which is
    /// the GNU conflict.
    Files0Conflict,
    /// Writing to standard output (or the diagnostic stream) failed.
    /// Carries the underlying [`Errno`].
    Output(Errno),
}

impl fmt::Display for WcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::BadTotal(miss, word) => write!(
                f,
                "{}",
                BadWord {
                    miss: *miss,
                    word,
                    option: "--total",
                    choices: &crate::command::MODES,
                }
            ),
            Self::Files0Conflict => {
                f.write_str("file operands cannot be combined with --files0-from")
            }
            Self::Output(errno) => write!(f, "write error: {errno}"),
        }
    }
}
