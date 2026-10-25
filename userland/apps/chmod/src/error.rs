//! The outcomes of running a `chmod` command.

use alloc::string::String;
use core::fmt;
use tairix_abi::Errno;
use tairix_fscopy::tree::RootRefusal;
use tairix_util::getopt::Refusal;
use tairix_util::output::Failure;
use tairix_util::quote::quote;

/// Why a `chmod` invocation did not complete.
///
/// The variants are deliberately coarse: the CLI surfaces enough to print a
/// useful diagnostic and set a process exit status, while leaning on the
/// frozen [`Errno`] for the wire-level cause so it invents no parallel error
/// set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChmodError {
    /// An option `getopt` refuses, worded as it words it. Nothing is
    /// changed.
    Option(Refusal),
    /// The command line named fewer than two operands (`chmod` needs a mode
    /// and at least one file). Nothing is changed.
    Usage,
    /// The mode operand, carried, reads as neither an octal mode nor a
    /// symbolic one (`tairix_util::mode`). Nothing is changed.
    BadMode(String),
    /// Inspecting `path` failed — [`Errno::NotFound`] for a missing file,
    /// [`Errno::PermissionDenied`] for one the caller may not reach.
    Stat {
        /// The path inspected.
        path: String,
        /// Why.
        errno: Errno,
    },
    /// The operand `path` is a symbolic link naming nothing.
    Dangling(String),
    /// Under `--preserve-root`, the operand `path` reached the view root.
    PreserveRoot(String),
    /// Applying the new mode to `path` failed.
    Apply {
        /// The path changed.
        path: String,
        /// Why.
        errno: Errno,
    },
    /// Opening or listing the directory `path` during a recursive (`-R`)
    /// descent failed.
    Read {
        /// The directory read.
        path: String,
        /// Why.
        errno: Errno,
    },
    /// Writing the usage banner, a report or a diagnostic to the terminal
    /// failed.
    Output(Errno),
}

impl Failure for ChmodError {
    /// The command line or the terminal failing; a node's failure is carried
    /// past.
    fn is_fatal(&self) -> bool {
        matches!(
            self,
            Self::Option(_) | Self::Usage | Self::BadMode(_) | Self::Output(_)
        )
    }

    fn unreported(errno: Errno) -> Self {
        Self::Output(errno)
    }
}

impl fmt::Display for ChmodError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::Usage => f.write_str("invalid usage"),
            Self::BadMode(spec) => write!(f, "invalid mode: {}", quote(spec)),
            Self::Stat { path, errno } => write!(f, "cannot access {}: {errno}", quote(path)),
            Self::Dangling(path) => {
                write!(f, "cannot operate on dangling symlink {}", quote(path))
            }
            Self::PreserveRoot(path) => write!(
                f,
                "{}",
                RootRefusal {
                    word: "chmod",
                    path
                }
            ),
            Self::Apply { path, errno } => {
                write!(f, "changing permissions of {}: {errno}", quote(path))
            }
            Self::Read { path, errno } => {
                write!(f, "cannot read directory {}: {errno}", quote(path))
            }
            Self::Output(errno) => write!(f, "terminal write failed: {errno}"),
        }
    }
}
