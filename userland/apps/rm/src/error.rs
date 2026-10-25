//! The outcomes of running an `rm` command.

use alloc::string::String;
use core::fmt;

use tairix_abi::Errno;
use tairix_fscopy::tree::RootRefusal;
use tairix_util::getopt::Refusal;
use tairix_util::output::Failure;
use tairix_util::quote::quote;

/// Why an `rm` operand, or the whole invocation, did not complete.
///
/// A failure on one path names it in GNU's words and the run goes on; only a
/// [fatal](Failure::is_fatal) one ends it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RmError {
    /// An option `getopt` refuses, worded as it words it. Nothing is
    /// removed.
    Option(Refusal),
    /// `--no-preserve-root` was abbreviated: the switch that lets a
    /// recursive removal reach `/` is accepted only in full, as GNU accepts
    /// it. Nothing is removed.
    AbbreviatedNoPreserveRoot,
    /// The command line named no operand without `-f`. Nothing is removed.
    Usage,
    /// An operand named a directory but neither `-r` nor `-d` was given.
    IsDirectory(String),
    /// Under `-r` or `-d`, an operand's last component is `.` or `..`, which
    /// POSIX never lets `rm` remove. Nothing is looked up.
    Dot(String),
    /// Inspecting `path` failed — [`Errno::NotFound`] for a missing one
    /// (suppressed by `-f`), [`Errno::PermissionDenied`] for one the caller
    /// may not reach.
    Stat {
        /// The path inspected.
        path: String,
        /// Why.
        errno: Errno,
    },
    /// Opening or listing the directory `path` failed.
    Read {
        /// The directory read.
        path: String,
        /// Why.
        errno: Errno,
    },
    /// Removing `path` failed.
    Remove {
        /// The path removed.
        path: String,
        /// Why.
        errno: Errno,
    },
    /// Writing to the terminal failed.
    Output(Errno),
    /// A recursive removal named the view root, as spelled here, while
    /// `--preserve-root` (the default) is in effect. Nothing is removed.
    PreserveRoot(String),
    /// Asking the interactive confirmation question failed. Failing closed:
    /// an unanswerable prompt never counts as consent.
    Prompt(Errno),
}

impl Failure for RmError {
    /// The command line, the terminal or the confirmation channel failing;
    /// a path's failure is carried past.
    fn is_fatal(&self) -> bool {
        matches!(
            self,
            Self::Option(_)
                | Self::AbbreviatedNoPreserveRoot
                | Self::Usage
                | Self::Output(_)
                | Self::Prompt(_)
        )
    }

    fn unreported(errno: Errno) -> Self {
        Self::Output(errno)
    }
}

impl fmt::Display for RmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::AbbreviatedNoPreserveRoot => {
                f.write_str("you may not abbreviate the --no-preserve-root option")
            }
            Self::Usage => f.write_str("invalid usage"),
            Self::IsDirectory(path) => {
                write!(f, "cannot remove {}: {}", quote(path), Errno::IsADirectory)
            }
            Self::Dot(path) => write!(
                f,
                "refusing to remove '.' or '..' directory: skipping {}",
                quote(path)
            ),
            Self::Stat { path, errno }
            | Self::Read { path, errno }
            | Self::Remove { path, errno } => {
                write!(f, "cannot remove {}: {errno}", quote(path))
            }
            Self::Output(errno) => write!(f, "terminal write failed: {errno}"),
            Self::PreserveRoot(path) => write!(f, "{}", RootRefusal { word: "rm", path }),
            Self::Prompt(errno) => write!(f, "cannot read confirmation: {errno}"),
        }
    }
}
