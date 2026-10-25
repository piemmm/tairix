//! The outcomes of running a `setcap` command.

use alloc::string::String;
use core::fmt;

use tairix_abi::Errno;
use tairix_util::getopt::Refusal;
use tairix_util::output::Failure;
use tairix_util::quote::quote;

/// Why a `setcap` invocation did not complete.
///
/// The variants are deliberately coarse: the CLI surfaces enough to print a
/// useful diagnostic and set a process exit status, while leaning on the
/// frozen [`Errno`] for the wire-level cause so it invents no parallel error
/// set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SetcapError {
    /// An option `getopt` refuses, worded as it words it. Nothing is
    /// changed.
    Option(Refusal),
    /// The command line named fewer than two operands (`setcap` needs a
    /// capability spec and at least one file). Nothing is changed.
    Usage,
    /// The capability operand was neither a known canonical `CAP_*` name nor
    /// the literal `-` (clear the gate). Nothing is changed.
    BadCapability,
    /// Opening `path` failed.
    Stat {
        /// The path.
        path: String,
        /// Why.
        errno: Errno,
    },
    /// The operand `path` is a symbolic link naming nothing.
    Dangling(String),
    /// Setting the capability gate of `path` failed.
    Apply {
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
    /// Writing the usage banner to the terminal failed. Carries the underlying
    /// [`Errno`].
    Output(Errno),
}

impl Failure for SetcapError {
    /// The command line or the terminal failing; a node's failure is carried
    /// past.
    fn is_fatal(&self) -> bool {
        matches!(
            self,
            Self::Option(_) | Self::Usage | Self::BadCapability | Self::Output(_)
        )
    }

    fn unreported(errno: Errno) -> Self {
        Self::Output(errno)
    }
}

impl fmt::Display for SetcapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::Usage => f.write_str("invalid usage"),
            Self::BadCapability => f.write_str("invalid capability"),
            Self::Stat { path, errno } => write!(f, "cannot access {}: {errno}", quote(path)),
            Self::Dangling(path) => {
                write!(f, "cannot operate on dangling symlink {}", quote(path))
            }
            Self::Apply { path, errno } => write!(
                f,
                "cannot set the capability gate of {}: {errno}",
                quote(path)
            ),
            Self::Read { path, errno } => {
                write!(f, "cannot read directory {}: {errno}", quote(path))
            }
            Self::Output(errno) => write!(f, "terminal write failed: {errno}"),
        }
    }
}
