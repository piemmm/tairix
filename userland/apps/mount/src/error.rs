//! The outcomes of running a `mount` command.

use core::fmt;
use tairix_abi::Errno;
use tairix_procinfo::{CallError, ListError};
use tairix_util::getopt::Refusal;

/// Why a `mount` invocation did not complete.
///
/// The variants are deliberately coarse: the CLI surfaces enough to print a
/// useful diagnostic and set a process exit status, while leaning on the
/// frozen [`Errno`] for the wire-level cause so it invents no parallel error
/// set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MountError {
    /// An option `getopt` refuses, worded as it words it.
    Option(Refusal),
    /// The command line carried a number of operands other than zero (list)
    /// or two (mount), or mount options with no operands.
    Usage,
    /// A `-o` option value named an unknown mount option, or `-t`/`-o` was
    /// given an empty value.
    BadOption,
    /// The mount operation was refused or failed. Carries the underlying
    /// [`Errno`] — e.g. [`Errno::PermissionDenied`] when the caller lacks
    /// `CAP_FS_MOUNT`, which is the kernel's decision to make, not the
    /// tool's.
    Mount(Errno),
    /// Listing the mount table failed: the transport errored or the reply
    /// did not decode against `sysinfo-v1`. Carries the underlying
    /// [`Errno`].
    Service(Errno),
    /// Writing to the terminal failed. Carries the underlying [`Errno`].
    Output(Errno),
}

impl From<CallError> for MountError {
    fn from(err: CallError) -> Self {
        match err {
            // The mount-list query is ungated, so a
            // denial is a service-level anomaly rather than a missing
            // capability the user could grant; report it as such.
            CallError::PermissionDenied => Self::Service(Errno::PermissionDenied),
            CallError::Service(errno) => Self::Service(errno),
        }
    }
}

impl From<ListError> for MountError {
    fn from(err: ListError) -> Self {
        match err {
            ListError::Call(call) => call.into(),
            ListError::Sink(errno) => Self::Output(errno),
        }
    }
}

impl fmt::Display for MountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::Usage => f.write_str("invalid usage"),
            Self::BadOption => f.write_str("invalid mount option"),
            Self::Mount(errno) => write!(f, "mount failed: {errno}"),
            Self::Service(errno) => write!(f, "system information service error: {errno}"),
            Self::Output(errno) => write!(f, "terminal write failed: {errno}"),
        }
    }
}
