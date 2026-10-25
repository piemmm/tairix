//! The outcomes of running a `cat` command.

use alloc::string::String;
use core::fmt;
use tairix_abi::Errno;
use tairix_util::getopt::Refusal;
use tairix_util::output::Failure;
use tairix_util::quote::quote_subject;

/// Why a `cat` invocation did not complete.
///
/// The variants are deliberately coarse: the CLI surfaces enough to print a
/// useful diagnostic and set a process exit status, while leaning on the
/// frozen [`Errno`] for the wire-level cause so it invents no parallel error
/// set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CatError {
    /// An option `getopt` refuses, worded as it words it. No source is
    /// read.
    Option(Refusal),
    /// Reading a source failed — [`Errno::NotFound`] for a missing file,
    /// [`Errno::PermissionDenied`] for one the caller may not read.
    Read {
        /// The source as the command line named it, `-` for standard input.
        source: String,
        /// Why.
        errno: Errno,
    },
    /// Writing to the terminal failed. Carries the underlying [`Errno`].
    Output(Errno),
}

impl Failure for CatError {
    /// Everything but a source that cannot be read, which is carried past.
    fn is_fatal(&self) -> bool {
        !matches!(self, Self::Read { .. })
    }

    fn unreported(errno: Errno) -> Self {
        Self::Output(errno)
    }
}

impl fmt::Display for CatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::Read { source, errno } => write!(f, "{}: {errno}", quote_subject(source)),
            Self::Output(errno) => write!(f, "terminal write failed: {errno}"),
        }
    }
}
