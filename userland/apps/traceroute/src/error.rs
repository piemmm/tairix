//! Why a `traceroute` run ended early.

use alloc::string::String;
use core::fmt;

use tairix_abi::Errno;
use tairix_net::trace::TraceConfigError;
use tairix_resolver::TargetFailure;

/// A failure that defeats the trace, reported with its reason on standard
/// error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TraceError {
    /// The target did not resolve.
    Resolve {
        /// The target operand as typed.
        host: String,
        /// Why it named nothing.
        reason: TargetFailure,
    },
    /// The probing parameters cannot be traced.
    Config(TraceConfigError),
    /// The echo socket was refused for want of `CAP_NET`/`CAP_NET_RAW`.
    Denied,
    /// The echo socket could not be opened or connected.
    Socket(Errno),
    /// A probe could not be sent.
    Send(Errno),
    /// An answer could not be received.
    Receive(Errno),
    /// A line could not be written.
    Output(Errno),
}

impl fmt::Display for TraceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Resolve { host, reason } => write!(f, "{host}: {reason}"),
            Self::Config(error) => write!(f, "the probes cannot be configured: {error}"),
            Self::Denied => f.write_str(
                "opening an ICMP echo socket requires CAP_NET and CAP_NET_RAW, \
                 which this session lacks",
            ),
            Self::Socket(errno) => write!(f, "cannot open the echo socket: {errno}"),
            Self::Send(errno) => write!(f, "send: {errno}"),
            Self::Receive(errno) => write!(f, "recvmsg: {errno}"),
            Self::Output(errno) => write!(f, "cannot write output: {errno}"),
        }
    }
}
