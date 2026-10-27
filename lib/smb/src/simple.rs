//! The messages whose body is four bytes — `LOGOFF`, `TREE_DISCONNECT`,
//! `ECHO` and `CANCEL`, each way (MS-SMB2 §2.2.7, §2.2.11, §2.2.28, §2.2.30)
//! — and the error response (MS-SMB2 §2.2.2).

use alloc::vec::Vec;

use crate::header::structure;
use crate::wire::{owned, Encoder, Reader};
use crate::SmbError;

/// Append a four-byte body: `StructureSize` 4 and two reserved bytes.
pub fn encode_empty(enc: &mut Encoder) {
    enc.u16(4);
    enc.u16(0);
}

/// Check that `message` carries a four-byte body.
///
/// # Errors
///
/// [`SmbError::Malformed`] otherwise.
pub fn decode_empty(message: &[u8]) -> Result<(), SmbError> {
    structure(message, 4)?.skip(2)
}

/// The most error data a response may carry before it is refused.
const MAX_ERROR_DATA: usize = 4096;

/// An error response's body: the error contexts or legacy error data, which
/// only a few statuses carry (a symbolic link's target, a share redirect).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ErrorResponse {
    /// How many error contexts `data` holds (3.1.1), or zero.
    pub context_count: u8,
    /// The error data.
    pub data: Vec<u8>,
}

impl ErrorResponse {
    /// Append the response's body after its header.
    pub fn encode(&self, enc: &mut Encoder) {
        enc.u16(9);
        enc.u8(self.context_count);
        enc.u8(0);
        let len = enc.u32_of(self.data.len());
        enc.u32(len);
        enc.buffer(&self.data);
    }

    /// The error response in `message`, header included.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when it is not a well-formed error response,
    /// [`SmbError::TooLarge`] past the error-data bound.
    pub fn decode(message: &[u8]) -> Result<Self, SmbError> {
        let mut r: Reader<'_> = structure(message, 9)?;
        let context_count = r.u8()?;
        r.skip(1)?;
        let len = usize::try_from(r.u32()?).map_err(|_| SmbError::Malformed)?;
        if len > MAX_ERROR_DATA {
            return Err(SmbError::TooLarge);
        }
        Ok(Self {
            context_count,
            data: owned(r.bytes(len)?)?,
        })
    }
}
