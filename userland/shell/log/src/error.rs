//! The outcomes of running a `log` command.

use core::fmt;

use tairix_abi::Errno;
use tairix_log::SegmentError;
use tairix_util::getopt::Refusal;

/// Why a `log` invocation did not complete.
///
/// The variants are deliberately coarse: the CLI surfaces enough to print a
/// useful diagnostic and set a process exit status, leaning on the stable
/// [`Errno`] and the log crate's [`SegmentError`] for the precise cause so it
/// invents no parallel error set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LogError {
    /// An option `getopt` refuses, worded as it words it.
    Option(Refusal),
    /// The command line did not name a known subcommand, or carried a stray
    /// argument or a value the subcommand refuses.
    Usage,
    /// A segment could not be read from the source. Carries the underlying
    /// [`Errno`].
    Read(Errno),
    /// A stored segment did not parse or verify as a valid segment image —
    /// a corrupt or truncated header/footer, a broken hash chain, or an
    /// invalid seal. Carries the underlying [`SegmentError`]. `log verify`
    /// reports this per segment and exits non-zero.
    Corrupt(SegmentError),
    /// A committed record body failed to decode against the record format.
    /// Carries the underlying [`Errno`]. A committed record is covered by the
    /// segment hash, so this means the segment was tampered with.
    Decode(Errno),
    /// Writing the rendered output failed. Carries the underlying [`Errno`].
    Output(Errno),
}

impl fmt::Display for LogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::Usage => f.write_str("invalid usage"),
            Self::Read(errno) => write!(f, "cannot read log segment: {errno}"),
            Self::Corrupt(err) => write!(f, "corrupt log segment: {err:?}"),
            Self::Decode(errno) => write!(f, "corrupt log record: {errno}"),
            Self::Output(errno) => write!(f, "output write failed: {errno}"),
        }
    }
}
