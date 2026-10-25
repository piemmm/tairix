//! Why a `chown` run, or one node of it, did not complete.

use alloc::string::String;
use core::fmt;

use tairix_abi::Errno;
use tairix_fscopy::tree::RootRefusal;
use tairix_util::getopt::Refusal;
use tairix_util::output::Failure;
use tairix_util::quote::quote;

/// Why a `chown` invocation, or one node of it, did not complete.
///
/// A command-line, owner, reference or stream failure ends the run with
/// nothing changed after it; a node's is reported and carried past.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChownError {
    /// An option `getopt` refuses, worded as it words it.
    Option(Refusal),
    /// No operand, or `--reference` with no file.
    MissingOperand,
    /// The owner operand, carried, with no file after it.
    MissingFile(String),
    /// `-L` under `-R`: a walk follows no link it meets, so the option could
    /// only be honoured in part.
    EveryLink,
    /// The owner operand, carried, names no account and is no number.
    BadUser(String),
    /// The owner operand, carried, names a group that is neither listed nor a
    /// number.
    BadGroup(String),
    /// The account directory, or the group directory when `groups`, could not
    /// be read.
    Directory {
        /// The group directory rather than the account one.
        groups: bool,
        /// Why.
        errno: Errno,
    },
    /// The `--reference` file could not be read.
    Reference {
        /// The file.
        path: String,
        /// Why.
        errno: Errno,
    },
    /// The node `path` could not be reached.
    Stat {
        /// The node.
        path: String,
        /// Why.
        errno: Errno,
    },
    /// The operand, carried, is a symbolic link naming nothing.
    Dangling(String),
    /// Under `--preserve-root`, the operand `path` reached the view root.
    PreserveRoot(String),
    /// Changing `path` was refused; `group` when only the group was asked,
    /// which GNU words apart.
    Apply {
        /// The node.
        path: String,
        /// Why.
        errno: Errno,
        /// Only the group was being changed.
        group: bool,
    },
    /// The directory `path` could not be listed for `-R`.
    Read {
        /// The directory.
        path: String,
        /// Why.
        errno: Errno,
    },
    /// A stream refused a write.
    Output(Errno),
}

impl Failure for ChownError {
    /// The command line, a name, the reference or a stream failing; a node's
    /// failure is carried past.
    fn is_fatal(&self) -> bool {
        !matches!(
            self,
            Self::Stat { .. }
                | Self::Dangling(_)
                | Self::PreserveRoot(_)
                | Self::Apply { .. }
                | Self::Read { .. }
        )
    }

    fn unreported(errno: Errno) -> Self {
        Self::Output(errno)
    }
}

impl fmt::Display for ChownError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::MissingOperand => f.write_str("missing operand"),
            Self::MissingFile(spec) => write!(f, "missing operand after {}", quote(spec)),
            Self::EveryLink => f.write_str(
                "-L is not available: a recursive change follows no link it meets, so the \
                 option is refused rather than honoured in part (-H follows a link named as \
                 an operand)",
            ),
            Self::BadUser(spec) => write!(f, "invalid user: {}", quote(spec)),
            Self::BadGroup(spec) => write!(f, "invalid group: {}", quote(spec)),
            Self::Directory { groups, errno } => write!(
                f,
                "cannot read the {} directory: {errno}",
                if *groups { "group" } else { "account" }
            ),
            Self::Reference { path, errno } => {
                write!(f, "failed to get attributes of {}: {errno}", quote(path))
            }
            Self::Stat { path, errno } => write!(f, "cannot access {}: {errno}", quote(path)),
            Self::Dangling(path) => {
                write!(f, "cannot dereference {}: {}", quote(path), Errno::NotFound)
            }
            Self::PreserveRoot(path) => write!(
                f,
                "{}",
                RootRefusal {
                    word: "chown",
                    path
                }
            ),
            Self::Apply { path, errno, group } => write!(
                f,
                "changing {} of {}: {errno}",
                if *group { "group" } else { "ownership" },
                quote(path)
            ),
            Self::Read { path, errno } => {
                write!(f, "cannot read directory {}: {errno}", quote(path))
            }
            Self::Output(errno) => write!(f, "terminal write failed: {errno}"),
        }
    }
}
