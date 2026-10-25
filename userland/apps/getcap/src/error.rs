//! The outcomes of running a `getcap` command.

use alloc::string::String;
use core::fmt;

use tairix_abi::Errno;
use tairix_util::getopt::Refusal;
use tairix_util::output::Failure;
use tairix_util::quote::quote;

/// Why a `getcap` invocation did not complete.
///
/// The variants are deliberately coarse: the CLI surfaces enough to print a
/// useful diagnostic and set a process exit status, while leaning on the
/// frozen [`Errno`] for the wire-level cause so it invents no parallel error
/// set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GetcapError {
    /// An option `getopt` refuses, worded as it words it. Nothing is
    /// reported.
    Option(Refusal),
    /// The command line named no file operand. Nothing is reported.
    Usage,
    /// Opening `path` failed.
    Stat {
        /// The path.
        path: String,
        /// Why.
        errno: Errno,
    },
    /// The operand `path` is a symbolic link naming nothing.
    Dangling(String),
    /// Reading the capability gate of `path` failed.
    Query {
        /// The path.
        path: String,
        /// Why.
        errno: Errno,
    },
    /// Opening or listing the directory `path` during a recursive descent failed.
    Read {
        /// The path.
        path: String,
        /// Why.
        errno: Errno,
    },
    /// Writing the report or the usage banner to the terminal failed. Carries
    /// the underlying [`Errno`].
    Output(Errno),
}

impl Failure for GetcapError {
    /// The command line or the terminal failing; a node's failure is carried
    /// past.
    fn is_fatal(&self) -> bool {
        matches!(self, Self::Option(_) | Self::Usage | Self::Output(_))
    }

    fn unreported(errno: Errno) -> Self {
        Self::Output(errno)
    }
}

impl fmt::Display for GetcapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::Usage => f.write_str("invalid usage"),
            Self::Stat { path, errno } => write!(f, "cannot access {}: {errno}", quote(path)),
            Self::Dangling(path) => {
                write!(f, "cannot operate on dangling symlink {}", quote(path))
            }
            Self::Query { path, errno } => write!(
                f,
                "cannot read the capability gate of {}: {errno}",
                quote(path)
            ),
            Self::Read { path, errno } => {
                write!(f, "cannot read directory {}: {errno}", quote(path))
            }
            Self::Output(errno) => write!(f, "terminal write failed: {errno}"),
        }
    }
}
