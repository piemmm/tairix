//! The outcomes of running an `unmount` command.

use alloc::string::String;
use core::fmt;

use tairix_abi::Errno;
use tairix_help::MISUSE;
use tairix_procinfo::{CallError, ListError};
use tairix_util::getopt::Refusal;
use tairix_util::quote::quote_subject;

/// Why an `unmount` invocation did not complete.
///
/// The wire-level causes lean on the frozen [`Errno`] so no parallel
/// error set is invented; the name-shaped cases carry the operand so the
/// diagnostic names what the user typed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UnmountError {
    /// An option `getopt` refuses, worded as it words it.
    Option(Refusal),
    /// A number of operands other than one.
    Usage,
    /// No mounted filesystem matches the named volume.
    NotFound(String),
    /// No volume shown to the caller matches the name, though another
    /// user's private volume, whose source and bindings are withheld, might.
    NotShown(String),
    /// The path named could not be resolved.
    Path {
        /// The path, as named.
        name: String,
        /// Its refusal.
        errno: Errno,
    },
    /// The file name more than one volume was attached from, so it names
    /// none of them.
    Ambiguous(String),
    /// A file name the start of which is all the mount listing keeps of a
    /// volume's, so no volume is known to be the one it names.
    NameCut(String),
    /// The named mount exists but carries no detachable volume identity
    /// (a boot volume or an in-RAM view binding — permanent by design).
    NotDetachable(String),
    /// The kernel refused or failed the detach. `unavailable` is `true`
    /// when the mount listing already showed the volume as
    /// surprise-removed, so the diagnostic can spell out the `--force`
    /// consequence.
    Detach {
        /// The volume, as named.
        name: String,
        /// The kernel's exact refusal.
        errno: Errno,
        /// Whether the volume was listed as unavailable (dirty or lost).
        unavailable: bool,
    },
    /// The `MOUNT_LIST` or `VIEW_LIST` query failed: the transport errored,
    /// the service refused, or the reply did not decode against
    /// `sysinfo-v1`.
    Service(Errno),
    /// A diagnostic (or the short help) could not be written.
    Output(Errno),
}

impl UnmountError {
    /// The exit status util-linux's `umount` gives the failure: `1` for a
    /// command line it did not understand or a detach the caller may not
    /// make, `2` for a service or terminal that failed it, `32` for an
    /// unmount that failed.
    #[must_use]
    pub const fn status(&self) -> i32 {
        match self {
            Self::Option(_)
            | Self::Usage
            | Self::Detach {
                errno: Errno::PermissionDenied,
                ..
            } => MISUSE,
            Self::Service(_) | Self::Output(_) => 2,
            Self::NotFound(_)
            | Self::NotShown(_)
            | Self::Path { .. }
            | Self::Ambiguous(_)
            | Self::NameCut(_)
            | Self::NotDetachable(_)
            | Self::Detach { .. } => 32,
        }
    }
}

impl From<CallError> for UnmountError {
    fn from(err: CallError) -> Self {
        match err {
            // The mount-list query is ungated, so a denial is a
            // service-level anomaly rather than a missing capability the
            // user could grant; report it as such.
            CallError::PermissionDenied => Self::Service(Errno::PermissionDenied),
            CallError::Service(errno) => Self::Service(errno),
        }
    }
}

impl From<ListError> for UnmountError {
    fn from(err: ListError) -> Self {
        match err {
            ListError::Call(call) => call.into(),
            ListError::Sink(errno) => Self::Output(errno),
        }
    }
}

/// A failed unmount is worded as util-linux's `umount` words one, a sentence
/// naming the volume and ending in a full stop; a command line not
/// understood is not.
impl fmt::Display for UnmountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::Usage => f.write_str("invalid usage"),
            Self::NotFound(name) => write!(f, "{name}: not mounted.", name = quote_subject(name)),
            Self::NotShown(name) => write!(
                f,
                "{name}: no volume shown to you; another user's private volume is named \
                 by where it is mounted in /Storage.",
                name = quote_subject(name)
            ),
            Self::Ambiguous(name) => write!(
                f,
                "{name}: more than one volume was mounted from a file of that name; \
                 name it by where it is mounted.",
                name = quote_subject(name)
            ),
            Self::NameCut(name) => write!(
                f,
                "{name}: the mount listing keeps only the start of that file name; \
                 name the volume by where it is mounted.",
                name = quote_subject(name)
            ),
            Self::NotDetachable(name) => {
                write!(
                    f,
                    "{name}: not a detachable volume.",
                    name = quote_subject(name)
                )
            }
            Self::Detach {
                name,
                errno,
                unavailable: true,
            } => write!(
                f,
                "{name}: volume holds retained uncommitted data \
                 (use --force to discard it): {errno}.",
                name = quote_subject(name)
            ),
            Self::Path { name, errno }
            | Self::Detach {
                name,
                errno,
                unavailable: false,
            } => write!(f, "{name}: {errno}.", name = quote_subject(name)),
            Self::Service(errno) => write!(f, "system information service error: {errno}"),
            Self::Output(errno) => write!(f, "cannot write output: {errno}"),
        }
    }
}
