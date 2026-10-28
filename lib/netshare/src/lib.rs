//! TAIRiX network-share client: the `netshare-v1` requests a program makes of
//! the network-share service, `smbfs` (`plans/SMB.md` SMB8).
//!
//! [`Client`] asks an endpoint for its shares, has one mounted, withdraws a
//! mount, and lists the caller's own, over an injected [`Transport`]; the
//! `program` feature adds `RtTransport`, the service's endpoint called through
//! the kernel. Each request is built in a buffer erased when its call
//! returns, so the password a request carries outlives the exchange nowhere
//! on this side. An answer borrows the client's reply buffer until the next
//! request.
//!
//! A request is network work the service answers once it completes, so an
//! interactive surface makes it from a worker, never from its own loop.
//!
//! A mount's answer is an offer, not a volume: the caller attaches it itself
//! (`plans/FSSERVE.md` FSS6), choosing where it goes and how it is presented.
//! Every name an answer carries was written by the server: structurally
//! valid, shown only through the display-safety policy.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::vec::Vec;
use core::fmt;

use tairix_abi::fsserve::OfferId;
use tairix_abi::netshare_ipc::{
    Connect, MountEntry, NetshareError, NetshareReply, NetshareRequest, Pin, Records, Rejection,
    Shares, Shortfall, NETSHARE_MAX_REQUEST,
};
use tairix_abi::Errno;
use tairix_util::secret::Wiped;

#[cfg(all(feature = "program", target_os = "none"))]
mod rt;
#[cfg(all(feature = "program", target_os = "none"))]
pub use rt::RtTransport;

/// How the client reaches the service.
pub trait Transport {
    /// Send `request` and write the reply into `reply`, returning its length.
    ///
    /// # Errors
    ///
    /// The call's refusal; [`Errno::NotFound`] means no service is bound.
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno>;
}

/// Why a request did not succeed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    /// The request is one the protocol refuses — a host, account or share
    /// name out of shape, an address naming no one node, port zero — so it
    /// was never sent.
    Invalid,
    /// No network-share service is running.
    Unavailable,
    /// This program may not reach the network: it holds no `CAP_NET`, which
    /// the kernel requires of every caller.
    NotPermitted,
    /// The service refused the request, for the reason carried.
    Refused(NetshareError),
    /// The service could not read the request: it speaks another version of
    /// the protocol.
    Unreadable,
    /// The call itself failed.
    Transport(Errno),
    /// The reply was not one the service sends.
    Malformed,
    /// No buffer could be had for the reply.
    OutOfMemory,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid => f.write_str("the host, account or share named cannot be used"),
            Self::Unavailable => f.write_str("network shares are not available on this system"),
            Self::NotPermitted => f.write_str("this program may not reach the network"),
            Self::Refused(error) => f.write_str(refusal(*error)),
            Self::Unreadable => f.write_str("the network-share service could not read the request"),
            Self::Transport(errno) => {
                write!(f, "the network-share service could not be reached: {errno}")
            }
            Self::Malformed => f.write_str("the network-share service sent a reply it never sends"),
            Self::OutOfMemory => f.write_str("out of memory"),
        }
    }
}

const fn refusal(error: NetshareError) -> &'static str {
    match error {
        NetshareError::UnknownHost => "no such host",
        NetshareError::Unreachable => "the host cannot be reached",
        NetshareError::PinMismatch(_) => {
            "the host is not the one the saved sign-in is for: it is a new, unknown host"
        }
        NetshareError::Unsupported(Shortfall::Dialect) => {
            "the server speaks no SMB dialect as new as the one asked for"
        }
        NetshareError::Unsupported(Shortfall::Encryption) => "the server cannot encrypt",
        NetshareError::Unsupported(Shortfall::Signing) => {
            "the server offers no signing this system accepts"
        }
        NetshareError::Unsupported(Shortfall::Authentication) => {
            "the server offers no sign-in this system can use"
        }
        NetshareError::Unsupported(Shortfall::Guest) => "the server offers only guest access",
        NetshareError::Tampered => "the connection was tampered with on the way",
        NetshareError::Protocol => "the server broke the SMB protocol",
        NetshareError::TooLarge => "the server answered with more than this system accepts",
        NetshareError::Rejected(Rejection::Credentials) => "the user name or password is wrong",
        NetshareError::Rejected(Rejection::Disabled) => "the account is disabled",
        NetshareError::Rejected(Rejection::Locked) => "the account is locked out",
        NetshareError::Rejected(Rejection::Expired) => "the account's password has expired",
        NetshareError::Rejected(Rejection::Restricted) => {
            "the account may not sign in now, or from here"
        }
        NetshareError::NoSuchShare => "the server has no such share",
        NetshareError::Denied => "the server refused the account access",
        NetshareError::NoSession => "a mount needs a login session to end with",
        NetshareError::NoSuchMount => "no such mount",
        NetshareError::Limit => "no more mounts or connections may be made",
        NetshareError::Failed => "the network-share service failed",
    }
}

/// A share the service serves, waiting to be attached.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Mounted {
    /// What the connection reported of the endpoint: what a credential the
    /// user asks to remember is remembered under.
    pub pin: Pin,
    /// The offer the caller attaches.
    pub offer: OfferId,
}

/// A client of the network-share service.
pub struct Client<T: Transport> {
    transport: T,
    reply: Vec<u8>,
}

impl<T: Transport> Client<T> {
    /// A client calling through `transport`.
    #[must_use]
    pub const fn new(transport: T) -> Self {
        Self {
            transport,
            reply: Vec::new(),
        }
    }

    /// The disk shares `connect`'s endpoint offers, the special ones only
    /// when `special` is set.
    ///
    /// # Errors
    ///
    /// As [`Error`]; the service's reasons are [`Error::Refused`].
    pub fn shares(&mut self, connect: Connect<'_>, special: bool) -> Result<Shares<'_>, Error> {
        match self.ask(&NetshareRequest::Shares { connect, special })? {
            NetshareReply::Shares(shares) => Ok(shares),
            _ => Err(Error::Malformed),
        }
    }

    /// Have `share` on `connect`'s endpoint served, read-only when
    /// `read_only` is set, and offered to this caller.
    ///
    /// # Errors
    ///
    /// As [`Self::shares`].
    pub fn mount(
        &mut self,
        connect: Connect<'_>,
        share: &str,
        read_only: bool,
    ) -> Result<Mounted, Error> {
        let request = NetshareRequest::Mount {
            connect,
            share,
            read_only,
        };
        match self.ask(&request)? {
            NetshareReply::Mounted { pin, offer } => Ok(Mounted { pin, offer }),
            _ => Err(Error::Malformed),
        }
    }

    /// Withdraw the mount `offer` names, which this caller made.
    ///
    /// # Errors
    ///
    /// As [`Self::shares`]; a mount this caller did not make is
    /// [`NetshareError::NoSuchMount`].
    pub fn unmount(&mut self, offer: OfferId) -> Result<(), Error> {
        match self.ask(&NetshareRequest::Unmount { offer })? {
            NetshareReply::Unmounted => Ok(()),
            _ => Err(Error::Malformed),
        }
    }

    /// The mounts this caller made.
    ///
    /// # Errors
    ///
    /// As [`Self::shares`].
    pub fn mounts(&mut self) -> Result<Records<'_, MountEntry<'_>>, Error> {
        match self.ask(&NetshareRequest::Mounts)? {
            NetshareReply::Mounts(mounts) => Ok(mounts),
            _ => Err(Error::Malformed),
        }
    }

    /// Make `request`'s call and return the answer, or why there is none.
    fn ask(&mut self, request: &NetshareRequest<'_>) -> Result<NetshareReply<'_>, Error> {
        let op = request.op();
        let mut frame = Wiped::<NETSHARE_MAX_REQUEST>::new();
        let len = request.encode(&mut *frame).map_err(|_| Error::Invalid)?;
        let reply = reply_buffer(&mut self.reply, op.max_reply())?;
        let got = self
            .transport
            .call(frame.get(..len).ok_or(Error::Invalid)?, reply)
            .map_err(|errno| match errno {
                Errno::NotFound => Error::Unavailable,
                Errno::PermissionDenied => Error::NotPermitted,
                errno => Error::Transport(errno),
            })?;
        frame.wipe();
        let reply = reply.get(..got).ok_or(Error::Malformed)?;
        match NetshareReply::decode(reply).map_err(|_| Error::Malformed)? {
            NetshareReply::Refused { op: refused, error } if refused == op => {
                Err(Error::Refused(error))
            }
            NetshareReply::Refused { .. } => Err(Error::Malformed),
            NetshareReply::Unreadable => Err(Error::Unreadable),
            answer => Ok(answer),
        }
    }
}

/// `buffer`'s first `len` bytes, grown to hold them once and kept after.
fn reply_buffer(buffer: &mut Vec<u8>, len: usize) -> Result<&mut [u8], Error> {
    if let Some(more) = len.checked_sub(buffer.len()).filter(|&more| more > 0) {
        buffer
            .try_reserve_exact(more)
            .map_err(|_| Error::OutOfMemory)?;
        buffer.resize(len, 0);
    }
    buffer.get_mut(..len).ok_or(Error::OutOfMemory)
}

#[cfg(test)]
mod tests;
