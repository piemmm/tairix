//! The outcomes of running a `useradd` command.

use core::fmt;
use tairix_abi::Errno;
use tairix_util::getopt::Refusal;

/// Why a `useradd` invocation did not complete.
///
/// The variants are deliberately coarse: the CLI surfaces enough to print a
/// useful diagnostic and set a process exit status, while leaning on the
/// frozen [`Errno`] for the wire-level cause so it invents no parallel error
/// set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UseraddError {
    /// An option `getopt` refuses, worded as it words it. Nothing is
    /// created.
    Option(Refusal),
    /// The command line omitted the required primary group (`-g`), or did
    /// not name exactly one account. Nothing is created.
    Usage,
    /// The login name is not a valid `[a-z_][a-z0-9_-]*` within the length
    /// bound ([`MAX_NAME_LEN`](crate::MAX_NAME_LEN)). Nothing is created.
    BadName,
    /// A `-u`, `-g`, or `-G` value was not a decimal id (empty, non-digit, or
    /// overflowing a [`u32`]), or the `-G` list had an empty element. Nothing
    /// is created.
    BadId,
    /// The login name is already present in the database. Nothing is created.
    Exists,
    /// Consulting the database for the name failed. Carries the underlying
    /// [`Errno`] — e.g. [`Errno::PermissionDenied`] when the caller may not
    /// read the user database.
    Lookup(Errno),
    /// Creating the account failed. Carries the underlying [`Errno`] — e.g.
    /// [`Errno::PermissionDenied`] when the caller lacks `CAP_USER_ADMIN`,
    /// [`Errno::NotFound`] when a referenced group does not exist, or
    /// [`Errno::LengthOutOfRange`] when the supplementary set is too large.
    Create(Errno),
    /// Writing the usage banner to the terminal failed. Carries the underlying
    /// [`Errno`].
    Output(Errno),
}

impl fmt::Display for UseraddError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::Usage => f.write_str("invalid usage"),
            Self::BadName => f.write_str("invalid user name"),
            Self::BadId => f.write_str("invalid numeric id"),
            Self::Exists => f.write_str("user already exists"),
            Self::Lookup(errno) => write!(f, "cannot read user database: {errno}"),
            Self::Create(errno) => write!(f, "cannot create user: {errno}"),
            Self::Output(errno) => write!(f, "terminal write failed: {errno}"),
        }
    }
}
