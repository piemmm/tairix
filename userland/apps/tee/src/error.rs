//! The outcomes of parsing and running a `tee` command.

use alloc::string::String;
use core::fmt;
use tairix_abi::Errno;
use tairix_util::argv::{BadWord, WordMiss};
use tairix_util::getopt::Refusal;

/// Why a `tee` invocation did not complete.
///
/// Usage failures mirror the GNU tool's diagnostics; the runtime failure
/// leans on the frozen [`Errno`] for the wire-level cause so it invents no
/// parallel error set. A *per-output* open or write failure is not a
/// variant here: the client reports it on the diagnostic stream and
/// continues (or stops) per the selected output-error mode, exactly as the
/// GNU tool does.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TeeError {
    /// An option `getopt` refuses, worded as it words it.
    Option(Refusal),
    /// The `--output-error` value named no one of `warn`, `warn-nopipe`,
    /// `exit`, `exit-nopipe`; carries how, and the text.
    BadMode(WordMiss, String),
    /// Writing the diagnostic stream failed. Carries the underlying
    /// [`Errno`]. The tool never continues silently past an unreportable
    /// failure.
    Output(Errno),
}

impl fmt::Display for TeeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::BadMode(miss, word) => write!(
                f,
                "{}",
                BadWord {
                    miss: *miss,
                    word,
                    option: "--output-error",
                    choices: &crate::command::MODES,
                }
            ),
            Self::Output(errno) => write!(f, "write error: {errno}"),
        }
    }
}
