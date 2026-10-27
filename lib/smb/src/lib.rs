//! The SMB 3 protocol engine (`plans/SMB.md`).
//!
//! Pure: no I/O, no clock, no randomness. Every state machine is driven by
//! typed calls and answers with typed results, so the code the tests and fuzz
//! harnesses exercise is the code the sandboxed `smbfs` worker runs — a
//! process that can reach neither a socket nor an RNG. What the protocol needs
//! from either arrives as input: the bytes the socket read, and the random
//! values a request needs passed with it.
//!
//! Only dialects 3.0, 3.0.2 and 3.1.1 are spoken, and only encrypted: what a
//! server offers below that floor is refused with a [`Refusal`] naming why.
//!
//! # Modules
//!
//! - [`wire`] — the little-endian field codec every message is built from.
//! - [`status`] — NT status codes.
//! - [`header`] — the 64-byte SMB2 header and the command set.
//! - [`negotiate`] — dialects, capabilities, negotiate contexts, and
//!   validate-negotiate.
//! - [`session`] — `SESSION_SETUP`.
//! - [`tree`] — `TREE_CONNECT`.
//! - [`ioctl`] — `IOCTL` and the control codes this client issues.
//! - [`simple`] — the four-byte bodies of `LOGOFF`, `TREE_DISCONNECT`,
//!   `ECHO` and `CANCEL`, and the error response.
//! - [`crypt`] — key derivation, signing, the encryption transform and its
//!   nonces, and the pre-auth integrity hash.
//! - [`frame`] — the Direct TCP framing and compounds.
//! - [`connection`] — the client connection core: credits, message ids,
//!   sealing on the way out, opening and matching on the way in.
//! - [`ntlm`] — NTLMv2, the initiator.
//! - [`spnego`] — the negotiation SMB carries its authentication tokens in.
//! - [`client`] — one principal's connection: negotiation, the session's
//!   authentication, and its trees.
//! - [`file`](mod@file) — what is done on a tree: create and close, read, write and
//!   flush, directory listings, file information, change notification,
//!   leases and durable handles.
//! - [`rpc`] — DCE/RPC over a named pipe, and listing a server's shares
//!   through its `srvsvc` pipe on `IPC$`.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

/// A set of bit flags over one unsigned integer, as a field carries it.
macro_rules! flags {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident($bits:ty) {
            $( $(#[$flag_meta:meta])* const $flag:ident = $value:expr; )*
        }
    ) => {
        $(#[$meta])*
        #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
        $vis struct $name($bits);

        impl $name {
            $( $(#[$flag_meta])* pub const $flag: Self = Self($value); )*

            /// No flag set.
            pub const NONE: Self = Self(0);

            /// The flags of raw value `bits`.
            #[must_use]
            pub const fn from_bits(bits: $bits) -> Self {
                Self(bits)
            }

            /// The raw value.
            #[must_use]
            pub const fn bits(self) -> $bits {
                self.0
            }

            /// These flags with `other` set too.
            #[must_use]
            pub const fn with(self, other: Self) -> Self {
                Self(self.0 | other.0)
            }

            /// These flags with `other` clear.
            #[must_use]
            pub const fn without(self, other: Self) -> Self {
                Self(self.0 & !other.0)
            }

            /// Whether every flag of `other` is set.
            #[must_use]
            pub const fn contains(self, other: Self) -> bool {
                self.0 & other.0 == other.0
            }
        }
    };
}

pub mod client;
pub mod connection;
pub mod crypt;
pub mod file;
pub mod frame;
pub mod header;
pub mod ioctl;
pub mod negotiate;
pub mod ntlm;
pub mod rpc;
pub mod session;
pub mod simple;
pub mod spnego;
pub mod status;
pub mod tree;
pub mod wire;

#[cfg(any(test, feature = "test-support"))]
pub mod peer;

#[cfg(test)]
mod vectors;

use core::fmt;

use status::NtStatus;
use tairix_der::DerError;

/// Which end of a connection an engine is.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Role {
    /// The end that dialled and sends requests.
    Client,
    /// The end that answers.
    Server,
}

/// Why the client will not go on with what a server offered.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Refusal {
    /// The server offered no dialect at or above the floor.
    Dialect,
    /// The server cannot encrypt, or offered no cipher this client accepts.
    Encryption,
    /// The server negotiated no signing algorithm this client accepts.
    Signing,
    /// The server offered a guest or anonymous session, which cannot be
    /// protected.
    Guest,
    /// The server's pre-auth integrity choice is one this client did not
    /// offer.
    Integrity,
    /// The server's validate-negotiate answer disagrees with what was
    /// negotiated: a downgrade.
    Downgrade,
    /// A 3.0 or 3.0.2 server answered validate-negotiate with an error, so
    /// the negotiation cannot be shown untampered.
    Validation,
    /// The server offers no authentication mechanism this client holds a
    /// credential for.
    Mechanism,
    /// The server will not agree to NTLMv2 with extended session security,
    /// target information and 128-bit keys.
    Ntlm,
}

/// Why an operation failed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SmbError {
    /// A message or field is malformed: short, an offset outside its message,
    /// a structure size that is not the command's.
    Malformed,
    /// The server broke the protocol: an unexpected message, a choice it was
    /// not offered, a response to nothing outstanding.
    Protocol,
    /// The server offered less than this client accepts.
    Refused(Refusal),
    /// A signature or an encrypted message's tag did not verify, or a
    /// message that must be protected arrived without protection.
    Integrity,
    /// The server answered with this status.
    Status(NtStatus),
    /// A buffer could not grow.
    OutOfMemory,
    /// A field or message is past a fixed bound.
    TooLarge,
    /// No credit or message id is free to send with.
    NoCredit,
    /// A time the protocol cannot carry exactly: before 1601, past
    /// FILETIME's range, or finer than its 100 nanoseconds.
    TimeOutOfRange,
    /// A name or path one side cannot carry: empty, a dot name, past a length
    /// bound, or holding a character TAIRiX or SMB reserves.
    Name,
    /// A remote call failed with this status: an RPC fault, a refused bind's
    /// reason, or the call's own result.
    Rpc(u32),
}

impl fmt::Display for SmbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed => f.write_str("smb: malformed message"),
            Self::Protocol => f.write_str("smb: protocol violation"),
            Self::Refused(refusal) => f.write_str(match refusal {
                Refusal::Dialect => "smb: server speaks no dialect from 3.0 up",
                Refusal::Encryption => "smb: server cannot encrypt",
                Refusal::Signing => "smb: server offered no accepted signing algorithm",
                Refusal::Guest => "smb: server offered only a guest session",
                Refusal::Integrity => "smb: server chose an unoffered integrity algorithm",
                Refusal::Downgrade => "smb: negotiation was tampered with",
                Refusal::Validation => "smb: server would not confirm the negotiation",
                Refusal::Mechanism => "smb: server offers no usable authentication mechanism",
                Refusal::Ntlm => "smb: server refused NTLMv2's protections",
            }),
            Self::Integrity => f.write_str("smb: message failed its integrity check"),
            Self::Status(status) => write!(f, "smb: server status {:#010x}", status.code()),
            Self::OutOfMemory => f.write_str("smb: out of memory"),
            Self::TooLarge => f.write_str("smb: message too large"),
            Self::NoCredit => f.write_str("smb: no credit to send with"),
            Self::TimeOutOfRange => f.write_str("smb: time FILETIME cannot carry exactly"),
            Self::Name => f.write_str("smb: name TAIRiX and SMB cannot both carry"),
            Self::Rpc(status) => write!(f, "smb: remote call failed with status {status:#010x}"),
        }
    }
}

impl From<DerError> for SmbError {
    fn from(err: DerError) -> Self {
        match err {
            DerError::OutOfMemory => Self::OutOfMemory,
            DerError::TooLong | DerError::TooDeep => Self::TooLarge,
            _ => Self::Malformed,
        }
    }
}
