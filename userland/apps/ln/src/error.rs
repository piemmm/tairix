//! The outcomes of running an `ln` command.

use alloc::string::String;
use core::fmt;

use tairix_abi::Errno;
use tairix_util::getopt::Refusal;
use tairix_util::output::Failure;
use tairix_util::quote::{quote, quote_subject};

/// Why an `ln` invocation did not complete.
///
/// Each variant carries the operand the GNU tool names in its own
/// diagnostic, so the message a user reads points at the path that failed;
/// the wire-level cause is the frozen [`Errno`], never a parallel error set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LnError {
    /// An option `getopt` refuses, worded as it words it. No path is
    /// touched.
    Option(Refusal),
    /// `-t` and `-T` together: the two destination readings contradict.
    TargetConflict,
    /// `-r` was given without `-s`. A hard link stores no target, so there
    /// is nothing to make relative; refused rather than ignored.
    RelativeNeedsSymbolic,
    /// No operand was given.
    MissingOperand,
    /// A form that needs a destination operand was given only one operand.
    MissingDestination(String),
    /// More operands than the form accepts (`-T` takes exactly two).
    ExtraOperand(String),
    /// The `-t` directory is not one.
    NotADirectory(String),
    /// The last of several operands, which must be a directory to hold their
    /// links, is missing ([`Errno::NotFound`]) or is not one
    /// ([`Errno::NotADirectory`]).
    Destination(String, Errno),
    /// A path could not be reached: a link name, the `-t` directory, or a hard
    /// link's target. Carries the path and the underlying [`Errno`].
    Stat(String, Errno),
    /// `-r` could not canonicalise a path it needs to compute the relative
    /// target — the target itself, or the link's own directory. Carries the
    /// path and the underlying [`Errno`].
    Canonicalize(String, Errno),
    /// The link could not be created.
    Create {
        /// The link's name.
        link: String,
        /// The target it was to name, as given.
        target: String,
        /// Whether it was to be a symbolic link.
        symbolic: bool,
        /// Why: [`Errno::AlreadyExists`] for a taken name,
        /// [`Errno::IsADirectory`] for a hard link to a directory,
        /// [`Errno::NotSupported`] on a format that stores no link of the kind.
        errno: Errno,
    },
    /// The new link would take the place of the very file it names.
    SameFile {
        /// The target, as given.
        target: String,
        /// The link's name.
        link: String,
    },
    /// A confirmation could not be read. Never treated as consent.
    Prompt(Errno),
    /// Writing the usage banner or a `-v` report failed.
    Output(Errno),
}

impl Failure for LnError {
    /// The confirmation channel or the terminal failing; a link's failure is
    /// carried past. (A destination that cannot be inspected ends the run
    /// before any link.)
    fn is_fatal(&self) -> bool {
        !matches!(
            self,
            Self::Stat(..) | Self::Canonicalize(..) | Self::Create { .. } | Self::SameFile { .. }
        )
    }

    fn unreported(errno: Errno) -> Self {
        Self::Output(errno)
    }
}

impl fmt::Display for LnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::TargetConflict => {
                f.write_str("cannot combine --target-directory and --no-target-directory")
            }
            Self::RelativeNeedsSymbolic => f.write_str("cannot do --relative without --symbolic"),
            Self::MissingOperand => f.write_str("missing file operand"),
            Self::MissingDestination(target) => {
                write!(
                    f,
                    "missing destination file operand after {target}",
                    target = quote(target)
                )
            }
            Self::ExtraOperand(operand) => {
                write!(f, "extra operand {operand}", operand = quote(operand))
            }
            Self::NotADirectory(path) => {
                write!(f, "target {path} is not a directory", path = quote(path))
            }
            Self::Destination(path, errno) => write!(f, "target {}: {errno}", quote(path)),
            Self::Stat(path, errno) => write!(f, "failed to access {}: {errno}", quote(path)),
            Self::Canonicalize(path, errno) => {
                write!(f, "cannot canonicalize {path}: {errno}", path = quote(path))
            }
            Self::Create {
                link,
                symbolic: true,
                errno,
                ..
            } => write!(f, "failed to create symbolic link {}: {errno}", quote(link)),
            Self::Create {
                target,
                errno: Errno::IsADirectory,
                ..
            } => write!(
                f,
                "{}: hard link not allowed for directory",
                quote_subject(target)
            ),
            Self::Create {
                link,
                errno: errno @ Errno::AlreadyExists,
                ..
            } => write!(f, "failed to create hard link {}: {errno}", quote(link)),
            Self::Create {
                link,
                target,
                errno,
                ..
            } => write!(
                f,
                "failed to create hard link {} => {}: {errno}",
                quote(link),
                quote(target)
            ),
            Self::SameFile { target, link } => {
                write!(f, "{} and {} are the same file", quote(target), quote(link))
            }
            Self::Prompt(errno) => write!(f, "cannot read the reply: {errno}"),
            Self::Output(errno) => write!(f, "terminal write failed: {errno}"),
        }
    }
}
