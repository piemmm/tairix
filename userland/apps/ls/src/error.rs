//! The outcomes of running an `ls` command.

use core::fmt;
use tairix_abi::Errno;
use tairix_util::getopt::Refusal;

/// Why an `ls` invocation did not complete: an operand or directory that
/// cannot be reached is reported and listed past, never one of these.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LsError {
    /// An option `getopt` refuses, worded as it words it. No path is
    /// inspected.
    Option(Refusal),
    /// An option's value `ls` cannot read: a width, a block size, a name
    /// pattern, or a word outside the option's set. No path is inspected.
    Usage,
    /// Writing to the terminal failed. Carries the underlying [`Errno`].
    Output(Errno),
    /// A name filter's match needed more memory than the heap would give:
    /// GNU's `memory exhausted`, which ends the run.
    MemoryExhausted,
}

impl fmt::Display for LsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::Usage => f.write_str("invalid usage"),
            Self::Output(errno) => write!(f, "terminal write failed: {errno}"),
            Self::MemoryExhausted => f.write_str("memory exhausted"),
        }
    }
}
