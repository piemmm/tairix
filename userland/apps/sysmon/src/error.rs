//! The outcomes of running the `sysmon` monitor.

use core::fmt;

use tairix_curses::CursesError;
use tairix_util::getopt::Refusal;

/// Why a `sysmon` session ended other than by the user quitting.
///
/// The variants are deliberately few: every System Information query the
/// monitor issues degrades in place (a capability refusal or a service
/// failure renders as that panel's stated reason while the session
/// continues — observing a machine under stress is the tool's purpose, so
/// a hiccuping service must never kill the observer). Only the terminal
/// itself is load-bearing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SysmonError {
    /// An option `getopt` refuses, worded as it words it. The monitor never
    /// starts.
    Option(Refusal),
    /// The command line carried an operand or a malformed delay. The monitor
    /// never starts.
    Usage,
    /// Drawing to or reading from the terminal failed.
    Terminal(CursesError),
}

impl From<CursesError> for SysmonError {
    fn from(err: CursesError) -> Self {
        Self::Terminal(err)
    }
}

impl fmt::Display for SysmonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::Usage => f.write_str("invalid usage"),
            Self::Terminal(err) => write!(f, "terminal error: {err:?}"),
        }
    }
}
