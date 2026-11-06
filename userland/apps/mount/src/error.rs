//! The outcomes of running a `mount` command.

use alloc::string::String;
use core::fmt;

use tairix_abi::imageattach::ImageRefusal;
use tairix_abi::volume::ReplayAnswer;
use tairix_abi::Errno;
use tairix_help::MISUSE;
use tairix_procinfo::{CallError, ListError, Unnamed};
use tairix_util::getopt::Refusal;
use tairix_util::quote::{quote, quote_if_needed, quote_subject};

/// Why a `mount` invocation did not complete.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MountError {
    /// An option `getopt` refuses, worded as it words it.
    Option(Refusal),
    /// More than two operands, options with nothing to mount, or two
    /// different names asked for.
    Usage,
    /// An unknown or empty `-o` element.
    BadOption,
    /// A name no volume may take.
    BadName(String),
    /// A `-t` value no disk image holds.
    UnknownType {
        /// The image, as given.
        image: String,
        /// The type, as given.
        name: String,
    },
    /// The image or its filesystem takes no writes, and `-w` asked for them.
    WriteProtected {
        /// The image, as given.
        image: String,
    },
    /// The image holds nothing the kernel mounts.
    Refused {
        /// The image, as given.
        image: String,
        /// Why.
        why: ImageRefusal,
    },
    /// The caller has the volume the image holds mounted already.
    AlreadyMounted {
        /// The image, as given.
        image: String,
        /// Where it was asked to be shown.
        asked: String,
        /// Where it is mounted.
        at: String,
    },
    /// The kernel refused or failed the mount.
    Mount {
        /// The image, as given.
        image: String,
        /// Its refusal.
        errno: Errno,
    },
    /// The directory the image was to be shown at, or the catalog entry its
    /// name asked for, refused it.
    Target {
        /// The directory as resolved, or as given where it did not resolve, or
        /// the catalog entry its name asked for.
        target: String,
        /// Its refusal.
        errno: Errno,
    },
    /// No volume, or no one volume, is the one the name an answer was asked
    /// for names.
    Unresolved {
        /// The name, as given.
        name: String,
        /// Why it names none.
        why: Unnamed,
    },
    /// The volume named holds no writes awaiting an answer.
    NotHeld(String),
    /// The kernel refused or failed an answer for a volume's held writes.
    Answer {
        /// The volume, as named.
        name: String,
        /// What was asked of its writes.
        answer: ReplayAnswer,
        /// Its refusal.
        errno: Errno,
    },
    /// Listing the mount table failed: the transport errored or the reply
    /// did not decode against `sysinfo-v1`.
    Service(Errno),
    /// Writing to the terminal failed.
    Output(Errno),
}

impl MountError {
    /// The exit status util-linux's `mount` gives the failure: `1` for a
    /// command line it did not understand or an answer the caller may not
    /// give, `2` for a service or terminal that failed it, `32` for a mount or
    /// an answer that failed.
    #[must_use]
    pub const fn status(&self) -> i32 {
        match self {
            Self::Option(_)
            | Self::Usage
            | Self::BadOption
            | Self::BadName(_)
            | Self::Answer {
                errno: Errno::PermissionDenied,
                ..
            } => MISUSE,
            Self::Service(_) | Self::Output(_) => 2,
            Self::UnknownType { .. }
            | Self::WriteProtected { .. }
            | Self::Refused { .. }
            | Self::AlreadyMounted { .. }
            | Self::Mount { .. }
            | Self::Target { .. }
            | Self::Unresolved { .. }
            | Self::NotHeld(_)
            | Self::Answer { .. } => 32,
        }
    }

    /// Whether it is a command line not understood, which the hint to
    /// `--help` follows; a mount or an answer that failed is a sentence alone.
    #[must_use]
    pub const fn misused(&self) -> bool {
        matches!(
            self,
            Self::Option(_) | Self::Usage | Self::BadOption | Self::BadName(_)
        )
    }
}

impl From<CallError> for MountError {
    fn from(err: CallError) -> Self {
        match err {
            // The mount-list query is ungated, so a denial is the service
            // misbehaving, not an authority the user could be granted.
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

/// A failed mount is worded as util-linux words one, a sentence ending in a
/// full stop; a command line not understood is not.
impl fmt::Display for MountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::Usage => f.write_str("invalid usage"),
            Self::BadOption => f.write_str("invalid mount option"),
            Self::BadName(name) => write!(f, "{} cannot name a volume", quote(name)),
            Self::UnknownType { image, name } => write!(
                f,
                "{}: unknown filesystem type {}.",
                quote_subject(image),
                quote(name)
            ),
            Self::WriteProtected { image } => write!(
                f,
                "{} is write-protected but explicit read-write mode requested.",
                quote_if_needed(image)
            ),
            Self::Refused { image, why } => {
                write!(f, "{}: {}.", quote_subject(image), why.reason())
            }
            Self::AlreadyMounted { image, asked, at } => write!(
                f,
                "{}: {} already mounted on {}.",
                quote_subject(asked),
                quote_if_needed(image),
                quote_if_needed(at)
            ),
            Self::Mount { image, errno } => write!(f, "{}: {errno}.", quote_subject(image)),
            Self::Target { target, errno } => write!(f, "{}: {errno}.", quote_subject(target)),
            Self::Unresolved { name, why } => write!(f, "{}: {why}.", quote_subject(name)),
            Self::NotHeld(name) => write!(
                f,
                "{}: holds no writes awaiting an answer.",
                quote_subject(name)
            ),
            Self::Answer {
                name,
                answer: ReplayAnswer::Write,
                errno: Errno::ReadOnly,
            } => write!(
                f,
                "{}: the disk no longer matches the one the writes were made on; they are \
                 kept, and the volume is read-only.",
                quote_subject(name)
            ),
            Self::Answer {
                name,
                answer,
                errno,
            } => {
                let undone = match answer {
                    ReplayAnswer::Write => "written",
                    ReplayAnswer::Discard => "discarded",
                };
                write!(
                    f,
                    "{}: held writes not {undone}: {errno}.",
                    quote_subject(name)
                )
            }
            Self::Service(errno) => write!(f, "system information service error: {errno}"),
            Self::Output(errno) => write!(f, "terminal write failed: {errno}"),
        }
    }
}
