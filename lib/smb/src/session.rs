//! `SESSION_SETUP` (MS-SMB2 §2.2.5, §2.2.6).

use alloc::vec::Vec;

use crate::header::{structure, HEADER_LEN};
use crate::negotiate::{Capabilities, SecurityMode};
use crate::wire::{buffer_at, owned, Encoder};
use crate::SmbError;

/// The fixed part of a `SESSION_SETUP` request's body, which its security
/// buffer follows.
const REQUEST_FIXED: usize = 24;

/// The fixed part of a `SESSION_SETUP` response's body.
const RESPONSE_FIXED: usize = 8;

/// A `SESSION_SETUP` request: one leg of authentication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionSetupRequest {
    /// Whether the request binds a new channel to an existing session.
    pub binding: bool,
    /// The client's security mode.
    pub security_mode: SecurityMode,
    /// The capabilities, of which only DFS is meaningful here.
    pub capabilities: Capabilities,
    /// A session this one replaces after a reconnect, or zero.
    pub previous_session_id: u64,
    /// The security token: one SPNEGO leg.
    pub security_buffer: Vec<u8>,
}

impl SessionSetupRequest {
    /// Append the request's body after its header.
    pub fn encode(&self, enc: &mut Encoder) {
        enc.u16(25);
        enc.u8(u8::from(self.binding));
        enc.u8(self.security_mode.bits().to_le_bytes()[0]);
        enc.u32(self.capabilities.bits());
        enc.u32(0);
        let offset = enc.u16_of(HEADER_LEN + REQUEST_FIXED);
        enc.u16(offset);
        let len = enc.u16_of(self.security_buffer.len());
        enc.u16(len);
        enc.u64(self.previous_session_id);
        enc.bytes(&self.security_buffer);
    }

    /// The request in `message`, header included.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when it is not a well-formed request.
    pub fn decode(message: &[u8]) -> Result<Self, SmbError> {
        let mut r = structure(message, 25)?;
        let binding = r.u8()? & 0x01 != 0;
        let security_mode = SecurityMode::from_bits(u16::from(r.u8()?));
        let capabilities = Capabilities::from_bits(r.u32()?);
        r.skip(4)?;
        let offset = usize::from(r.u16()?);
        let len = usize::from(r.u16()?);
        let previous_session_id = r.u64()?;
        Ok(Self {
            binding,
            security_mode,
            capabilities,
            previous_session_id,
            security_buffer: owned(buffer_at(message, offset, len)?)?,
        })
    }
}

flags! {
    /// A `SESSION_SETUP` response's flags (MS-SMB2 §2.2.6).
    pub struct SessionFlags(u16) {
        /// The session is a guest's.
        const IS_GUEST = 0x0001;
        /// The session is anonymous.
        const IS_NULL = 0x0002;
        /// The server requires the session's messages encrypted.
        const ENCRYPT_DATA = 0x0004;
    }
}

/// A `SESSION_SETUP` response: the server's leg of authentication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionSetupResponse {
    /// The session's flags.
    pub session_flags: SessionFlags,
    /// The security token: one SPNEGO leg.
    pub security_buffer: Vec<u8>,
}

impl SessionSetupResponse {
    /// Append the response's body after its header.
    pub fn encode(&self, enc: &mut Encoder) {
        enc.u16(9);
        enc.u16(self.session_flags.bits());
        let offset = enc.u16_of(HEADER_LEN + RESPONSE_FIXED);
        enc.u16(offset);
        let len = enc.u16_of(self.security_buffer.len());
        enc.u16(len);
        enc.bytes(&self.security_buffer);
    }

    /// The response in `message`, header included.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when it is not a well-formed response.
    pub fn decode(message: &[u8]) -> Result<Self, SmbError> {
        let mut r = structure(message, 9)?;
        let session_flags = SessionFlags::from_bits(r.u16()?);
        let offset = usize::from(r.u16()?);
        let len = usize::from(r.u16()?);
        Ok(Self {
            session_flags,
            security_buffer: owned(buffer_at(message, offset, len)?)?,
        })
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
