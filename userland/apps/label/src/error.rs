//! The outcomes of running a `label` command.

use alloc::string::String;
use core::fmt;

use tairix_abi::Errno;
use tairix_help::MISUSE;
use tairix_procinfo::{CallError, ListError, Unnamed};
use tairix_util::getopt::Refusal;
use tairix_util::quote::quote_subject;

/// Why a `label` invocation did not complete.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LabelError {
    /// An option `getopt` refuses, worded as it words it.
    Option(Refusal),
    /// No operand, an empty name, or more than two operands.
    Usage,
    /// The name names no volume.
    Unresolved {
        /// The volume, as named.
        name: String,
        /// Why it names none.
        why: Unnamed,
    },
    /// The title asked for is longer than any volume keeps.
    TitleTooLong(String),
    /// The title asked for carries a control character.
    TitleControl(String),
    /// The volume's title is withheld from the caller: another user's
    /// private volume.
    Withheld(String),
    /// The name names no volume a title can be set on: the catalog, a boot
    /// volume, a view binding.
    NotTitleable(String),
    /// The kernel refused or failed the retitle.
    Retitle {
        /// The volume, as named.
        name: String,
        /// The kernel's exact refusal.
        errno: Errno,
    },
    /// The `MOUNT_LIST` or `VIEW_LIST` query failed.
    Service(Errno),
    /// The title, the help or the version could not be written.
    Output(Errno),
}

impl LabelError {
    /// The exit status: `1` for a command line not understood or a title not
    /// shown or set, `2` for a service or terminal that failed it.
    #[must_use]
    pub const fn status(&self) -> i32 {
        match self {
            Self::Option(_) | Self::Usage => MISUSE,
            Self::Service(_) | Self::Output(_) => 2,
            Self::Unresolved { .. }
            | Self::TitleTooLong(_)
            | Self::TitleControl(_)
            | Self::Withheld(_)
            | Self::NotTitleable(_)
            | Self::Retitle { .. } => 1,
        }
    }
}

impl From<CallError> for LabelError {
    fn from(err: CallError) -> Self {
        match err {
            // The listings are ungated, so a denial is a service anomaly,
            // not a capability the user could be granted.
            CallError::PermissionDenied => Self::Service(Errno::PermissionDenied),
            CallError::Service(errno) => Self::Service(errno),
        }
    }
}

impl From<ListError> for LabelError {
    fn from(err: ListError) -> Self {
        match err {
            ListError::Call(call) => call.into(),
            ListError::Sink(errno) => Self::Output(errno),
        }
    }
}

/// A failure is a sentence naming the volume or the title and ending in a
/// full stop; a command line not understood is worded as `getopt` words it.
impl fmt::Display for LabelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::Usage => f.write_str("invalid usage"),
            Self::Unresolved { name, why } => write!(f, "{}: {why}.", quote_subject(name)),
            Self::TitleTooLong(title) => write!(f, "{}: title too long.", quote_subject(title)),
            Self::TitleControl(title) => write!(f, "{}: unprintable title.", quote_subject(title)),
            Self::Withheld(name) => {
                write!(f, "{}: its title is not shown to you.", quote_subject(name))
            }
            Self::NotTitleable(name) => write!(
                f,
                "{}: not a volume a title can be given.",
                quote_subject(name)
            ),
            Self::Retitle { name, errno } => {
                let name = quote_subject(name);
                match errno {
                    Errno::NotSupported => write!(f, "{name}: the volume takes no new title."),
                    Errno::ReadOnly => write!(f, "{name}: the volume is read-only."),
                    Errno::LengthOutOfRange => {
                        write!(f, "{name}: the title is too long for the volume's format.")
                    }
                    Errno::OutOfRange => {
                        write!(f, "{name}: the volume's format cannot spell the title.")
                    }
                    errno => write!(f, "{name}: {errno}."),
                }
            }
            Self::Service(errno) => write!(f, "system information service error: {errno}"),
            Self::Output(errno) => write!(f, "cannot write output: {errno}"),
        }
    }
}
